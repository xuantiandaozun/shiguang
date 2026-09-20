//! 对话摘要压缩：工具结果裁剪之后仍然过长时，把较早历史收成检查点。
//! 摘要调用回放当前系统提示和工具定义，指令放在最后一条 user 消息，以便复用前缀缓存。

use anyhow::Result;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::commands::Settings;
use crate::llm::client::{self, LlmConfig};

/// 摘要后逐字保留的近期消息预算
pub const RETAIN_TAIL_BYTES: usize = 12_000;
const CONTEXT_USAGE_RATIO: f64 = 0.80;
const RESERVED_OUTPUT_TOKENS: usize = 4_096;
const MIN_COMPACTABLE_MESSAGES: usize = 6;
const MAX_SUMMARY_CHARS: usize = 4_000;

const SUMMARY_OPEN: &str = "<compacted-summary>";
const SUMMARY_CLOSE: &str = "</compacted-summary>";

const CHECKPOINT_PREAMBLE: &str = "这是自动生成的检查点，把更早的对话收成既定背景。直接接着后面的消息继续做，不要复述这段摘要，也不要向用户提起这次压缩。检查点没写进去的用户原话、定稿、路径和数字仍在本会话记录里，需要原样使用时先调用 recall_session 查回再继续，不要凭这段摘要重写，也不要把 message_id 传给 get_tool_call_history。";

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
- 若上面已有 <compacted-summary>，那是旧检查点：保留仍成立的事实，丢掉过时内容，合并成一份。
- 不必把长正文抄进摘要。定稿、原文、用户纠正过的措辞如果没有逐字保留，就在对应小节写上可用 recall_session 命中的检索词（专有名称或原话短句），并注明「原文未收入摘要」。"#;

pub fn messages_bytes(messages: &[Value]) -> usize {
    messages.iter().map(|m| m.to_string().len()).sum()
}

/// 轻量、保守的 token 估算：ASCII 文本约 4 字符/token，中文及其它 Unicode
/// 大致按 1 字符/token，并为 JSON 结构留少量余量。真正触发点由模型窗口决定。
pub fn estimate_tokens(value: &Value) -> usize {
    let text = value.to_string();
    let (ascii, non_ascii) = text.chars().fold((0usize, 0usize), |(a, n), ch| {
        if ch.is_ascii() {
            (a + 1, n)
        } else {
            (a, n + 1)
        }
    });
    ascii.div_ceil(4) + non_ascii + 64
}

fn context_tokens(messages: &[Value]) -> usize {
    messages.iter().map(estimate_tokens).sum::<usize>()
        + estimate_tokens(&crate::llm::tools::definitions())
}

fn should_compact(settings: &Settings, messages: &[Value]) -> bool {
    let usable = ((settings.context_window as f64) * CONTEXT_USAGE_RATIO) as usize;
    context_tokens(messages).saturating_add(RESERVED_OUTPUT_TOKENS) >= usable
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
    });
    if cfg.base_url.contains("deepseek") {
        body["thinking"] = json!({ "type": "disabled" });
    }
    crate::llm::provider_headers::sanitize_agentrouter_body(&mut body, &cfg.base_url, &cfg.model);
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
    if !should_compact(settings, messages) {
        return Ok(false);
    }
    let retain_bytes =
        if settings.system_one_context_assist && crate::system_one::configured(settings) {
            match crate::system_one::context_retention_strategy(http, settings, messages).await {
                Ok(answer) => {
                    crate::llm::persist_usage(
                        app,
                        "system_one_context",
                        &settings.system_one_model,
                        &answer.usage,
                    );
                    if answer.confidence < settings.system_one_min_confidence {
                        RETAIN_TAIL_BYTES
                    } else {
                        match answer.choice.as_str() {
                            "preserve_more" => RETAIN_TAIL_BYTES * 2,
                            "compress_more" => RETAIN_TAIL_BYTES * 2 / 3,
                            _ => RETAIN_TAIL_BYTES,
                        }
                    }
                }
                Err(e) => {
                    log::warn!("System One 上下文策略失败，使用标准保留预算: {e}");
                    RETAIN_TAIL_BYTES
                }
            }
        } else {
            RETAIN_TAIL_BYTES
        };
    let Some(cut) = find_retain_start(messages, retain_bytes) else {
        return Ok(false);
    };
    let prefix = stable_prefix_len(messages);
    let compactable = messages_bytes(&messages[prefix..cut]);
    if compactable < RETAIN_TAIL_BYTES {
        return Ok(false);
    }

    let compact_cfg = if settings.compact_model.trim().is_empty() {
        cfg.clone()
    } else {
        LlmConfig::new(&cfg.base_url, &cfg.api_key, &settings.compact_model)
    };
    let body = compact_request_body(&compact_cfg, settings, &messages[..cut]);
    let resp = client::stream_chat(http, &compact_cfg, &body, cancel, |_| {}, |_| {}).await?;
    crate::llm::persist_usage(app, "compact", &compact_cfg.model, &resp.usage);
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

pub fn persist_cover(app: &AppHandle, session_id: i64, cover_until_id: i64, messages: &[Value]) {
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

    #[test]
    fn token_estimate_counts_chinese_more_than_ascii_per_character() {
        let ascii = estimate_tokens(&json!("a".repeat(400)));
        let chinese = estimate_tokens(&json!("中".repeat(400)));
        assert!(chinese > ascii * 2);
    }

    #[test]
    fn large_context_does_not_compact_at_old_fixed_byte_threshold() {
        let mut settings: Settings = serde_json::from_value(json!({
            "base_url":"x","api_key":"x","model":"x","llm_profiles":[],"active_llm_profile_id":"",
            "organize_root":"","auto_organize":false,"autostart":false,"desktop_path":"",
            "thinking_enabled":false,"reasoning_effort":"low","vision_base_url":"","vision_api_key":"","vision_model":"",
            "subagent_thinking_enabled":false,"subagent_reasoning_effort":"low","subagent_model":"",
            "context_window":128000,"command_tools_enabled":false,"permission_level":"balanced",
            "profile_name":"","profile_alias":"","profile_gender":"","profile_birth":"","profile_phone":"","profile_email":"","profile_city":"","temp_path":""
        })).unwrap();
        settings.context_window = 128_000;
        assert!(!should_compact(
            &settings,
            &[json!({"role":"user","content":"x".repeat(64_000)})]
        ));
    }
}
