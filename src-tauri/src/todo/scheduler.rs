use tauri::{AppHandle, Emitter, Manager};
use tokio::time::{interval, Duration};

/// 后台提醒调度：每 30 秒检查一次到期待办，按 remind_mode 分发——
/// notify：Windows 系统通知；popup / popup_input：提醒弹窗（reminder 窗口）。
/// 重复待办（daily/weekly）在提醒后自动顺延到下一周期。
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut ticker = interval(Duration::from_secs(30));
        loop {
            ticker.tick().await;
            if let Err(e) = tick(&app) {
                log::warn!("提醒调度失败: {}", e);
            }
        }
    });
}

fn tick(app: &AppHandle) -> anyhow::Result<()> {
    let now_str = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let state = app.state::<crate::AppState>();
    let due = state.db.due_todos(&now_str)?;
    for todo in due {
        if todo.remind_mode == "popup" || todo.remind_mode == "popup_input" {
            // 弹窗提醒：reminder 窗口监听 reminder-popup 入队并展示
            let _ = app.emit("reminder-popup", &todo);
            crate::windows::show_reminder(app);
        } else {
            let body = match &todo.due_at {
                Some(d) => format!("{}\n截止：{}", todo.title, d),
                None => todo.title.clone(),
            };
            crate::notify_user(app, "待办提醒", &body);
        }

        match todo.repeat_rule.as_str() {
            "daily" | "weekly" => {
                if let Some(due) = &todo.due_at {
                    if let Ok(t) = chrono::NaiveDateTime::parse_from_str(due, "%Y-%m-%d %H:%M:%S") {
                        let days = if todo.repeat_rule == "daily" { 1 } else { 7 };
                        let next = t + chrono::Duration::days(days);
                        let _ = state
                            .db
                            .snooze(todo.id, &next.format("%Y-%m-%d %H:%M:%S").to_string());
                    }
                }
            }
            _ => {
                let _ = state.db.mark_reminded(todo.id);
            }
        }

        let _ = app.emit(
            "reminder-fired",
            serde_json::json!({ "id": todo.id, "title": todo.title }),
        );
        let _ = app.emit("todos-changed", ());
    }
    // 工作流和待办分开存储：待办提醒用户，工作流直接向 AI 发起一次固定执行请求。
    // 先推进 next_run_at，避免聊天忙碌时每 30 秒重复触发同一条流程。
    for workflow in state.db.due_automation_workflows(&now_str)? {
        let app_handle = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = crate::commands::run_automation_workflow(app_handle, workflow.id, true).await {
                log::warn!("定时工作流「{}」未能启动: {}", workflow.name, error);
            }
        });
    }
    Ok(())
}
