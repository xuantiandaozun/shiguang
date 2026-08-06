use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager};

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
    let mut x = bp.x + BALL - CHAT_W;
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
