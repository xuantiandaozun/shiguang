//! 桌面录屏：整块屏幕、指定软件窗口或屏幕上的一块区域。
//!
//! - 画面：GDI BitBlt 按固定帧率采集（拾光自己的窗口用 WDA_EXCLUDEFROMCAPTURE 排除，可选包含）。
//!   画面没变化就不存帧；变化很小（光标闪烁、时钟跳秒）的帧标为 minor，紧凑节奏不把它算作「有动静」。
//! - 事件：轮询鼠标位置、按键状态和前台窗口。按键只记「按了键」，不记按了什么。
//!   点击目标用 UI Automation 取名称和边框（给自动放大和 AI 写解说用）。
//! - 产出和网页录制一样：frames/*.jpg + 事件消息，后期处理完全复用。

use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

/// 要录的范围（虚拟屏幕物理像素）
#[derive(Debug, Clone)]
pub struct Target {
    /// screen | window | region
    pub kind: &'static str,
    pub rect: [i32; 4],
    pub hwnd: Option<i64>,
    pub title: String,
    pub process: String,
}

impl Target {
    pub fn describe(&self) -> String {
        match self.kind {
            "window" => format!("窗口：{}（{}）", self.title, self.process),
            "region" => format!("屏幕区域 {}×{}", self.rect[2], self.rect[3]),
            _ => format!("屏幕 {}×{}", self.rect[2], self.rect[3]),
        }
    }
}

/// 帧尺寸取偶数（编码器要求），太小的拒绝
fn normalize(mut r: [i32; 4]) -> Result<[i32; 4]> {
    r[2] &= !1;
    r[3] &= !1;
    if r[2] < 64 || r[3] < 64 {
        bail!("录制范围太小（至少 64×64）");
    }
    Ok(r)
}

/// 整块屏幕：monitor 为 1 起的序号；不给时取鼠标所在的屏幕
pub fn screen_target(monitor: Option<usize>) -> Result<Target> {
    let mons = platform::monitors();
    if mons.is_empty() {
        bail!("没有找到显示器");
    }
    let rect = match monitor {
        Some(n) if n >= 1 && n <= mons.len() => mons[n - 1],
        Some(n) => bail!("没有第 {n} 块屏幕（共 {} 块）", mons.len()),
        None => platform::monitor_at_cursor().unwrap_or(mons[0]),
    };
    Ok(Target { kind: "screen", rect: normalize(rect)?, hwnd: None, title: "屏幕".into(), process: String::new() })
}

/// 指定软件窗口（按标题或进程名）。最小化的会先还原；录制前把它切到前台
pub fn window_target(query: &str, include_self: bool) -> Result<Target> {
    let (mut w, _) = super::screenshot::find_window(query, include_self)?;
    if w.minimized {
        platform::restore(w.hwnd as isize);
        std::thread::sleep(std::time::Duration::from_millis(400));
        if let Some(r) = platform::window_rect(w.hwnd as isize) {
            w.rect = r;
        }
    }
    platform::bring_to_front(w.hwnd as isize);
    std::thread::sleep(std::time::Duration::from_millis(150));
    Ok(Target { kind: "window", rect: normalize(w.rect)?, hwnd: Some(w.hwnd), title: w.title, process: w.process })
}

/// 从框选结果（screenshot::select_area 的返回）得到录制范围
pub fn from_selection(sel: &Value) -> Result<Target> {
    let rect = sel
        .get("rect")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).map(|v| v as i32).collect::<Vec<_>>())
        .filter(|a| a.len() == 4)
        .ok_or_else(|| anyhow::anyhow!("选区无效"))?;
    let rect = [rect[0], rect[1], rect[2], rect[3]];
    match sel.get("kind").and_then(Value::as_str) {
        Some("window") => {
            let hwnd = sel.get("hwnd").and_then(Value::as_i64);
            if let Some(h) = hwnd {
                platform::bring_to_front(h as isize);
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            Ok(Target {
                kind: "window",
                rect: normalize(hwnd.and_then(|h| platform::window_rect(h as isize)).unwrap_or(rect))?,
                hwnd,
                title: sel.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
                process: sel.get("process").and_then(Value::as_str).unwrap_or("").to_string(),
            })
        }
        _ => region_target(rect),
    }
}

pub fn region_target(rect: [i32; 4]) -> Result<Target> {
    Ok(Target { kind: "region", rect: normalize(rect)?, hwnd: None, title: "区域".into(), process: String::new() })
}

pub struct Options {
    pub fps: u32,
    pub include_self: bool,
}

struct Shared {
    stop: AtomicBool,
    /// 当前采集范围（窗口移动时跟着变，尺寸不变）
    rect: Mutex<[i32; 4]>,
    include_self: bool,
    own_pid: u32,
    tx: UnboundedSender<Value>,
}

impl Shared {
    fn emit(&self, v: Value) {
        let _ = self.tx.send(json!({"event": "rec_event", "payload": v.to_string()}));
    }
    fn end(&self, reason: &str) {
        let _ = self.tx.send(json!({"event": "rec_end", "reason": reason}));
    }
}

pub struct Session {
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Session {
    /// 停止并等所有线程收尾（会阻塞，放在 spawn_blocking 里调用）
    pub fn stop(self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        for t in self.threads {
            let _ = t.join();
        }
    }
}

fn now_ms() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64
}

/// 帧边长上限：超过就按整数倍缩小（4K 屏缩成 1920 宽），控制编码耗时和磁盘占用
const MAX_W: i32 = 2560;
const MAX_H: i32 = 1600;

fn downscale_factor(w: i32, h: i32) -> u32 {
    ((w as f64 / MAX_W as f64).max(h as f64 / MAX_H as f64).ceil() as u32).max(1)
}

/// 开始录制。画面写到 dir/frames，帧与事件通过 tx 交给 write_loop
pub fn start(target: &Target, dir: PathBuf, opts: Options, tx: UnboundedSender<Value>) -> Result<Session> {
    if !cfg!(windows) {
        bail!("桌面录屏目前只支持 Windows");
    }
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        rect: Mutex::new(target.rect),
        include_self: opts.include_self,
        own_pid: std::process::id(),
        tx,
    });
    let mut threads = Vec::new();
    {
        let (sh, t, fps) = (shared.clone(), target.clone(), opts.fps.clamp(5, 30));
        threads.push(std::thread::Builder::new().name("desk-capture".into()).spawn(move || capture_loop(sh, t, dir, fps))?);
    }
    {
        let sh = shared.clone();
        threads.push(std::thread::Builder::new().name("desk-input".into()).spawn(move || input_loop(sh))?);
    }
    Ok(Session { shared, threads })
}

// ---------- 画面 ----------

struct Frame {
    i: u64,
    t: f64,
    w: i32,
    h: i32,
    bgra: Arc<Vec<u8>>,
    minor: bool,
}

/// 两帧不同的像素数，数到 cap 为止（够判断是不是「大变化」就停）
fn diff_count(a: &[u8], b: &[u8], cap: usize) -> usize {
    if a.len() != b.len() {
        return usize::MAX;
    }
    let mut n = 0;
    for (ca, cb) in a.chunks(256).zip(b.chunks(256)) {
        if ca == cb {
            continue;
        }
        for (pa, pb) in ca.chunks_exact(4).zip(cb.chunks_exact(4)) {
            if pa[..3] != pb[..3] {
                n += 1;
            }
        }
        if n >= cap {
            return n;
        }
    }
    n
}

/// BGRA → RGB，按 f 倍做方块平均缩小
fn to_rgb(bgra: &[u8], w: i32, h: i32, f: u32) -> (Vec<u8>, u32, u32) {
    let (w, h, f) = (w as usize, h as usize, f as usize);
    if f <= 1 {
        let mut out = Vec::with_capacity(w * h * 3);
        for px in bgra.chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0]]);
        }
        return (out, w as u32, h as u32);
    }
    let (ow, oh) = (w / f, h / f);
    let mut out = Vec::with_capacity(ow * oh * 3);
    let n = (f * f) as u32;
    for oy in 0..oh {
        for ox in 0..ow {
            let mut acc = [0u32; 3];
            for dy in 0..f {
                let row = (oy * f + dy) * w;
                for dx in 0..f {
                    let p = (row + ox * f + dx) * 4;
                    acc[0] += bgra[p + 2] as u32;
                    acc[1] += bgra[p + 1] as u32;
                    acc[2] += bgra[p] as u32;
                }
            }
            out.extend_from_slice(&[(acc[0] / n) as u8, (acc[1] / n) as u8, (acc[2] / n) as u8]);
        }
    }
    (out, ow as u32, oh as u32)
}

fn encode_jpeg(rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(rgb.len() / 8);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode(rgb, w, h, image::ExtendedColorType::Rgb8)?;
    Ok(out)
}

fn encoder_loop(rx: Arc<Mutex<std::sync::mpsc::Receiver<Frame>>>, dir: PathBuf, sh: Arc<Shared>) {
    loop {
        let frame = match rx.lock().unwrap().recv() {
            Ok(f) => f,
            Err(_) => break,
        };
        let f = downscale_factor(frame.w, frame.h);
        let (rgb, w, h) = to_rgb(&frame.bgra, frame.w, frame.h, f);
        let r = encode_jpeg(&rgb, w, h).and_then(|bytes| Ok(std::fs::write(dir.join("frames").join(format!("{:06}.jpg", frame.i)), bytes)?));
        match r {
            Ok(()) => {
                let _ = sh.tx.send(json!({"event": "rec_frame_saved", "i": frame.i, "t": frame.t, "w": w, "h": h, "minor": frame.minor}));
            }
            Err(e) => log::warn!("保存录屏画面失败: {e:#}"),
        }
    }
}

fn capture_loop(sh: Arc<Shared>, target: Target, dir: PathBuf, fps: u32) {
    platform::dpi_thread();
    let [_, _, w, h] = target.rect;
    let mut grabber = match platform::Grabber::new(w, h) {
        Ok(g) => g,
        Err(e) => {
            sh.end(&format!("无法采集屏幕：{e:#}"));
            return;
        }
    };
    let (enc_tx, enc_rx) = std::sync::mpsc::sync_channel::<Frame>(4);
    let enc_rx = Arc::new(Mutex::new(enc_rx));
    let encoders: Vec<_> = (0..3)
        .filter_map(|k| {
            let (rx, dir, sh) = (enc_rx.clone(), dir.clone(), sh.clone());
            std::thread::Builder::new().name(format!("desk-encode-{k}")).spawn(move || encoder_loop(rx, dir, sh)).ok()
        })
        .collect();
    let interval = std::time::Duration::from_secs_f64(1.0 / fps as f64);
    // 小于这么多像素的变化算 minor（约 0.05% 画面，比如输入光标闪烁）
    let minor_cap = ((w as usize * h as usize) / 2000).max(64);
    let mut prev: Option<Arc<Vec<u8>>> = None;
    let mut index = 0u64;
    let mut next = std::time::Instant::now();
    while !sh.stop.load(Ordering::SeqCst) {
        let now = std::time::Instant::now();
        if now < next {
            std::thread::sleep(next - now);
        }
        next += interval;
        if next < std::time::Instant::now() {
            next = std::time::Instant::now();
        }
        let mut rect = *sh.rect.lock().unwrap();
        if let Some(hwnd) = target.hwnd {
            match platform::window_state(hwnd as isize) {
                platform::WinState::Gone => {
                    sh.end("录制的窗口已关闭");
                    break;
                }
                platform::WinState::Minimized => continue,
                platform::WinState::At(x, y) => {
                    rect[0] = x;
                    rect[1] = y;
                    *sh.rect.lock().unwrap() = rect;
                }
            }
        }
        let t = now_ms();
        let buf = match grabber.grab(rect[0], rect[1]) {
            Ok(b) => Arc::new(b),
            Err(e) => {
                log::debug!("采集画面失败: {e:#}");
                continue;
            }
        };
        let changed = prev.as_ref().map(|p| diff_count(p, &buf, minor_cap)).unwrap_or(usize::MAX);
        if changed == 0 {
            continue;
        }
        let frame = Frame { i: index + 1, t, w, h, bgra: buf.clone(), minor: changed < minor_cap };
        // 编码跟不上就丢这一帧；prev 不更新，下一轮会再次检测到变化
        if enc_tx.try_send(frame).is_ok() {
            index += 1;
            prev = Some(buf);
        }
    }
    drop(enc_tx);
    for e in encoders {
        let _ = e.join();
    }
}

// ---------- 鼠标、按键、窗口 ----------

struct Click {
    t: f64,
    x: i32,
    y: i32,
    rect: [i32; 4],
}

/// 不记录的按键：鼠标键、Shift/Ctrl/Alt/Win（单独按修饰键不算输入）
fn ignored_key(vk: i32) -> bool {
    matches!(vk, 0x01..=0x07 | 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

fn input_loop(sh: Arc<Shared>) {
    platform::dpi_thread();
    // 点击目标识别放在单独线程：UI Automation 跨进程查询可能要几十到几百毫秒
    let (click_tx, click_rx) = std::sync::mpsc::sync_channel::<Click>(8);
    let uia = {
        let sh = sh.clone();
        std::thread::Builder::new().name("desk-uia".into()).spawn(move || uia_loop(sh, click_rx)).ok()
    };
    let rel = |t: f64, kind: &str, x: i32, y: i32, r: [i32; 4]| json!({"t": t, "type": kind, "x": x - r[0], "y": y - r[1], "vw": r[2], "vh": r[3]});
    let mut last_pos = (i32::MIN, i32::MIN);
    let mut last_move = 0.0;
    let mut inside = false;
    let mut on_self = false;
    let mut buttons = [false; 3];
    let mut down_at: [Option<(i32, i32)>; 3] = [None; 3];
    let mut keys = [false; 256];
    let mut last_key = 0.0;
    let mut last_fg: isize = 0;
    let mut tick: u64 = 0;
    while !sh.stop.load(Ordering::SeqCst) {
        let t = now_ms();
        let r = *sh.rect.lock().unwrap();
        let (x, y) = platform::cursor_pos();
        let in_rect = x >= r[0] && y >= r[1] && x < r[0] + r[2] && y < r[1] + r[3];
        if (x, y) != last_pos {
            // 光标停在拾光自己的窗口上（窗口没录进画面）：当作离开
            on_self = !sh.include_self && in_rect && platform::pid_at(x, y) == sh.own_pid;
        }
        let now_inside = in_rect && !on_self;
        if now_inside {
            if (x, y) != last_pos && t - last_move >= 30.0 {
                sh.emit(rel(t, "move", x, y, r));
                last_move = t;
                last_pos = (x, y);
            }
        } else if inside {
            sh.emit(json!({"t": t, "type": "leave"}));
            last_pos = (x, y);
        } else {
            last_pos = (x, y);
        }
        inside = now_inside;

        for (k, (vk, button)) in [(0x01, 0), (0x04, 1), (0x02, 2)].into_iter().enumerate() {
            let down = platform::key_down(vk);
            if down && !buttons[k] {
                down_at[k] = None;
                if now_inside {
                    let mut v = rel(t, "down", x, y, r);
                    v["button"] = json!(button);
                    sh.emit(v);
                    down_at[k] = Some((x, y));
                }
            } else if !down && buttons[k] {
                if let Some((dx, dy)) = down_at[k].take() {
                    let mut v = rel(t, "up", x, y, r);
                    v["button"] = json!(button);
                    sh.emit(v);
                    // 左键按下抬起位置相近才算点击（拖动不算）
                    if button == 0 && now_inside && (x - dx).abs() <= 8 && (y - dy).abs() <= 8 {
                        let c = Click { t, x, y, rect: r };
                        if let Err(std::sync::mpsc::TrySendError::Full(c) | std::sync::mpsc::TrySendError::Disconnected(c)) = click_tx.try_send(c) {
                            sh.emit(rel(c.t, "click", c.x, c.y, c.rect));
                        }
                    }
                }
            }
            buttons[k] = down;
        }

        tick += 1;
        // 按键：每 30ms 扫一次，只记有输入发生（不记内容），最多 10 次/秒
        if tick % 3 == 0 {
            let fg_self = !sh.include_self && platform::window_pid(platform::foreground()) == sh.own_pid;
            for vk in 0x08..0xFF {
                if ignored_key(vk) {
                    continue;
                }
                let d = platform::key_down(vk);
                if d && !keys[vk as usize] && !fg_self && t - last_key >= 100.0 {
                    sh.emit(json!({"t": t, "type": "key"}));
                    last_key = t;
                }
                keys[vk as usize] = d;
            }
        }
        // 前台窗口切换（给 AI 写解说用）
        if tick % 25 == 1 {
            let fg = platform::foreground();
            if fg != 0 && fg != last_fg {
                last_fg = fg;
                let pid = platform::window_pid(fg);
                if sh.include_self || pid != sh.own_pid {
                    let title = platform::window_title(fg);
                    if !title.is_empty() {
                        sh.emit(json!({"t": t, "type": "window", "title": title, "process": platform::process_name(pid)}));
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(click_tx);
    if let Some(u) = uia {
        let _ = u.join();
    }
}

fn control_name(id: i32) -> &'static str {
    match id {
        50000 => "按钮",
        50002 => "复选框",
        50003 => "下拉框",
        50004 => "输入框",
        50005 => "链接",
        50007 => "列表项",
        50011 => "菜单项",
        50013 => "单选框",
        50019 => "标签页",
        50024 => "树节点",
        50029 => "单元格",
        50031 => "分割按钮",
        _ => "",
    }
}

fn uia_loop(sh: Arc<Shared>, rx: std::sync::mpsc::Receiver<Click>) {
    platform::dpi_thread();
    let uia = platform::Uia::new();
    if uia.is_none() {
        log::info!("UI Automation 不可用，点击不带目标信息");
    }
    for c in rx {
        let mut v = json!({"t": c.t, "type": "click", "x": c.x - c.rect[0], "y": c.y - c.rect[1], "vw": c.rect[2], "vh": c.rect[3]});
        if let Some(el) = uia.as_ref().and_then(|u| u.element_at(c.x, c.y)) {
            let kind = control_name(el.control);
            let name: String = el.name.trim().chars().take(30).collect();
            let label = match (name.is_empty(), kind.is_empty()) {
                (false, false) => format!("{name}（{kind}）"),
                (false, true) => name,
                (true, _) => kind.to_string(),
            };
            let mut target = json!({"label": label});
            if let Some([l, t, w, h]) = el.rect {
                // 元素框比录制范围还大（整块面板）就不要了
                if w > 0 && h > 0 && w <= c.rect[2] && h <= c.rect[3] {
                    target["bbox"] = json!([l - c.rect[0], t - c.rect[1], w, h]);
                }
            }
            v["target"] = target;
            if el.control == 50004 {
                // 点进输入框：和网页一样当作聚焦，后续输入时保持放大
                let mut f = v.clone();
                f["type"] = json!("focus");
                f["t"] = json!(c.t + 1.0);
                sh.emit(v);
                sh.emit(f);
                continue;
            }
        }
        sh.emit(v);
    }
}

// ---------- Windows 实现 ----------

#[cfg(windows)]
mod platform {
    use anyhow::{anyhow, bail, Result};
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EnumDisplayMonitors, GdiFlush, GetDC, GetMonitorInfoW,
        MonitorFromPoint, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC, HGDIOBJ, HMONITOR,
        MONITORINFO, MONITOR_DEFAULTTONULL, SRCCOPY,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetCursorPos, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, SetForegroundWindow,
        ShowWindow, WindowFromPoint, GA_ROOT, SW_RESTORE,
    };

    pub use crate::recorder::screenshot::win::process_name;

    /// 本线程切到每显示器 DPI 感知：坐标全是物理像素
    pub fn dpi_thread() {
        use windows_sys::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }

    unsafe extern "system" fn monitor_cb(h: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> i32 {
        let out = &mut *(lp as *mut Vec<[i32; 4]>);
        if let Some(r) = monitor_rect(h) {
            out.push(r);
        }
        1
    }

    fn monitor_rect(h: HMONITOR) -> Option<[i32; 4]> {
        unsafe {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if h.is_null() || GetMonitorInfoW(h, &mut mi) == 0 {
                return None;
            }
            let r = mi.rcMonitor;
            Some([r.left, r.top, r.right - r.left, r.bottom - r.top])
        }
    }

    pub fn monitors() -> Vec<[i32; 4]> {
        crate::recorder::screenshot::win::dpi_aware(|| {
            let mut out: Vec<[i32; 4]> = Vec::new();
            unsafe {
                EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(monitor_cb), &mut out as *mut _ as LPARAM);
            }
            out
        })
    }

    pub fn monitor_at_cursor() -> Option<[i32; 4]> {
        crate::recorder::screenshot::win::dpi_aware(|| unsafe {
            let (x, y) = cursor_pos();
            monitor_rect(MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL))
        })
    }

    pub fn cursor_pos() -> (i32, i32) {
        unsafe {
            let mut p = POINT { x: 0, y: 0 };
            GetCursorPos(&mut p);
            (p.x, p.y)
        }
    }

    pub fn key_down(vk: i32) -> bool {
        unsafe { (GetAsyncKeyState(vk) as u16) & 0x8000 != 0 }
    }

    pub fn window_pid(h: isize) -> u32 {
        if h == 0 {
            return 0;
        }
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(h as HWND, &mut pid);
        }
        pid
    }

    /// 屏幕某点所在顶层窗口的进程
    pub fn pid_at(x: i32, y: i32) -> u32 {
        unsafe {
            let h = WindowFromPoint(POINT { x, y });
            if h.is_null() {
                return 0;
            }
            window_pid(GetAncestor(h, GA_ROOT) as isize)
        }
    }

    pub fn foreground() -> isize {
        unsafe { GetForegroundWindow() as isize }
    }

    pub fn window_title(h: isize) -> String {
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(h as HWND, buf.as_mut_ptr(), buf.len() as i32) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    pub fn window_rect(h: isize) -> Option<[i32; 4]> {
        crate::recorder::screenshot::win::dpi_aware(|| {
            crate::recorder::screenshot::win::frame_bounds(h as HWND).map(|r| [r.left, r.top, r.right - r.left, r.bottom - r.top])
        })
    }

    pub fn restore(h: isize) {
        unsafe {
            ShowWindow(h as HWND, SW_RESTORE);
        }
    }

    pub fn bring_to_front(h: isize) {
        unsafe {
            if IsIconic(h as HWND) != 0 {
                ShowWindow(h as HWND, SW_RESTORE);
            }
            SetForegroundWindow(h as HWND);
        }
    }

    pub enum WinState {
        Gone,
        Minimized,
        At(i32, i32),
    }

    /// 窗口当前位置（调用线程需已是 DPI 感知）
    pub fn window_state(h: isize) -> WinState {
        unsafe {
            if IsWindow(h as HWND) == 0 {
                return WinState::Gone;
            }
            if IsIconic(h as HWND) != 0 {
                return WinState::Minimized;
            }
        }
        match crate::recorder::screenshot::win::frame_bounds(h as HWND) {
            Some(r) => WinState::At(r.left, r.top),
            None => WinState::Gone,
        }
    }

    /// 复用同一块 DIB 的屏幕采集器（只在创建它的线程里用）
    pub struct Grabber {
        screen: HDC,
        mem: HDC,
        bmp: *mut c_void,
        old: HGDIOBJ,
        bits: *mut u8,
        w: i32,
        h: i32,
    }

    impl Grabber {
        pub fn new(w: i32, h: i32) -> Result<Grabber> {
            unsafe {
                let screen = GetDC(std::ptr::null_mut());
                if screen.is_null() {
                    bail!("拿不到屏幕");
                }
                let mem = CreateCompatibleDC(screen);
                let mut info: BITMAPINFO = std::mem::zeroed();
                info.bmiHeader = BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..std::mem::zeroed()
                };
                let mut bits: *mut c_void = std::ptr::null_mut();
                let bmp = CreateDIBSection(mem, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
                if bmp.is_null() || bits.is_null() {
                    DeleteDC(mem);
                    ReleaseDC(std::ptr::null_mut(), screen);
                    bail!("创建画面缓冲失败");
                }
                let old = SelectObject(mem, bmp);
                Ok(Grabber { screen, mem, bmp, old, bits: bits as *mut u8, w, h })
            }
        }

        /// 采集以 (x, y) 为左上角的一帧，返回 BGRA
        pub fn grab(&mut self, x: i32, y: i32) -> Result<Vec<u8>> {
            unsafe {
                // 不用 CAPTUREBLT：连续采集时它会让鼠标指针闪烁；DWM 合成下分层窗口照样能采到
                if BitBlt(self.mem, 0, 0, self.w, self.h, self.screen, x, y, SRCCOPY) == 0 {
                    return Err(anyhow!("BitBlt 失败"));
                }
                GdiFlush();
                let len = (self.w * self.h * 4) as usize;
                Ok(std::slice::from_raw_parts(self.bits, len).to_vec())
            }
        }
    }

    impl Drop for Grabber {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.mem, self.old);
                DeleteObject(self.bmp);
                DeleteDC(self.mem);
                ReleaseDC(std::ptr::null_mut(), self.screen);
            }
        }
    }

    pub struct Element {
        pub name: String,
        pub control: i32,
        pub rect: Option<[i32; 4]>,
    }

    /// UI Automation：取屏幕某点下控件的名称、类型和边框
    pub struct Uia(windows::Win32::UI::Accessibility::IUIAutomation);

    impl Uia {
        pub fn new() -> Option<Uia> {
            use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
            use windows::Win32::UI::Accessibility::CUIAutomation;
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok().map(Uia)
            }
        }

        pub fn element_at(&self, x: i32, y: i32) -> Option<Element> {
            unsafe {
                let el = self.0.ElementFromPoint(windows::Win32::Foundation::POINT { x, y }).ok()?;
                let control = el.CurrentControlType().map(|c| c.0).unwrap_or(0);
                let password = el.CurrentIsPassword().map(|b| b.as_bool()).unwrap_or(false);
                let name = if password { String::new() } else { el.CurrentName().map(|b| b.to_string()).unwrap_or_default() };
                let rect = el
                    .CurrentBoundingRectangle()
                    .ok()
                    .map(|r| [r.left, r.top, r.right - r.left, r.bottom - r.top])
                    .filter(|r| r[2] > 0 && r[3] > 0);
                Some(Element { name, control, rect })
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use anyhow::{bail, Result};
    pub fn dpi_thread() {}
    pub fn monitors() -> Vec<[i32; 4]> {
        Vec::new()
    }
    pub fn monitor_at_cursor() -> Option<[i32; 4]> {
        None
    }
    pub fn cursor_pos() -> (i32, i32) {
        (0, 0)
    }
    pub fn key_down(_: i32) -> bool {
        false
    }
    pub fn window_pid(_: isize) -> u32 {
        0
    }
    pub fn pid_at(_: i32, _: i32) -> u32 {
        0
    }
    pub fn foreground() -> isize {
        0
    }
    pub fn window_title(_: isize) -> String {
        String::new()
    }
    pub fn process_name(_: u32) -> String {
        String::new()
    }
    pub fn window_rect(_: isize) -> Option<[i32; 4]> {
        None
    }
    pub fn restore(_: isize) {}
    pub fn bring_to_front(_: isize) {}
    pub enum WinState {
        Gone,
        #[allow(dead_code)]
        Minimized,
        #[allow(dead_code)]
        At(i32, i32),
    }
    pub fn window_state(_: isize) -> WinState {
        WinState::Gone
    }
    pub struct Grabber;
    impl Grabber {
        pub fn new(_: i32, _: i32) -> Result<Grabber> {
            bail!("只支持 Windows")
        }
        pub fn grab(&mut self, _: i32, _: i32) -> Result<Vec<u8>> {
            bail!("只支持 Windows")
        }
    }
    pub struct Element {
        pub name: String,
        pub control: i32,
        pub rect: Option<[i32; 4]>,
    }
    pub struct Uia;
    impl Uia {
        pub fn new() -> Option<Uia> {
            None
        }
        pub fn element_at(&self, _: i32, _: i32) -> Option<Element> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_counts_pixels_and_caps() {
        let a = vec![0u8; 4 * 1000];
        let mut b = a.clone();
        assert_eq!(diff_count(&a, &b, 100), 0);
        b[4 * 10] = 9;
        b[4 * 500 + 1] = 9;
        b[4 * 500 + 3] = 9; // alpha 不算
        assert_eq!(diff_count(&a, &b, 100), 2);
        for i in 0..300 {
            b[4 * i + 2] = 7;
        }
        assert!(diff_count(&a, &b, 100) >= 100);
    }

    #[test]
    fn downscale_halves_4k_and_averages() {
        assert_eq!(downscale_factor(1920, 1080), 1);
        assert_eq!(downscale_factor(2560, 1440), 1);
        assert_eq!(downscale_factor(3840, 2160), 2);
        // 2×2 BGRA → 1 个 RGB 像素，取平均
        let bgra = [10, 20, 30, 255, 30, 40, 50, 255, 50, 60, 70, 255, 70, 80, 90, 255];
        let (rgb, w, h) = to_rgb(&bgra, 2, 2, 2);
        assert_eq!((w, h), (1, 1));
        assert_eq!(rgb, vec![60, 50, 40]);
        let (rgb, w, _) = to_rgb(&bgra, 2, 2, 1);
        assert_eq!(w, 2);
        assert_eq!(&rgb[..3], &[30, 20, 10]);
    }

    #[test]
    fn normalize_makes_even_and_rejects_tiny() {
        assert_eq!(normalize([0, 0, 1921, 1081]).unwrap(), [0, 0, 1920, 1080]);
        assert!(normalize([0, 0, 40, 400]).is_err());
    }

    #[test]
    fn ignores_modifier_and_mouse_keys() {
        assert!(ignored_key(0x01) && ignored_key(0x10) && ignored_key(0xA2) && ignored_key(0x5B));
        assert!(!ignored_key(0x41) && !ignored_key(0x0D));
    }

    /// 真机：录 1.5 秒主屏，检查有帧写出、事件通道正常（cargo test --lib desktop::tests::live -- --ignored）
    #[test]
    #[ignore]
    fn live_capture_writes_frames() {
        let dir = std::env::temp_dir().join(format!("sg-desk-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("frames")).unwrap();
        let target = screen_target(None).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let t0 = std::time::Instant::now();
        let s = start(&target, dir.clone(), Options { fps: 20, include_self: true }, tx).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        s.stop();
        let mut frames = 0;
        while let Ok(m) = rx.try_recv() {
            if m["event"] == "rec_frame_saved" {
                frames += 1;
            }
        }
        println!("{} 帧，目标 {:?}，用时 {:?}", frames, target.rect, t0.elapsed());
        assert!(frames >= 1);
        assert!(dir.join("frames").join("000001.jpg").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(all(test, windows))]
mod live_uia {
    /// 真机：识别鼠标下的控件（cargo test --lib desktop::live_uia -- --ignored --nocapture）
    #[test]
    #[ignore]
    fn element_under_cursor() {
        super::platform::dpi_thread();
        let u = super::platform::Uia::new().expect("UIA 可用");
        let (x, y) = super::platform::cursor_pos();
        let t = std::time::Instant::now();
        let el = u.element_at(x, y).expect("取到控件");
        println!("({x},{y}) → 「{}」类型 {} {} 框 {:?}，用时 {:?}", el.name, el.control, super::control_name(el.control), el.rect, t.elapsed());
        println!("屏幕 {:?}", super::platform::monitors());
    }
}
