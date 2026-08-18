// tools.rs 的 json! 工具定义较大，默认 128 递归上限不够
#![recursion_limit = "256"]

pub mod browser;
pub mod builtin_skills;
pub mod cli_json;
pub mod commands;
pub mod db;
pub mod file_index;
pub mod llm;
pub mod lookup_cache;
pub mod machine;
pub mod ntfs_helper;
pub mod ntfs_usn;
pub mod ocr;
pub mod organizer;
pub mod reader;
pub mod skills;
pub mod tasks;
pub mod tempfs;
pub mod todo;
pub mod tray;
pub mod windows;
pub mod writer;

use std::sync::atomic::AtomicBool;
use tauri::Manager;

pub struct AppState {
    pub db: db::Db,
    /// 托盘「暂停自动整理」开关；true 时 watcher 不做任何自动移动
    pub auto_paused: AtomicBool,
    /// 防止聊天并发发送
    pub chat_busy: AtomicBool,
    /// 当前聊天回复的取消令牌；每次发送新建，stop_chat_message 触发取消
    pub chat_cancel: std::sync::Mutex<Option<tokio_util::sync::CancellationToken>>,
    /// 浏览器操作统一入口（扩展桥 / CDP）
    pub browser: browser::Hub,
    /// 本地 OCR（PaddleOCR），首次识别时懒加载模型
    pub ocr: ocr::OcrEngine,
    /// 后台命令任务管理器（输出落日志文件，不占对话上下文）
    pub tasks: tasks::TaskManager,
    /// 持久化文件元数据索引（简化版 Everything）
    pub file_index: file_index::FileIndex,
    /// Agent Skills（app_data/skills/）
    pub skills: skills::SkillStore,
    /// 外部 CLI/API 的稳定对照数据缓存
    pub lookup_cache: lookup_cache::LookupCache,
}

pub fn notify_user(app: &tauri::AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        log::warn!("发送系统通知失败: {}", e);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::init();
    // WebView2 默认走系统代理；开着 Clash/V2ray 时内置页面地址 tauri.localhost
    // 不在代理绕过列表里，会被送进代理导致"拒绝连接"。本窗口只加载本地资源，直接禁用代理。
    // 追加而非覆盖，保留外部传入额外参数的能力（如远程调试）。
    #[cfg(target_os = "windows")]
    {
        let existing = std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").unwrap_or_default();
        if !existing.contains("--no-proxy-server") {
            let merged = if existing.is_empty() {
                "--no-proxy-server".to_string()
            } else {
                format!("{} --no-proxy-server", existing)
            };
            std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", merged);
        }
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            windows::show_chat(app);
        }))
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::all()
                        ^ tauri_plugin_window_state::StateFlags::VISIBLE,
                )
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let label = window.label();
                if label == "main" || label == "chat" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            let app_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            std::fs::create_dir_all(&app_dir)?;
            let _ = std::fs::create_dir_all(tempfs::temp_dir_in(&app_dir));
            let db = db::Db::new(&app_dir.join("deskhelper.db"))?;
            let paused = db
                .get_setting("auto_organize_paused")
                .ok()
                .flatten()
                .map(|v| v == "true")
                .unwrap_or(false);
            app.manage(AppState {
                db,
                auto_paused: AtomicBool::new(paused),
                chat_busy: AtomicBool::new(false),
                chat_cancel: std::sync::Mutex::new(None),
                browser: browser::Hub::spawn(&app_dir),
                ocr: ocr::OcrEngine::new(&app_dir),
                tasks: tasks::TaskManager::new(&app_dir),
                file_index: file_index::FileIndex::new(&app_dir)?,
                skills: skills::SkillStore::new(&app_dir),
                lookup_cache: lookup_cache::LookupCache::new(&app_dir),
            });
            // 每次进程启动开空白会话（已有空会话则复用），与聊天窗欢迎态一致。
            {
                let state = app.state::<AppState>();
                if let Err(e) = state.db.start_fresh_session_if_needed() {
                    log::warn!("启动时创建新会话失败: {}", e);
                }
            }
            // Catch up NTFS changes that occurred while the app was closed.
            // This runs in the background; the previous index remains searchable.
            app.state::<AppState>().file_index.recover_usn_async();
            // 旧版「工作流经验」一次性迁成外部 Skills
            {
                let state = app.state::<AppState>();
                if let Err(e) = state.skills.migrate_workflows(&state.db) {
                    log::warn!("工作流迁移失败: {}", e);
                }
            }
            tray::create_tray(app.handle())?;
            todo::scheduler::spawn(app.handle().clone());
            organizer::watcher::spawn(app.handle().clone());
            log::info!("拾光已启动，数据目录: {:?}", app_dir);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::send_chat_message,
            commands::stop_chat_message,
            commands::get_current_session,
            commands::list_sessions,
            commands::new_session,
            commands::switch_session,
            commands::delete_session,
            commands::recall_message,
            commands::load_chat_history,
            commands::clear_chat_history,
            commands::toggle_chat,
            commands::open_main_window,
            commands::hide_chat,
            commands::open_external,
            commands::show_chat_window,
            commands::list_todos,
            commands::add_todo,
            commands::update_todo,
            commands::delete_todo,
            commands::set_todo_done,
            commands::snooze_todo_cmd,
            commands::get_pending_plan,
            commands::execute_plan_cmd,
            commands::cancel_plan,
            commands::list_batches,
            commands::undo_batch_cmd,
            commands::list_rules,
            commands::upsert_rule,
            commands::toggle_rule,
            commands::delete_rule,
            commands::get_settings,
            commands::save_settings,
            commands::get_temp_info,
            commands::clear_temp_dir,
            commands::scan_desktop_preview,
            commands::list_profile_entries,
            commands::save_profile_entry,
            commands::delete_profile_entry,
            commands::list_bg_tasks,
            commands::stop_bg_task,
            commands::read_bg_task_tail,
            commands::list_skills_cmd,
            commands::create_skill_cmd,
            commands::delete_skill_cmd,
            commands::set_skill_enabled,
            commands::scan_external_skills,
            commands::sync_skills_cmd,
            commands::read_skill_content,
            commands::get_llm_usage_stats,
        ])
        .run(tauri::generate_context!())
        .expect("error while running shiguang");
}
