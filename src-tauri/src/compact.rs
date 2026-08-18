//! 对话摘要压缩：工具结果裁剪之后仍然过长时，把较早历史收成检查点。
//! 摘要调用回放当前系统提示和工具定义，指令放在最后一条 user 消息，以便复用前缀缓存。

use anyhow::Result;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::commands::Settings;
use crate::llm::client::{self, LlmConfig};

/// 裁剪工具结果后，对话序列化字节仍超过该值才做摘要
pub const COMPACT_THRESHOLD_BYTES: usize = 64_000;
/// 摘要后逐字保留的近期消息预算
pub const RETAIN_TAIL_BYTES: usize = 12_000;
const MIN_COMPACTABLE_MESSAGES: usize = 6;
const MAX_SUMMARY_CHARS: usize = 4_000;

const SUMMARY_OPEN: &str = "<compacted-summary>";
const SUMMARY_CLOSE: &str = "</compacted-summary>";

const CHECKPOINT_PREAMBLE: &str = "这是自动生成的检查点，把更早的对话收成既定背景。直接接着后面的消息继续做，不要复述这段摘要，也不要向用户提起这次压缩。";

const COMPACTION_INSTRUCTION: &str = r#"你现在只负责压缩上面的对话，让另一个模型能接着做完用户的事。

严格按照下面的 Markdown 结构输出，每个小节都要保留；没有内容就写「（无）」。用短要点，不要写长段落。

## 用户目标
- [用户最初和后来的目标；措辞关键时原文引用]

## 已确认事实
- [路径、数据、页面状态、用户明确给出的信息]

## 已执行操作
- [做过什么、验证过什么、结果如何]

## 错误与纠正
- [失败原因和后来怎么改的；用户纠正过的偏好]

## 未完成事项
- [明确要求但还没做完的]

## 当前进度
- [检查点这一刻正在做的事]

## 下一步
- [紧接着该做的一件事，或「（无）」]

## 约束与偏好
- [红线、用户偏好、未决问题和继续所需材料]

规则：
- 用简体中文。路径、命令、报错原文、数字、专有名称保持原样。
- 忠实保留用户纠正和明确指令。
- 不要提及这次压缩请求本身。
- 只输出检查点正文，不要调用工具。
- 若上面已有 <compacted-summary>，那是旧检查点：保留仍成立的事实，丢掉过时内容，合并成一份。"#;

pub fn messages_bytes(messages: &[Value]) -> usize {
    messages.iter().map(|m| m.to_string().len()).sum()
}

pub fn stable_prefix_len(messages: &[Value]) -> usize {
    if messages.first().and_then(|m| m["role"].as_str()) != Some("system") {
        return 0;
    }
    let mut n = 1;
    if messages
        .get(1)
        .and_then(|m| m["content"].as_str())
        .is_some_and(|c| c.contains("<available_skills>"))
    {
        n = 2;
    }
    n
}

/// 找到应保留的尾部起点（该下标起的消息不进摘要）。切分对齐到工具调用/结果配对。
pub fn find_retain_start(messages: &[Value], retain_bytes: usize) -> Option<usize> {
    let prefix = stable_prefix_len(messages);
    if messages.len() <= prefix + MIN_COMPACTABLE_MESSAGES {
        return None;
    }
    let mut acc = 0usize;
    let mut retain_from = messages.len();
    for i in (prefix..messages.len()).rev() {
        acc += messages[i].to_string().len();
        retain_from = i;
        if acc >= retain_bytes {
            break;
        }
    }
    retain_from = align_tool_pair(messages, prefix, retain_from);
    if retain_from <= prefix + 2 {
        return None;
    }
    Some(retain_from)
}

fn align_tool_pair(messages: &[Value], prefix: usize, mut idx: usize) -> usize {
    if idx <= prefix {
        return prefix;
    }
    if messages.get(idx).and_then(|m| m["role"].as_str()) == Some("tool") {
        while idx > prefix && messages[idx]["role"].as_str() == Some("tool") {
            idx -= 1;
        }
        // 停在发出这些 tool 的 assistant 上，整组划进尾部
        return idx.max(prefix);
    }
    idx
}

pub fn checkpoint_message(summary: &str) -> Value {
    json!({
        "role": "user",
        "content": format!(
            "{preamble}\n\n{open}\n{summary}\n{close}",
            preamble = CHECKPOINT_PREAMBLE,
            open = SUMMARY_OPEN,
            close = SUMMARY_CLOSE,
            summary = summary.trim()
        ),
    })
}

fn compact_request_body(cfg: &LlmConfig, _settings: &Settings, messages: &[Value]) -> Value {
    let mut replayed = messages.to_vec();
    replayed.push(json!({
        "role": "user",
        "content": COMPACTION_INSTRUCTION,
    }));
    let mut body = json!({
        "model": cfg.model,
        "messages": replayed,
        "stream": true,
        "stream_options": { "include_usage": true },
        "temperature": 0.2,
        "max_tokens": 2048,
        "tools": crate::llm::tools::definitions(),
        "tool_choice": "none",
    });
    if cfg.base_url.contains("deepseek") {
        body["thinking"] = json!({ "type": "disabled" });
    }
    body
}

fn sanitize_summary(raw: &str) -> Option<String> {
    let cleaned = crate::llm::agent::strip_tool_call_text(raw);
    let mut text = cleaned.trim().to_string();
    if text.is_empty() {
        return None;
    }
    if let Some(start) = text.find(SUMMARY_OPEN) {
        let rest = &text[start + SUMMARY_OPEN.len()..];
        if let Some(end) = rest.find(SUMMARY_CLOSE) {
            text = rest[..end].trim().to_string();
        }
    }
    if text.chars().count() > MAX_SUMMARY_CHARS {
        text = text.chars().take(MAX_SUMMARY_CHARS).collect::<String>() + "…";
    }
    if text.chars().count() < 40 {
        return None;
    }
    Some(text)
}

/// 若对话仍过长，把较早消息换成检查点。失败时保持原文。
pub async fn compact_if_needed(
    app: &AppHandle,
    http: &reqwest::Client,
    cfg: &LlmConfig,
    settings: &Settings,
    messages: &mut Vec<Value>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<bool> {
    if messages_bytes(messages) < COMPACT_THRESHOLD_BYTES {
        return Ok(false);
    }
    let Some(cut) = find_retain_start(messages, RETAIN_TAIL_BYTES) else {
        return Ok(false);
    };
    let prefix = stable_prefix_len(messages);
    let compactable = messages_bytes(&messages[prefix..cut]);
    if compactable < RETAIN_TAIL_BYTES {
        return Ok(false);
    }

    let body = compact_request_body(cfg, settings, &messages[..cut]);
    let resp = client::stream_chat(http, cfg, &body, cancel, |_| {}, |_| {}).await?;
    crate::llm::persist_usage(app, "compact", &cfg.model, &resp.usage);
    if resp.interrupted || cancel.is_cancelled() {
        return Ok(false);
    }
    let Some(summary) = sanitize_summary(&resp.content) else {
        log::warn!("摘要压缩未得到可用正文，跳过");
        return Ok(false);
    };
    let checkpoint = checkpoint_message(&summary);
    if checkpoint.to_string().len() >= compactable {
        log::info!("摘要未能缩小上下文，跳过");
        return Ok(false);
    }

    let mut next = messages[..prefix].to_vec();
    next.push(checkpoint);
    next.extend_from_slice(&messages[cut..]);
    *messages = next;
    log::info!(
        "已压缩较早对话：保留 {} 条近期消息",
        messages.len().saturating_sub(prefix + 1)
    );
    Ok(true)
}

pub fn persist_cover(
    app: &AppHandle,
    session_id: i64,
    cover_until_id: i64,
    messages: &[Value],
) {
    let prefix = stable_prefix_len(messages);
    let Some(content) = messages
        .get(prefix)
        .and_then(|m| m["content"].as_str())
        .filter(|c| c.contains(SUMMARY_OPEN))
    else {
        return;
    };
    let inner = content
        .split(SUMMARY_OPEN)
        .nth(1)
        .and_then(|rest| rest.split(SUMMARY_CLOSE).next())
        .unwrap_or("")
        .trim();
    if inner.is_empty() || cover_until_id <= 0 {
        return;
    }
    let state = app.state::<crate::AppState>();
    if let Err(e) = state
        .db
        .put_session_compact(session_id, cover_until_id, inner)
    {
        log::warn!("保存对话摘要失败: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sys(content: &str) -> Value {
        json!({ "role": "system", "content": content })
    }
    fn user(content: &str) -> Value {
        json!({ "role": "user", "content": content })
    }
    fn assistant(content: &str) -> Value {
        json!({ "role": "assistant", "content": content })
    }
    fn tool(id: &str, content: &str) -> Value {
        json!({ "role": "tool", "tool_call_id": id, "content": content })
    }
    fn assistant_tools(id: &str) -> Value {
        json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": [{ "id": id, "type": "function", "function": { "name": "read_file", "arguments": "{}" } }]
        })
    }

    #[test]
    fn prefix_includes_skills_catalog() {
        let messages = vec![
            sys("你是拾光"),
            sys("Skills 是可复用的任务说明。当前已启用：\n\n<available_skills>\n- `a`: b\n</available_skills>"),
            user("整理桌面"),
        ];
        assert_eq!(stable_prefix_len(&messages), 2);
    }

    #[test]
    fn retain_start_does_not_split_tool_pair() {
        let mut messages = vec![sys("sys")];
        for i in 0..10 {
            messages.push(user(&format!("u{i}")));
            messages.push(assistant(&format!("a{i}")));
        }
        messages.push(assistant_tools("c1"));
        let big = "huge output ".repeat(800);
        messages.push(tool("c1", &big));
        messages.push(user("继续"));
        let cut = find_retain_start(&messages, 500).unwrap();
        assert_eq!(messages[cut]["role"], "assistant");
        assert!(messages[cut].get("tool_calls").is_some());
    }

    #[test]
    fn checkpoint_wraps_summary() {
        let msg = checkpoint_message("## 用户目标\n- 整理桌面");
        let content = msg["content"].as_str().unwrap();
        assert!(content.contains(SUMMARY_OPEN));
        assert!(content.contains("整理桌面"));
        assert!(content.contains(CHECKPOINT_PREAMBLE));
    }

    #[test]
    fn sanitize_extracts_inner_tag() {
        let raw = format!("废话\n{SUMMARY_OPEN}\n## 用户目标\n- 做一件足够长的事，好让摘要通过最短字数检查，这里再补一些说明文字。\n{SUMMARY_CLOSE}\n");
        let got = sanitize_summary(&raw).unwrap();
        assert!(got.contains("用户目标"));
        assert!(!got.contains(SUMMARY_OPEN));
    }
}
