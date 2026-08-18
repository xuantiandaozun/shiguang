pub mod agent;
pub mod client;
pub mod profile;
pub mod prompts;
pub mod subagent;
pub mod tools;
pub mod vision;

use tauri::{AppHandle, Emitter, Manager};

/// 把一次模型调用的用量写入本地库，并通知主窗口刷新。
/// usage 为空（接口未返回、或请求被中断）时静默跳过。
pub fn persist_usage(app: &AppHandle, source: &str, model: &str, usage: &client::TokenUsage) {
    if usage.is_empty() {
        return;
    }
    let state = app.state::<crate::AppState>();
    if let Err(e) = state.db.insert_llm_usage(
        source,
        model,
        usage.prompt_tokens,
        usage.completion_tokens,
        usage.total_tokens,
        usage.cache_hit_tokens,
        usage.cache_miss_tokens,
    ) {
        log::warn!("记录模型用量失败: {e}");
        return;
    }
    let _ = app.emit("llm-usage-changed", ());
}
