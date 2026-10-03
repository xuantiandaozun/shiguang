//! TypeSafe System One：为主代理提供低延迟、结构化且带置信度的判断。
//! 它不生成自由文本，也不替代主模型；不可用时调用方应保持原有能力。

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::time::Duration;

use crate::commands::Settings;
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone)]
pub struct DecisionAnswer {
    pub choice: String,
    pub confidence: f64,
    pub probabilities: Value,
    pub usage: crate::llm::client::TokenUsage,
}

fn endpoint(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/systemone") {
        base.to_string()
    } else {
        format!("{base}/systemone")
    }
}

pub fn configured(settings: &Settings) -> bool {
    settings.system_one_enabled
        && !settings.system_one_api_key.trim().is_empty()
        && !settings.system_one_model.trim().is_empty()
}

pub async fn choose(
    http: &reqwest::Client,
    settings: &Settings,
    state: Value,
    instructions: &str,
    criteria: Value,
) -> Result<DecisionAnswer> {
    if !configured(settings) {
        bail!("System One 未启用或未配置 API Key");
    }
    let body = json!({
        "state": state,
        "model": settings.system_one_model.trim(),
        "questions": {
            "decision": {
                "type": "choice",
                "instructions": instructions,
                "criteria": criteria
            }
        }
    });
    let request = http
        .post(endpoint(&settings.system_one_base_url))
        .bearer_auth(settings.system_one_api_key.trim())
        .json(&body)
        .send();
    let response = tokio::time::timeout(Duration::from_secs(4), request)
        .await
        .map_err(|_| anyhow!("System One 请求超时"))??;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!(
            "System One API 错误 {status}: {}",
            text.chars().take(300).collect::<String>()
        );
    }
    let value: Value = response.json().await?;
    let answer = value
        .pointer("/answers/decision")
        .ok_or_else(|| anyhow!("System One 响应缺少 decision"))?;
    let usage = crate::llm::client::TokenUsage::from_response(&value);
    Ok(DecisionAnswer {
        choice: answer
            .get("choice")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        confidence: answer
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        probabilities: answer.get("probabilities").cloned().unwrap_or(Value::Null),
        usage,
    })
}

/// 审核主模型提出的浏览器动作。复杂页面仍由主模型补充候选，但 Jev 持续参与
/// 对齐性判断；低置信度不阻断，高置信度认为不合适时要求主模型修改后重试。
pub async fn review_browser_action(
    http: &reqwest::Client,
    settings: &Settings,
    goal: &str,
    tool: &str,
    args: &Value,
) -> Result<DecisionAnswer> {
    choose(
        http,
        settings,
        json!({ "user_goal": goal, "proposed_tool": tool, "arguments": args }),
        "Should this proposed browser action be executed as the next safe step toward the user goal?",
        json!({
            "execute": "The action is a relevant, bounded next step and does not add unnecessary risk.",
            "revise": "The general approach may help, but the action or arguments should be changed before execution.",
            "escalate": "The action is sensitive, irreversible, unrelated, or lacks enough evidence; require additional inspection or user confirmation."
        }),
    ).await
}

pub async fn context_retention_strategy(
    http: &reqwest::Client,
    settings: &Settings,
    messages: &[Value],
) -> Result<DecisionAnswer> {
    let tail_start = messages.len().saturating_sub(24);
    choose(
        http,
        settings,
        json!({
            "conversation_tail": &messages[tail_start..],
            "message_count": messages.len(),
            "purpose": "Choose how much recent verbatim context to retain before older content is summarized."
        }),
        "How much recent verbatim context should be retained to safely continue the unfinished task?",
        json!({
            "preserve_more": "Recent messages contain dense user corrections, exact wording, paths, numbers, state transitions, or interdependent work that should remain verbatim.",
            "balanced": "Recent messages contain a normal mix of task state and reproducible details; standard retention is sufficient.",
            "compress_more": "Recent messages are dominated by repetitive observations, replaceable tool output, or superseded attempts; a shorter verbatim tail is safe."
        }),
    ).await
}

/// 为单轮主任务选择主要工具类别。只有调用方认为高置信度时才真正裁剪；
/// multi_or_uncertain 明确表示多领域任务或证据不足，应保留完整目录。
pub async fn route_tool_category(
    http: &reqwest::Client,
    settings: &Settings,
    user_goal: &str,
    category_cards: Value,
) -> Result<DecisionAnswer> {
    let mut criteria = serde_json::Map::new();
    if let Some(groups) = category_cards.as_object() {
        for (category, tools) in groups {
            let names = tools
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|tool| tool.get("name").and_then(Value::as_str))
                .take(20)
                .collect::<Vec<_>>()
                .join(", ");
            criteria.insert(
                category.clone(),
                Value::String(format!("The task can be completed primarily with this category. Available tools: {names}")),
            );
        }
    }
    criteria.insert(
        "no_tools".into(),
        Value::String("The user only needs conversation, explanation, or a response based on information already present; no external action or lookup is needed.".into()),
    );
    criteria.insert(
        "multi_or_uncertain".into(),
        Value::String("The task spans multiple categories, may need follow-up capabilities, or there is not enough evidence to safely hide other tools.".into()),
    );
    choose(
        http,
        settings,
        json!({ "user_goal": user_goal, "tool_categories": category_cards }),
        "Choose the primary tool category for this user turn. Choose no_tools for ordinary conversation that needs no external capability. Choose multi_or_uncertain whenever more than one category may be needed or hiding tools could block completion.",
        Value::Object(criteria),
    )
    .await
}

const SENSITIVE_LABELS: &[&str] = &[
    "提交",
    "发布",
    "发送",
    "付款",
    "支付",
    "购买",
    "删除",
    "移除",
    "注销",
    "确认订单",
    "confirm purchase",
    "pay",
    "delete",
    "send",
    "publish",
    "submit",
];

fn snapshot_options(snapshot: &str) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for line in snapshot.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix('[') else {
            continue;
        };
        let Some(end) = rest.find(']') else { continue };
        let Ok(reference) = rest[..end].parse::<u64>() else {
            continue;
        };
        let description = rest[end + 1..].trim();
        if description.is_empty()
            || SENSITIVE_LABELS
                .iter()
                .any(|word| description.to_lowercase().contains(word))
        {
            continue;
        }
        out.insert(
            format!("click_{reference}"),
            Value::String(format!("Click element [{reference}] {description}")),
        );
        if out.len() >= 60 {
            break;
        }
    }
    out.insert(
        "scroll_down".into(),
        Value::String("Scroll down to inspect more of the current page".into()),
    );
    out.insert(
        "done".into(),
        Value::String("The user goal is already visibly complete".into()),
    );
    out.insert("need_main".into(), Value::String("No listed low-risk action is sufficiently supported; ask the main model to inspect or propose new candidates".into()));
    out
}

/// 一次主模型调用之后的低风险多步循环。明确输入值先通过页面语义定位并接受
/// Jev 复核；随后每一步点击均由 Jev 从当前快照候选中选择。敏感动作永不进入候选。
pub async fn fast_browser_loop(app: &AppHandle, args: &Value) -> Result<Value> {
    let state = app.state::<crate::AppState>();
    let settings = crate::commands::load_settings(&state.db);
    if !configured(&settings) || !settings.system_one_browser_assist {
        bail!("请先在设置 → 智能加速中启用并配置 TypeSafe System One");
    }
    let goal = args
        .get("goal")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("缺少 goal"))?;
    let max_steps = args
        .get("max_steps")
        .and_then(Value::as_u64)
        .unwrap_or(8)
        .clamp(1, 15);
    let inputs = args
        .get("inputs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if inputs.values().any(|v| !v.is_string()) {
        bail!("inputs 的值必须是字符串");
    }
    let http = reqwest::Client::new();
    let mut completed = Vec::new();
    let mut usage = crate::llm::client::TokenUsage::default();

    // 主模型只提供用户已经明确给出的输入值；唯一候选才允许进入快速通道。
    for (query, value) in inputs {
        let text = value.as_str().unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        let found = state
            .browser
            .call("find", json!({ "query": query, "limit": 4 }))
            .await?;
        let candidates = found
            .get("candidates")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if candidates.len() != 1 {
            return Ok(
                json!({ "status":"need_main", "reason":format!("输入字段「{query}」候选数为 {}", candidates.len()), "completed":completed }),
            );
        }
        let reference = candidates[0]
            .get("ref")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("输入候选缺少 ref"))?;
        let review = review_browser_action(
            &http,
            &settings,
            goal,
            "browser_type",
            &json!({"ref":reference,"text":text,"clear":true}),
        )
        .await?;
        usage.prompt_tokens += review.usage.prompt_tokens;
        usage.completion_tokens += review.usage.completion_tokens;
        usage.total_tokens += review.usage.total_tokens;
        if review.choice != "execute" && review.confidence >= settings.system_one_min_confidence {
            return Ok(
                json!({ "status":"need_main", "reason":"Jev 要求调整输入动作", "decision":review.choice, "confidence":review.confidence, "completed":completed }),
            );
        }
        let result = state
            .browser
            .call("type", json!({"ref":reference,"text":text,"clear":true}))
            .await?;
        completed.push(json!({"action":"type","query":query,"result":result}));
    }

    for step in 0..max_steps {
        let page = state
            .browser
            .call("snapshot", json!({"max_chars":6000}))
            .await?;
        let snapshot = page.get("snapshot").and_then(Value::as_str).unwrap_or("");
        let criteria = snapshot_options(snapshot);
        if criteria.len() <= 3 {
            return Ok(
                json!({"status":"need_main","reason":"页面没有足够的低风险交互候选","completed":completed,"usage":usage.total_tokens}),
            );
        }
        let answer = choose(
            &http,
            &settings,
            json!({"goal":goal,"page":{"title":page.get("title"),"url":page.get("url"),"snapshot":snapshot},"recent_actions":completed}),
            "Choose the single best next low-risk browser action toward the goal. Choose done only when current page evidence shows completion. Choose need_main when evidence is insufficient.",
            Value::Object(criteria),
        ).await?;
        usage.prompt_tokens += answer.usage.prompt_tokens;
        usage.completion_tokens += answer.usage.completion_tokens;
        usage.total_tokens += answer.usage.total_tokens;
        if answer.confidence < settings.system_one_min_confidence || answer.choice == "need_main" {
            crate::llm::persist_usage(
                app,
                "system_one_browser_fast",
                &settings.system_one_model,
                &usage,
            );
            return Ok(
                json!({"status":"need_main","reason":"Jev 置信度不足或需要主模型补充候选","decision":answer.choice,"confidence":answer.confidence,"completed":completed}),
            );
        }
        if answer.choice == "done" {
            crate::llm::persist_usage(
                app,
                "system_one_browser_fast",
                &settings.system_one_model,
                &usage,
            );
            return Ok(
                json!({"status":"complete","steps":step,"completed":completed,"confidence":answer.confidence}),
            );
        }
        let result = if answer.choice == "scroll_down" {
            state
                .browser
                .call("scroll", json!({"direction":"down","amount":600}))
                .await?
        } else if let Some(raw) = answer.choice.strip_prefix("click_") {
            let reference: u64 = raw.parse().map_err(|_| anyhow!("Jev 返回了无效 ref"))?;
            state
                .browser
                .call("click", json!({"ref":reference}))
                .await?
        } else {
            bail!("Jev 返回了未知动作 {}", answer.choice);
        };
        completed
            .push(json!({"action":answer.choice,"confidence":answer.confidence,"result":result}));
    }
    crate::llm::persist_usage(
        app,
        "system_one_browser_fast",
        &settings.system_one_model,
        &usage,
    );
    Ok(
        json!({"status":"need_main","reason":"快速步骤预算已用完，需要主模型结合当前页面继续补充候选","completed":completed}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_accepts_base_or_full_path() {
        assert_eq!(
            endpoint("https://api.typesafe.ai/v1"),
            "https://api.typesafe.ai/v1/systemone"
        );
        assert_eq!(
            endpoint("https://x/v1/systemone/"),
            "https://x/v1/systemone"
        );
    }
}
