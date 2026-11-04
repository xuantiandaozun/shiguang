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

/// 一次请求问多道题（choice / noul / score / box）。各题针对同一 state 并行判断，
/// 多问几题几乎不增加耗时，所以相关判断尽量合并成一次请求。
pub struct Ask<'a> {
    /// 用量统计来源，记为 `system_one_<source>`。
    pub source: &'a str,
    pub state: Value,
    /// `{问题 id: 问题}`；id 不发给模型，答案按同一 id 返回。
    pub questions: Value,
    /// 图片资产 `{引用 key: {type, format, data_base64}}`；有图片时自动带预览请求头。
    pub assets: Option<Value>,
    pub timeout: Duration,
}

pub struct Answers {
    pub answers: Value,
    pub model: String,
    pub usage: crate::llm::client::TokenUsage,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoxAnswer {
    pub top: f64,
    pub bottom: f64,
    pub left: f64,
    pub right: f64,
    pub confidence: Option<f64>,
}

impl BoxAnswer {
    /// 框中心换算到给定尺寸（视口 CSS 像素）。
    pub fn center_in(&self, width: f64, height: f64) -> (f64, f64) {
        (
            ((self.left + self.right) / 2.0 * width * 100.0).round() / 100.0,
            ((self.top + self.bottom) / 2.0 * height * 100.0).round() / 100.0,
        )
    }
}

impl Answers {
    pub fn choice(&self, id: &str) -> Option<DecisionAnswer> {
        let answer = self.answers.get(id)?;
        Some(DecisionAnswer {
            choice: answer.get("choice").and_then(Value::as_str)?.to_string(),
            confidence: answer.get("confidence").and_then(Value::as_f64).unwrap_or(0.0),
            probabilities: answer.get("probabilities").cloned().unwrap_or(Value::Null),
            usage: self.usage.clone(),
        })
    }

    /// 是非题：答案为"是"的概率。
    pub fn noul(&self, id: &str) -> Option<f64> {
        self.answers.get(id)?.get("noul").and_then(Value::as_f64)
    }

    /// 评分题：(score, confidence)。score 是按概率加权的等级平均值。
    pub fn score(&self, id: &str) -> Option<(f64, f64)> {
        let answer = self.answers.get(id)?;
        Some((answer.get("score").and_then(Value::as_f64)?, answer.get("confidence").and_then(Value::as_f64).unwrap_or(0.0)))
    }

    pub fn bbox(&self, id: &str) -> Option<BoxAnswer> {
        let answer = self.answers.get(id)?;
        let b = answer.get("box")?;
        let edge = |k: &str| b.get(k).and_then(Value::as_f64).filter(|v| (0.0..=1.0).contains(v));
        Some(BoxAnswer {
            top: edge("top")?,
            bottom: edge("bottom")?,
            left: edge("left")?,
            right: edge("right")?,
            confidence: answer.get("confidence").and_then(Value::as_f64),
        })
    }
}

fn request_body(settings: &Settings, ask: &Ask<'_>) -> Value {
    let mut body = json!({
        "model": settings.system_one_model.trim(),
        "state": ask.state,
        "questions": ask.questions,
    });
    if let Some(assets) = &ask.assets {
        body["assets"] = assets.clone();
    }
    body
}

pub async fn ask(
    app: Option<&AppHandle>,
    http: &reqwest::Client,
    settings: &Settings,
    ask: Ask<'_>,
) -> Result<Answers> {
    if !configured(settings) {
        bail!("System One 未启用或未配置 API Key");
    }
    let body = request_body(settings, &ask);
    let deadline = tokio::time::Instant::now() + ask.timeout;
    let mut attempt = 0;
    let value: Value = loop {
        attempt += 1;
        let mut request = http
            .post(endpoint(&settings.system_one_base_url))
            .bearer_auth(settings.system_one_api_key.trim())
            .json(&body);
        if ask.assets.is_some() {
            // 图片与 Box 仍是预览能力，默认关闭，需要显式开启。
            request = request.header("TypeSafe-Danger-Zone-Images", "true");
        }
        let response = tokio::time::timeout_at(deadline, request.send())
            .await
            .map_err(|_| anyhow!("System One 请求超时"))??;
        let status = response.status();
        // 限流/过载按文档建议退避重试；只在剩余时间足够时重试一次。
        let retryable = status.as_u16() == 429 || status.as_u16() == 529;
        if retryable && attempt == 1 && deadline.saturating_duration_since(tokio::time::Instant::now()) > Duration::from_millis(900) {
            tokio::time::sleep(Duration::from_millis(400)).await;
            continue;
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("System One API 错误 {status}: {}", text.chars().take(300).collect::<String>());
        }
        break tokio::time::timeout_at(deadline, response.json())
            .await
            .map_err(|_| anyhow!("System One 响应读取超时"))??;
    };
    let usage = crate::llm::client::TokenUsage::from_response(&value);
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(settings.system_one_model.trim())
        .to_string();
    if let Some(app) = app {
        crate::llm::persist_usage(app, &format!("system_one_{}", ask.source), &model, &usage);
    }
    Ok(Answers {
        answers: value.get("answers").cloned().unwrap_or(Value::Null),
        model,
        usage,
    })
}

pub async fn choose(
    app: Option<&AppHandle>,
    source: &str,
    http: &reqwest::Client,
    settings: &Settings,
    state: Value,
    instructions: &str,
    criteria: Value,
) -> Result<DecisionAnswer> {
    let answers = ask(app, http, settings, Ask {
        source,
        state,
        questions: json!({ "decision": { "type": "choice", "instructions": instructions, "criteria": criteria } }),
        assets: None,
        timeout: Duration::from_secs(4),
    })
    .await?;
    answers.choice("decision").ok_or_else(|| anyhow!("System One 响应缺少 decision"))
}

/// 读 PNG 头里的宽高（不解码像素）。
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

/// Jev 图片尺寸要求：每边 280–1568 像素（接口不缩放，超出直接拒绝）。
pub fn image_size_ok(w: u32, h: u32) -> bool {
    (280..=1568).contains(&w) && (280..=1568).contains(&h)
}

pub struct Located {
    pub present: f64,
    pub bbox: BoxAnswer,
    pub model: String,
}

/// 在截图里找描述的目标：Box 给位置，同一请求里的是非题确认目标确实存在
/// （Box 总会返回一个框，即使目标不在画面上）。
pub async fn locate_in_screenshot(
    app: Option<&AppHandle>,
    http: &reqwest::Client,
    settings: &Settings,
    png_base64: &str,
    description: &str,
) -> Result<Located> {
    let key = format!("img-{}", uuid::Uuid::new_v4());
    let answers = ask(app, http, settings, Ask {
        source: "visual",
        state: json!({ "screenshot": key }),
        questions: json!({
            "target": { "type": "box", "instructions": format!("{description} in `screenshot`") },
            "present": { "type": "noul", "instructions": format!("Is {description} visible in `screenshot`?") }
        }),
        assets: Some(json!({ key: { "type": "image", "format": "png", "data_base64": png_base64 } })),
        timeout: Duration::from_secs(15),
    })
    .await?;
    let bbox = answers.bbox("target").ok_or_else(|| anyhow!("System One 没有返回目标框"))?;
    Ok(Located {
        present: answers.noul("present").unwrap_or(0.0),
        bbox,
        model: answers.model,
    })
}

/// 审核主模型提出的浏览器动作。复杂页面仍由主模型补充候选，但 Jev 持续参与
/// 对齐性判断；低置信度不阻断，高置信度认为不合适时要求主模型修改后重试。
pub async fn review_browser_action(
    app: &AppHandle,
    http: &reqwest::Client,
    settings: &Settings,
    goal: &str,
    tool: &str,
    args: &Value,
) -> Result<DecisionAnswer> {
    choose(
        Some(app),
        "browser",
        http,
        settings,
        json!({ "user_goal": goal, "proposed_tool": tool, "arguments": args,
            "structured_page_operation": match tool {
                "browser_capabilities" => "Read-only: report available page operations and limitations.",
                "browser_wait" => "Read-only: bounded, cancellable observation of an explicit DOM condition; no clicks or submissions.",
                "browser_extract" => "Read-only: bounded extraction of currently rendered table/list/field text; no pagination or requests.",
                "browser_action" => "locate is read-only. click/double_click/hover/fill/press/select/set_checked/upload/dialog send real mouse and keyboard input and can change page state; `steps` runs several actions in order, so judge every step. target.visual locates the target on a screenshot instead of the DOM. Unique-target, disabled and occlusion checks are enforced, but authorization for publication/submission/sending/deletion/payment is still required. Judge the actual targets and values, not just the wrapper tool name.",
                "browser_click" | "browser_type" => "Real mouse/keyboard input on the referenced element; can change page state. Judge the target and value against the goal and the user's authorization.",
                _ => "Existing browser tool; judge arguments and user authorization.",
            }
        }),
        "Should this proposed browser action be executed as the next safe step toward the user goal?",
        json!({
            "execute": "The action is a relevant, bounded next step and does not add unnecessary risk.",
            "revise": "The general approach may help, but the action or arguments should be changed before execution.",
            "escalate": "The action is sensitive, irreversible, unrelated, or lacks enough evidence; require additional inspection or user confirmation."
        }),
    ).await
}

pub async fn context_retention_strategy(
    app: &AppHandle,
    http: &reqwest::Client,
    settings: &Settings,
    messages: &[Value],
) -> Result<DecisionAnswer> {
    let tail_start = messages.len().saturating_sub(24);
    choose(
        Some(app),
        "context",
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

/// 工具类别路由的选择题。multi_or_uncertain 表示多领域或证据不足，调用方据此保留完整目录。
fn route_question(category_cards: &Value) -> Value {
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
    json!({
        "type": "choice",
        "instructions": "Choose the primary tool category for this user turn. Choose no_tools for ordinary conversation that needs no external capability. Choose multi_or_uncertain whenever more than one category may be needed or hiding tools could block completion.",
        "criteria": Value::Object(criteria),
    })
}

/// 技能推荐门槛：先判断这个请求是否需要按某个技能做，再看选哪个。
/// 做法与阈值来自 TypeSafe 的 skill suggestion 实践（两次请求：排序 + 复核）。
const SKILL_GATES: &[(&str, &str)] = &[
    ("gate::act", "Does the request ask the assistant to carry out actions on the user's computer, files, accounts, websites or devices, rather than only answer in prose?"),
    ("gate::procedure", "Would an expert handle this request by following a documented, repeatable procedure?"),
    ("gate::named", "Does the request name a specific website, app, tool, product, file format, template or method?"),
];
const SKILL_NONE: &str = "none";
const SKILL_FIT_THRESHOLD: f64 = 0.30;

fn skill_wanted(answers: &Answers) -> bool {
    let act = answers.noul("gate::act").unwrap_or(0.0);
    let procedure = answers.noul("gate::procedure").unwrap_or(0.0);
    let named = answers.noul("gate::named").unwrap_or(0.0);
    (act + procedure) / 2.0 >= SKILL_FIT_THRESHOLD || named >= 0.5
}

/// 选择题概率最高的若干技能（不含 none）。
fn top_skills(answers: &Answers, id: &str, n: usize) -> Vec<String> {
    let mut ranked: Vec<(String, f64)> = answers
        .answers
        .get(id)
        .and_then(|a| a.get("probabilities"))
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(name, _)| name.as_str() != SKILL_NONE)
        .filter_map(|(name, p)| Some((name.clone(), p.as_f64()?)))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.into_iter().take(n).map(|(name, _)| name).collect()
}

/// 复核结果：逐个候选的"是否直接覆盖这次要做的事"取最高；低于阈值不推荐。
fn pick_skill(answers: &Answers, names: &[String]) -> Option<String> {
    let mut best: Option<(String, f64)> = None;
    for (i, name) in names.iter().enumerate() {
        let fit = answers.noul(&format!("fits::{i}")).unwrap_or(0.0);
        if best.as_ref().map_or(true, |(_, b)| fit > *b) {
            best = Some((name.clone(), fit));
        }
    }
    best.filter(|(_, fit)| *fit >= SKILL_FIT_THRESHOLD).map(|(name, _)| name)
}

pub struct SkillCard {
    pub name: String,
    pub description: String,
}

pub struct TurnPlan {
    pub route: Option<DecisionAnswer>,
    pub skill: Option<String>,
    /// 用户这轮指的是哪个已登记工作区（名称）；没把握时为 None。
    pub workspace: Option<String>,
}

const WORKSPACE_NONE: &str = "none";

fn pick_workspace(answers: &Answers, min_confidence: f64) -> Option<String> {
    let answer = answers.choice("workspace")?;
    (answer.choice != WORKSPACE_NONE && answer.confidence >= min_confidence).then_some(answer.choice)
}

/// 每轮开始前的一次规划：工具类别路由 + 技能推荐门槛与排序在同一请求里并行判断；
/// 门槛通过时再用技能正文开头复核前三名。复核失败只影响推荐，不影响路由。
pub async fn plan_turn(
    app: Option<&AppHandle>,
    http: &reqwest::Client,
    settings: &Settings,
    user_goal: &str,
    category_cards: Value,
    skills: &[SkillCard],
    skill_excerpt: impl Fn(&str) -> Option<String>,
    workspaces: &[SkillCard],
) -> Result<TurnPlan> {
    let mut questions = serde_json::Map::new();
    questions.insert("decision".into(), route_question(&category_cards));
    if !workspaces.is_empty() {
        let mut criteria = serde_json::Map::new();
        for ws in workspaces.iter().take(100) {
            criteria.insert(ws.name.clone(), json!(ws.description.chars().take(180).collect::<String>()));
        }
        criteria.insert(WORKSPACE_NONE.into(), json!("The request is not about any of these projects, or it is unclear which one."));
        questions.insert("workspace".into(), json!({
            "type": "choice",
            "instructions": "Which of the user's registered projects is this request about?",
            "criteria": Value::Object(criteria),
        }));
    }
    if !skills.is_empty() {
        let mut criteria = serde_json::Map::new();
        for skill in skills.iter().take(250) {
            criteria.insert(skill.name.clone(), json!(skill.description.chars().take(180).collect::<String>()));
        }
        criteria.insert(SKILL_NONE.into(), json!("None of the listed skills matches this request."));
        questions.insert("skill".into(), json!({
            "type": "choice",
            "instructions": "Which skill (a reusable task guide) best matches what the user asks for in this turn?",
            "criteria": Value::Object(criteria),
        }));
        for (id, question) in SKILL_GATES {
            questions.insert((*id).into(), json!({ "type": "noul", "instructions": question }));
        }
    }
    let first = ask(app, http, settings, Ask {
        source: "route",
        state: json!({ "user_request": user_goal, "tool_categories": category_cards }),
        questions: Value::Object(questions),
        assets: None,
        timeout: Duration::from_secs(4),
    })
    .await?;
    let route = first.choice("decision");
    let conversational = route
        .as_ref()
        .is_some_and(|r| r.choice == "no_tools" && r.confidence >= settings.system_one_min_confidence);
    let workspace = pick_workspace(&first, settings.system_one_min_confidence);
    if skills.is_empty() || conversational || !skill_wanted(&first) {
        return Ok(TurnPlan { route, skill: None, workspace });
    }
    let shortlist = top_skills(&first, "skill", 3);
    if shortlist.is_empty() {
        return Ok(TurnPlan { route, skill: None, workspace });
    }
    let describe = |name: &str| {
        let description = skills.iter().find(|s| s.name == name).map(|s| s.description.clone()).unwrap_or_default();
        json!({ "name": name, "description": description, "excerpt": skill_excerpt(name).unwrap_or_default() })
    };
    let mut recheck = serde_json::Map::new();
    let mut criteria = serde_json::Map::new();
    for (i, name) in shortlist.iter().enumerate() {
        criteria.insert(name.clone(), describe(name));
        recheck.insert(format!("fits::{i}"), json!({
            "type": "noul",
            "instructions": {
                "question": "Does this skill directly cover the specific thing the user asks for in this turn?",
                "skill": describe(name),
            },
        }));
    }
    criteria.insert(SKILL_NONE.into(), json!("None of these skills covers the specific request."));
    recheck.insert("which".into(), json!({
        "type": "choice",
        "instructions": "Which skill should the assistant follow for this request?",
        "criteria": Value::Object(criteria),
    }));
    let skill = match ask(app, http, settings, Ask {
        source: "skill",
        state: json!({ "user_request": user_goal }),
        questions: Value::Object(recheck),
        assets: None,
        timeout: Duration::from_secs(4),
    })
    .await
    {
        Ok(second) => pick_skill(&second, &shortlist),
        Err(e) => {
            log::warn!("技能推荐复核失败: {e}");
            None
        }
    };
    Ok(TurnPlan { route, skill, workspace })
}

/// 工具轮数快用完时评估最近的步骤是否在推进目标（评分题，0=原地打转，2=明显推进）。
/// 返回 (score, confidence)。
pub async fn assess_progress(
    app: Option<&AppHandle>,
    http: &reqwest::Client,
    settings: &Settings,
    user_goal: &str,
    todos: &Value,
    recent_actions: &Value,
) -> Result<(f64, f64)> {
    let answers = ask(app, http, settings, Ask {
        source: "progress",
        state: json!({ "user_goal": user_goal, "progress_checklist": todos, "recent_actions_newest_first": recent_actions }),
        questions: json!({
            "progress": {
                "type": "score",
                "instructions": "How much have the recent agent actions advanced the user's goal?",
                "criteria": [
                    "Stuck: the actions repeat similar steps, keep failing, or go in circles without producing new useful results.",
                    "Some progress: a few new results were obtained, but it is unclear whether the goal is getting closer.",
                    "Clear progress: the actions produced new, relevant results and the goal is visibly closer to completion."
                ]
            }
        }),
        assets: None,
        timeout: Duration::from_secs(4),
    })
    .await?;
    answers.score("progress").ok_or_else(|| anyhow!("System One 响应缺少 progress"))
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
            app,
            &http,
            &settings,
            goal,
            "browser_type",
            &json!({"ref":reference,"text":text,"clear":true}),
        )
        .await?;
        if review.choice != "execute" && review.confidence >= settings.system_one_min_confidence {
            return Ok(
                json!({ "status":"need_main", "reason":"Jev 要求调整输入动作", "decision":review.choice, "confidence":review.confidence, "completed":completed }),
            );
        }
        let result = state
            .browser
            .call("type", json!({"ref":reference,"text":text,"clear":true}))
            .await?;
        let failure = action_failure(&result);
        completed.push(json!({"action":"type","query":query,"result":result}));
        if let Some(reason) = failure {
            return Ok(json!({ "status":"need_main", "reason":format!("输入「{query}」未成功：{reason}"), "completed":completed }));
        }
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
                json!({"status":"need_main","reason":"页面没有足够的低风险交互候选","completed":completed}),
            );
        }
        // 同一请求里：选下一步 + 每个点击候选"是否不可撤销"。关键词表只能按字面拦截，
        // 是非题补上"确认""Post""去结算"这类不含关键词的提交按钮。
        let mut questions = risk_questions(&criteria);
        questions.insert(
            "decision".into(),
            json!({
                "type": "choice",
                "instructions": "Choose the single best next low-risk browser action toward the goal. Choose done only when current page evidence shows completion. Choose need_main when evidence is insufficient.",
                "criteria": Value::Object(criteria),
            }),
        );
        let answers = ask(Some(app), &http, &settings, Ask {
            source: "browser_fast",
            state: json!({"goal":goal,"page":{"title":page.get("title"),"url":page.get("url"),"snapshot":snapshot},"recent_actions":completed}),
            questions: Value::Object(questions),
            assets: None,
            timeout: Duration::from_secs(6),
        })
        .await?;
        let answer = answers
            .choice("decision")
            .ok_or_else(|| anyhow!("System One 响应缺少 decision"))?;
        if answer.confidence < settings.system_one_min_confidence || answer.choice == "need_main" {
            return Ok(
                json!({"status":"need_main","reason":"Jev 置信度不足或需要主模型补充候选","decision":answer.choice,"confidence":answer.confidence,"completed":completed}),
            );
        }
        if answer.choice == "done" {
            return Ok(
                json!({"status":"complete","steps":step,"completed":completed,"confidence":answer.confidence}),
            );
        }
        if let Some(risk) = risky_choice(&answers, &answer.choice) {
            return Ok(json!({
                "status":"need_main",
                "reason":format!("Jev 选中的 {} 可能是发布/提交/删除等不可撤销操作（概率 {risk:.2}），交回主模型确认", answer.choice),
                "completed":completed,
            }));
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
        let failure = action_failure(&result);
        completed
            .push(json!({"action":answer.choice,"confidence":answer.confidence,"result":result}));
        if let Some(reason) = failure {
            return Ok(json!({"status":"need_main","reason":format!("动作未成功：{reason}"),"completed":completed}));
        }
    }
    Ok(
        json!({"status":"need_main","reason":"快速步骤预算已用完，需要主模型结合当前页面继续补充候选","completed":completed}),
    )
}

/// 为每个点击候选生成一道是非题：点了会不会造成不可撤销的外部变化。
fn risk_questions(criteria: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    criteria
        .iter()
        .filter(|(key, _)| key.starts_with("click_"))
        .map(|(key, description)| {
            (
                format!("risk::{key}"),
                json!({
                    "type": "noul",
                    "instructions": {
                        "question": "Would clicking this element publish, post, send, submit, pay, purchase, delete, or otherwise make a change outside this page that cannot be undone?",
                        "element": description,
                    },
                }),
            )
        })
        .collect()
}

/// 选中的点击候选被判为不可撤销的概率（≥0.5 才算），否则 None。
fn risky_choice(answers: &Answers, choice: &str) -> Option<f64> {
    answers
        .noul(&format!("risk::{choice}"))
        .filter(|p| *p >= 0.5)
}

/// 真实输入引擎把失败（遮挡、未获得焦点、读回不一致、原生对话框）放在返回体里，
/// 而不是抛错；快速循环遇到就停，交回主模型处理。
fn action_failure(result: &Value) -> Option<String> {
    if let Some(error) = result.get("error").and_then(Value::as_str) {
        return Some(error.to_string());
    }
    result
        .pointer("/effect/native_dialog/message")
        .and_then(Value::as_str)
        .map(|message| format!("页面弹出原生对话框：{message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings(base_url: &str) -> Settings {
        let path = std::env::temp_dir().join(format!("shiguang-s1-{}.db", uuid::Uuid::new_v4()));
        let db = crate::db::Db::new(&path).unwrap();
        let mut settings = crate::commands::load_settings(&db);
        drop(db);
        let _ = std::fs::remove_file(&path);
        settings.system_one_enabled = true;
        settings.system_one_api_key = "test-key".into();
        settings.system_one_model = "jev-1.14.0".into();
        settings.system_one_base_url = base_url.into();
        settings
    }

    #[test]
    fn request_body_carries_questions_and_assets() {
        let settings = test_settings("https://x/v1");
        let body = request_body(&settings, &Ask {
            source: "t",
            state: json!({"screenshot": "img-1"}),
            questions: json!({"target": {"type": "box", "instructions": "the button in `screenshot`"}}),
            assets: Some(json!({"img-1": {"type": "image", "format": "png", "data_base64": "AAA"}})),
            timeout: Duration::from_secs(1),
        });
        assert_eq!(body["model"], "jev-1.14.0");
        assert_eq!(body["questions"]["target"]["type"], "box");
        assert_eq!(body["assets"]["img-1"]["format"], "png");
        let plain = request_body(&settings, &Ask { source: "t", state: json!({}), questions: json!({}), assets: None, timeout: Duration::from_secs(1) });
        assert!(plain.get("assets").is_none());
    }

    #[test]
    fn answers_parse_choice_noul_and_box() {
        let answers = Answers {
            answers: json!({
                "decision": {"type": "choice", "choice": "click_3", "confidence": 0.9, "probabilities": {"click_3": 0.95}},
                "risk::click_3": {"type": "noul", "noul": 0.82},
                "target": {"type": "box", "box": {"top": 0.1, "bottom": 0.3, "left": 0.5, "right": 0.7}, "confidence": 0.77},
                "broken": {"type": "box", "box": {"top": 1.4, "bottom": 0.3, "left": 0.5, "right": 0.7}}
            }),
            model: "jev-1.14.0".into(),
            usage: Default::default(),
        };
        assert_eq!(answers.choice("decision").unwrap().choice, "click_3");
        assert_eq!(answers.noul("risk::click_3"), Some(0.82));
        let b = answers.bbox("target").unwrap();
        assert_eq!(b.confidence, Some(0.77));
        assert_eq!(b.center_in(1000.0, 500.0), (600.0, 100.0));
        assert!(answers.bbox("broken").is_none());
        assert_eq!(risky_choice(&answers, "click_3"), Some(0.82));
        assert_eq!(risky_choice(&answers, "scroll_down"), None);
    }

    #[test]
    fn risk_questions_cover_only_click_candidates() {
        let criteria = snapshot_options("[1] <button> 下一步\n[2] <button> 确认\n[3] <a> 帮助");
        let questions = risk_questions(&criteria);
        assert!(questions.contains_key("risk::click_1"));
        assert!(questions.contains_key("risk::click_2"));
        assert!(!questions.keys().any(|k| k.contains("scroll") || k.contains("done") || k.contains("need_main")));
        assert_eq!(questions["risk::click_2"]["type"], "noul");
    }

    #[test]
    fn png_header_size_and_jev_image_limits() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&1500u32.to_be_bytes());
        png.extend_from_slice(&900u32.to_be_bytes());
        assert_eq!(png_size(&png), Some((1500, 900)));
        assert!(png_size(b"not a png at all, definitely").is_none());
        assert!(image_size_ok(1500, 900));
        assert!(!image_size_ok(3000, 900));
        assert!(!image_size_ok(1500, 200));
    }

    #[test]
    fn typesafe_usage_fields_are_recorded() {
        let usage = crate::llm::client::TokenUsage::from_response(&json!({"usage": {"input_tokens": 120, "output_tokens": 3}}));
        assert_eq!(usage.prompt_tokens, 120);
        assert_eq!(usage.completion_tokens, 3);
        assert_eq!(usage.total_tokens, 123);
    }

    // 本地模拟服务：首个请求 429，第二个返回答案；确认图片请求带预览头并会重试一次。
    #[tokio::test]
    async fn ask_retries_rate_limit_and_sends_image_header() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut seen = Vec::new();
            for i in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 65536];
                let n = socket.read(&mut buf).await.unwrap();
                seen.push(String::from_utf8_lossy(&buf[..n]).to_string());
                let response = if i == 0 {
                    "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
                } else {
                    let body = r#"{"model":"jev-1.14.0","answers":{"present":{"type":"noul","noul":0.91}},"usage":{"input_tokens":50,"output_tokens":0}}"#;
                    format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
                };
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            seen
        });
        let settings = test_settings(&format!("http://{address}/v1"));
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let answers = ask(None, &http, &settings, Ask {
            source: "visual",
            state: json!({"screenshot": "img-1"}),
            questions: json!({"present": {"type": "noul", "instructions": "Is the button visible in `screenshot`?"}}),
            assets: Some(json!({"img-1": {"type": "image", "format": "png", "data_base64": "AAA"}})),
            timeout: Duration::from_secs(5),
        })
        .await
        .unwrap();
        assert_eq!(answers.noul("present"), Some(0.91));
        assert_eq!(answers.model, "jev-1.14.0");
        assert_eq!(answers.usage.prompt_tokens, 50);
        let seen = server.await.unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|r| r.to_lowercase().contains("typesafe-danger-zone-images: true")));
        assert!(seen[0].starts_with("POST /v1/systemone"));
    }

    fn answers(value: Value) -> Answers {
        Answers { answers: value, model: "jev-1.14.0".into(), usage: Default::default() }
    }

    #[test]
    fn skill_gate_ranking_and_recheck() {
        let wanted = answers(json!({"gate::act":{"noul":0.5},"gate::procedure":{"noul":0.2},"gate::named":{"noul":0.1}}));
        assert!(skill_wanted(&wanted));
        let named = answers(json!({"gate::act":{"noul":0.05},"gate::procedure":{"noul":0.1},"gate::named":{"noul":0.8}}));
        assert!(skill_wanted(&named));
        let chat = answers(json!({"gate::act":{"noul":0.05},"gate::procedure":{"noul":0.2},"gate::named":{"noul":0.1}}));
        assert!(!skill_wanted(&chat));

        let ranked = answers(json!({"skill":{"type":"choice","choice":"a","confidence":0.5,
            "probabilities":{"a":0.5,"none":0.3,"b":0.15,"c":0.04,"d":0.01}}}));
        assert_eq!(top_skills(&ranked, "skill", 3), vec!["a", "b", "c"]);

        let names = vec!["a".to_string(), "b".to_string()];
        let recheck = answers(json!({"fits::0":{"noul":0.2},"fits::1":{"noul":0.7}}));
        assert_eq!(pick_skill(&recheck, &names).as_deref(), Some("b"));
        let weak = answers(json!({"fits::0":{"noul":0.1},"fits::1":{"noul":0.25}}));
        assert_eq!(pick_skill(&weak, &names), None);
    }

    #[test]
    fn route_question_keeps_escape_options_and_score_parses() {
        let q = route_question(&json!({"browser":[{"name":"browser_click"}],"files":[{"name":"read_file"}]}));
        let criteria = q["criteria"].as_object().unwrap();
        assert!(criteria.contains_key("browser") && criteria.contains_key("no_tools") && criteria.contains_key("multi_or_uncertain"));
        assert!(criteria["browser"].as_str().unwrap().contains("browser_click"));
        let a = answers(json!({"progress":{"type":"score","score":1.6,"confidence":0.8}}));
        assert_eq!(a.score("progress"), Some((1.6, 0.8)));
    }

    // 真实调用：用本机实际的技能目录和历史里的真实请求检查推荐是否合理。
    // cargo test --lib live_ -- --ignored --nocapture，需要 TYPESAFE_API_KEY、SHIGUANG_APP_DIR，可选 TYPESAFE_PROXY。
    #[tokio::test]
    #[ignore]
    async fn live_skill_suggestion_and_progress() {
        let (Ok(key), Ok(app_dir)) = (std::env::var("TYPESAFE_API_KEY"), std::env::var("SHIGUANG_APP_DIR")) else {
            eprintln!("skip: TYPESAFE_API_KEY / SHIGUANG_APP_DIR not set");
            return;
        };
        let mut settings = test_settings("https://api.typesafe.ai/v1");
        settings.system_one_api_key = key;
        settings.system_one_model = "jev-latest".into();
        let mut builder = reqwest::Client::builder();
        if let Ok(proxy) = std::env::var("TYPESAFE_PROXY") {
            builder = builder.proxy(reqwest::Proxy::all(proxy).unwrap());
        }
        let http = builder.build().unwrap();
        let store = crate::skills::SkillStore::new(std::path::Path::new(&app_dir));
        let skills: Vec<SkillCard> = store.list().into_iter().filter(|s| s.enabled)
            .map(|s| SkillCard { name: s.name, description: s.description }).collect();
        let cards = json!({"browser":[{"name":"browser_click"}],"commands":[{"name":"run_command"}],"files":[{"name":"read_file"}],"web":[{"name":"web_search"}]});
        let cases: &[(&str, Option<&str>)] = &[
            ("发个头条文章吧，你自己找一些合适的配图", Some("toutiao-publish-zh")),
            ("帮我在咸鱼工作台再添加一个商品，小程序预约系统", Some("xianyu-goofish-seller-publish")),
            ("帮我写一下今天的日报，提交到飞书", Some("daily-report-feishu-base")),
            ("帮我把桌面整理一下", Some("desktop-organize")),
            ("你好", None),
            ("量子纠缠是什么，简单解释一下", None),
        ];
        let mut correct = 0;
        for (request, expected) in cases {
            let plan = plan_turn(None, &http, &settings, request, cards.clone(), &skills, |name| {
                store.read_raw(name).ok().map(|raw| crate::skills::body_excerpt(&raw, 700))
            }, &[])
            .await
            .unwrap();
            let ok = plan.skill.as_deref() == *expected;
            correct += ok as usize;
            println!("{} | {request} → skill={:?} route={:?}", if ok { "OK  " } else { "MISS" }, plan.skill, plan.route.map(|r| (r.choice, (r.confidence * 100.0).round() / 100.0)));
        }
        println!("skill suggestion: {correct}/{}", cases.len());
        assert!(correct >= cases.len() - 1, "推荐命中太少");
        let stuck = assess_progress(None, &http, &settings, "发布 3 条微头条",
            &json!([{"content":"发布第一条","status":"in_progress"}]),
            &json!(vec!["browser_click({\"ref\":12}) → {\"error\":\"目标被遮挡\"}"; 8])).await.unwrap();
        let moving = assess_progress(None, &http, &settings, "发布 3 条微头条",
            &json!([{"content":"发布第一条","status":"completed"},{"content":"发布第二条","status":"completed"},{"content":"发布第三条","status":"in_progress"}]),
            &json!(["browser_action(steps) → {\"ok\":true,\"effect\":{\"messages\":[\"发布成功\"]}}", "todo_write → 已完成 2", "browser_navigate → 发布页"])).await.unwrap();
        println!("progress stuck={stuck:?} moving={moving:?}");
        assert!(stuck.0 < moving.0);
    }

    #[test]
    fn engine_failures_stop_the_fast_loop() {
        assert!(action_failure(&json!({"ok":true,"effect":{"settled":true}})).is_none());
        assert_eq!(action_failure(&json!({"error":"目标被遮挡","code":"covered"})).as_deref(), Some("目标被遮挡"));
        assert!(action_failure(&json!({"ok":true,"effect":{"native_dialog":{"type":"confirm","message":"确定？"}}})).unwrap().contains("确定？"));
    }
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
