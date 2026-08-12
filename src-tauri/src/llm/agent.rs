use crate::commands::load_settings;
use crate::llm::{client, prompts, tools};
use anyhow::Result;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

/// 单次任务的工具调用轮次预算。浏览器类任务动辄 10+ 轮，给足余量。
const MAX_TOOL_ROUNDS: usize = 50;
/// 倒数第几轮时注入收尾提醒
const WARN_BEFORE_END: usize = 3;
/// 对话总长度超过该阈值时压缩较早的工具结果，防止超出模型上下文窗口
const CONTEXT_TRIM_CHARS: usize = 80_000;

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

    let (history, profile_block) = {
        let state = app.state::<crate::AppState>();
        let history = state.db.load_chat(session_id, 30)?;
        let latest_user = history
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        // 个人信息按需注入：命中求职/发帖等场景或用户主动要求时才加载
        let profile = if crate::llm::profile::should_inject(&latest_user) {
            let entries = state.db.pf_list().unwrap_or_default();
            crate::llm::profile::injection_block(&settings, &entries)
        } else {
            None
        };
        (history, profile)
    };

    // 上下文缓存优化：系统提示词 + 历史消息构成逐字节稳定的前缀（命中缓存计费极低）；
    // 秒级时间、Skills 目录等每轮都变的内容放在末尾的系统消息里，只有尾巴不命中
    let sys = prompts::system_prompt(&settings);
    let mut messages: Vec<Value> = vec![json!({
        "role": "system",
        "content": sys,
    })];
    for msg in history {
        let role = if msg.role == "user" {
            "user"
        } else {
            "assistant"
        };
        messages.push(json!({ "role": role, "content": msg.content }));
    }
    let mut tail = format!(
        "当前时间：{}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S %A")
    );
    if let Some(pb) = &profile_block {
        tail.push_str(pb);
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
    let mut active_tools = tools::core_tool_names();

    for round in 0..MAX_TOOL_ROUNDS {
        if cancel.is_cancelled() {
            finalize_cancelled(&app, session_id, "", &tool_log).await;
            return Ok(());
        }
        if round == MAX_TOOL_ROUNDS - WARN_BEFORE_END {
            messages.push(json!({
                "role": "system",
                "content": "注意：本轮对话的工具调用预算即将用完，请优先收尾，用最少的步骤完成任务。",
            }));
        }
        trim_context(&mut messages);
        let body = request_body(&cfg, &settings, &messages, Some(&active_tools));

        let resp = stream_filtered(&http, &cfg, &body, &cancel, &app).await?;

        if resp.interrupted {
            finalize_cancelled(&app, session_id, &resp.content, &tool_log).await;
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
            let _ = state.db.save_chat(session_id, "assistant", &cleaned);
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

        for call in &resp.tool_calls {
            // 正在执行的工具让其跑完（保证文件操作一致性），在下一个工具前中断
            if cancel.is_cancelled() {
                finalize_cancelled(&app, session_id, &resp.content, &tool_log).await;
                return Ok(());
            }
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
            if call.name == tools::DISCOVER_TOOL {
                activate_discovered_tools(&mut active_tools, &result);
            }
            let tool_status = if is_failed_tool_result(&result) {
                "error"
            } else {
                "done"
            };
            push_tool_log(&mut tool_log, &call.name, &call.arguments, &result);
            let _ = app.emit(
                "tool-status",
                json!({ "name": call.name, "status": tool_status, "result": result }),
            );
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": result.to_string(),
            }));
        }
    }

    finalize_with_summary(
        &app, &http, &cfg, &settings, session_id, messages, &cancel, &tool_log,
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

fn activate_discovered_tools(active_tools: &mut Vec<String>, result: &Value) {
    let Some(names) = result.get("activated_tools").and_then(Value::as_array) else {
        return;
    };
    for name in names.iter().filter_map(Value::as_str) {
        if !active_tools.iter().any(|active| active == name) {
            active_tools.push(name.to_string());
        }
    }
}

/// 构造请求体。DeepSeek 思考模式参数（thinking / reasoning_effort）只对 DeepSeek 接口附加，
/// 其它 OpenAI 兼容服务收到未知字段可能直接 400；v4 模型思考默认开启，关闭需显式声明。
fn request_body(
    cfg: &client::LlmConfig,
    settings: &crate::commands::Settings,
    messages: &[Value],
    active_tools: Option<&[String]>,
) -> Value {
    let mut body = json!({
        "model": cfg.model,
        "messages": messages,
        "stream": true,
        "temperature": 0.3,
    });
    if let Some(active_tools) = active_tools {
        body["tools"] = tools::definitions_for(active_tools);
        body["tool_choice"] = json!("auto");
    }
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
async fn finalize_cancelled(app: &AppHandle, session_id: i64, partial: &str, tool_log: &[String]) {
    let cleaned = strip_tool_call_text(partial);
    let mut saved = cleaned.clone();
    if !tool_log.is_empty() {
        if !saved.is_empty() {
            saved.push_str("\n\n");
        }
        saved.push_str("【中断前已完成的工具调用与结果】\n");
        saved.push_str(&digest_of(tool_log));
    }
    if !saved.is_empty() {
        saved.push_str(
            "\n（回复被用户中断；以上资料已收集完毕，继续时请直接基于它们推进，不要重复收集）",
        );
        let state = app.state::<crate::AppState>();
        let _ = state.db.save_chat(session_id, "assistant", &saved);
    }
    let _ = app.emit("llm-cancelled", json!({ "content": cleaned }));
    let _ = app.emit("sessions-changed", ());
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

/// 工具清单总预算 4000 字符，超出部分省略（最近的调用通常最相关，保留靠前的轮次）
fn digest_of(log: &[String]) -> String {
    const BUDGET: usize = 4000;
    let mut out = String::new();
    for (i, entry) in log.iter().enumerate() {
        if out.len() + entry.len() > BUDGET {
            out.push_str(&format!("- …（其余 {} 条工具结果已省略）\n", log.len() - i));
            break;
        }
        out.push_str(entry);
        out.push('\n');
    }
    out
}

/// 工具预算耗尽：不再给工具，让模型基于已有信息做阶段性收尾。
/// 收尾内容会存入聊天记录，用户说「继续」时模型能看到进展，而不是从零开始。
async fn finalize_with_summary(
    app: &AppHandle,
    http: &reqwest::Client,
    cfg: &client::LlmConfig,
    settings: &crate::commands::Settings,
    session_id: i64,
    mut messages: Vec<Value>,
    cancel: &tokio_util::sync::CancellationToken,
    tool_log: &[String],
) -> Result<()> {
    messages.push(json!({
        "role": "system",
        "content": "工具调用轮次已用完，你现在没有任何工具可用。请基于已获得的信息，直接用自然语言给出阶段性回答：1) 已完成的部分；2) 卡在什么地方；3) 如果用户说「继续」，你接下来打算怎么做。不要声称完成了并未完成的步骤。严禁输出任何工具调用语法（例如 <tool_call>…</tool_call>、<｜tool▁call▁begin｜>…、<｜｜DSML｜｜tool_calls>…</｜｜DSML｜｜tool_calls>、或带 \"name\"/\"arguments\" 字段的 JSON 代码块），只能输出给用户看的自然语言。",
    }));
    trim_context(&mut messages);
    let body = request_body(cfg, settings, &messages, None);
    let resp = stream_filtered(http, cfg, &body, cancel, app).await;
    match resp {
        Ok(r) if r.interrupted => {
            finalize_cancelled(app, session_id, &r.content, tool_log).await;
        }
        Ok(r) => {
            let cleaned = strip_tool_call_text(&r.content);
            // 与中断路径一致：收尾正文之外把本轮工具清单一起入库，
            // 否则用户说「继续」时上一轮收集的资料全部丢失，只能从头再来
            let mut saved = cleaned.clone();
            if !tool_log.is_empty() {
                if !saved.is_empty() {
                    saved.push_str("\n\n");
                }
                saved.push_str("【工具预算耗尽前已完成的工具调用与结果】\n");
                saved.push_str(&digest_of(tool_log));
                saved.push_str(
                    "（以上资料已收集完毕，用户说「继续」时请直接基于它们推进，不要重复收集）",
                );
            }
            if saved.is_empty() {
                let _ = app.emit(
                    "llm-error",
                    json!({ "message": "工具调用轮次已用完，且收尾总结为空。可直接说「继续」重试。" }),
                );
            } else {
                let state = app.state::<crate::AppState>();
                let _ = state.db.save_chat(session_id, "assistant", &saved);
                // 收尾正文可能整段跑偏成文本工具调用而被清空，此时给用户一句兜底提示
                let display = if cleaned.is_empty() {
                    "（工具调用轮次已用完，本轮进展已保存。说「继续」即可接着推进。）".to_string()
                } else {
                    cleaned
                };
                let _ = app.emit("llm-message-done", json!({ "content": display }));
                let _ = app.emit("sessions-changed", ());
            }
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

/// 上下文保护：对话过长时，把较早的工具结果替换成占位符（保留最近几条完整内容）。
/// tool 消息必须与 assistant 的 tool_calls 配对保留，只压缩其 content。
fn trim_context(messages: &mut [Value]) {
    let total: usize = messages.iter().map(|m| m.to_string().len()).sum();
    if total <= CONTEXT_TRIM_CHARS {
        return;
    }
    let keep_last = 8;
    let cutoff = messages.len().saturating_sub(keep_last);
    for m in messages.iter_mut().take(cutoff) {
        if m.get("role").and_then(|r| r.as_str()) != Some("tool") {
            continue;
        }
        let len = m
            .get("content")
            .and_then(|c| c.as_str())
            .map(|c| c.len())
            .unwrap_or(0);
        if len > 200 {
            m["content"] = json!(format!("（较早的工具结果已省略，原 {} 字符）", len));
        }
    }
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

    #[test]
    fn discovered_tools_are_added_once() {
        let mut active = tools::core_tool_names();
        activate_discovered_tools(
            &mut active,
            &json!({ "activated_tools": ["read_file", "run_command", "read_file"] }),
        );
        assert!(active.iter().any(|name| name == "read_file"));
        assert!(active.iter().any(|name| name == "run_command"));
        assert_eq!(active.iter().filter(|name| *name == "read_file").count(), 1);
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
}
