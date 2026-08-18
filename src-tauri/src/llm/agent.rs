use crate::commands::load_settings;
use crate::db::{ChatMsg, ToolCallReplay};
use crate::llm::{client, prompts, tools};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use tauri::{AppHandle, Emitter, Manager};

/// 单次任务的工具调用轮次预算。浏览器类任务动辄 10+ 轮，给足余量。
const MAX_TOOL_ROUNDS: usize = 50;
/// 倒数第几轮时注入收尾提醒
const WARN_BEFORE_END: usize = 3;

/// 模型可能以纯文本形式输出工具调用的标记对（DeepSeek 特殊 token / DSML / XML 风格）。
/// 无工具请求（收尾总结）时模型尤其容易把这些写进正文，需过滤。
const TOOL_TEXT_MARKERS: &[(&str, &str)] = &[
    ("<｜tool▁calls▁begin｜>", "<｜tool▁calls▁end｜>"),
    ("<｜tool▁call▁begin｜>", "<｜tool▁call▁end｜>"),
    ("<tool_call>", "</tool_call>"),
    // DeepSeek DSML 格式：invoke 标记不带结尾 >，因为后面跟着 name 属性
    ("<｜｜DSML｜｜tool_calls>", "</｜｜DSML｜｜tool_calls>"),
    ("<｜｜DSML｜｜invoke", "</｜｜DSML｜｜invoke>"),
];

/// 模型把工具调用写成正文时，提醒改用标准格式重试的上限（防死循环）
const MAX_TEXT_CALL_RETRIES: usize = 3;

pub async fn run_chat(
    app: AppHandle,
    session_id: i64,
    request_message_id: i64,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let mut settings = {
        let state = app.state::<crate::AppState>();
        load_settings(&state.db)
    };
    settings.temp_path = crate::tempfs::temp_path_display(&app);

    if settings.api_key.trim().is_empty() {
        let msg = "尚未配置大模型 API Key。请打开主窗口 →「设置」页填写 Base URL / API Key / 模型后再试。";
        let state = app.state::<crate::AppState>();
        let _ = state.db.save_chat(session_id, "assistant", msg);
        let _ = app.emit("llm-error", json!({ "message": msg }));
        return Ok(());
    }

    let (history, profile_block, replay_calls, skills_block, lookup_block, stored_compact) = {
        let state = app.state::<crate::AppState>();
        let mut history = state.db.load_chat(session_id, 30)?;
        let stored_compact = state.db.get_session_compact(session_id).ok().flatten();
        if let Some((cover_until, _)) = &stored_compact {
            history.retain(|m| m.id > *cover_until);
        }
        let latest_user = history
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let request_ids: Vec<i64> = history
            .iter()
            .filter(|m| m.role == "user")
            .map(|m| m.id)
            .collect();
        let mut replay_calls = state
            .db
            .load_tool_calls_for_messages(session_id, &request_ids)?;
        if stored_compact.is_some() {
            let keep: HashSet<i64> = request_ids.iter().copied().collect();
            replay_calls.retain(|c| keep.contains(&c.request_message_id));
        }
        // 个人信息按需注入：命中求职/发帖等场景或用户主动要求时才加载
        let profile = if crate::llm::profile::should_inject(&latest_user) {
            let entries = state.db.pf_list().unwrap_or_default();
            crate::llm::profile::injection_block(&settings, &entries)
        } else {
            None
        };
        let skills_block = state.skills.catalog_reminder();
        let lookup_block = state.lookup_cache.catalog_block();
        (history, profile, replay_calls, skills_block, lookup_block, stored_compact)
    };

    // 上下文缓存：系统提示必须逐字节稳定；Skills 目录紧随其后（启用集不变则同样稳定）。
    // 当前时间插在历史之后、本轮工具轮之前：同一轮只往后面追加 assistant/tool，
    // 才能走 DeepSeek「A+B → A+B+C」前缀命中；不要每轮把时间挪到最后。
    let sys = prompts::system_prompt(&settings);
    let mut messages: Vec<Value> = vec![json!({
        "role": "system",
        "content": sys,
    })];
    if let Some(catalog) = &skills_block {
        messages.push(json!({
            "role": "system",
            "content": catalog,
        }));
    }
    if let Some((_, summary)) = &stored_compact {
        messages.push(crate::compact::checkpoint_message(summary));
    }
    messages.extend(history_with_tool_replay(&history, &replay_calls));
    let mut tail = format!(
        "当前时间：{}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S %A")
    );
    if let Some(pb) = &profile_block {
        tail.push_str(pb);
    }
    if let Some(lb) = &lookup_block {
        tail.push_str(lb);
    }
    messages.push(json!({
        "role": "system",
        "content": tail,
    }));
    // 本轮已完成的工具调用清单（名称+参数+结果摘要），中断时随内容入库，
    // 让下一轮对话能看到已收集的资料而不是从零开始
    let mut tool_log: Vec<String> = Vec::new();

    let http = reqwest::Client::new();
    let cfg = client::LlmConfig {
        base_url: settings.base_url.clone(),
        api_key: settings.api_key.clone(),
        model: settings.model.clone(),
    };
    let mut text_call_retries = 0usize;
    let mut repeat_guard = crate::repeat_guard::RepeatGuard::new();

    for round in 0..MAX_TOOL_ROUNDS {
        if cancel.is_cancelled() {
            finalize_cancelled(&app, session_id, request_message_id, "", &tool_log).await?;
            return Ok(());
        }
        if round == MAX_TOOL_ROUNDS - WARN_BEFORE_END {
            messages.push(json!({
                "role": "system",
                "content": "注意：本轮对话的工具调用预算即将用完，请优先收尾，用最少的步骤完成任务。",
            }));
        }
        trim_context(&app, &mut messages);
        if let Ok(true) = crate::compact::compact_if_needed(
            &app,
            &http,
            &cfg,
            &settings,
            &mut messages,
            &cancel,
        )
        .await
        {
            let cover_until = history
                .iter()
                .map(|m| m.id)
                .filter(|&id| id < request_message_id)
                .max()
                .unwrap_or(0);
            crate::compact::persist_cover(&app, session_id, cover_until, &messages);
        }
        let body = request_body(&cfg, &settings, &messages, true);

        let resp = stream_filtered(&http, &cfg, &body, &cancel, &app).await?;

        if resp.interrupted {
            finalize_cancelled(
                &app,
                session_id,
                request_message_id,
                &resp.content,
                &tool_log,
            )
            .await?;
            return Ok(());
        }

        if resp.tool_calls.is_empty() {
            let cleaned = strip_tool_call_text(&resp.content);
            // 模型把工具调用写成了正文：本意是继续调工具而非收尾。
            // 清理后的正文放回对话并提醒改用标准格式，给重试机会（限次数防死循环）
            if contains_tool_call_text(&resp.content) && text_call_retries < MAX_TEXT_CALL_RETRIES {
                text_call_retries += 1;
                log::warn!(
                    "模型以文本形式输出工具调用，提醒改用标准格式（第 {text_call_retries} 次）"
                );
                if !cleaned.is_empty() {
                    messages.push(json!({ "role": "assistant", "content": cleaned }));
                }
                messages.push(json!({
                    "role": "system",
                    "content": "注意：你刚才把工具调用以纯文本形式写进了回复正文，工具并未真正执行。如需使用工具，必须通过标准工具调用机制发起，正文里不要出现任何调用语法（包括 <｜｜DSML｜｜tool_calls>、<tool_call> 或调用形态的 JSON）。",
                }));
                continue;
            }
            if cleaned.is_empty() {
                // 正文整体跑偏成了文本形式的工具调用，没有可展示的内容
                let _ = app.emit(
                    "llm-error",
                    json!({ "message": "模型输出异常（未产生有效内容），请重试。" }),
                );
                return Ok(());
            }
            let state = app.state::<crate::AppState>();
            let response_message_id = state.db.save_chat(session_id, "assistant", &cleaned)?;
            state.db.link_tool_calls_response(
                session_id,
                request_message_id,
                response_message_id,
            )?;
            let _ = app.emit("llm-message-done", json!({ "content": cleaned }));
            let _ = app.emit("sessions-changed", ());
            drop(state);
            return Ok(());
        }

        let tool_calls_json: Vec<Value> = resp
            .tool_calls
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "type": "function",
                    "function": { "name": t.name, "arguments": t.arguments },
                })
            })
            .collect();
        // DeepSeek 思考模式要求：工具调用轮次的思维链必须随 assistant 消息回传，否则下轮请求 400
        let clean_content = strip_tool_call_text(&resp.content);
        let mut assistant_msg = json!({
            "role": "assistant",
            "content": if clean_content.is_empty() { Value::Null } else { json!(clean_content) },
            "tool_calls": tool_calls_json,
        });
        if !resp.reasoning_content.is_empty() {
            assistant_msg["reasoning_content"] = json!(resp.reasoning_content);
        }
        messages.push(assistant_msg);

        for (call_index, call) in resp.tool_calls.iter().enumerate() {
            // 正在执行的工具让其跑完（保证文件操作一致性），在下一个工具前中断
            if cancel.is_cancelled() {
                finalize_cancelled(
                    &app,
                    session_id,
                    request_message_id,
                    &resp.content,
                    &tool_log,
                )
                .await?;
                return Ok(());
            }
            // 先落 running 记录再执行，避免有副作用的动作成功后却无调用轨迹。
            let tool_record_id = {
                let state = app.state::<crate::AppState>();
                state.db.start_tool_call(
                    session_id,
                    request_message_id,
                    round,
                    call_index,
                    &call.id,
                    &call.name,
                    &call.arguments,
                    &clean_content,
                    &resp.reasoning_content,
                )?
            };
            let _ = app.emit(
                "tool-status",
                json!({ "name": call.name, "status": "running" }),
            );
            let parsed_args: Value =
                serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));
            let result = match tools::execute(&app, &call.name, &parsed_args, &cancel).await {
                Ok(v) => v,
                Err(e) => json!({ "error": e.to_string() }),
            };
            let tool_status = if is_failed_tool_result(&result) {
                "error"
            } else {
                "done"
            };
            let content = model_tool_content(&app, &call.name, &call.id, &result);
            {
                let state = app.state::<crate::AppState>();
                state
                    .db
                    .finish_tool_call(tool_record_id, tool_status, &content)?;
            }
            push_tool_log(&mut tool_log, &call.name, &call.arguments, &result);
            let emit_result = if crate::retention::is_bounded(&content) {
                json!({
                    "truncated": true,
                    "ok": result.get("ok"),
                    "error": result.get("error"),
                    "status": result.get("status"),
                })
            } else {
                result.clone()
            };
            let _ = app.emit(
                "tool-status",
                json!({ "name": call.name, "status": tool_status, "result": emit_result }),
            );
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": content,
            }));
            if let Some(reminder) = repeat_guard.observe(&call.name, &call.arguments) {
                messages.push(crate::repeat_guard::reminder_message(&reminder));
            }
        }
    }

    finalize_with_summary(
        &app,
        &http,
        &cfg,
        &settings,
        session_id,
        request_message_id,
        messages,
        &cancel,
        &tool_log,
    )
    .await
}

/// 工具协议层成功返回并不等于业务动作成功；命令非零退出等情况也要让界面显示失败。
fn is_failed_tool_result(result: &Value) -> bool {
    if result.get("ok").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    if result.get("error").is_some() {
        return true;
    }
    matches!(
        result.get("status").and_then(Value::as_str),
        Some("failed" | "timeout" | "cancelled")
    )
}

/// 构造请求体。工具定义每轮都发同一份全量清单，避免中途增删打断前缀缓存。
/// 收尾轮次仍带上同一份 tools，只用 tool_choice=none 禁止再调。
/// DeepSeek 思考模式参数只对 DeepSeek 接口附加，其它兼容服务收到未知字段可能 400。
fn request_body(
    cfg: &client::LlmConfig,
    settings: &crate::commands::Settings,
    messages: &[Value],
    allow_tools: bool,
) -> Value {
    let mut body = json!({
        "model": cfg.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
        "temperature": 0.3,
        "tools": tools::definitions(),
        "tool_choice": if allow_tools { json!("auto") } else { json!("none") },
    });
    if cfg.base_url.contains("deepseek") {
        if settings.thinking_enabled {
            body["thinking"] = json!({ "type": "enabled" });
            body["reasoning_effort"] = json!(settings.reasoning_effort);
        } else {
            body["thinking"] = json!({ "type": "disabled" });
        }
    }
    body
}

/// 用户中断：部分内容 + 本轮已完成的工具调用摘要一起入库（标注中断），
/// 后续「继续」时模型能直接基于已收集的资料推进，而不是从头再来。
/// 通知前端收尾。被中断的路径不做工作流提炼，也不计工作流使用次数。
async fn finalize_cancelled(
    app: &AppHandle,
    session_id: i64,
    request_message_id: i64,
    partial: &str,
    tool_log: &[String],
) -> Result<()> {
    let cleaned = strip_tool_call_text(partial);
    // 工具参数/结果已在 chat_tool_calls，不要写进可见正文，否则加载历史会把调用参数渲染出来。
    let saved = if cleaned.is_empty() && !tool_log.is_empty() {
        "（已中断）".to_string()
    } else {
        cleaned.clone()
    };
    if !saved.is_empty() {
        let state = app.state::<crate::AppState>();
        let response_message_id = state.db.save_chat(session_id, "assistant", &saved)?;
        state
            .db
            .link_tool_calls_response(session_id, request_message_id, response_message_id)?;
    }
    let _ = app.emit("llm-cancelled", json!({ "content": cleaned }));
    let _ = app.emit("sessions-changed", ());
    Ok(())
}

/// 记录一次工具调用的简要信息（参数 160 字符、结果 500 字符封顶）
fn push_tool_log(log: &mut Vec<String>, name: &str, args: &str, result: &Value) {
    log.push(format!(
        "- {}({}) → {}",
        name,
        brief(args, 160),
        brief(&result.to_string(), 500)
    ));
}

fn brief(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > n {
        format!("{}…", flat.chars().take(n).collect::<String>())
    } else {
        flat
    }
}

/// 工具预算耗尽：不再给工具，让模型基于已有信息做阶段性收尾。
/// 收尾内容会存入聊天记录，用户说「继续」时模型能看到进展，而不是从零开始。
async fn finalize_with_summary(
    app: &AppHandle,
    http: &reqwest::Client,
    cfg: &client::LlmConfig,
    settings: &crate::commands::Settings,
    session_id: i64,
    request_message_id: i64,
    mut messages: Vec<Value>,
    cancel: &tokio_util::sync::CancellationToken,
    tool_log: &[String],
) -> Result<()> {
    messages.push(json!({
        "role": "system",
        "content": "工具调用轮次已用完。请基于已获得的信息，直接用自然语言给出阶段性回答：1) 已完成的部分；2) 卡在什么地方；3) 如果用户说「继续」，你接下来打算怎么做。不要声称完成了并未完成的步骤。严禁输出任何工具调用语法（例如 <tool_call>…</tool_call>、<｜tool▁call▁begin｜>…、<｜｜DSML｜｜tool_calls>…</｜｜DSML｜｜tool_calls>、或带 \"name\"/\"arguments\" 字段的 JSON 代码块），只能输出给用户看的自然语言。",
    }));
    trim_context(app, &mut messages);
    let body = request_body(cfg, settings, &messages, false);
    let resp = stream_filtered(http, cfg, &body, cancel, app).await;
    match resp {
        Ok(r) if r.interrupted => {
            finalize_cancelled(app, session_id, request_message_id, &r.content, tool_log).await?;
        }
        Ok(r) => {
            let cleaned = strip_tool_call_text(&r.content);
            // 工具结果已在 chat_tool_calls 回放，不要把参数摘要写进聊天正文。
            let display = if cleaned.is_empty() {
                "（工具调用轮次已用完，本轮进展已保存。说「继续」即可接着推进。）".to_string()
            } else {
                cleaned
            };
            let state = app.state::<crate::AppState>();
            let response_message_id = state.db.save_chat(session_id, "assistant", &display)?;
            state.db.link_tool_calls_response(
                session_id,
                request_message_id,
                response_message_id,
            )?;
            let _ = app.emit("llm-message-done", json!({ "content": display }));
            let _ = app.emit("sessions-changed", ());
        }
        Err(e) => {
            let _ = app.emit(
                "llm-error",
                json!({ "message": format!("工具调用轮次已用完，收尾总结失败: {}。可直接说「继续」重试。", e) }),
            );
        }
    }
    Ok(())
}

/// 发起一轮流式请求：正文增量先经 ToolTextFilter（剔除文本形式的工具调用）再透传前端，
/// 思维链增量直接透传；流结束后把过滤器里残留的正常文本尾巴补发出去。
async fn stream_filtered(
    http: &reqwest::Client,
    cfg: &client::LlmConfig,
    body: &Value,
    cancel: &tokio_util::sync::CancellationToken,
    app: &AppHandle,
) -> Result<client::AssistantResp> {
    let filter = std::sync::Arc::new(std::sync::Mutex::new(ToolTextFilter::new()));
    let filter_cb = filter.clone();
    let app_text = app.clone();
    let app_reasoning = app.clone();
    let resp = client::stream_chat(
        http,
        cfg,
        body,
        cancel,
        move |delta| {
            if let Ok(mut f) = filter_cb.lock() {
                let pass = f.feed(delta);
                if !pass.is_empty() {
                    let _ = app_text.emit("llm-token", json!({ "delta": pass }));
                }
            }
        },
        move |delta| {
            let _ = app_reasoning.emit("llm-reasoning", json!({ "delta": delta }));
        },
    )
    .await?;
    crate::llm::persist_usage(app, "chat", &cfg.model, &resp.usage);
    if let Ok(mut f) = filter.lock() {
        let tail = f.flush();
        if !tail.is_empty() {
            let _ = app.emit("llm-token", json!({ "delta": tail }));
        }
    }
    Ok(resp)
}

/// 流式文本过滤器：在增量文本透传给前端前，实时剔除文本形式的工具调用。
/// 标记可能随 SSE 分片到达，尾部与标记前缀重叠的部分暂缓透传，等后续片段拼全再判定。
struct ToolTextFilter {
    pending: String,
    /// 已进入工具调用块时，等待的结束标记
    suppress_end: Option<&'static str>,
}

impl ToolTextFilter {
    fn new() -> Self {
        Self {
            pending: String::new(),
            suppress_end: None,
        }
    }

    /// 追加一段增量，返回可立即透传的部分
    fn feed(&mut self, delta: &str) -> String {
        self.pending.push_str(delta);
        let mut out = String::new();
        loop {
            if let Some(end) = self.suppress_end {
                match self.pending.find(end) {
                    Some(i) => {
                        self.pending.drain(..i + end.len());
                        self.suppress_end = None;
                    }
                    None => {
                        // 结束标记可能分片：只保留尾部与其前缀重叠的部分，其余吞掉
                        let keep = suffix_prefix_overlap(&self.pending, end);
                        let cut = self.pending.len() - keep;
                        self.pending.drain(..cut);
                        break;
                    }
                }
            } else {
                let hit = TOOL_TEXT_MARKERS
                    .iter()
                    .filter_map(|&(s, e)| self.pending.find(s).map(|i| (i, e)))
                    .min_by_key(|&(i, _)| i);
                match hit {
                    Some((i, end)) => {
                        out.push_str(&self.pending[..i]);
                        self.pending.drain(..i);
                        self.suppress_end = Some(end);
                    }
                    None => {
                        let keep = TOOL_TEXT_MARKERS
                            .iter()
                            .map(|&(s, _)| suffix_prefix_overlap(&self.pending, s))
                            .max()
                            .unwrap_or(0);
                        let cut = self.pending.len() - keep;
                        out.push_str(&self.pending[..cut]);
                        self.pending.drain(..cut);
                        break;
                    }
                }
            }
        }
        out
    }

    /// 流结束：正常文本的尾巴补发；仍在工具块内的残留直接丢弃
    fn flush(&mut self) -> String {
        if self.suppress_end.is_some() {
            self.pending.clear();
            String::new()
        } else {
            std::mem::take(&mut self.pending)
        }
    }
}

/// s 的后缀与 marker 前缀的最长重叠字节数（结果总落在字符边界上）
fn suffix_prefix_overlap(s: &str, marker: &str) -> usize {
    let mut best = 0;
    for (i, _) in s.char_indices() {
        let tail = &s[i..];
        if tail.len() > best && tail.len() < marker.len() && marker.starts_with(tail) {
            best = tail.len();
        }
    }
    best
}

/// 判断正文是否包含文本形式的工具调用（特殊标记对 / 调用形态 JSON）。
/// 用于识别模型"想调工具却写成正文"的情况，提醒它改用标准格式重试。
fn contains_tool_call_text(content: &str) -> bool {
    TOOL_TEXT_MARKERS.iter().any(|(s, _)| content.contains(s))
        || strip_json_tool_blocks(content) != content
        || strip_bare_json_lines(content) != content
}

/// 剔除正文里以纯文本形式写出的工具调用：特殊 token / XML 标记对、疑似调用的 JSON
/// 代码块、裸写的调用 JSON 行。用于入库与 llm-message-done 回显前的最终清理。
pub(crate) fn strip_tool_call_text(content: &str) -> String {
    let mut out = content.to_string();
    for &(start, end) in TOOL_TEXT_MARKERS {
        while let Some(s) = out.find(start) {
            let e = out[s..]
                .find(end)
                .map(|i| s + i + end.len())
                .unwrap_or(out.len());
            out.replace_range(s..e, "");
        }
    }
    let out = strip_json_tool_blocks(&out);
    let out = strip_bare_json_lines(&out);
    let mut cleaned = out.trim().to_string();
    while cleaned.contains("\n\n\n") {
        cleaned = cleaned.replace("\n\n\n", "\n\n");
    }
    cleaned
}

/// 剔除内容像工具调用的 ``` 代码块（{"name": …, "arguments"/"parameters": …}）
fn strip_json_tool_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(s) = rest.find("```") {
        out.push_str(&rest[..s]);
        let after_open = &rest[s + 3..];
        match after_open.find("```") {
            Some(close) => {
                let block = &after_open[..close];
                if !looks_like_tool_json(block) {
                    out.push_str("```");
                    out.push_str(block);
                    out.push_str("```");
                }
                rest = &after_open[close + 3..];
            }
            None => {
                if !looks_like_tool_json(after_open) {
                    out.push_str(&rest[s..]);
                }
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// 剔除整行就是一个裸调用 JSON 的行
fn strip_bare_json_lines(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let t = line.trim();
            !(t.starts_with('{') && t.ends_with('}') && looks_like_tool_json(t))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 判断一段文本（代码块或单行）是否解析为工具调用形态：对象或数组，
/// 含 name 字段且带 arguments / parameters 字段。
fn looks_like_tool_json(block: &str) -> bool {
    let trimmed = block.trim();
    let body = if trimmed.starts_with('{') || trimmed.starts_with('[') {
        trimmed
    } else {
        // 剥掉首行语言标记（如 ```json 的 json）
        match trimmed.find('\n') {
            Some(i) => trimmed[i + 1..].trim(),
            None => return false,
        }
    };
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    let is_call = |o: &serde_json::Map<String, Value>| {
        o.contains_key("name") && (o.contains_key("arguments") || o.contains_key("parameters"))
    };
    match &v {
        Value::Object(o) => is_call(o),
        Value::Array(a) => a.iter().filter_map(Value::as_object).any(is_call),
        _ => false,
    }
}

/// 把持久化的工具调用按 OpenAI 协议插回 user 与最终 assistant 之间，
/// 这样下一轮既看得到已做的事，也能接上上一轮请求的缓存前缀。
fn history_with_tool_replay(history: &[ChatMsg], tool_calls: &[ToolCallReplay]) -> Vec<Value> {
    let mut by_request: BTreeMap<i64, Vec<&ToolCallReplay>> = BTreeMap::new();
    for call in tool_calls {
        by_request
            .entry(call.request_message_id)
            .or_default()
            .push(call);
    }
    let mut messages = Vec::new();
    for msg in history {
        if msg.role == "user" {
            messages.push(json!({ "role": "user", "content": msg.content }));
            if let Some(calls) = by_request.get(&msg.id) {
                append_replayed_tool_rounds(&mut messages, calls);
            }
            continue;
        }
        let content = strip_persisted_tool_digest(&msg.content);
        if content.is_empty() {
            continue;
        }
        messages.push(json!({ "role": "assistant", "content": content }));
    }
    messages
}

fn append_replayed_tool_rounds(messages: &mut Vec<Value>, calls: &[&ToolCallReplay]) {
    let mut i = 0;
    while i < calls.len() {
        let round = calls[i].round_index;
        let mut round_calls = Vec::new();
        while i < calls.len() && calls[i].round_index == round {
            round_calls.push(calls[i]);
            i += 1;
        }
        let first = round_calls[0];
        let tool_calls_json: Vec<Value> = round_calls
            .iter()
            .map(|c| {
                json!({
                    "id": c.tool_call_id,
                    "type": "function",
                    "function": { "name": c.tool_name, "arguments": c.arguments_json },
                })
            })
            .collect();
        let content = first.assistant_content.trim();
        let mut assistant_msg = json!({
            "role": "assistant",
            "content": if content.is_empty() { Value::Null } else { json!(content) },
            "tool_calls": tool_calls_json,
        });
        if !first.reasoning_content.is_empty() {
            assistant_msg["reasoning_content"] = json!(first.reasoning_content);
        }
        messages.push(assistant_msg);
        for c in round_calls {
            let content = match c.result_json.as_deref() {
                Some(result) if !result.is_empty() => result.to_string(),
                _ if c.status == "running" => "（调用未完成）".to_string(),
                _ => "{}".to_string(),
            };
            messages.push(json!({
                "role": "tool",
                "tool_call_id": c.tool_call_id,
                "content": content,
            }));
        }
    }
}

/// 入库时为中断/预算耗尽附带的工具摘要，回放真实 tool 记录后不再送给模型，避免重复。
fn strip_persisted_tool_digest(content: &str) -> String {
    const MARKERS: &[&str] = &[
        "【中断前已完成的工具调用与结果】",
        "【工具预算耗尽前已完成的工具调用与结果】",
    ];
    let mut cut = content.len();
    for marker in MARKERS {
        if let Some(i) = content.find(marker) {
            cut = cut.min(i);
        }
    }
    content[..cut].trim_end().to_string()
}

const MODEL_ONLY_FOOTERS: &[&str] = &[
    "（回复被用户中断；以上资料已收集完毕，继续时请直接基于它们推进，不要重复收集）",
    "（以上资料已收集完毕，用户说「继续」时请直接基于它们推进，不要重复收集）",
];

/// 给界面看的助手正文：去掉内部工具摘要和写给模型的续跑说明。
pub(crate) fn visible_assistant_content(content: &str) -> String {
    let mut text = strip_persisted_tool_digest(content);
    for footer in MODEL_ONLY_FOOTERS {
        if let Some(i) = text.find(footer) {
            text = text[..i].trim_end().to_string();
        }
    }
    if text.is_empty()
        && (content.contains("【中断前已完成的工具调用与结果】")
            || content.contains("回复被用户中断"))
    {
        return "（已中断）".to_string();
    }
    text
}

fn model_tool_content(app: &AppHandle, name: &str, call_id: &str, result: &Value) -> String {
    let dir = crate::tempfs::tool_spill_dir(app).ok();
    crate::retention::bound_and_spill(
        dir.as_deref(),
        name,
        call_id,
        &result.to_string(),
        crate::retention::FRESH,
    )
}

/// 对话过长时压缩较早的工具结果：保留头尾，全文落到 temp/tool-spills。
fn trim_context(app: &AppHandle, messages: &mut [Value]) {
    let dir = crate::tempfs::tool_spill_dir(app).ok();
    crate::retention::trim_old_tool_messages(messages, dir.as_deref());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_distinguishes_failure_from_intentional_stop() {
        assert!(is_failed_tool_result(&json!({
            "ok": false,
            "status": "failed",
            "exit_code": 1
        })));
        assert!(is_failed_tool_result(&json!({ "error": "boom" })));
        assert!(!is_failed_tool_result(&json!({
            "ok": true,
            "status": "cancelled"
        })));
        assert!(!is_failed_tool_result(&json!({ "status": "done" })));
    }

    fn chat_msg(id: i64, role: &str, content: &str) -> ChatMsg {
        ChatMsg {
            id,
            role: role.to_string(),
            content: content.to_string(),
            created_at: String::new(),
        }
    }

    fn replay_call(
        request_message_id: i64,
        round_index: i64,
        call_index: i64,
        tool_call_id: &str,
        tool_name: &str,
        arguments_json: &str,
        result_json: &str,
        assistant_content: &str,
        reasoning_content: &str,
    ) -> ToolCallReplay {
        ToolCallReplay {
            request_message_id,
            round_index,
            call_index,
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            arguments_json: arguments_json.to_string(),
            result_json: Some(result_json.to_string()),
            status: "done".to_string(),
            assistant_content: assistant_content.to_string(),
            reasoning_content: reasoning_content.to_string(),
        }
    }

    #[test]
    fn history_replays_tool_rounds_between_user_and_final_assistant() {
        let history = vec![
            chat_msg(1, "user", "打开网页"),
            chat_msg(
                2,
                "assistant",
                "已完成。\n\n【中断前已完成的工具调用与结果】\n- browser_snapshot",
            ),
            chat_msg(3, "user", "继续"),
        ];
        let calls = vec![
            replay_call(
                1,
                0,
                0,
                "call-1",
                "browser_snapshot",
                "{}",
                r#"{"ok":true}"#,
                "先看页面",
                "观察 DOM",
            ),
            replay_call(
                1,
                0,
                1,
                "call-2",
                "browser_click",
                r#"{"ref":1}"#,
                "{}",
                "",
                "",
            ),
        ];
        let messages = history_with_tool_replay(&history, &calls);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"], "先看页面");
        assert_eq!(messages[1]["reasoning_content"], "观察 DOM");
        assert_eq!(messages[1]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call-1");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call-2");
        assert_eq!(messages[4]["role"], "assistant");
        assert_eq!(messages[4]["content"], "已完成。");
        assert_eq!(messages[5]["role"], "user");
        assert_eq!(messages[5]["content"], "继续");
    }

    #[test]
    fn request_body_keeps_full_tools_even_when_choice_is_none() {
        let settings = crate::commands::Settings {
            base_url: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            model: "test".into(),
            organize_root: String::new(),
            auto_organize: false,
            autostart: false,
            desktop_path: String::new(),
            thinking_enabled: false,
            reasoning_effort: "low".into(),
            vision_base_url: String::new(),
            vision_api_key: String::new(),
            vision_model: String::new(),
            subagent_thinking_enabled: false,
            subagent_reasoning_effort: "low".into(),
            subagent_model: String::new(),
            command_tools_enabled: true,
            llm_profiles: vec![],
            active_llm_profile_id: String::new(),
            profile_name: String::new(),
            profile_alias: String::new(),
            profile_gender: String::new(),
            profile_birth: String::new(),
            profile_phone: String::new(),
            profile_email: String::new(),
            profile_city: String::new(),
            temp_path: String::new(),
        };
        let cfg = client::LlmConfig {
            base_url: settings.base_url.clone(),
            api_key: String::new(),
            model: "test".into(),
        };
        let auto = request_body(&cfg, &settings, &[], true);
        let none = request_body(&cfg, &settings, &[], false);
        assert_eq!(auto["tools"], tools::definitions());
        assert_eq!(none["tools"], tools::definitions());
        assert_eq!(auto["tool_choice"], "auto");
        assert_eq!(none["tool_choice"], "none");
        assert_eq!(auto["tools"], none["tools"]);
    }

    #[test]
    fn filter_passes_plain_text() {
        let mut f = ToolTextFilter::new();
        assert_eq!(f.feed("你好，世界"), "你好，世界");
        assert_eq!(f.flush(), "");
    }

    #[test]
    fn filter_strips_whole_marker_pair() {
        let mut f = ToolTextFilter::new();
        assert_eq!(f.feed("前面<tool_call>{\"name\":\"x\"}"), "前面");
        assert_eq!(f.feed("</tool_call>后面"), "后面");
        assert_eq!(f.flush(), "");
        // 一整段一次性到达也能正确处理
        let mut f2 = ToolTextFilter::new();
        assert_eq!(
            f2.feed("前面<tool_call>{\"name\":\"x\"}</tool_call>后面"),
            "前面后面"
        );
    }

    #[test]
    fn filter_handles_fragmented_deepseek_marker() {
        let mut f = ToolTextFilter::new();
        // DeepSeek 特殊 token 分片到达
        assert_eq!(f.feed("结果如下<｜tool▁cal"), "结果如下");
        assert_eq!(f.feed("l▁begin｜>{\"name\":\"browser_click\"}"), "");
        assert_eq!(f.feed("<｜tool▁call▁end｜>收尾"), "收尾");
        assert_eq!(f.flush(), "");
    }

    #[test]
    fn filter_unclosed_block_swallows_to_end() {
        let mut f = ToolTextFilter::new();
        assert_eq!(f.feed("说一半<tool_call>{\"name\":"), "说一半");
        assert_eq!(f.feed("\"x\",\"arguments\":{}}"), "");
        assert_eq!(f.flush(), "");
    }

    #[test]
    fn filter_keeps_partial_prefix_at_flush() {
        let mut f = ToolTextFilter::new();
        assert_eq!(f.feed("结尾有个小于号 <"), "结尾有个小于号 ");
        assert_eq!(f.flush(), "<");
    }

    #[test]
    fn strip_removes_marker_pairs_and_unclosed() {
        let s = "完成。<｜tool▁call▁begin｜>{\"name\":\"x\"}<｜tool▁call▁end｜>";
        assert_eq!(strip_tool_call_text(s), "完成。");
        let s2 = "部分完成\n<tool_call>{\"name\":\"x\",\"arguments\":{}}";
        assert_eq!(strip_tool_call_text(s2), "部分完成");
    }

    #[test]
    fn strip_removes_json_code_block_but_keeps_normal_json() {
        let s = "进度：\n```json\n{\"name\": \"browser_click\", \"arguments\": {\"id\": 3}}\n```\n以上。";
        assert_eq!(strip_tool_call_text(s), "进度：\n\n以上。");
        let keep = "```json\n{\"title\": \"你好\", \"count\": 2}\n```";
        assert_eq!(strip_tool_call_text(keep), keep);
    }

    #[test]
    fn strip_removes_bare_json_line() {
        let s = "下一步：\n{\"name\": \"scan_desktop\", \"arguments\": {}}\n稍等";
        assert_eq!(strip_tool_call_text(s), "下一步：\n稍等");
    }

    #[test]
    fn filter_strips_dsml_block_fragmented() {
        let mut f = ToolTextFilter::new();
        // DSML 标记分片到达
        assert_eq!(f.feed("→ 打开网页\n<｜｜DSML｜｜tool_ca"), "→ 打开网页\n");
        assert_eq!(
            f.feed("lls><｜｜DSML｜｜invoke name=\"browser_evaluate\">"),
            ""
        );
        assert_eq!(f.feed("(() => 1)()</｜｜DSML｜｜parameter>"), "");
        assert_eq!(
            f.feed("</｜｜DSML｜｜invoke></｜｜DSML｜｜tool_calls>收尾"),
            "收尾"
        );
        assert_eq!(f.flush(), "");
    }

    #[test]
    fn strip_removes_dsml_pair_and_unclosed() {
        let s = "打开网页<｜｜DSML｜｜tool_calls><｜｜DSML｜｜invoke name=\"x\"><｜｜DSML｜｜parameter name=\"e\" string=\"true\">1</｜｜DSML｜｜parameter></｜｜DSML｜｜invoke></｜｜DSML｜｜tool_calls>";
        assert_eq!(strip_tool_call_text(s), "打开网页");
        // 未闭合的 DSML 块截断到结尾
        let s2 = "部分完成\n<｜｜DSML｜｜tool_calls><｜｜DSML｜｜invoke name=\"x\"";
        assert_eq!(strip_tool_call_text(s2), "部分完成");
        // 单独出现的 invoke 也能清理
        let s3 = "试试\n<｜｜DSML｜｜invoke name=\"x\">…</｜｜DSML｜｜invoke>";
        assert_eq!(strip_tool_call_text(s3), "试试");
    }

    #[test]
    fn detects_text_form_tool_calls() {
        assert!(contains_tool_call_text(
            "看看<｜｜DSML｜｜tool_calls><｜｜DSML｜｜invoke name=\"x\">"
        ));
        assert!(contains_tool_call_text(
            "<tool_call>{\"name\":\"x\"}</tool_call>"
        ));
        assert!(contains_tool_call_text("{\"name\":\"x\",\"arguments\":{}}"));
        assert!(contains_tool_call_text(
            "说明：\n```json\n{\"name\":\"x\",\"arguments\":{}}\n```"
        ));
        assert!(!contains_tool_call_text("这是正常回答。"));
        assert!(!contains_tool_call_text(
            "```json\n{\"title\":\"你好\"}\n```"
        ));
    }

    #[test]
    fn visible_assistant_content_hides_tool_digest() {
        let raw = "日报已写好。\n\n【工具预算耗尽前已完成的工具调用与结果】\n- run_command({\"argv\":[\"lark-cli\",\"base\",\"+record-list\"]}) → {\"ok\":true}\n（以上资料已收集完毕，用户说「继续」时请直接基于它们推进，不要重复收集）";
        assert_eq!(visible_assistant_content(raw), "日报已写好。");

        let interrupted = "【中断前已完成的工具调用与结果】\n- load_skill({\"name\":\"windows-cli\"}) → {}";
        assert_eq!(visible_assistant_content(interrupted), "（已中断）");

        assert_eq!(visible_assistant_content("普通回复"), "普通回复");
    }
}
