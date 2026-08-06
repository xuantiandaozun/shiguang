use std::sync::atomic::Ordering;
use tauri::menu::{CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

pub fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    let show_chat = MenuItemBuilder::with_id("show_chat", "打开聊天窗").build(app)?;
    let open_main = MenuItemBuilder::with_id("open_main", "打开主窗口").build(app)?;
    let paused = {
        let state = app.state::<crate::AppState>();
        state.auto_paused.load(Ordering::Relaxed)
    };
    let pause_item = CheckMenuItemBuilder::with_id("pause_auto", "暂停自动整理")
        .checked(paused)
        .build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "退出拾光").build(app)?;

    let menu = MenuBuilder::new(app)
        .items(&[&show_chat, &open_main])
        .separator()
        .item(&pause_item)
        .separator()
        .item(&quit)
        .build()?;

    TrayIconBuilder::with_id("main-tray")
        .tooltip("拾光 · AI 桌面助手")
        .icon(app.default_window_icon().unwrap().clone())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show_chat" => crate::windows::show_chat(app),
            "open_main" => crate::windows::open_main(app),
            "pause_auto" => {
                let state = app.state::<crate::AppState>();
                let new_val = !state.auto_paused.load(Ordering::Relaxed);
                state.auto_paused.store(new_val, Ordering::Relaxed);
                let _ = state
                    .db
                    .set_setting("auto_organize_paused", if new_val { "true" } else { "false" });
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::windows::toggle_chat(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}
