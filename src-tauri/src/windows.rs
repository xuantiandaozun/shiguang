use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindowBuilder};

const CHAT_W: f64 = 400.0;
const CHAT_H: f64 = 620.0;
const BALL: f64 = 76.0;

pub fn toggle_chat(app: &AppHandle) {
    if let Some(chat) = app.get_webview_window("chat") {
        if chat.is_visible().unwrap_or(false) {
            let _ = chat.hide();
        } else {
            show_chat(app);
        }
    }
}

pub fn show_chat(app: &AppHandle) {
    if let Some(chat) = app.get_webview_window("chat") {
        position_chat_near_ball(app);
        let _ = chat.show();
        let _ = chat.set_focus();
    }
}

fn position_chat_near_ball(app: &AppHandle) {
    let (Some(ball), Some(chat)) = (
        app.get_webview_window("floatball"),
        app.get_webview_window("chat"),
    ) else {
        return;
    };
    let Ok(pos) = ball.outer_position() else {
        return;
    };
    let scale = ball.scale_factor().unwrap_or(1.0);
    let bp: LogicalPosition<f64> = pos.to_logical(scale);
    let ball_width = ball.outer_size().map(|s| s.to_logical::<f64>(scale).width).unwrap_or(BALL);
    let mut x = bp.x + ball_width / 2.0 + BALL / 2.0 - CHAT_W;
    let mut y = bp.y - CHAT_H - 14.0;
    if let Ok(Some(mon)) = ball.current_monitor() {
        let ms: LogicalSize<f64> = mon.size().to_logical(scale);
        let mp: LogicalPosition<f64> = mon.position().to_logical(scale);
        // 悬浮球上方放不下时，改放到球下方
        if y < mp.y {
            y = bp.y + BALL + 14.0;
        }
        x = x.clamp(mp.x, (mp.x + ms.width - CHAT_W).max(mp.x));
        y = y.clamp(mp.y, (mp.y + ms.height - CHAT_H).max(mp.y));
    }
    let _ = chat.set_position(LogicalPosition::new(x, y));
}

pub fn open_main(app: &AppHandle) {
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.show();
        let _ = main.unminimize();
        let _ = main.set_focus();
    }
}

pub fn show_delegation(app: &AppHandle, id: &str) -> tauri::Result<()> {
    let label = format!("delegation-{id}");
    if let Some(window) = app.get_webview_window(&label) {
        window.show()?;
        window.unminimize()?;
        window.set_focus()?;
        return Ok(());
    }
    let url = WebviewUrl::App(format!("/#/delegation?id={id}").into());
    let window = WebviewWindowBuilder::new(app, label, url)
        .title("拾光 · 委托任务")
        .inner_size(430.0, 650.0)
        .min_inner_size(360.0, 480.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .resizable(true)
        .center()
        .build()?;
    window.set_focus()?;
    Ok(())
}

/// 外派运行窗口：不抢焦点、不置顶，用户可随时打开查看或插话。
pub fn show_agent_run(app: &AppHandle, id: &str) -> tauri::Result<()> {
    let label = format!("agent-run-{id}");
    if let Some(window) = app.get_webview_window(&label) {
        window.show()?;
        window.unminimize()?;
        return Ok(());
    }
    let url = WebviewUrl::App(format!("/#/agent-run?id={id}").into());
    WebviewWindowBuilder::new(app, label, url)
        .title("拾光 · 外派任务")
        .inner_size(460.0, 680.0)
        .min_inner_size(360.0, 480.0)
        .decorations(false)
        .transparent(true)
        .resizable(true)
        .focused(false)
        .center()
        .build()?;
    Ok(())
}

/// 提醒弹窗：定位到屏幕右下角（贴着工作区边缘），置顶展示
pub fn show_reminder(app: &AppHandle) {
    let Some(win) = app.get_webview_window("reminder") else {
        return;
    };
    if let (Ok(size), Ok(Some(mon))) = (win.outer_size(), win.current_monitor()) {
        let scale = win.scale_factor().unwrap_or(1.0);
        let ws: LogicalSize<f64> = mon.size().to_logical(scale);
        let mp: LogicalPosition<f64> = mon.position().to_logical(scale);
        let wl: LogicalSize<f64> = size.to_logical(scale);
        // 留出任务栏高度的安全边距
        let x = mp.x + ws.width - wl.width - 16.0;
        let y = mp.y + ws.height - wl.height - 56.0;
        let _ = win.set_position(LogicalPosition::new(x, y));
    }
    let _ = win.show();
    let _ = win.set_focus();
}

pub fn hide_chat(app: &AppHandle) {
    if let Some(chat) = app.get_webview_window("chat") {
        let _ = chat.hide();
    }
}

/// 悬浮球可见形状（逻辑像素，相对窗口左上角）。
/// 原生窗口固定 160×76 且透明，透明部分不该挡鼠标。
/// 早先用 SetWindowRgn 裁窗口，但透明窗口裁区域后，后面窗口变化时透明部分不重绘，
/// 会留下别的窗口的残影；现在改为后台跟踪鼠标：鼠标不在球上时让整个窗口点击穿透。
#[cfg(windows)]
static FLOATBALL_SHAPE: std::sync::Mutex<Option<(f64, f64, f64, f64, bool)>> = std::sync::Mutex::new(None);

/// shape: "circle" 圆球；"left"/"right" 贴边胶囊（按矩形判断）。
#[cfg(windows)]
pub fn set_floatball_region(app: &AppHandle, x: f64, y: f64, w: f64, h: f64, shape: &str) -> Result<(), String> {
    let circle = match shape {
        "circle" => true,
        "left" | "right" => false,
        _ => return Err(format!("未知形状: {shape}")),
    };
    let first = {
        let mut guard = FLOATBALL_SHAPE.lock().map_err(|e| e.to_string())?;
        let first = guard.is_none();
        *guard = Some((x, y, w, h, circle));
        first
    };
    if first {
        let app = app.clone();
        std::thread::spawn(move || floatball_hit_loop(app));
    }
    Ok(())
}

#[cfg(windows)]
fn floatball_hit_loop(app: AppHandle) {
    use windows_sys::Win32::Foundation::{POINT, RECT};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect};
    let mut ignoring = false;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(40));
        let Some(win) = app.get_webview_window("floatball") else { return };
        let Ok(hwnd) = win.hwnd() else { continue };
        let Some((x, y, w, h, circle)) = FLOATBALL_SHAPE.lock().ok().and_then(|g| *g) else { continue };
        let mut pt = POINT { x: 0, y: 0 };
        let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        if unsafe { GetCursorPos(&mut pt) } == 0 || unsafe { GetWindowRect(hwnd.0 as _, &mut rc) } == 0 {
            continue;
        }
        // 按住左键（正在拖动/点击）时保持可交互，避免拖到一半被穿透
        let pressed = unsafe { GetAsyncKeyState(VK_LBUTTON as i32) } as u16 & 0x8000 != 0;
        if pressed && !ignoring {
            continue;
        }
        let s = win.scale_factor().unwrap_or(1.0);
        let px = (pt.x - rc.left) as f64 / s;
        let py = (pt.y - rc.top) as f64 / s;
        let pad = 1.0;
        let inside = if circle {
            let (cx, cy, r) = (x + w / 2.0, y + h / 2.0, w.min(h) / 2.0 + pad);
            (px - cx).powi(2) + (py - cy).powi(2) <= r * r
        } else {
            px >= x - pad && px <= x + w + pad && py >= y - pad && py <= y + h + pad
        };
        if inside == ignoring {
            ignoring = !inside;
            let _ = win.set_ignore_cursor_events(ignoring);
        }
    }
}

#[cfg(not(windows))]
pub fn set_floatball_region(_: &AppHandle, _: f64, _: f64, _: f64, _: f64, _: &str) -> Result<(), String> {
    Ok(())
}
