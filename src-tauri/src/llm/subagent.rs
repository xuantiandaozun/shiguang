//! 子代理：主代理把「要读一堆材料 / 做多步分析」的子任务整体委托出去。
//! 子代理跑独立的 LLM 循环（受限的只读工具集、独立的思考模式配置），
//! 只有最终结论会回到主对话上下文——中间的几十轮工具调用和材料内容都不占主上下文。
//! 子代理默认关闭思考模式（设置页可改），简单任务更快更省。

use crate::commands::load_settings;
use crate::llm::{client, tools};
use anyhow::Result;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

/// 子代理的工具调用轮次预算（任务单一，远小于主代理）
const MAX_ROUNDS: usize = 15;
/// 整体超时，防止子代理卡死拖住主对话
const TIMEOUT_SECS: u64 = 300;
/// 返回给主代理的结论长度上限
const RESULT_MAX_CHARS: usize = 6000;

/// 子代理可用的只读工具白名单。
/// 不含浏览器（共享会话，会打乱主代理的快照编号）、不含写入类工具
/// （create_file/edit_file/整理/规则等副作用操作仍由主代理亲自执行）、
/// 不含 run_subagent 自身（防止嵌套递归）。
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
];

pub async fn run(
    app: &AppHandle,
    task: &str,
    context: Option<&str>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<String> {
    match tokio::time::timeout(
        std::time::Duration::from_secs(TIMEOUT_SECS),
        run_inner(app, task, context, cancel),
    )
    .await
    {
        Ok(r) => r,
        Err(_) => Ok(format!(
            "（子代理执行超过 {} 秒被中止，子任务未完成；可拆小后重试）",
            TIMEOUT_SECS
        )),
    }
}

async fn run_inner(
    app: &AppHandle,
    task: &str,
    context: Option<&str>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<String> {
    let settings = {
        let state = app.state::<crate::AppState>();
        load_settings(&state.db)
    };
    // 子代理可用独立模型；未配置时跟随主模型
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
    let mut messages: Vec<Value> = vec![
        json!({ "role": "system", "content": system_prompt() }),
        json!({ "role": "user", "content": user_msg }),
    ];

    let http = reqwest::Client::new();
    for _round in 0..MAX_ROUNDS {
        if cancel.is_cancelled() {
            return Ok("（子代理已被用户中断，子任务未完成）".to_string());
        }
        let body = request_body(&cfg, &settings, &messages);
        let resp = client::stream_chat(&http, &cfg, &body, cancel, |_| {}, |_| {}).await?;

        if resp.interrupted {
            return Ok("（子代理已被用户中断，子任务未完成）".to_string());
        }
        if resp.tool_calls.is_empty() {
            // 子代理的结论回传给主代理入库前，同样剔除文本形式的工具调用
            return Ok(clamp_result(&crate::llm::agent::strip_tool_call_text(
                &resp.content,
            )));
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
        // DeepSeek 思考模式要求工具轮次的思维链随消息回传
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
                    continue;
                }
                // execute → subagent::run → execute 构成异步递归，Box::pin 打破无限大小的 future
                match Box::pin(tools::execute(app, &call.name, &parsed, cancel)).await {
                    Ok(v) => v,
                    Err(e) => json!({ "error": e.to_string() }),
                }
            } else {
                json!({ "error": format!("子代理无权使用工具 {}，请基于已有信息直接给结论", call.name) })
            };
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": result.to_string(),
            }));
        }
    }
    Ok("（子代理步骤预算用完，未能得出结论；可把子任务拆得更小后重试）".to_string())
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
        "temperature": 0.2,
        "tools": tools::definitions_for(ALLOWED_TOOLS),
        "tool_choice": "auto",
    });
    // 思考参数与主代理同规则：只对 DeepSeek 接口附加，但开关/强度用子代理自己的配置
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

fn clamp_result(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return "（子代理未产出有效结论）".to_string();
    }
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

fn system_prompt() -> String {
    "你是拾光的子代理，负责完成主代理交办的一个具体子任务（通常是阅读一批材料后给出分析结论）。\n\
     - 只围绕该子任务行动，用可用的只读工具收集信息；\n\
     - 主代理看不到你的任何中间过程和工具结果，只能看到你的最终结论——结论必须自包含，把关键事实、路径、数据写全；\n\
     - 用简体中文、要点式输出，控制在 6000 字符以内；\n\
     - 信息不足或工具不够用时如实说明，不要编造；\n\
     - 严禁在正文里输出工具调用语法。"
        .to_string()
}
