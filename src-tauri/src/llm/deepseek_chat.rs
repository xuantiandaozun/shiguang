//! DeepSeek 专属能力（仅特定场景，不是独立对话模式）：
//! - 对话前缀续写 (`prefix: true` on `/beta`)，用于继续生成被截断的回复。
//! - 云端文档解析 (`parse_document`)：Files API 上传 + deepseek-flash 读取，作为 agent 工具
//!   `deepseek_parse_file`，仅在本地解析不了或效果不好时使用。
//!
//! 主对话统一走 agent（本地 skill / 工具优先）。DeepSeek 的联网搜索、读链接只作为
//! `web_search` / `web_fetch` 的兜底（见 `web.rs`），不再有单独的“原生对话”模式。

use crate::commands::load_settings;
use crate::db::ChatMsg;
use crate::llm::{client, deepseek, provider_headers};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::time::UNIX_EPOCH;
use tauri::{AppHandle, Emitter, Manager};
use tokio_util::sync::CancellationToken;

const MAX_CHAT_HISTORY: usize = 100;
const REFUSAL_DISPLAY: &str = "抱歉，该内容未能通过安全审核，无法提供回答。";

fn parse_meta(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| json!({}))
}

/// 把历史还原成纯文本的 user / assistant 消息（agent 的工具调用过程不回放）。
fn build_history_messages(history: &[ChatMsg]) -> Vec<Value> {
    let mut messages = Vec::with_capacity(history.len());
    for msg in history {
        if msg.role == "user" {
            messages.push(json!({ "role": "user", "content": msg.content }));
        } else if msg.role == "assistant" {
            let content = crate::llm::agent::visible_assistant_content(&msg.content);
            if content.is_empty() {
                continue;
            }
            messages.push(json!({ "role": "assistant", "content": content }));
        }
    }
    messages
}

/// 使用 DeepSeek `/beta` 对话前缀续写 (`prefix: true`) 继续生成一条未完成的回复。
pub async fn continue_deepseek_chat(
    app: AppHandle,
    session_id: i64,
    message_id: i64,
    cancel: CancellationToken,
) -> Result<()> {
    let settings = {
        let state = app.state::<crate::AppState>();
        load_settings(&state.db)
    };
    let history = {
        let state = app.state::<crate::AppState>();
        state.db.load_chat(session_id, MAX_CHAT_HISTORY)?
    };
    let Some(pos) = history.iter().position(|m| m.id == message_id) else {
        bail!("找不到要续写的消息");
    };
    let target = &history[pos];
    if target.role != "assistant" {
        bail!("只能续写 AI 的回复");
    }
    let target_meta = parse_meta(&target.meta_json);

    let cfg = deepseek::resolve_deepseek_chat_config(&settings, false)?;
    let beta_cfg = client::LlmConfig {
        base_url: provider_headers::deepseek_beta_base_url(&cfg.base_url),
        api_key: cfg.api_key.clone(),
        model: cfg.model.clone(),
    };

    let mut messages = build_history_messages(&history[..pos]);
    let prefix_content = target.content.clone();
    messages.push(json!({
        "role": "assistant",
        "content": prefix_content,
        "prefix": true,
    }));

    let mut body = json!({
        "model": beta_cfg.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if settings.deepseek_safety_check {
        body["safety_check"] = json!({ "level": "high" });
    }

    let http = reqwest::Client::new();
    let app_text = app.clone();
    let app_reasoning = app.clone();
    let resp = client::stream_chat_with_events(
        &http,
        &beta_cfg,
        &body,
        &cancel,
        move |delta| {
            let _ = app_text.emit("deepseek-continue-token", json!({ "db_id": message_id, "delta": delta }));
        },
        move |delta| {
            let _ = app_reasoning.emit("llm-reasoning", json!({ "delta": delta }));
        },
        |_| {},
    )
    .await?;

    crate::llm::persist_usage(&app, "deepseek_continue", &beta_cfg.model, &resp.usage);

    let refused = resp.finish_reason == "content_filter";
    let merged = if refused {
        REFUSAL_DISPLAY.to_string()
    } else {
        format!("{}{}", prefix_content, resp.content)
    };
    let mut next_meta = target_meta;
    next_meta["finish_reason"] = json!(if resp.interrupted { "cancelled" } else { &resp.finish_reason });
    next_meta["refused"] = json!(refused);
    let meta_str = serde_json::to_string(&next_meta).unwrap_or_else(|_| "{}".to_string());
    {
        let state = app.state::<crate::AppState>();
        state.db.update_chat_message(message_id, &merged, &meta_str)?;
    }

    let _ = app.emit(
        "deepseek-continue-done",
        json!({
            "db_id": message_id,
            "content": merged,
            "meta": next_meta,
            "interrupted": resp.interrupted,
        }),
    );
    Ok(())
}

const MAX_PARSE_CHARS: usize = 60_000;
const DEFAULT_PARSE_QUESTION: &str = "请完整转写该文件内容为 Markdown：保留标题层级、列表，表格用 Markdown 表格，图表/图片中的文字与数据尽量提取，无法识别的图用方括号简述。不要总结、不要省略。";

/// 云端文档解析：上传到 DeepSeek Files API（按路径+大小+修改时间缓存 file_id），再让 deepseek-flash 读取。
/// `layout=true` 用图文混排解析（含图表），否则只提取纯文字。
pub async fn parse_document(
    app: &AppHandle,
    path: &Path,
    question: Option<&str>,
    layout: bool,
    cancel: &CancellationToken,
) -> Result<Value> {
    let settings = {
        let state = app.state::<crate::AppState>();
        load_settings(&state.db)
    };
    let (root, api_key) = deepseek::resolve_deepseek_credentials(&settings)
        .ok_or_else(|| anyhow!("当前主模型不是 DeepSeek，或尚未填写 DeepSeek API Key，无法使用云端解析"))?;
    let doc_purpose = if layout { "rich_layout_recognition" } else { "text_recognition" };
    let plan = deepseek::classify_file_purpose(path, doc_purpose)?;
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| anyhow!("读取文件信息失败: {e}"))?;
    let size = meta.len() as i64;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let key = path.to_string_lossy().to_string();
    let http = reqwest::Client::new();

    let cached = {
        let state = app.state::<crate::AppState>();
        state
            .db
            .get_cached_deepseek_file(&key, size, modified, &plan.purpose)
            .ok()
            .flatten()
    };
    let file_id = match cached {
        Some(id) => id,
        None => {
            let (uploaded, _) =
                deepseek::upload_file(&http, &root, &api_key, path, doc_purpose, Some(cancel)).await?;
            let state = app.state::<crate::AppState>();
            let _ = state.db.put_cached_deepseek_file(
                &key,
                size,
                modified,
                &plan.purpose,
                &uploaded.id,
                &uploaded.filename,
            );
            uploaded.id
        }
    };

    let cfg = deepseek::resolve_deepseek_chat_config(&settings, true)?;
    let ask = question.map(str::trim).filter(|q| !q.is_empty()).unwrap_or(DEFAULT_PARSE_QUESTION);
    let body = json!({
        "model": cfg.model,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "file", "file_id": file_id },
                { "type": "text", "text": ask },
            ],
        }],
        "stream": true,
        "stream_options": { "include_usage": true },
        "thinking": { "type": "disabled" },
    });
    let resp = client::stream_chat(&http, &cfg, &body, cancel, |_| {}, |_| {}).await?;
    crate::llm::persist_usage(app, "deepseek_parse_file", &cfg.model, &resp.usage);
    let total = resp.content.chars().count();
    let content: String = resp.content.chars().take(MAX_PARSE_CHARS).collect();
    Ok(json!({
        "path": key,
        "engine": "deepseek",
        "parse_mode": doc_purpose,
        "content": content,
        "truncated": total > MAX_PARSE_CHARS,
        "note": "这是云端解析结果，可能有遗漏；关键数字/表格请与本地读取结果交叉核对。",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_plain_text_turns() {
        let history = vec![
            ChatMsg {
                id: 1,
                role: "user".into(),
                content: "总结文档".into(),
                created_at: String::new(),
                meta_json: "{}".into(),
            },
            ChatMsg {
                id: 2,
                role: "assistant".into(),
                content: "文档讲了三点。".into(),
                created_at: String::new(),
                meta_json: "{}".into(),
            },
        ];
        let msgs = build_history_messages(&history);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["content"], "文档讲了三点。");
    }
}
