//! 截图：整屏、指定窗口（PrintWindow，被遮挡也能截完整）、区域；框选（冻结屏幕后在全屏选区窗口里拖选或点选窗口）；
//! 默认把拾光自己的窗口排除出画面（SetWindowDisplayAffinity，不闪烁），可选包含；可复制到剪贴板；全局快捷键。
//! 窗口枚举与采集以后也给桌面录屏用。坐标一律是物理像素的虚拟屏幕坐标。

use anyhow::{anyhow, bail, Context, Result};
use image::RgbaImage;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Debug, Clone, Serialize)]
pub struct WinInfo {
    pub hwnd: i64,
    pub pid: u32,
    pub title: String,
    pub process: String,
    /// 可见边框（不含阴影），虚拟屏幕物理像素 [x, y, w, h]
    pub rect: [i32; 4],
    pub minimized: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Virtual {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

// ---------- 设置 ----------

fn settings(app: &AppHandle) -> Value {
    crate::plugins::settings::values(app, "recorder")
}

pub fn output_dir(app: &AppHandle) -> PathBuf {
    settings(app)
        .get("screenshot_dir")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::picture_dir().map(|d| d.join("拾光截图")))
        .unwrap_or_else(|| std::env::temp_dir().join("拾光截图"))
}

fn default_copy(app: &AppHandle) -> bool {
    settings(app).get("screenshot_copy").and_then(Value::as_bool).unwrap_or(true)
}

fn default_include_self(app: &AppHandle) -> bool {
    settings(app).get("screenshot_include_self").and_then(Value::as_bool).unwrap_or(false)
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    s.trim().trim_end_matches('.').chars().take(40).collect()
}

/// 保存 PNG（必要时复制到剪贴板），返回结果描述
fn deliver(app: &AppHandle, img: &RgbaImage, label: &str, copy: bool) -> Result<Value> {
    let dir = output_dir(app);
    std::fs::create_dir_all(&dir).with_context(|| format!("创建截图目录 {}", dir.display()))?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let base = if label.trim().is_empty() { "截图".to_string() } else { sanitize(label) };
    let mut path = dir.join(format!("{base}-{stamp}.png"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{base}-{stamp}-{n}.png"));
        n += 1;
    }
    img.save(&path).with_context(|| format!("保存截图 {}", path.display()))?;
    let copied = copy && {
        match win::clipboard_set_image(img) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("截图复制到剪贴板失败: {e:#}");
                false
            }
        }
    };
    let _ = app.emit("screenshot-saved", json!({"path": path.to_string_lossy()}));
    Ok(json!({
        "path": path.to_string_lossy(), "width": img.width(), "height": img.height(), "copied": copied,
        "note": "需要看图片内容时用 read_image 读这个路径。",
    }))
}

// ---------- 拾光自身窗口 ----------

pub(crate) fn own_hwnds(app: &AppHandle) -> Vec<isize> {
    app.webview_windows()
        .values()
        .filter(|w| !w.label().starts_with("capture-"))
        .filter_map(|w| w.hwnd().ok().map(|h| h.0 as isize))
        .collect()
}

/// 已被设为「不被采集」的拾光窗口
static EXCLUDED: Mutex<Vec<isize>> = Mutex::new(Vec::new());
/// 桌面录屏期间要求排除（截图结束时不能把它恢复掉）
static RECORD_HOLD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// 截图期间临时排除
static SHOT_HOLD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 让拾光窗口的「不被采集」状态与需要一致，返回这次新排除的窗口数。
/// 只改可见窗口：对隐藏窗口调用 SetWindowDisplayAffinity，Windows 会把它画成一块关不掉的白板（提醒窗、主窗口都中过）。
/// 隐藏窗口的状态留到它下次可见时再同步。
fn sync_self_excluded(app: &AppHandle) -> usize {
    use std::sync::atomic::Ordering;
    let want = RECORD_HOLD.load(Ordering::SeqCst) || SHOT_HOLD.load(Ordering::SeqCst);
    let own = own_hwnds(app);
    let mut ex = EXCLUDED.lock().unwrap();
    ex.retain(|h| own.contains(h));
    let mut added = 0;
    for h in own {
        if !win::is_visible(h) {
            continue;
        }
        let has = ex.contains(&h);
        if want && !has {
            win::set_exclude(&[h], true);
            ex.push(h);
            added += 1;
        } else if !want && has {
            win::set_exclude(&[h], false);
            ex.retain(|x| *x != h);
        }
    }
    added
}

/// 桌面录屏全程把拾光的窗口排除出画面（录制中新打开的窗口每秒补一次）；off 时恢复
pub(crate) fn set_self_excluded(app: &AppHandle, on: bool) {
    use std::sync::atomic::Ordering;
    let was = RECORD_HOLD.swap(on, Ordering::SeqCst);
    sync_self_excluded(app);
    if on && !was {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            while RECORD_HOLD.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if RECORD_HOLD.load(Ordering::SeqCst) {
                    sync_self_excluded(&app);
                }
            }
        });
    }
}

/// 在 f 执行期间把拾光的窗口排除出屏幕采集（include_self 时只把之前没恢复的窗口恢复）
fn with_self_hidden<T>(app: &AppHandle, include_self: bool, f: impl FnOnce() -> T) -> T {
    use std::sync::atomic::Ordering;
    SHOT_HOLD.store(!include_self, Ordering::SeqCst);
    // 设置后合成器要一帧才生效
    if sync_self_excluded(app) > 0 {
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
    let r = f();
    SHOT_HOLD.store(false, Ordering::SeqCst);
    sync_self_excluded(app);
    r
}

pub fn list_windows(include_self: bool) -> Vec<WinInfo> {
    let me = std::process::id();
    win::list_windows().into_iter().filter(|w| include_self || w.pid != me).collect()
}

/// 按标题或进程名找窗口（不区分大小写；多个命中取最上层）
pub(crate) fn find_window(query: &str, include_self: bool) -> Result<(WinInfo, Vec<String>)> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        bail!("需要窗口标题或进程名");
    }
    let all = list_windows(include_self);
    let hits: Vec<&WinInfo> = all
        .iter()
        .filter(|w| w.title.to_lowercase().contains(&q) || w.process.to_lowercase().trim_end_matches(".exe").contains(q.trim_end_matches(".exe")))
        .collect();
    match hits.first() {
        Some(w) => Ok(((*w).clone(), hits.iter().skip(1).take(5).map(|w| format!("{}（{}）", w.title, w.process)).collect())),
        None => {
            let names: Vec<String> = all.iter().take(15).map(|w| format!("{}（{}）", w.title, w.process)).collect();
            bail!("没有找到包含「{query}」的窗口。当前窗口：{}", names.join("、"))
        }
    }
}

// ---------- 非交互截图 ----------

/// target: screen | window | region
pub fn capture(app: &AppHandle, target: &str, window: Option<&str>, rect: Option<[i32; 4]>, include_self: Option<bool>, copy: Option<bool>) -> Result<Value> {
    let include_self = include_self.unwrap_or_else(|| default_include_self(app));
    let copy = copy.unwrap_or_else(|| default_copy(app));
    match target {
        "screen" => {
            let v = win::virtual_screen();
            let img = with_self_hidden(app, include_self, || win::capture_rect(v.x, v.y, v.w, v.h))?;
            deliver(app, &img, "全屏", copy)
        }
        "region" => {
            let [x, y, w, h] = rect.ok_or_else(|| anyhow!("区域截图需要 rect [x, y, 宽, 高]（屏幕物理像素），或用 interactive 让用户框选"))?;
            if w <= 0 || h <= 0 {
                bail!("区域宽高要大于 0");
            }
            let img = with_self_hidden(app, include_self, || win::capture_rect(x, y, w as u32, h as u32))?;
            deliver(app, &img, "区域", copy)
        }
        "window" => {
            let (w, others) = find_window(window.unwrap_or(""), include_self)?;
            if w.minimized {
                bail!("窗口「{}」已最小化，先把它还原再截", w.title);
            }
            let img = capture_window_best(&w)?;
            let mut v = deliver(app, &img, &w.title, copy)?;
            v["window"] = json!({"title": w.title, "process": w.process});
            if !others.is_empty() {
                v["other_matches"] = json!(others);
            }
            Ok(v)
        }
        _ => bail!("未知截图方式 {target}（screen / window / region / interactive）"),
    }
}

/// 窗口截图：优先 PrintWindow（被遮挡也完整）；得到全黑画面时（部分硬件加速程序）退回按屏幕区域截
pub(crate) fn capture_window_best(w: &WinInfo) -> Result<RgbaImage> {
    match win::capture_window(w.hwnd as isize) {
        Ok(img) if !is_blank(&img) => Ok(img),
        _ => {
            let [x, y, cw, ch] = w.rect;
            win::capture_rect(x, y, cw.max(1) as u32, ch.max(1) as u32)
        }
    }
}

fn is_blank(img: &RgbaImage) -> bool {
    let step = (img.width() * img.height() / 2000).max(1) as usize;
    img.pixels().step_by(step).all(|p| p[0] < 4 && p[1] < 4 && p[2] < 4)
}

// ---------- 交互截图（框选 / 点选窗口） ----------

struct Session {
    id: String,
    screen: Arc<RgbaImage>,
    virt: Virtual,
    windows: Vec<WinInfo>,
    image_path: PathBuf,
    copy: bool,
    /// 只选区域不截图（桌面录屏选录制范围）
    select_only: bool,
    tx: Option<tokio::sync::oneshot::Sender<Result<Value, String>>>,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);
const OVERLAY: &str = "capture-overlay";

/// 冻结屏幕 → 打开全屏选区窗口 → 等用户选完（最多 5 分钟）
pub async fn interactive(app: &AppHandle, include_self: Option<bool>, copy: Option<bool>) -> Result<Value> {
    let include_self = include_self.unwrap_or_else(|| default_include_self(app));
    let copy = copy.unwrap_or_else(|| default_copy(app));
    open_overlay(app, include_self, copy, false).await
}

/// 让用户框选一个区域或单击一个窗口（不截图）。返回 {kind: region|window, rect: 虚拟屏幕物理像素, hwnd?, title?}
pub async fn select_area(app: &AppHandle, include_self: bool) -> Result<Value> {
    open_overlay(app, include_self, false, true).await
}

async fn open_overlay(app: &AppHandle, include_self: bool, copy: bool, select_only: bool) -> Result<Value> {
    cancel_session("开始了新的截图");
    let app2 = app.clone();
    let (screen, virt, windows) = tokio::task::spawn_blocking(move || -> Result<_> {
        let v = win::virtual_screen();
        let (img, wins) = with_self_hidden(&app2, include_self, || (win::capture_rect(v.x, v.y, v.w, v.h), list_windows(include_self)));
        Ok((img?, v, wins))
    })
    .await
    .map_err(|e| anyhow!("截图线程异常：{e}"))??;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let temp = crate::tempfs::temp_dir_in(&app.path().app_data_dir()?);
    std::fs::create_dir_all(&temp)?;
    let image_path = temp.join(format!("capture-{id}.png"));
    let screen = Arc::new(screen);
    {
        let (s, p) = (screen.clone(), image_path.clone());
        tokio::task::spawn_blocking(move || save_fast_png(&s, &p)).await.map_err(|e| anyhow!("{e}"))??;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    *SESSION.lock().unwrap() = Some(Session { id: id.clone(), screen, virt, windows, image_path, copy, select_only, tx: Some(tx) });
    if let Some(old) = app.get_webview_window(OVERLAY) {
        let _ = old.destroy();
    }
    let url = tauri::WebviewUrl::App(format!("/#/capture?id={id}").into());
    tauri::WebviewWindowBuilder::new(app, OVERLAY, url)
        .title("拾光截图")
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(false)
        .visible(false)
        .position(0.0, 0.0)
        .inner_size(400.0, 300.0)
        .build()
        .context("打开截图窗口失败")?;
    if let Some(w) = app.get_webview_window(OVERLAY) {
        let _ = w.set_position(tauri::PhysicalPosition::new(virt.x, virt.y));
        let _ = w.set_size(tauri::PhysicalSize::new(virt.w, virt.h));
    }
    match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
        Ok(Ok(Ok(v))) => Ok(v),
        Ok(Ok(Err(e))) => bail!("{e}"),
        Ok(Err(_)) => bail!("截图已取消"),
        Err(_) => {
            cancel_session("超时");
            close_overlay(app);
            bail!("5 分钟内没有完成截图，已取消")
        }
    }
}

fn save_fast_png(img: &RgbaImage, path: &Path) -> Result<()> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let f = std::io::BufWriter::new(std::fs::File::create(path)?);
    PngEncoder::new_with_quality(f, CompressionType::Fast, FilterType::NoFilter).write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(())
}

fn cancel_session(reason: &str) {
    if let Some(mut s) = SESSION.lock().unwrap().take() {
        let _ = std::fs::remove_file(&s.image_path);
        if let Some(tx) = s.tx.take() {
            let _ = tx.send(Err(format!("截图已取消（{reason}）")));
        }
    }
}

fn close_overlay(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(OVERLAY) {
        let _ = w.destroy();
    }
}

/// 选区窗口初始化数据：冻结画面、虚拟屏幕、窗口框（相对虚拟屏幕左上角）
#[tauri::command]
pub fn capture_overlay_init(id: String) -> Result<Value, String> {
    let guard = SESSION.lock().unwrap();
    let s = guard.as_ref().filter(|s| s.id == id).ok_or("截图会话已结束")?;
    let windows: Vec<Value> = s
        .windows
        .iter()
        .filter(|w| !w.minimized)
        .map(|w| json!({"hwnd": w.hwnd, "title": w.title, "process": w.process,
            "rect": [w.rect[0] - s.virt.x, w.rect[1] - s.virt.y, w.rect[2], w.rect[3]]}))
        .collect();
    Ok(json!({"image": s.image_path.to_string_lossy(), "width": s.virt.w, "height": s.virt.h, "windows": windows, "copy": s.copy,
        "purpose": if s.select_only { "record" } else { "screenshot" }}))
}

/// 冻结画面加载完再显示，避免黑屏闪一下
#[tauri::command]
pub fn capture_overlay_show(app: AppHandle) -> Result<(), String> {
    let w = app.get_webview_window(OVERLAY).ok_or("截图窗口不存在")?;
    w.show().map_err(|e| e.to_string())?;
    let _ = w.set_focus();
    Ok(())
}

/// 完成：kind=region 时 rect 为相对虚拟屏幕的物理像素；kind=window 时截该窗口（hwnd）
#[tauri::command]
pub async fn capture_finish(app: AppHandle, id: String, kind: String, rect: Option<[i32; 4]>, hwnd: Option<i64>, copy: Option<bool>) -> Result<Value, String> {
    let session = {
        let mut guard = SESSION.lock().unwrap();
        match guard.as_ref() {
            Some(s) if s.id == id => guard.take().unwrap(),
            _ => return Err("截图会话已结束".into()),
        }
    };
    close_overlay(&app);
    let Session { screen, virt, windows, image_path, copy: default_copy, select_only, mut tx, .. } = session;
    if select_only {
        let _ = std::fs::remove_file(&image_path);
        let result = match kind.as_str() {
            "window" => windows
                .iter()
                .find(|w| Some(w.hwnd) == hwnd)
                .map(|w| json!({"kind": "window", "hwnd": w.hwnd, "title": w.title, "process": w.process, "rect": w.rect}))
                .ok_or_else(|| "窗口已不存在".to_string()),
            _ => rect
                .filter(|r| r[2] >= 16 && r[3] >= 16)
                .map(|r| json!({"kind": "region", "rect": [r[0] + virt.x, r[1] + virt.y, r[2], r[3]]}))
                .ok_or_else(|| "选区太小".to_string()),
        };
        if let Some(tx) = tx.take() {
            let _ = tx.send(result.clone());
        }
        return result;
    }
    let copy = copy.unwrap_or(default_copy);
    let app2 = app.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Value> {
        let (img, label) = match kind.as_str() {
            "window" => {
                let h = hwnd.ok_or_else(|| anyhow!("缺少窗口"))?;
                let w = windows.iter().find(|w| w.hwnd == h).cloned().ok_or_else(|| anyhow!("窗口已不存在"))?;
                // 窗口可能已关闭：退回从冻结画面里裁
                let img = capture_window_best(&w).or_else(|_| crop(&screen, [w.rect[0] - virt.x, w.rect[1] - virt.y, w.rect[2], w.rect[3]]))?;
                (img, w.title.clone())
            }
            _ => {
                let r = rect.ok_or_else(|| anyhow!("缺少选区"))?;
                (crop(&screen, r)?, "区域".to_string())
            }
        };
        let mut v = deliver(&app2, &img, &label, copy)?;
        if kind == "window" {
            v["window"] = json!(label);
        }
        Ok(v)
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r.map_err(|e| format!("{e:#}")));
    let _ = std::fs::remove_file(&image_path);
    if let Some(tx) = tx.take() {
        let _ = tx.send(result.clone());
    }
    result
}

#[tauri::command]
pub fn capture_cancel(app: AppHandle, id: String) -> Result<(), String> {
    let matches = SESSION.lock().unwrap().as_ref().is_some_and(|s| s.id == id);
    if matches {
        cancel_session("用户取消");
    }
    close_overlay(&app);
    Ok(())
}

pub fn crop(img: &RgbaImage, [x, y, w, h]: [i32; 4]) -> Result<RgbaImage> {
    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = ((x + w).max(0) as u32).min(img.width());
    let y1 = ((y + h).max(0) as u32).min(img.height());
    if x1 <= x0 + 1 || y1 <= y0 + 1 {
        bail!("选区太小");
    }
    Ok(image::imageops::crop_imm(img, x0, y0, x1 - x0, y1 - y0).to_image())
}

// ---------- 全局快捷键 ----------

/// 解析 "Ctrl+Shift+X" → (修饰键, 虚拟键码)
pub fn parse_hotkey(s: &str) -> Result<(u32, u32)> {
    let mut mods = 0u32;
    let mut vk = None;
    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= 0x2,
            "alt" => mods |= 0x1,
            "shift" => mods |= 0x4,
            "win" | "super" | "meta" => mods |= 0x8,
            "printscreen" | "prtsc" | "print" => vk = Some(0x2C),
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => vk = Some(k.to_ascii_uppercase().as_bytes()[0] as u32),
            k if k.starts_with('f') && k[1..].parse::<u32>().is_ok_and(|n| (1..=24).contains(&n)) => vk = Some(0x6F + k[1..].parse::<u32>().unwrap()),
            other => bail!("不认识的按键「{other}」"),
        }
    }
    let vk = vk.ok_or_else(|| anyhow!("快捷键缺少主键，例如 Ctrl+Shift+X"))?;
    if mods == 0 && !(0x70..=0x87).contains(&vk) && vk != 0x2C {
        bail!("快捷键需要带 Ctrl / Alt / Shift / Win 之一（F1–F24、PrintScreen 除外）");
    }
    Ok((mods, vk))
}

/// 按设置注册全局快捷键：截图、开始/停止桌面录屏（设置为空则不注册）。返回生效的快捷键说明
pub fn apply_hotkey(app: &AppHandle) -> Result<String> {
    let s = settings(app);
    let key = |k: &str, default: &str| s.get(k).and_then(Value::as_str).unwrap_or(default).trim().to_string();
    let mut specs: Vec<(String, u32, u32, win::Callback)> = Vec::new();
    let mut errors = Vec::new();
    let shot = key("screenshot_hotkey", "Ctrl+Shift+X");
    if !shot.is_empty() {
        match parse_hotkey(&shot) {
            Ok((m, vk)) => {
                let app2 = app.clone();
                specs.push((format!("截图 {shot}"), m, vk, Box::new(move || {
                    let app = app2.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = interactive(&app, None, None).await {
                            log::info!("快捷键截图: {e:#}");
                        }
                    });
                })));
            }
            Err(e) => errors.push(format!("截图快捷键：{e:#}")),
        }
    }
    let rec = key("record_hotkey", "Ctrl+Alt+R");
    if !rec.is_empty() {
        match parse_hotkey(&rec) {
            Ok((m, vk)) => {
                let app2 = app.clone();
                specs.push((format!("录屏 {rec}"), m, vk, Box::new(move || {
                    let app = app2.clone();
                    tauri::async_runtime::spawn(async move {
                        super::hotkey_toggle(&app).await;
                    });
                })));
            }
            Err(e) => errors.push(format!("录屏快捷键：{e:#}")),
        }
    }
    let names: Vec<String> = specs.iter().map(|s| s.0.clone()).collect();
    let failed = win::set_hotkeys(specs.into_iter().map(|(_, m, vk, cb)| (m, vk, cb)).collect())?;
    for i in failed {
        errors.push(format!("{} 已被其它程序占用，请在「插件 → 录屏与截图」设置里换一个", names[i]));
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("；"));
    }
    Ok(names.join("，"))
}

// ---------- AI 工具 ----------

pub const TOOL_NAMES: &[&str] = &["screenshot", "list_windows"];

pub async fn execute(app: &AppHandle, name: &str, args: &Value) -> Result<Value> {
    let b = |k: &str| args.get(k).and_then(Value::as_bool);
    match name {
        "list_windows" => {
            let all = list_windows(b("include_self").unwrap_or(false));
            Ok(json!({"windows": all.iter().take(60).map(|w| json!({
                "title": w.title, "process": w.process, "size": [w.rect[2], w.rect[3]], "minimized": w.minimized,
            })).collect::<Vec<_>>()}))
        }
        "screenshot" => {
            let mode = args.get("mode").and_then(Value::as_str).unwrap_or("interactive");
            if mode == "interactive" {
                return interactive(app, b("include_self"), b("copy")).await;
            }
            let rect = args.get("rect").and_then(Value::as_array).and_then(|a| {
                let n: Vec<i32> = a.iter().filter_map(Value::as_f64).map(|v| v.round() as i32).collect();
                (n.len() == 4).then(|| [n[0], n[1], n[2], n[3]])
            });
            let (app2, mode, window) = (app.clone(), mode.to_string(), args.get("window").and_then(Value::as_str).map(str::to_string));
            let (inc, copy) = (b("include_self"), b("copy"));
            tokio::task::spawn_blocking(move || capture(&app2, &mode, window.as_deref(), rect, inc, copy))
                .await
                .map_err(|e| anyhow!("截图线程异常：{e}"))?
        }
        _ => bail!("未知截图工具 {name}"),
    }
}

// ---------- 界面命令 ----------

#[tauri::command]
pub async fn screenshot_capture(app: AppHandle, mode: String, window: Option<String>, include_self: Option<bool>, copy: Option<bool>) -> Result<Value, String> {
    if mode == "interactive" {
        return interactive(&app, include_self, copy).await.map_err(|e| format!("{e:#}"));
    }
    tokio::task::spawn_blocking(move || capture(&app, &mode, window.as_deref(), None, include_self, copy))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn screenshot_windows(include_self: Option<bool>) -> Vec<WinInfo> {
    list_windows(include_self.unwrap_or(false)).into_iter().filter(|w| !w.minimized).collect()
}

/// 最近的截图（截图目录里最新的若干个 png）
#[tauri::command]
pub fn screenshot_recent(app: AppHandle, limit: Option<usize>) -> Vec<Value> {
    let dir = output_dir(&app);
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("png")))
                .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
                .collect()
        })
        .unwrap_or_default();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files
        .into_iter()
        .take(limit.unwrap_or(12))
        .map(|(_, p)| json!({"path": p.to_string_lossy(), "name": p.file_name().map(|n| n.to_string_lossy().to_string())}))
        .collect()
}

/// 把已保存的截图再复制到剪贴板（只允许截图目录里的文件）
#[tauri::command]
pub fn screenshot_copy(app: AppHandle, path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.starts_with(output_dir(&app)) {
        return Err("只能复制截图目录里的图片".into());
    }
    let img = image::open(&p).map_err(|e| e.to_string())?.to_rgba8();
    win::clipboard_set_image(&img).map_err(|e| format!("{e:#}"))
}

// ---------- Windows 实现 ----------

#[cfg(windows)]
pub(crate) mod win {
    use super::{Virtual, WinInfo};
    use anyhow::{anyhow, bail, Result};
    use image::RgbaImage;
    use std::ffi::c_void;
    use std::sync::Mutex;
    use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
    use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HDC, SRCCOPY,
    };
    use windows_sys::Win32::Storage::Xps::PrintWindow;
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData};
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows_sys::Win32::System::Threading::{GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetMessageW, GetSystemMetrics, GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, PostThreadMessageW, SetWindowDisplayAffinity, GWL_EXSTYLE, MSG,
        SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
        WM_HOTKEY, WM_QUIT, WS_EX_TOOLWINDOW,
    };

    /// 线程级切到每显示器 DPI 感知，保证所有坐标都是物理像素（与 DWM 返回的一致）
    pub fn dpi_aware<T>(f: impl FnOnce() -> T) -> T {
        use windows_sys::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        unsafe {
            let old = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let r = f();
            if !old.is_null() {
                SetThreadDpiAwarenessContext(old);
            }
            r
        }
    }

    pub fn virtual_screen() -> Virtual {
        dpi_aware(|| unsafe {
            Virtual {
                x: GetSystemMetrics(SM_XVIRTUALSCREEN),
                y: GetSystemMetrics(SM_YVIRTUALSCREEN),
                w: GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1) as u32,
                h: GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1) as u32,
            }
        })
    }

    /// 把内存 DC 里的位图读成 RGBA
    unsafe fn read_bitmap(dc: HDC, bmp: *mut c_void, w: u32, h: u32) -> Result<RgbaImage> {
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // 自上而下
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(dc, bmp, 0, h, buf.as_mut_ptr() as *mut c_void, &mut info, DIB_RGB_COLORS);
        if lines == 0 {
            bail!("读取画面失败");
        }
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        RgbaImage::from_raw(w, h, buf).ok_or_else(|| anyhow!("画面尺寸无效"))
    }

    pub fn capture_rect(x: i32, y: i32, w: u32, h: u32) -> Result<RgbaImage> {
        dpi_aware(|| unsafe {
            let screen = GetDC(std::ptr::null_mut());
            let mem = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, w as i32, h as i32);
            let old = SelectObject(mem, bmp);
            let ok = BitBlt(mem, 0, 0, w as i32, h as i32, screen, x, y, SRCCOPY | CAPTUREBLT);
            let r = if ok == 0 { Err(anyhow!("截取屏幕失败")) } else { read_bitmap(mem, bmp, w, h) };
            SelectObject(mem, old);
            DeleteObject(bmp);
            DeleteDC(mem);
            ReleaseDC(std::ptr::null_mut(), screen);
            r
        })
    }

    pub fn frame_bounds(hwnd: HWND) -> Option<RECT> {
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            let hr = DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS as u32, &mut r as *mut RECT as *mut c_void, std::mem::size_of::<RECT>() as u32);
            if hr == 0 {
                return Some(r);
            }
            let mut r: RECT = std::mem::zeroed();
            (GetWindowRect(hwnd, &mut r) != 0).then_some(r)
        }
    }

    /// PrintWindow 截整个窗口（含被遮挡部分），再裁掉不可见的边框阴影
    pub fn capture_window(hwnd: isize) -> Result<RgbaImage> {
        let hwnd = hwnd as HWND;
        dpi_aware(|| unsafe {
            if IsIconic(hwnd) != 0 {
                bail!("窗口已最小化");
            }
            let mut wr: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut wr) == 0 {
                bail!("窗口已不存在");
            }
            let (w, h) = ((wr.right - wr.left).max(1) as u32, (wr.bottom - wr.top).max(1) as u32);
            let screen = GetDC(std::ptr::null_mut());
            let mem = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, w as i32, h as i32);
            let old = SelectObject(mem, bmp);
            const PW_RENDERFULLCONTENT: u32 = 2;
            let ok = PrintWindow(hwnd, mem, PW_RENDERFULLCONTENT);
            let r = if ok == 0 { Err(anyhow!("该窗口不支持直接截取")) } else { read_bitmap(mem, bmp, w, h) };
            SelectObject(mem, old);
            DeleteObject(bmp);
            DeleteDC(mem);
            ReleaseDC(std::ptr::null_mut(), screen);
            let img = r?;
            let fb = frame_bounds(hwnd).unwrap_or(wr);
            let crop = [fb.left - wr.left, fb.top - wr.top, fb.right - fb.left, fb.bottom - fb.top];
            super::crop(&img, crop).or(Ok(img))
        })
    }

    pub fn process_name(pid: u32) -> String {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return String::new();
            }
            let mut buf = [0u16; 520];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
            CloseHandle(h);
            if ok == 0 {
                return String::new();
            }
            let full = String::from_utf16_lossy(&buf[..len as usize]);
            full.rsplit('\\').next().unwrap_or("").to_string()
        }
    }

    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> i32 {
        let out = &mut *(lparam as *mut Vec<HWND>);
        out.push(hwnd);
        1
    }

    /// 可见的顶层应用窗口，按 Z 序从上到下
    pub fn list_windows() -> Vec<WinInfo> {
        dpi_aware(list_windows_inner)
    }

    fn list_windows_inner() -> Vec<WinInfo> {
        let mut hwnds: Vec<HWND> = Vec::new();
        unsafe {
            EnumWindows(Some(enum_cb), &mut hwnds as *mut Vec<HWND> as LPARAM);
        }
        let mut out = Vec::new();
        for hwnd in hwnds {
            unsafe {
                if IsWindowVisible(hwnd) == 0 || GetWindowTextLengthW(hwnd) == 0 {
                    continue;
                }
                if (GetWindowLongW(hwnd, GWL_EXSTYLE) as u32) & WS_EX_TOOLWINDOW != 0 {
                    continue;
                }
                let mut cloaked: u32 = 0;
                DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, &mut cloaked as *mut u32 as *mut c_void, 4);
                if cloaked != 0 {
                    continue;
                }
                let mut buf = [0u16; 512];
                let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
                let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
                if title == "Program Manager" {
                    continue;
                }
                let Some(r) = frame_bounds(hwnd) else { continue };
                let minimized = IsIconic(hwnd) != 0;
                let (w, h) = (r.right - r.left, r.bottom - r.top);
                if !minimized && (w < 40 || h < 30) {
                    continue;
                }
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, &mut pid);
                out.push(WinInfo { hwnd: hwnd as i64, pid, title, process: process_name(pid), rect: [r.left, r.top, w, h], minimized });
            }
        }
        out
    }

    pub fn is_visible(hwnd: isize) -> bool {
        unsafe { IsWindowVisible(hwnd as HWND) != 0 }
    }

    pub fn set_exclude(hwnds: &[isize], on: bool) {
        for &h in hwnds {
            unsafe {
                SetWindowDisplayAffinity(h as HWND, if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE });
            }
        }
    }

    /// 32 位 DIB（自下而上 BGRA），给 CF_DIB 用
    pub fn dib_bytes(img: &RgbaImage) -> Vec<u8> {
        let (w, h) = (img.width(), img.height());
        let mut out = Vec::with_capacity(40 + (w * h * 4) as usize);
        let header = BITMAPINFOHEADER {
            biSize: 40,
            biWidth: w as i32,
            biHeight: h as i32,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: w * h * 4,
            ..unsafe { std::mem::zeroed() }
        };
        out.extend_from_slice(unsafe { std::slice::from_raw_parts(&header as *const _ as *const u8, 40) });
        for y in (0..h).rev() {
            for x in 0..w {
                let p = img.get_pixel(x, y).0;
                out.extend_from_slice(&[p[2], p[1], p[0], 255]);
            }
        }
        out
    }

    unsafe fn put(format: u32, bytes: &[u8]) -> bool {
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if mem.is_null() {
            return false;
        }
        let ptr = GlobalLock(mem) as *mut u8;
        if ptr.is_null() {
            GlobalFree(mem);
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        GlobalUnlock(mem);
        if SetClipboardData(format, mem as _).is_null() {
            GlobalFree(mem);
            return false;
        }
        true
    }

    pub fn clipboard_set_image(img: &RgbaImage) -> Result<()> {
        let dib = dib_bytes(img);
        let mut png = Vec::new();
        let _ = img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
        unsafe {
            let mut opened = false;
            for _ in 0..10 {
                if OpenClipboard(std::ptr::null_mut()) != 0 {
                    opened = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            if !opened {
                bail!("剪贴板被其它程序占用");
            }
            EmptyClipboard();
            const CF_DIB: u32 = 8;
            let ok = put(CF_DIB, &dib);
            if !png.is_empty() {
                let name: Vec<u16> = "PNG\0".encode_utf16().collect();
                let fmt = RegisterClipboardFormatW(name.as_ptr());
                if fmt != 0 {
                    put(fmt, &png);
                }
            }
            CloseClipboard();
            if !ok {
                bail!("写入剪贴板失败");
            }
        }
        Ok(())
    }

    pub type Callback = Box<dyn Fn() + Send + 'static>;
    /// 快捷键线程 id（换快捷键时让旧线程退出）
    static HOTKEY_THREAD: Mutex<Option<u32>> = Mutex::new(None);

    /// 注册一组全局快捷键（替换之前的）。返回注册失败（被占用）的序号
    pub fn set_hotkeys(specs: Vec<(u32, u32, Callback)>) -> Result<Vec<usize>> {
        if let Some(tid) = HOTKEY_THREAD.lock().unwrap().take() {
            unsafe {
                PostThreadMessageW(tid, WM_QUIT, 0, 0);
            }
        }
        if specs.is_empty() {
            return Ok(Vec::new());
        }
        let (tx, rx) = std::sync::mpsc::channel::<(u32, Vec<usize>)>();
        std::thread::Builder::new()
            .name("global-hotkeys".into())
            .spawn(move || unsafe {
                let mut failed = Vec::new();
                for (i, (mods, vk, _)) in specs.iter().enumerate() {
                    if RegisterHotKey(std::ptr::null_mut(), i as i32 + 1, mods | MOD_NOREPEAT, *vk) == 0 {
                        failed.push(i);
                    }
                }
                let _ = tx.send((GetCurrentThreadId(), failed));
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    if msg.message == WM_HOTKEY {
                        if let Some((_, _, cb)) = specs.get(msg.wParam.wrapping_sub(1)) {
                            cb();
                        }
                    }
                }
                for i in 0..specs.len() {
                    UnregisterHotKey(std::ptr::null_mut(), i as i32 + 1);
                }
            })?;
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok((tid, failed)) => {
                *HOTKEY_THREAD.lock().unwrap() = Some(tid);
                Ok(failed)
            }
            Err(_) => bail!("注册快捷键超时"),
        }
    }
}

#[cfg(not(windows))]
pub(crate) mod win {
    use super::{Virtual, WinInfo};
    use anyhow::{bail, Result};
    use image::RgbaImage;
    pub fn virtual_screen() -> Virtual {
        Virtual { x: 0, y: 0, w: 1, h: 1 }
    }
    pub fn capture_rect(_: i32, _: i32, _: u32, _: u32) -> Result<RgbaImage> {
        bail!("截图目前只支持 Windows")
    }
    pub fn capture_window(_: isize) -> Result<RgbaImage> {
        bail!("截图目前只支持 Windows")
    }
    pub fn list_windows() -> Vec<WinInfo> {
        Vec::new()
    }
    pub fn is_visible(_: isize) -> bool {
        false
    }
    pub fn set_exclude(_: &[isize], _: bool) {}
    pub fn clipboard_set_image(_: &RgbaImage) -> Result<()> {
        bail!("只支持 Windows")
    }
    pub type Callback = Box<dyn Fn() + Send + 'static>;
    pub fn set_hotkeys(_: Vec<(u32, u32, Callback)>) -> Result<Vec<usize>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_parsing() {
        assert_eq!(parse_hotkey("Ctrl+Shift+X").unwrap(), (0x6, b'X' as u32));
        assert_eq!(parse_hotkey("alt + a").unwrap(), (0x1, b'A' as u32));
        assert_eq!(parse_hotkey("F1").unwrap(), (0, 0x70));
        assert_eq!(parse_hotkey("PrintScreen").unwrap(), (0, 0x2C));
        assert!(parse_hotkey("X").is_err());
        assert!(parse_hotkey("Ctrl+Shift").is_err());
        assert!(parse_hotkey("Ctrl+Space?").is_err());
    }

    #[test]
    fn crop_clamps_to_image() {
        let img = RgbaImage::new(100, 50);
        assert_eq!(crop(&img, [90, 40, 50, 50]).unwrap().dimensions(), (10, 10));
        assert!(crop(&img, [200, 0, 10, 10]).is_err());
    }

    #[test]
    fn blank_detection() {
        assert!(is_blank(&RgbaImage::new(20, 20)));
        let mut img = RgbaImage::new(20, 20);
        for p in img.pixels_mut() {
            *p = image::Rgba([200, 10, 10, 255]);
        }
        assert!(!is_blank(&img));
    }

    #[cfg(windows)]
    #[test]
    fn dib_is_bottom_up_bgra() {
        let mut img = RgbaImage::new(2, 2);
        img.put_pixel(0, 0, image::Rgba([1, 2, 3, 255])); // 左上
        let dib = win::dib_bytes(&img);
        assert_eq!(dib.len(), 40 + 16);
        // 自下而上：左上像素在最后一行开头
        assert_eq!(&dib[40 + 8..40 + 12], &[3, 2, 1, 255]);
    }

    /// 真实截屏与窗口枚举（只检查尺寸，不保存）：cargo test --lib screenshot::tests::live -- --ignored
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn live_capture_and_windows() {
        let v = win::virtual_screen();
        let img = win::capture_rect(v.x, v.y, v.w.min(400), v.h.min(300)).unwrap();
        assert_eq!(img.width(), v.w.min(400));
        let wins = win::list_windows();
        eprintln!("virtual {v:?}, {} windows", wins.len());
        if let Some(w) = wins.iter().find(|w| !w.minimized) {
            let shot = capture_window_best(w).unwrap();
            eprintln!("window {}x{} (rect {:?}) blank={}", shot.width(), shot.height(), w.rect, is_blank(&shot));
        }
    }
}
