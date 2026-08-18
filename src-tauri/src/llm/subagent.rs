//! 子代理：主代理把「要读一堆材料 / 做多步分析」的子任务整体委托出去。
//! 可同步等待，也可丢到后台；结论是 structured complete/blocked 汇报，中间过程不占主上下文。

use crate::commands::load_settings;
use crate::llm::{client, tools};
use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const MAX_ROUNDS: usize = 15;
const TIMEOUT_SECS: u64 = 300;
const RESULT_MAX_CHARS: usize = 6000;

const ALLOWED_TOOLS: &[&str] = &[
    "scan_desktop",
    "search_files",
    "read_file",
    "get_file_info",
    "ocr_image",
    "read_image",
    "list_todos",
    "get_system_info",
    "list_skills",
    "load_skill",
    "web_search",
    "web_fetch",
];

#[derive(Debug, Clone, Serialize)]
pub struct SubagentJob {
    pub id: String,
    pub task: String,
    pub status: String,
    pub report: Option<Value>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

struct JobEntry {
    info: SubagentJob,
    cancel: CancellationToken,
    done_rx: watch::Receiver<bool>,
}

pub struct SubagentHub {
    seq: AtomicU64,
    jobs: Mutex<HashMap<String, JobEntry>>,
}

impl SubagentHub {
    pub fn new() -> Self {
        Self {
            seq: AtomicU64::new(1),
            jobs: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, id: &str) -> Option<SubagentJob> {
        self.lock().get(id).map(|e| e.info.clone())
    }

    pub fn cancel_all(&self) {
        let jobs = self.lock();
        for entry in jobs.values() {
            entry.cancel.cancel();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, JobEntry>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn start(
        &self,
        app: AppHandle,
        session_id: i64,
        task: String,
        context: Option<String>,
        parent_cancel: CancellationToken,
    ) -> String {
        let id = format!("sa-{}", self.seq.fetch_add(1, Ordering::Relaxed));
        let cancel = parent_cancel.child_token();
        let (done_tx, done_rx) = watch::channel(false);
        let info = SubagentJob {
            id: id.clone(),
            task: task.clone(),
            status: "running".into(),
            report: None,
            started_at: crate::db::now_str(),
            finished_at: None,
        };
        self.lock().insert(
            id.clone(),
            JobEntry {
                info: info.clone(),
                cancel: cancel.clone(),
                done_rx,
            },
        );
        let hub_id = id.clone();
        tauri::async_runtime::spawn(async move {
            let report = run_job(&app, &task, context.as_deref(), &cancel).await;
            finish_job(&app, &hub_id, session_id, report).await;
            let _ = done_tx.send(true);
        });
        id
    }

    async fn wait(
        &self,
        id: &str,
        timeout_secs: u64,
        cancel: &CancellationToken,
    ) -> Result<SubagentJob> {
        let (mut rx, existing) = {
            let jobs = self.lock();
            let entry = jobs.get(id).ok_or_else(|| anyhow!("子代理任务不存在: {id}"))?;
            if entry.info.status != "running" {
                return Ok(entry.info.clone());
            }
            (entry.done_rx.clone(), entry.info.clone())
        };
        if *rx.borrow() {
            return Ok(self.get(id).unwrap_or(existing));
        }
        let timeout = tokio::time::sleep(std::time::Duration::from_secs(timeout_secs.max(1)));
        tokio::select! {
            _ = cancel.cancelled() => {
                if let Some(entry) = self.lock().get(id) {
                    entry.cancel.cancel();
                }
                Err(anyhow!("已中断"))
            }
            _ = rx.changed() => Ok(self.get(id).unwrap_or(existing)),
            _ = timeout => {
                self.get(id).ok_or_else(|| anyhow!("子代理任务不存在: {id}"))
            }
        }
    }
}

async fn finish_job(app: &AppHandle, id: &str, session_id: i64, report: Value) {
    let status = report
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("complete")
        .to_string();
    let summary = report
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    {
        let state = app.state::<crate::AppState>();
        let mut jobs = state.subagents.lock();
        if let Some(entry) = jobs.get_mut(id) {
            entry.info.status = status.clone();
            entry.info.report = Some(report.clone());
            entry.info.finished_at = Some(crate::db::now_str());
        }
    }
    let _ = app.emit(
        "subagent-finished",
        json!({
            "job_id": id,
            "session_id": session_id,
            "status": status,
            "summary": summary,
            "report": report,
        }),
    );
    let busy = app
        .state::<crate::AppState>()
        .chat_busy
        .load(std::sync::atomic::Ordering::SeqCst);
    if !busy {
        let line = match status.as_str() {
            "blocked" => format!("后台子任务卡住了：{summary}"),
            "cancelled" => "后台子任务已中断。".to_string(),
            "error" => format!("后台子任务出错：{summary}"),
            _ => format!("后台子任务完成：{summary}"),
        };
        let state = app.state::<crate::AppState>();
        let _ = state.db.save_chat(session_id, "system", &line);
        let _ = app.emit("subagent-chat", json!({ "session_id": session_id, "content": line }));
    }
}

async fn run_job(
    app: &AppHandle,
    task: &str,
    context: Option<&str>,
    cancel: &CancellationToken,
) -> Value {
    match tokio::time::timeout(
        std::time::Duration::from_secs(TIMEOUT_SECS),
        run_inner(app, task, context, cancel),
    )
    .await
    {
        Ok(Ok(text)) => parse_report(&text),
        Ok(Err(e)) => json!({
            "status": "error",
            "summary": e.to_string(),
            "findings": [],
            "evidence": [],
            "blockers": [e.to_string()],
        }),
        Err(_) => json!({
            "status": "blocked",
            "summary": format!("子代理超过 {TIMEOUT_SECS} 秒仍未结束"),
            "findings": [],
            "evidence": [],
            "blockers": [format!("执行超过 {TIMEOUT_SECS} 秒被中止，可拆小后重试")],
        }),
    }
}

/// 同步跑完子代理，返回结构化汇报（兼容旧调用）。
pub async fn run(
    app: &AppHandle,
    task: &str,
    context: Option<&str>,
    cancel: &CancellationToken,
) -> Result<Value> {
    Ok(run_job(app, task, context, cancel).await)
}

pub async fn execute(
    app: &AppHandle,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    if args.get("job_id").and_then(Value::as_str).is_some() {
        return await_job(app, args, cancel).await;
    }
    let task = args
        .get("task")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("缺少 task"))?;
    let context = args
        .get("context")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let background = args
        .get("run_in_background")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let should_await = args.get("await").and_then(Value::as_bool).unwrap_or(false);
    if !background {
        let report = run(app, task, context.as_deref(), cancel).await?;
        return Ok(json!({
            "report": report,
            "note": "以上是子代理的结构化结论；中间过程未占用本对话。status=blocked 表示缺材料或权限，不要当成已完成。",
        }));
    }
    let session_id = {
        let state = app.state::<crate::AppState>();
        state.db.current_session_id()?
    };
    let id = {
        let state = app.state::<crate::AppState>();
        state
            .subagents
            .start(app.clone(), session_id, task.to_string(), context, cancel.clone())
    };
    if should_await {
        return await_job(app, &json!({ "job_id": id, "timeout_secs": TIMEOUT_SECS }), cancel).await;
    }
    Ok(json!({
        "status": "started",
        "job_id": id,
        "note": "子代理已在后台运行。主对话可以继续；需要结论时调用 await_subagent。不要轮询。",
    }))
}

pub async fn await_job(
    app: &AppHandle,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    let id = args
        .get("job_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("缺少 job_id"))?;
    let timeout = args
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .unwrap_or(TIMEOUT_SECS as u64);
    let job = {
        let state = app.state::<crate::AppState>();
        state.subagents.wait(id, timeout, cancel).await?
    };
    if job.status == "running" {
        return Ok(json!({
            "status": "running",
            "job_id": job.id,
            "note": "仍在运行。不要反复查询；再次 await_subagent，或继续做别的。",
        }));
    }
    Ok(json!({
        "job_id": job.id,
        "report": job.report,
        "status": job.status,
        "note": "status=blocked 表示缺材料或权限，不要当成已完成。",
    }))
}

pub fn parse_report(text: &str) -> Value {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return json!({
            "status": "blocked",
            "summary": "子代理未产出有效结论",
            "findings": [],
            "evidence": [],
            "blockers": ["未产出有效结论"],
        });
    }
    if let Some(value) = extract_json_object(trimmed) {
        return normalize_report(value, trimmed);
    }
    let clamped = clamp_text(trimmed);
    json!({
        "status": "complete",
        "summary": first_line(&clamped),
        "findings": [clamped],
        "evidence": [],
        "blockers": [],
    })
}

fn extract_json_object(text: &str) -> Option<Value> {
    let candidates = [
        text,
        text.strip_prefix("```json").and_then(|s| s.strip_suffix("```")).unwrap_or(text),
        text.strip_prefix("```").and_then(|s| s.strip_suffix("```")).unwrap_or(text),
    ];
    for raw in candidates {
        let raw = raw.trim();
        if let Ok(v) = serde_json::from_str::<Value>(raw) {
            if v.is_object() {
                return Some(v);
            }
        }
        if let (Some(start), Some(end)) = (raw.find('{'), raw.rfind('}')) {
            if end > start {
                if let Ok(v) = serde_json::from_str::<Value>(&raw[start..=end]) {
                    if v.is_object() {
                        return Some(v);
                    }
                }
            }
        }
    }
    None
}

fn normalize_report(mut value: Value, fallback: &str) -> Value {
    let obj = match value.as_object_mut() {
        Some(o) => o,
        None => {
            return json!({
                "status": "complete",
                "summary": clamp_text(fallback),
                "findings": [],
                "evidence": [],
                "blockers": [],
            })
        }
    };
    let status = obj
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("complete");
    let status = match status {
        "blocked" | "error" | "cancelled" => status,
        _ => "complete",
    };
    let summary = obj
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| first_line(fallback))
        .to_string();
    let findings = obj.get("findings").cloned().unwrap_or_else(|| json!([]));
    let evidence = obj.get("evidence").cloned().unwrap_or_else(|| json!([]));
    let blockers = obj.get("blockers").cloned().unwrap_or_else(|| json!([]));
    json!({
        "status": status,
        "summary": summary,
        "findings": findings,
        "evidence": evidence,
        "blockers": blockers,
    })
}

fn first_line(text: &str) -> &str {
    text.lines().find(|l| !l.trim().is_empty()).unwrap_or(text).trim()
}

fn clamp_text(content: &str) -> String {
    let trimmed = content.trim();
    let count = trimmed.chars().count();
    if count <= RESULT_MAX_CHARS {
        trimmed.to_string()
    } else {
        format!(
            "{}…\n（结论过长已截断，原 {} 字符）",
            trimmed.chars().take(RESULT_MAX_CHARS).collect::<String>(),
            count
        )
    }
}

async fn run_inner(
    app: &AppHandle,
    task: &str,
    context: Option<&str>,
    cancel: &CancellationToken,
) -> Result<String> {
    if cancel.is_cancelled() {
        return Ok(json!({
            "status": "cancelled",
            "summary": "子代理已被用户中断",
            "findings": [],
            "evidence": [],
            "blockers": ["用户中断"],
        })
        .to_string());
    }
    let (settings, skills_catalog) = {
        let state = app.state::<crate::AppState>();
        (load_settings(&state.db), state.skills.catalog_reminder())
    };
    let cfg = client::LlmConfig {
        base_url: settings.base_url.clone(),
        api_key: settings.api_key.clone(),
        model: if settings.subagent_model.trim().is_empty() {
            settings.model.clone()
        } else {
            settings.subagent_model.trim().to_string()
        },
    };

    let mut user_msg = format!("子任务：{}", task.trim());
    if let Some(ctx) = context.map(str::trim).filter(|s| !s.is_empty()) {
        user_msg.push_str(&format!("\n\n背景信息（主代理提供）：\n{}", ctx));
    }
    let mut messages: Vec<Value> = vec![json!({ "role": "system", "content": system_prompt() })];
    if let Some(catalog) = skills_catalog {
        messages.push(json!({ "role": "system", "content": catalog }));
    }
    messages.push(json!({ "role": "user", "content": user_msg }));

    let http = reqwest::Client::new();
    let spill_dir = crate::tempfs::tool_spill_dir(app).ok();
    let mut repeat_guard = crate::repeat_guard::RepeatGuard::new();
    for _round in 0..MAX_ROUNDS {
        if cancel.is_cancelled() {
            return Ok(json!({
                "status": "cancelled",
                "summary": "子代理已被用户中断",
                "findings": [],
                "evidence": [],
                "blockers": ["用户中断"],
            })
            .to_string());
        }
        crate::retention::trim_old_tool_messages(&mut messages, spill_dir.as_deref());
        let body = request_body(&cfg, &settings, &messages);
        let resp = client::stream_chat(&http, &cfg, &body, cancel, |_| {}, |_| {}).await?;
        crate::llm::persist_usage(app, "subagent", &cfg.model, &resp.usage);

        if resp.interrupted {
            return Ok(json!({
                "status": "cancelled",
                "summary": "子代理已被用户中断",
                "findings": [],
                "evidence": [],
                "blockers": ["用户中断"],
            })
            .to_string());
        }
        if resp.tool_calls.is_empty() {
            return Ok(crate::llm::agent::strip_tool_call_text(&resp.content));
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
        let mut assistant_msg = json!({
            "role": "assistant",
            "content": if resp.content.is_empty() { Value::Null } else { json!(resp.content) },
            "tool_calls": tool_calls_json,
        });
        if !resp.reasoning_content.is_empty() {
            assistant_msg["reasoning_content"] = json!(resp.reasoning_content);
        }
        messages.push(assistant_msg);

        for call in &resp.tool_calls {
            let result = if ALLOWED_TOOLS.contains(&call.name.as_str()) {
                let parsed: Value =
                    serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));
                if call.name == "search_files"
                    && !matches!(
                        parsed
                            .get("action")
                            .and_then(Value::as_str)
                            .unwrap_or("search"),
                        "search" | "status"
                    )
                {
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": call.id,
                        "content": json!({
                            "error": "子代理只能查询已有文件索引；建库、停止和 NTFS/UAC 动作必须由主代理向用户申请后执行"
                        }).to_string(),
                    }));
                    if let Some(reminder) = repeat_guard.observe(&call.name, &call.arguments) {
                        messages.push(crate::repeat_guard::reminder_message(&reminder));
                    }
                    continue;
                }
                match Box::pin(tools::execute(app, &call.name, &parsed, cancel)).await {
                    Ok(v) => v,
                    Err(e) => json!({ "error": e.to_string() }),
                }
            } else {
                json!({ "error": format!("子代理无权使用工具 {}，请基于已有信息直接给结论", call.name) })
            };
            let content = crate::retention::bound_and_spill(
                spill_dir.as_deref(),
                &call.name,
                &call.id,
                &result.to_string(),
                crate::retention::FRESH,
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
    Ok(json!({
        "status": "blocked",
        "summary": "子代理步骤预算用完，未能得出结论",
        "findings": [],
        "evidence": [],
        "blockers": ["步骤预算用完，可把子任务拆得更小后重试"],
    })
    .to_string())
}

fn request_body(
    cfg: &client::LlmConfig,
    settings: &crate::commands::Settings,
    messages: &[Value],
) -> Value {
    let mut body = json!({
        "model": cfg.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
        "temperature": 0.2,
        "tools": tools::definitions_for(ALLOWED_TOOLS),
        "tool_choice": "auto",
    });
    if cfg.base_url.contains("deepseek") {
        if settings.subagent_thinking_enabled {
            body["thinking"] = json!({ "type": "enabled" });
            body["reasoning_effort"] = json!(settings.subagent_reasoning_effort);
        } else {
            body["thinking"] = json!({ "type": "disabled" });
        }
    }
    body
}

fn system_prompt() -> String {
    "你是拾光的子代理，负责完成主代理交办的一个具体子任务（通常是阅读一批材料后给出分析结论）。\n\
     - 只围绕该子任务行动，用可用的只读工具收集信息；\n\
     - 系统提示之后若有 Skills 目录，任务明显匹配时先 load_skill；对话里已有该技能的 <skill_content> 则不要再 load；\n\
     - 查公开网页可用 web_search / web_fetch；不要使用浏览器工具；\n\
     - 主代理看不到你的中间过程，只能看到最终 JSON 汇报——关键事实、路径、数据必须写进汇报；\n\
     - 信息不足或工具不够用时 status 用 blocked，不要编造；\n\
     - 严禁在正文里输出工具调用语法。\n\
     最终只输出一个 JSON 对象，不要 Markdown 围栏以外的说明：\n\
     {\"status\":\"complete 或 blocked\",\"summary\":\"一两句结论\",\"findings\":[\"要点\"],\"evidence\":[{\"ref\":\"路径或出处\",\"note\":\"说明\"}],\"blockers\":[\"卡住的原因\"]}"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_text_as_complete() {
        let v = parse_report("桌面上有 3 个 PDF，都是发票。");
        assert_eq!(v["status"], "complete");
        assert!(v["summary"].as_str().unwrap().contains("PDF"));
    }

    #[test]
    fn parse_json_blocked() {
        let v = parse_report(
            r#"{"status":"blocked","summary":"没有索引","findings":[],"evidence":[],"blockers":["需要 UAC"]}"#,
        );
        assert_eq!(v["status"], "blocked");
        assert_eq!(v["blockers"][0], "需要 UAC");
    }

    #[test]
    fn parse_fenced_json() {
        let v = parse_report("```json\n{\"status\":\"complete\",\"summary\":\"已汇总\",\"findings\":[\"a\"],\"evidence\":[],\"blockers\":[]}\n```");
        assert_eq!(v["status"], "complete");
        assert_eq!(v["summary"], "已汇总");
    }
}
