use crate::db::{Db, PlanCategory, ToolCallRecord};
use crate::organizer::{executor, rules, scanner};
use anyhow::{anyhow, bail, Result};
use chrono::TimeZone;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tauri::{AppHandle, Emitter, Manager};

pub const DISCOVER_TOOL: &str = "discover_capabilities";


/// 完整工具目录是基线；System One 路由可精简类别，但必须保留发现入口。
/// discover_capabilities 检索 Skills、建议组合与索引策略，并补齐被路由省略的类别。
/// 核心工具由内置插件的 core_tools 声明。
pub fn core_tool_names() -> Vec<String> {
    crate::plugins::snapshot().core_tools.clone()
}

fn tools_for_category(category: &str) -> Vec<String> {
    crate::plugins::snapshot().tools_for_category(category)
}

fn activate_categories(categories: &[String]) -> Vec<String> {
    let mut names = core_tool_names();
    for category in categories {
        for name in tools_for_category(category) {
            if !names.iter().any(|existing| existing == &name) {
                names.push(name);
            }
        }
    }
    names
}

fn retired_tool(name: &str) -> bool {
    crate::plugins::snapshot().retired_message(name).is_some()
}

/// 全部工具定义（来自已启用插件的 tools/*.json），已下线的除外。
pub fn definitions() -> Value {
    let snapshot = crate::plugins::snapshot();
    let mut tools: Vec<Value> = snapshot
        .tools
        .iter()
        .filter(|t| snapshot.retired_message(&t.name).is_none())
        .map(|t| t.definition.clone())
        .collect();
    extend_discovery_categories(&snapshot, &mut tools);
    Value::Array(tools)
}

/// discover_capabilities 的类别枚举写在 core 插件里；其它插件新增的类别在这里补进去，
/// 内置类别集合不变时定义逐字节不变。
fn extend_discovery_categories(snapshot: &crate::plugins::Snapshot, tools: &mut [Value]) {
    for tool in tools.iter_mut() {
        if tool.pointer("/function/name").and_then(Value::as_str) != Some(DISCOVER_TOOL) {
            continue;
        }
        let known: Vec<String> = tool
            .pointer("/function/parameters/properties/categories/items/enum")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        let extra: Vec<&crate::plugins::Category> = snapshot
            .categories
            .iter()
            .filter(|c| !c.tools.is_empty() && !known.contains(&c.id))
            .collect();
        if extra.is_empty() {
            continue;
        }
        if let Some(items) = tool
            .pointer_mut("/function/parameters/properties/categories/items/enum")
            .and_then(Value::as_array_mut)
        {
            for category in &extra {
                items.push(json!(category.id));
            }
        }
        if let Some(description) = tool.pointer_mut("/function/parameters/properties/categories/description") {
            if let Some(text) = description.as_str() {
                let mut text = text.to_string();
                for category in &extra {
                    let summary = if category.description.is_empty() { &category.label } else { &category.description };
                    text.push_str(&format!("；{}={}", category.id, summary));
                }
                *description = json!(text);
            }
        }
    }
}

/// 工具是否满足插件声明的 when 条件（如 setting:read_image_enabled）。
fn tool_condition_met(db: &Db, name: &str) -> bool {
    let snapshot = crate::plugins::snapshot();
    let Some(when) = snapshot.tool(name).and_then(|t| t.when.clone()) else { return true };
    match when.as_str() {
        "setting:read_image_enabled" => crate::commands::load_settings(db).read_image_enabled,
        other => {
            if let Some(key) = other.strip_prefix("setting:") {
                db.get_setting(key).ok().flatten().is_some_and(|v| v == "true")
            } else {
                true
            }
        }
    }
}

/// 把插件提供的工具写入运行时注册表。INSERT OR IGNORE 保证后续可直接修改数据库里的
/// 描述、Schema、启停状态，重启、升级或插件重载都不会把它们覆盖。
/// 内置插件的工具 source=builtin；用户插件的工具 source=plugin:<id>。
pub fn seed_registry(db: &Db) -> Result<usize> {
    let now = crate::db::now_str();
    let snapshot = crate::plugins::snapshot();
    let entries = snapshot
        .tools
        .iter()
        .filter(|tool| snapshot.retired_message(&tool.name).is_none())
        .map(|tool| {
            let builtin = snapshot.plugin(&tool.plugin).is_some_and(|p| p.builtin);
            let browser = tool.executor_kind() == "browser_action";
            crate::db::ToolRegistryEntry {
                category: snapshot.category_for_tool(&tool.name),
                // 注册表只记录白名单内的执行器；插件执行器（script/mcp/...）以插件文件为准。
                executor_kind: if browser { "browser_action" } else { "builtin" }.to_string(),
                executor_config: if browser { tool.executor.clone() } else { json!({}) },
                name: tool.name.clone(),
                definition: tool.definition.clone(),
                enabled: true,
                source: if builtin { "builtin".to_string() } else { format!("plugin:{}", tool.plugin) },
                revision: 1,
                updated_at: now.clone(),
            }
        })
        .collect::<Vec<_>>();
    db.seed_tool_registry(&entries)
}

/// 注册表条目是否来自插件（而非手工加入）。来自插件的条目在插件停用/删除后不再提供。
fn plugin_sourced(entry: &crate::db::ToolRegistryEntry) -> bool {
    entry.source == "builtin" || entry.source.starts_with("plugin:")
}

/// 未被手工改过的条目，类别跟随插件；改过的保留数据库里的值。
fn effective_category(entry: &crate::db::ToolRegistryEntry) -> String {
    if plugin_sourced(entry) && entry.revision <= 1 {
        let snapshot = crate::plugins::snapshot();
        if snapshot.tool(&entry.name).is_some() {
            return snapshot.category_for_tool(&entry.name);
        }
    }
    entry.category.clone()
}

pub fn registry_definitions(db: &Db, native_vision: bool) -> Value {
    let Ok(entries) = db.enabled_tool_registry() else {
        return chat_definitions(native_vision);
    };
    if entries.is_empty() {
        return chat_definitions(native_vision);
    }
    // 种子定义只在首次启动写入；用户没改过的内置工具（revision=1）应随版本升级，
    // 否则新版的说明和参数永远到不了模型。改过的（revision>1）保持原样。
    let current: HashMap<String, Value> = definitions()
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| Some((d.pointer("/function/name")?.as_str()?.to_string(), d.clone())))
        .collect();
    Value::Array(
        entries
            .into_iter()
            .filter(|entry| entry.definition.is_object() && !retired_tool(&entry.name))
            .filter(|entry| !plugin_sourced(entry) || current.contains_key(&entry.name))
            .filter(|entry| tool_condition_met(db, &entry.name))
            .map(|entry| {
                let mut definition = entry.definition;
                if entry.source == "builtin" && entry.revision <= 1 {
                    if let Some(fresh) = current.get(&entry.name) {
                        definition = fresh.clone();
                    }
                }
                if entry.name == "discover_capabilities" && entry.executor_kind == "builtin" {
                    if let Some(categories) = definition.pointer_mut("/function/parameters/properties/categories/items/enum").and_then(Value::as_array_mut) {
                        if !categories.iter().any(|v| v.as_str() == Some("servers")) { categories.push(json!("servers")); }
                    }
                    if let Some(description) = definition.pointer_mut("/function/parameters/properties/categories/description") {
                        if let Some(text) = description.as_str() {
                            if !text.contains("servers=") { *description = json!(format!("{text}；servers=拾光SSH连接新增/修改/删除、远程命令与SFTP传输")); }
                        }
                    }
                    if let Some(description) = definition.pointer_mut("/function/description") {
                        if let Some(text) = description.as_str() {
                            let text = text.replace("全部工具已经可用，不必等待激活。", "本轮工具列表可能按类别精简；缺少工具时发现相关类别，下一轮自动补齐。");
                            *description = json!(text);
                        }
                    }
                }
                // Upgrade the built-in delegation contract without replacing user edits.
                if entry.name == "run_subagent" && entry.executor_kind == "builtin" {
                    let current = definitions_for(&["run_subagent"]);
                    let current = &current.as_array().unwrap()[0];
                    if let Some(properties) = definition.pointer_mut("/function/parameters/properties").and_then(Value::as_object_mut) {
                        properties.entry("role".to_string()).or_insert_with(|| current["function"]["parameters"]["properties"]["role"].clone());
                    }
                    let legacy = "把「需要多步阅读或分析」的子任务交给子代理。子代理在独立上下文中工作（只读文件/索引/公开网页等），你只能拿到结构化汇报（status=complete 或 blocked，含 findings/evidence/blockers）。适合会占掉大量对话上下文的汇总任务。子任务必须具体、自包含。不能操作浏览器、不能写文件、不能再委托。耗时任务设 run_in_background=true，主对话先继续，需要结论时 await_subagent；不要轮询。";
                    if definition["function"]["description"].as_str() == Some(legacy) {
                        definition["function"]["description"] = current["function"]["description"].clone();
                    }
                }
                definition
            })
            .collect::<Vec<_>>(),
    )
    .pipe_discovery()
}

trait PipeDiscovery {
    fn pipe_discovery(self) -> Value;
}

impl PipeDiscovery for Value {
    /// 被手工改过的发现工具也要能看到插件新增的类别。
    fn pipe_discovery(mut self) -> Value {
        if let Some(items) = self.as_array_mut() {
            extend_discovery_categories(&crate::plugins::snapshot(), items);
        }
        self
    }
}

pub fn registry_definitions_for(db: &Db, names: &[String], native_vision: bool) -> Value {
    let all = registry_definitions(db, native_vision);
    Value::Array(
        all.as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|tool| {
                tool.pointer("/function/name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| names.iter().any(|candidate| candidate == name))
            })
            .collect(),
    )
}

pub fn registry_definitions_for_categories(
    db: &Db,
    categories: &[String],
    native_vision: bool,
) -> Value {
    let Ok(entries) = db.enabled_tool_registry() else {
        return chat_definitions(native_vision);
    };
    if entries.is_empty() || categories.is_empty() {
        return registry_definitions(db, native_vision);
    }
    let core = core_tool_names();
    let selected: HashSet<String> = entries.into_iter()
        .filter(|entry| {
            let category = effective_category(entry);
            category == "core" || core.contains(&entry.name) || categories.contains(&category)
        })
        .map(|entry| entry.name)
        .collect();
    // Apply the same compatibility upgrades as the full inventory before filtering.
    // Returning raw persisted schemas here hid new categories from routed sessions.
    let upgraded = registry_definitions(db, native_vision);
    Value::Array(upgraded.as_array().into_iter().flatten()
        .filter(|tool| tool.pointer("/function/name").and_then(Value::as_str).is_some_and(|name| selected.contains(name)))
        .cloned().collect())
}

pub fn registry_category_cards(db: &Db) -> Value {
    let Ok(entries) = db.enabled_tool_registry() else {
        return json!({});
    };
    let mut grouped = serde_json::Map::new();
    for entry in entries.into_iter().filter(|entry| entry.category != "core") {
        let label = entry
            .definition
            .pointer("/function/description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(90)
            .collect::<String>();
        let bucket = grouped
            .entry(entry.category)
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(items) = bucket.as_array_mut() {
            items.push(json!({ "name": entry.name, "summary": label }));
        }
    }
    Value::Object(grouped)
}

/// 从全量工具定义中按名称挑出子集（子代理等受限场景使用）
pub fn definitions_for<S: AsRef<str>>(names: &[S]) -> Value {
    let all = definitions();
    let Some(arr) = all.as_array() else {
        return json!([]);
    };
    Value::Array(
        arr.iter()
            .filter(|t| {
                t.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .map(|n| names.iter().any(|candidate| candidate.as_ref() == n))
                    .unwrap_or(false)
            })
            .cloned()
            .collect(),
    )
}

/// 原生视觉模型同样需要 read_image 来打开本地图片。
pub fn chat_definitions(_native_vision: bool) -> Value {
    definitions()
}

const RECALL_PAGE_CHARS: usize = 6_000;
const RECALL_SNIPPET_CHARS: usize = 180;

fn recall_needles(query: Option<&str>) -> Vec<String> {
    query
        .map(|value| {
            value
                .split_whitespace()
                .filter(|token| token.chars().count() >= 2)
                .take(8)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn snippet_around(text: &str, needles: &[String]) -> String {
    let lower = text.to_lowercase();
    let found = needles.iter().find_map(|needle| {
        let needle = needle.to_lowercase();
        lower
            .find(&needle)
            .map(|byte_at| text[..byte_at].chars().count())
    });
    let Some(char_at) = found else {
        return text_preview(text, RECALL_SNIPPET_CHARS);
    };
    let radius = RECALL_SNIPPET_CHARS / 2;
    let start = char_at.saturating_sub(radius);
    let chars: Vec<char> = text.chars().collect();
    let end = (start + RECALL_SNIPPET_CHARS).min(chars.len());
    let mut snippet: String = chars[start..end].iter().collect();
    if start > 0 {
        snippet.insert_str(0, "…");
    }
    if end < chars.len() {
        snippet.push('…');
    }
    snippet
}

fn text_page(text: &str, offset: usize) -> (String, usize, Option<usize>) {
    let chars: Vec<char> = text.chars().collect();
    let start = offset.min(chars.len());
    let end = (start + RECALL_PAGE_CHARS).min(chars.len());
    let page: String = chars[start..end].iter().collect();
    let next_offset = (end < chars.len()).then_some(end);
    (page, chars.len(), next_offset)
}

fn recall_current_session(db: &Db, session_id: i64, args: &Value) -> Result<Value> {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("search");
    let cover_until = db
        .get_session_compact(session_id)
        .ok()
        .flatten()
        .map(|(id, _)| id)
        .unwrap_or(0);
    match action {
        "get" => {
            let id = args
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow!("action=get 需要 message_id"))?;
            let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
            if let Some(msg) = db.get_session_message(session_id, id)? {
                return Ok(recall_message_detail(
                    db,
                    session_id,
                    &msg,
                    cover_until,
                    offset,
                )?);
            }
            if db
                .get_tool_call(id)?
                .is_some_and(|record| record.session_id == session_id)
            {
                bail!(
                    "id {id} 是工具调用 id，不是对话消息 id。工具参数和结果用 get_tool_call_history action=get id={id}；对话原文请用本工具 search 返回的 message_id。"
                );
            }
            bail!("当前会话没有 message_id={id} 的消息")
        }
        "search" => {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let before_id = args.get("before_id").and_then(Value::as_i64);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(8)
                .clamp(1, 20) as usize;
            let needles = recall_needles(query);
            let messages = db.search_session_messages(session_id, query, before_id, limit)?;
            let drafts = if query.is_some() {
                db.search_session_drafts(session_id, query, limit)?
            } else {
                Vec::new()
            };
            let next_before_id = (messages.len() == limit)
                .then(|| messages.first().map(|msg| msg.id))
                .flatten();
            let message_items: Vec<Value> = messages
                .iter()
                .map(|msg| {
                    json!({
                        "message_id": msg.id,
                        "role": msg.role,
                        "created_at": msg.created_at,
                        "covered": msg.id <= cover_until,
                        "chars": msg.content.chars().count(),
                        "snippet": snippet_around(&msg.content, &needles),
                    })
                })
                .collect();
            let draft_items: Vec<Value> = drafts
                .iter()
                .map(|draft| {
                    json!({
                        "message_id": draft.response_message_id,
                        "request_message_id": draft.request_message_id,
                        "tool_call_id": draft.tool_call_id,
                        "tool_name": draft.tool_name,
                        "covered": draft.request_message_id <= cover_until,
                        "chars": draft.assistant_content.chars().count(),
                        "snippet": snippet_around(&draft.assistant_content, &needles),
                        "note": "这是工具轮里的中间草稿，没有进入最终助手气泡。读取全文用 action=get，id 填 request_message_id。",
                    })
                })
                .collect();
            Ok(json!({
                "count": message_items.len(),
                "messages": message_items,
                "drafts": draft_items,
                "next_before_id": next_before_id,
                "cover_until_message_id": (cover_until > 0).then_some(cover_until),
                "note": "message_id 只能交给 recall_session 的 action=get。covered=true 表示这条原文已被检查点盖住，不在当前上下文里。不要把 message_id 传给 get_tool_call_history。",
            }))
        }
        other => bail!("未知 action: {other}"),
    }
}

fn recall_message_detail(
    db: &Db,
    session_id: i64,
    msg: &crate::db::ChatMsg,
    cover_until: i64,
    offset: usize,
) -> Result<Value> {
    let (content, chars, next_offset) = text_page(&msg.content, offset);
    let request_id = db.request_id_for_message(session_id, msg.id, &msg.role)?;
    let drafts = match request_id {
        Some(request_id) => db.drafts_for_request(session_id, request_id)?,
        None => Vec::new(),
    };
    let draft_items: Vec<Value> = drafts
        .into_iter()
        .map(|draft| {
            let (draft_text, draft_chars, draft_next) = text_page(&draft.assistant_content, 0);
            json!({
                "tool_call_id": draft.tool_call_id,
                "tool_name": draft.tool_name,
                "chars": draft_chars,
                "content": draft_text,
                "next_offset": draft_next,
                "note": "中间草稿，未进入最终助手气泡。",
            })
        })
        .collect();
    Ok(json!({
        "message_id": msg.id,
        "role": msg.role,
        "created_at": msg.created_at,
        "covered": msg.id <= cover_until,
        "offset": offset,
        "chars": chars,
        "content": content,
        "next_offset": next_offset,
        "request_message_id": request_id,
        "drafts": draft_items,
    }))
}

fn query_current_session_tool_history(db: &Db, session_id: i64, args: &Value) -> Result<Value> {
    if let Some(scope) = args.get("scope").and_then(Value::as_str) {
        match scope {
            "current_session" => {}
            "all_sessions" => bail!("只能查询当前这次会话内的工具调用记录"),
            other => bail!("未知 scope: {}", other),
        }
    }
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("search");
    match action {
        "get" => {
            let id = args
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow!("action=get 需要有效 id"))?;
            if let Some(record) = db
                .get_tool_call(id)?
                .filter(|record| record.session_id == session_id)
            {
                return Ok(tool_call_detail(&record));
            }
            if db.get_session_message(session_id, id)?.is_some() {
                bail!(
                    "未找到工具调用记录 {id}。id {id} 是对话消息 id，不是工具调用 id。读取这条原文请调用 recall_session，action=get，id={id}。"
                );
            }
            bail!("未找到工具调用记录 {id}。get 的 id 必须来自本工具 search 结果；对话原文请用 recall_session。")
        }
        "search" => {
            let tool_name = args
                .get("tool_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let status = args
                .get("status")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if status.is_some_and(|value| !matches!(value, "running" | "done" | "error")) {
                bail!("无效 status");
            }
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let before_id = args.get("before_id").and_then(Value::as_i64);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .clamp(1, 50) as usize;
            let include_history_queries = args
                .get("include_history_queries")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let records = db.query_tool_calls(
                Some(session_id),
                tool_name,
                status,
                query,
                before_id,
                limit,
                include_history_queries,
            )?;
            let next_before_id = (records.len() == limit)
                .then(|| records.first().map(|record| record.id))
                .flatten();
            let items: Vec<Value> = records.iter().map(tool_call_search_item).collect();
            Ok(json!({
                "count": items.len(),
                "calls": items,
                "next_before_id": next_before_id,
                "note": "search 返回截断预览；需要某次工具调用的完整参数和结果时，用 action=get + 结果里的工具调用 id。最终回复若被截断，用 recall_session action=get + response_message.id 读取全文，不要把 message_id 传给本工具。",
            }))
        }
        other => bail!("未知 action: {}", other),
    }
}

/// 工具执行入口：插件钩子（before_tool / after_tool）包在外层，内部按执行器分派。
pub async fn execute(
    app: &AppHandle,
    name: &str,
    args: &Value,
    cancel: &tokio_util::sync::CancellationToken,
    session_id: i64,
) -> Result<Value> {
    if let Some(message) = crate::plugins::snapshot().retired_message(name) {
        bail!("{message}");
    }
    if !crate::plugins::hooks::any_hooks() {
        return execute_inner(app, name, args, cancel, session_id).await;
    }
    let args = match crate::plugins::hooks::before_tool(app, name, args, session_id).await {
        crate::plugins::hooks::Before::Allow(args) => args,
        crate::plugins::hooks::Before::Block(reason) => bail!("{reason}"),
    };
    let result = Box::pin(execute_inner(app, name, &args, cancel, session_id)).await?;
    Ok(crate::plugins::hooks::after_tool(app, name, &args, result, session_id).await)
}

async fn execute_inner(
    app: &AppHandle,
    name: &str,
    args: &Value,
    cancel: &tokio_util::sync::CancellationToken,
    session_id: i64,
) -> Result<Value> {
    let state = app.state::<crate::AppState>();
    let db = &state.db;
    if let Some(entry) = db.tool_registry_entry(name)? {
        if entry.executor_kind == "browser_action" {
            let action = entry
                .executor_config
                .get("action")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("动态浏览器工具 {name} 缺少 action 映射"))?;
            let mut params = args.clone();
            if entry
                .executor_config
                .get("extension_only")
                .and_then(Value::as_bool)
                .unwrap_or(true)
            {
                params["channel"] = json!("extension");
            }
            return state.browser.call(action, params).await;
        }
        if entry.executor_kind != "builtin" {
            bail!("工具 {name} 的执行器 {} 尚未受支持", entry.executor_kind);
        }
    }
    let spec = crate::plugins::snapshot().tool(name).cloned();
    if let Some(spec) = &spec {
        // 插件执行器（alias / command / http / mcp / script / browser_action）。
        if let Some(result) = crate::plugins::executors::execute(app, spec, args, cancel, session_id).await {
            return result;
        }
        if spec.plugin == "servers" {
            return crate::servers::dispatch(app, name, args).await;
        }
    }
    if crate::plugins::authoring::TOOL_NAMES.contains(&name) {
        return crate::plugins::authoring::execute(app, name, args, cancel).await;
    }
    if crate::workspaces::TOOL_NAMES.contains(&name) {
        return crate::workspaces::execute(app, name, args, cancel, session_id).await;
    }
    if crate::agents::TOOL_NAMES.contains(&name) {
        return crate::agents::execute(app, name, args, cancel, session_id).await;
    }
    if crate::office::TOOL_NAMES.contains(&name) {
        return crate::office::execute(app, name, args).await;
    }
    if crate::desktop::TOOL_NAMES.contains(&name) {
        return crate::desktop::execute(app, name, args).await;
    }
    if crate::recorder::screenshot::TOOL_NAMES.contains(&name) {
        return crate::recorder::screenshot::execute(app, name, args).await;
    }
    if crate::recorder::TOOL_NAMES.contains(&name) {
        return crate::recorder::execute(app, name, args, cancel).await;
    }
    match name {
        "discover_capabilities" => {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|query| !query.is_empty())
                .ok_or_else(|| anyhow!("请具体描述要完成的目标"))?;
            let categories = string_array(args, "categories");
            if categories.is_empty() {
                bail!("至少选择一个能力类别");
            }
            let invalid: Vec<_> = categories
                .iter()
                .filter(|category| tools_for_category(category).is_empty())
                .cloned()
                .collect();
            if !invalid.is_empty() {
                bail!("未知能力类别: {}", invalid.join(", "));
            }
            let file_scan_intent = detect_bulk_file_scan(query);
            let mut effective_categories = categories.clone();
            if file_scan_intent.is_some()
                && !effective_categories
                    .iter()
                    .any(|category| category == "files")
            {
                effective_categories.push("files".to_string());
            }
            let mut activated_tools = activate_categories(&effective_categories);
            if file_scan_intent
                .as_ref()
                .is_some_and(|intent| !intent.direct_scan_override)
            {
                prioritize_index_tools(&mut activated_tools);
            }
            let matched_skills = rank_skills(query, state.skills.list(), 5);
            let has_matched_skills = !matched_skills.is_empty();
            let file_scan_policy = file_scan_intent
                .as_ref()
                .map(|intent| build_file_scan_policy(&state.file_index, intent))
                .transpose()?;
            let next = if has_matched_skills {
                let names: Vec<&str> = matched_skills
                    .iter()
                    .filter_map(|item| item.get("name").and_then(Value::as_str))
                    .collect();
                let skill_hint = format!(
                    "可能相关的 Skills：{}。请判断是否真正命中当前目标，命中则先 load_skill。",
                    names.join("、")
                );
                match file_scan_policy
                    .as_ref()
                    .and_then(|policy| policy.get("next").and_then(Value::as_str))
                {
                    Some(file_next) => format!("{} {}", skill_hint, file_next),
                    None => skill_hint,
                }
            } else if let Some(policy) = file_scan_policy.as_ref() {
                policy
                    .get("next")
                    .and_then(Value::as_str)
                    .unwrap_or("批量文件任务优先使用 search_files。")
                    .to_string()
            } else {
                "相关工具已可用。请选择满足目标的最小工具组合；若仍不足，可再次发现其它类别或用 commands 查看本地帮助并组合实现。".to_string()
            };
            Ok(json!({
                "query": query,
                "requested_categories": categories,
                "activated_categories": effective_categories,
                "activated_tools": activated_tools,
                "matched_skills": matched_skills,
                "file_scan_policy": file_scan_policy,
                "next": next,
            }))
        }
        "get_tool_call_history" => query_current_session_tool_history(db, session_id, args),
        "recall_session" => recall_current_session(db, session_id, args),
        "scan_desktop" => {
            let recursive = args
                .get("recursive")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let depth = args
                .get("depth")
                .and_then(|v| v.as_u64())
                .map(|d| (d as usize).clamp(1, 6))
                .unwrap_or(if recursive { 3 } else { 1 });
            let path_arg = args
                .get("path")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let (root, skip) = match path_arg {
                Some(p) => {
                    let cand = Path::new(p);
                    let full = if cand.is_absolute() {
                        cand.to_path_buf()
                    } else {
                        scanner::desktop_dir()?.join(cand)
                    };
                    (full, None)
                }
                None => {
                    let skip = crate::commands::organize_root_skip(db);
                    (scanner::desktop_dir()?, skip)
                }
            };
            let items = scanner::scan_path(&root, depth, 300, skip)?;
            Ok(json!({
                "root": root.to_string_lossy().replace('\\', "/"),
                "depth": depth,
                "count": items.len(),
                "items": items,
            }))
        }
        "search_files" => {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("search");
            match action {
                "status" => {
                    let roots = string_array(args, "roots");
                    let coverage = state.file_index.coverage(&roots, false)?;
                    let estimated_size_coverage =
                        state.file_index.coverage_for_estimated_sizes(&roots)?;
                    let exact_coverage = state.file_index.coverage(&roots, true)?;
                    let needs_index = !coverage.ready;
                    let needs_estimated_size_index = !estimated_size_coverage.ready;
                    let needs_exact_index = !exact_coverage.ready;
                    let indexed_roots = coverage.indexed_roots.clone();
                    Ok(json!({
                        "ok": true,
                        "status": state.file_index.status(),
                        "indexed_roots": indexed_roots,
                        "coverage": coverage,
                        "estimated_size_coverage": estimated_size_coverage,
                        "exact_coverage": exact_coverage,
                        "needs_index": needs_index,
                        "needs_estimated_size_index": needs_estimated_size_index,
                        "needs_exact_index": needs_exact_index,
                        "message": if !needs_index {
                            "目标范围已有名称索引。空间初步分析至少需要 metadata_level=estimated；精确大小/时间需要 metadata_level=full。"
                        } else {
                            "目标范围尚未维护索引。整盘空间初步分析优先在用户同意 UAC 后调用 ntfs_index 建立 MFT 估算索引；只有精确分析才在用户同意逐项读取元数据后调用 index。"
                        }
                    }))
                }
                "stop" => Ok(json!({
                    "stop_requested": state.file_index.stop(),
                    "status": state.file_index.status(),
                })),
                "ntfs_probe" => {
                    let volume = args
                        .get("volume")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| anyhow!("ntfs_probe 需要从当前目标解析出的 volume"))?;
                    if crate::ntfs_usn::volume_for_path(Path::new(volume)).as_deref()
                        != Some(volume)
                    {
                        bail!("volume 必须是从当前目标解析出的规范盘符根路径，格式为“盘符:/”");
                    }
                    if args.get("user_confirmed").and_then(Value::as_bool) != Some(true) {
                        return Ok(ntfs_confirmation_required(volume, "诊断 NTFS 快速索引能力"));
                    }
                    Ok(serde_json::to_value(
                        crate::ntfs_helper::probe_elevated(app, volume).await?,
                    )?)
                }
                "ntfs_sync" => {
                    let volume = args
                        .get("volume")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| anyhow!("ntfs_sync 需要从当前目标解析出的 volume"))?;
                    if crate::ntfs_usn::volume_for_path(Path::new(volume)).as_deref()
                        != Some(volume)
                    {
                        bail!("volume 必须是从当前目标解析出的规范盘符根路径，格式为“盘符:/”");
                    }
                    if args.get("user_confirmed").and_then(Value::as_bool) != Some(true) {
                        return Ok(ntfs_confirmation_required(volume, "同步 NTFS 索引变化"));
                    }
                    Ok(serde_json::to_value(
                        state.file_index.sync_usn_elevated(app, volume).await?,
                    )?)
                }
                "ntfs_index" => {
                    let volume = args
                        .get("volume")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| anyhow!("ntfs_index 需要从当前目标解析出的 volume"))?;
                    if crate::ntfs_usn::volume_for_path(Path::new(volume)).as_deref()
                        != Some(volume)
                    {
                        bail!("volume 必须是从当前目标解析出的规范盘符根路径，格式为“盘符:/”");
                    }
                    if args.get("user_confirmed").and_then(Value::as_bool) != Some(true) {
                        return Ok(ntfs_confirmation_required(volume, "快速建立 MFT 空间估算索引"));
                    }
                    Ok(serde_json::to_value(
                        state
                            .file_index
                            .rebuild_ntfs_elevated(app, volume)
                            .await?,
                    )?)
                }
                "index" => {
                    let raw_roots = string_array(args, "roots");
                    if raw_roots.is_empty() {
                        bail!("index 操作需要 roots，建议只索引任务所需目录；索引整个磁盘可能耗时较长");
                    }
                    if args.get("user_confirmed").and_then(Value::as_bool) != Some(true) {
                        return Ok(json!({
                            "ok": false,
                            "status": "confirmation_required",
                            "confirmation_required": true,
                            "roots": raw_roots,
                            "message": "开始维护索引前需要用户明确同意。请说明：首次建立会后台遍历所选目录，之后持续维护文件名、路径、大小和修改时间等元数据，不读取文件内容。"
                        }));
                    }
                    let desktop = scanner::desktop_dir()?;
                    let roots = raw_roots
                        .into_iter()
                        .map(|raw| {
                            let p = Path::new(&raw);
                            if p.is_absolute() {
                                p.to_path_buf()
                            } else {
                                desktop.join(p)
                            }
                        })
                        .collect();
                    Ok(json!({
                        "ok": true,
                        "message": "文件索引已在后台启动；请稍后用 action=status 查看进度，running=false 后即可搜索",
                        "status": state.file_index.start(roots)?,
                    }))
                }
                "summarize" => {
                    let root = args
                        .get("root")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|root| !root.is_empty())
                        .ok_or_else(|| anyhow!("summarize 需要用户指定或当前任务解析出的 root"))?;
                    let require_exact = matches!(
                        args.get("accuracy").and_then(Value::as_str),
                        Some("exact")
                    );
                    let coverage = if require_exact {
                        state.file_index.coverage(&[root.to_string()], true)?
                    } else {
                        state
                            .file_index
                            .coverage_for_estimated_sizes(&[root.to_string()])?
                    };
                    if !coverage.ready {
                        return Ok(if require_exact {
                            index_required_result(
                                coverage,
                                true,
                                "精确空间汇总需要普通逐项扫描形成的完整元数据索引",
                            )
                        } else {
                            fast_size_index_required_result(coverage, root)
                        });
                    }
                    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
                    Ok(json!({
                        "ok": true,
                        "summary": state.file_index.summarize_usage(root, limit, require_exact)?,
                    }))
                }
                "search" => {
                    let roots = string_array(args, "roots");
                    let require_full_metadata = args.get("min_size_mb").is_some()
                        || args.get("max_size_mb").is_some()
                        || args.get("modified_after").is_some()
                        || args.get("modified_before").is_some()
                        || matches!(
                            args.get("sort").and_then(Value::as_str),
                            Some("size_asc" | "size_desc" | "modified_asc" | "modified_desc")
                        );
                    let coverage = state
                        .file_index
                        .coverage(&roots, require_full_metadata)?;
                    if !coverage.ready {
                        return Ok(index_required_result(
                            coverage,
                            require_full_metadata,
                            "目标范围尚无可用于本次批量搜索的索引",
                        ));
                    }
                    let mb = 1024.0 * 1024.0;
                    let min_size_bytes = args
                        .get("min_size_mb")
                        .and_then(|v| v.as_f64())
                        .filter(|v| *v >= 0.0)
                        .map(|v| (v * mb) as u64);
                    let max_size_bytes = args
                        .get("max_size_mb")
                        .and_then(|v| v.as_f64())
                        .filter(|v| *v >= 0.0)
                        .map(|v| (v * mb) as u64);
                    let query = crate::file_index::SearchQuery {
                        roots,
                        text: args
                            .get("query")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                        extensions: string_array(args, "extensions"),
                        kind: args
                            .get("kind")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        min_size_bytes,
                        max_size_bytes,
                        modified_after: optional_local_timestamp(args, "modified_after")?,
                        modified_before: optional_local_timestamp(args, "modified_before")?,
                        sort: args
                            .get("sort")
                            .and_then(|v| v.as_str())
                            .unwrap_or("name_asc")
                            .to_string(),
                        limit: args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize,
                    };
                    Ok(serde_json::to_value(state.file_index.search(query)?)?)
                }
                other => bail!(
                    "未知 action: {}（可选 search/summarize/index/status/stop/ntfs_probe/ntfs_sync/ntfs_index）",
                    other
                ),
            }
        }
        "read_file" => {
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let path = crate::reader::resolve_path(app, raw)?;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let max_chars = args
                .get("max_chars")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            let res = crate::reader::read_file(&path, offset, max_chars, full)?;
            Ok(serde_json::to_value(&res)?)
        }
        "get_file_info" => {
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let path = crate::reader::resolve_path(app, raw)?;
            Ok(serde_json::to_value(&crate::reader::file_info(&path)?)?)
        }
        "create_file" => {
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 content"))?;
            let overwrite = args
                .get("overwrite")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            crate::writer::create_file(app, raw, content, overwrite)
        }
        "clear_temp_files" => {
            let before = crate::tempfs::clear(app)?;
            Ok(json!({
                "ok": true,
                "path": before.path,
                "removed_files": before.file_count,
                "removed_dirs": before.dir_count,
                "freed_bytes": before.total_bytes,
            }))
        }
        "edit_file" => {
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let mode = args
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("replace");
            crate::writer::edit_file(
                app,
                raw,
                mode,
                args.get("old_text").and_then(|v| v.as_str()),
                args.get("new_text").and_then(|v| v.as_str()),
                args.get("content").and_then(|v| v.as_str()),
                args.get("all").and_then(|v| v.as_bool()).unwrap_or(false),
            )
        }
        "read_image" => {
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let path = crate::reader::resolve_path(app, raw)?;
            let question = args.get("question").and_then(|v| v.as_str());
            let settings = crate::commands::load_settings(db);
            if !settings.read_image_enabled {
                bail!("图片读取工具已关闭，请在设置中启用。");
            }
            if settings.model_supports_vision {
                crate::llm::vision::encode_image_data_uri(&path, crate::llm::vision::MAX_NATIVE_IMAGE_BYTES)?;
                return Ok(json!({
                    "path": path.to_string_lossy().replace('\\', "/"),
                    "native_image": true,
                    "question": question,
                    "result": "图片已交给当前聊天模型，请直接查看并回答问题。"
                }));
            }
            if settings.vision_api_key.trim().is_empty() {
                bail!("尚未配置视觉模型 API Key。纯提取文字可改用 ocr_image（本地免费）；要看图理解请在「设置 → 图像识别」填写视觉 Key。");
            }
            let http = reqwest::Client::new();
            let recognized = crate::llm::vision::recognize_image(
                &http,
                &settings.vision_base_url,
                &settings.vision_api_key,
                &settings.vision_model,
                &path,
                question,
            )
            .await?;
            crate::llm::persist_usage(app, "vision", &settings.vision_model, &recognized.usage);
            Ok(json!({
                "path": path.to_string_lossy().replace('\\', "/"),
                "model": settings.vision_model,
                "result": recognized.content,
            }))
        }
        "ocr_image" => {
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let path = crate::reader::resolve_path(app, raw)?;
            let state = app.state::<crate::AppState>();
            // ONNX 推理为同步阻塞；在异步工具路径里用 block_in_place 避免卡住整 runtime
            let result = tokio::task::block_in_place(|| state.ocr.recognize(&path))?;
            Ok(serde_json::to_value(&result)?)
        }
        "find_browser_recipes" => {
            let url = args
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("缺少 url"))?;
            let goal = args.get("goal").and_then(Value::as_str).unwrap_or("");
            let recipes = db.browser_recipe_find(url, goal, 8)?;
            Ok(
                json!({ "count": recipes.len(), "recipes": recipes, "note": "配方只作加速参考；运行前仍会预检当前 URL 与唯一候选。" }),
            )
        }
        "save_browser_recipe" => save_browser_recipe(app, db, args),
        "run_browser_recipe" => run_browser_recipe(app, db, args).await,
        "browser_fast_loop" => crate::system_one::fast_browser_loop(app, args).await,
        n if crate::browser::operations::TOOL_NAMES.contains(&n) => crate::browser::operations::execute(app, n, args, cancel).await,
        n if n.starts_with("browser_") => browser_tool(app, n, args).await,
        "propose_organization" => propose_organization(app, db, args),
        "add_todo" => {
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 title"))?;
            let due = args
                .get("due_at")
                .and_then(|v| v.as_str())
                .and_then(crate::commands::normalize_due);
            let todo = db.insert_todo(
                title.trim(),
                args.get("note").and_then(|v| v.as_str()).unwrap_or(""),
                due.as_deref(),
                args.get("repeat_rule")
                    .and_then(|v| v.as_str())
                    .unwrap_or("none"),
                args.get("priority").and_then(|v| v.as_i64()).unwrap_or(1),
                &crate::commands::normalize_remind_mode(
                    args.get("remind_mode").and_then(|v| v.as_str()),
                ),
            )?;
            let _ = app.emit("todos-changed", ());
            Ok(json!({ "ok": true, "todo": todo }))
        }
        "list_todos" => {
            let filter = args
                .get("filter")
                .and_then(|v| v.as_str())
                .unwrap_or("pending");
            let todos = db.list_todos(filter)?;
            Ok(json!({ "count": todos.len(), "todos": todos }))
        }
        "complete_todo" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("缺少 id"))?;
            db.set_todo_done(id, true)?;
            let _ = app.emit("todos-changed", ());
            Ok(json!({ "ok": true }))
        }
        "snooze_todo" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("缺少 id"))?;
            let minutes = args.get("minutes").and_then(|v| v.as_i64()).unwrap_or(10);
            let new_due = (chrono::Local::now() + chrono::Duration::minutes(minutes))
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();
            db.snooze(id, &new_due)?;
            let _ = app.emit("todos-changed", ());
            Ok(json!({ "ok": true, "new_due": new_due }))
        }
        "get_operation_history" => {
            let batches = db.list_batches()?;
            Ok(json!({ "count": batches.len(), "batches": batches }))
        }
        "undo_batch" => {
            let batch_id = args
                .get("batch_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 batch_id"))?;
            let count = executor::undo_batch(db, batch_id)?;
            let _ = app.emit("history-changed", ());
            Ok(json!({ "ok": true, "restored": count }))
        }
        "create_rule" => {
            let name_s = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 name"))?;
            let match_type = args
                .get("match_type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 match_type"))?;
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 pattern"))?;
            let target_name = args
                .get("target_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 target_name"))?;
            if !["ext", "keyword", "regex"].contains(&match_type) {
                bail!("match_type 必须是 ext / keyword / regex");
            }
            if match_type == "regex" {
                regex::Regex::new(pattern).map_err(|e| anyhow!("正则无效: {}", e))?;
            }
            let target =
                crate::commands::resolve_target_folder(db, target_name).map_err(|e| anyhow!(e))?;
            let id = db.upsert_rule(None, name_s.trim(), match_type, pattern.trim(), &target)?;
            let _ = app.emit("rules-changed", ());
            Ok(json!({ "ok": true, "rule_id": id, "target_folder": target }))
        }
        "toggle_rule" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("缺少 id"))?;
            let enabled = args
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            db.toggle_rule(id, enabled)?;
            let _ = app.emit("rules-changed", ());
            Ok(json!({ "ok": true }))
        }
        "execute_plan" => Ok(json!({
            "message": "执行整理需要用户在聊天面板的方案卡片上点击「确认执行」，请提示用户操作。"
        })),
        "list_profile" => {
            let settings = crate::commands::load_settings(db);
            let entries = db.pf_list()?;
            Ok(json!({
                "fixed": {
                    "真实姓名": settings.profile_name,
                    "自媒体号名称": settings.profile_alias,
                    "性别": settings.profile_gender,
                    "出生年月": settings.profile_birth,
                    "手机": settings.profile_phone,
                    "邮箱": settings.profile_email,
                    "所在城市": settings.profile_city,
                },
                "entries": entries,
                "hint": "固定字段为空时引导用户去「设置 → 个人信息」填写；自由条目可由你通过 save_profile_entry 维护。对外默认用自媒体号名称，真实姓名仅招聘等实名场景使用。",
            }))
        }
        "save_profile_entry" => {
            let label = args
                .get("label")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("缺少 label"))?;
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("缺少 content"))?;
            let content: String = content.chars().take(500).collect();
            let id = db.pf_upsert(label, &content)?;
            let _ = app.emit("profile-changed", ());
            Ok(json!({
                "ok": true,
                "id": id,
                "message": format!("个人信息条目「{}」已保存，后续需要时会自动加载", label),
            }))
        }
        "delete_profile_entry" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("缺少 id"))?;
            db.pf_delete(id)?;
            let _ = app.emit("profile-changed", ());
            Ok(json!({ "ok": true, "deleted_id": id }))
        }
        "run_subagent" => crate::llm::subagent::execute_in_session(app, args, cancel, session_id).await,
        "await_subagent" => crate::llm::subagent::await_job(app, args, cancel).await,
        "reply_subagent" => crate::llm::subagent::reply_job(app, args).await,
        "web_search" | "web_fetch" => crate::web::execute(name, args, cancel).await,
        n @ ("run_command"
        | "run_command_background"
        | "await_task"
        | "check_task"
        | "list_tasks"
        | "stop_task") => {
            if !crate::commands::load_settings(db).command_tools_enabled {
                bail!(
                    "命令执行功能未启用。请用户在主窗口「设置 → 后台任务与命令执行」中打开开关。"
                );
            }
            // 会话绑定了工作区时，未给 workdir 的命令默认在工作区根目录执行。
            let mut args = args.clone();
            if matches!(n, "run_command" | "run_command_background")
                && args.get("workdir").and_then(Value::as_str).map_or(true, |w| w.trim().is_empty())
                && args.get("files").is_none()
            {
                if let Some(ws) = crate::workspaces::current(db, session_id) {
                    if Path::new(&ws.root).is_dir() {
                        args["workdir"] = json!(ws.root);
                    }
                }
            }
            command_tool(app, n, &args, cancel, session_id).await
        }
        "get_system_info" => {
            let category = args
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("overview");
            let sort_by = args
                .get("sort_by")
                .and_then(|v| v.as_str())
                .unwrap_or("memory");
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(15)
                .clamp(1, 50) as usize;
            crate::machine::query(category, sort_by, limit).await
        }
        "list_workflows" => {
            let list = db.automation_workflow_list()?;
            Ok(json!({ "count": list.len(), "workflows": list }))
        }
        "create_workflow" => {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let prompt = args
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let schedule_rule = args
                .get("schedule_rule")
                .and_then(Value::as_str)
                .unwrap_or("manual");
            if name.is_empty() || prompt.is_empty() {
                bail!("name 和 prompt 不能为空");
            }
            if !["manual", "once", "daily", "weekly"].contains(&schedule_rule) {
                bail!("schedule_rule 无效");
            }
            let next_run_at = args
                .get("next_run_at")
                .and_then(Value::as_str)
                .map(str::to_string);
            if schedule_rule != "manual" && next_run_at.as_deref().unwrap_or("").is_empty() {
                bail!("定时工作流必须提供 next_run_at");
            }
            let workflow = db.automation_workflow_save(&crate::db::AutomationWorkflow {
                id: args.get("id").and_then(Value::as_i64).unwrap_or(0),
                name: name.into(),
                description: args
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                prompt: prompt.into(),
                schedule_rule: schedule_rule.into(),
                next_run_at,
                enabled: args.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                run_count: 0,
                last_run_at: None,
                created_at: String::new(),
                updated_at: String::new(),
            })?;
            let _ = app.emit("automation-workflows-changed", ());
            Ok(json!({ "ok": true, "workflow": workflow }))
        }
        "manage_workflow" => {
            let id = args
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow!("缺少 id"))?;
            match args.get("action").and_then(Value::as_str).unwrap_or("") {
                "run" => Ok(
                    json!({ "message": "请用户在工作流页点击「运行」，或在聊天中发送要执行的流程名称；这样能确保当前聊天未忙。", "workflow_id": id }),
                ),
                "delete" => {
                    db.automation_workflow_delete(id)?;
                    let _ = app.emit("automation-workflows-changed", ());
                    Ok(json!({"ok": true, "deleted_id": id}))
                }
                "enable" | "disable" => {
                    let mut workflow = db
                        .automation_workflow_get(id)?
                        .ok_or_else(|| anyhow!("工作流不存在"))?;
                    workflow.enabled = args.get("action").and_then(Value::as_str) == Some("enable");
                    let workflow = db.automation_workflow_save(&workflow)?;
                    let _ = app.emit("automation-workflows-changed", ());
                    Ok(json!({"ok": true, "workflow": workflow}))
                }
                _ => bail!("未知 action"),
            }
        }
        "lookup_cache" => lookup_cache_tool(app, args),
        "list_skills" => {
            let skills = app.state::<crate::AppState>().skills.list();
            Ok(json!({ "count": skills.len(), "skills": skills }))
        }
        "load_skill" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("缺少 name"))?;
            let content = app.state::<crate::AppState>().skills.load(name)?;
            Ok(json!({
                "name": name,
                "content": crate::skills::wrap_skill_content(name, &content),
                "note": "以上是该技能的完整说明。结合当前用户目标、环境证据和风险使用；保留仍适用的稳定约束，情境不符时调整方法并验证。对话里已有这段 <skill_content> 后不要再 load_skill。",
            }))
        }
        "ask_user" => crate::ask_user::execute(app, args, cancel).await,
        "todo_write" => crate::session_todo::execute(app, args),
        "create_skill" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 name"))?;
            let description = args
                .get("description")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 description"))?;
            let body = args
                .get("body")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 body"))?;
            let info = app
                .state::<crate::AppState>()
                .skills
                .create(name, description, body)?;
            let _ = app.emit("skills-changed", ());
            Ok(json!({ "ok": true, "skill": info }))
        }
        "skill_resource" => {
            let action = args
                .get("action")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("缺少 action"))?;
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("缺少 name"))?;
            let store = &app.state::<crate::AppState>().skills;
            match action {
                "list" => {
                    let resources = store.list_resources(name)?;
                    Ok(json!({ "name": name, "count": resources.len(), "resources": resources }))
                }
                "read" => {
                    let path = args
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("read 需要 path"))?;
                    Ok(
                        json!({ "name": name, "path": path, "content": store.read_resource(name, path)? }),
                    )
                }
                "write" => {
                    let path = args
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("write 需要 path"))?;
                    let content = args
                        .get("content")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("write 需要 content"))?;
                    let result = store.write_resource(name, path, content)?;
                    let _ = app.emit("skills-changed", ());
                    Ok(json!({ "ok": true, "name": name, "resource": result }))
                }
                other => bail!("未知 action: {}（list/read/write）", other),
            }
        }
        "run_skill_program" => {
            if !crate::commands::load_settings(db).command_tools_enabled {
                bail!(
                    "命令执行功能未启用。请用户在主窗口「设置 → 后台任务与命令执行」中打开开关。"
                );
            }
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("缺少 name"))?;
            let entry = args
                .get("entry")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("缺少 entry"))?;
            let runtime = args
                .get("runtime")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("缺少 runtime"))?;
            let extra_args: Vec<String> = args
                .get("args")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            item.as_str()
                                .map(str::to_string)
                                .ok_or_else(|| anyhow!("args 必须是字符串数组"))
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();
            let (root, program) = app
                .state::<crate::AppState>()
                .skills
                .executable_resource(name, entry)?;
            let ext = program
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let program_text = program.to_string_lossy().to_string();
            let mut argv: Vec<String> = match runtime {
                "python" if ext == "py" => vec!["python".into(), program_text],
                "node" if ["js", "mjs", "cjs"].contains(&ext.as_str()) => {
                    vec!["node".into(), program_text]
                }
                "powershell" if ext == "ps1" => vec![
                    "powershell".into(),
                    "-NoProfile".into(),
                    "-File".into(),
                    program_text,
                ],
                "direct" if ["exe", "cmd", "bat", "com"].contains(&ext.as_str()) => {
                    vec![program_text]
                }
                "python" | "node" | "powershell" | "direct" => {
                    bail!("runtime 与入口文件扩展名不匹配")
                }
                other => bail!("未知 runtime: {}", other),
            };
            argv.extend(extra_args);
            let mut run_args = json!({
                "argv": argv,
                "workdir": root.to_string_lossy(),
                "timeout_secs": args.get("timeout_secs").and_then(Value::as_u64).unwrap_or(60).clamp(1, 300),
                "label": format!("Skill {} · {}", name, entry),
            });
            if let Some(stdin) = args.get("stdin").and_then(Value::as_str) {
                run_args["stdin"] = json!(stdin);
            }
            command_tool(app, "run_command", &run_args, cancel, session_id).await
        }
        "delete_skill" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 name"))?;
            app.state::<crate::AppState>().skills.delete(name)?;
            let _ = app.emit("skills-changed", ());
            Ok(json!({ "ok": true, "deleted": name }))
        }
        "manage_skill" => {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 action"))?;
            match action {
                "enable" | "disable" => {
                    let name = args
                        .get("name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow!("enable/disable 需要 name"))?;
                    let info = app
                        .state::<crate::AppState>()
                        .skills
                        .set_enabled(name, action == "enable")?;
                    let _ = app.emit("skills-changed", ());
                    Ok(json!({ "ok": true, "skill": info }))
                }
                "scan" => {
                    let source = args.get("source").and_then(|v| v.as_str());
                    let mut list = app.state::<crate::AppState>().skills.scan_external();
                    if let Some(sf) = source.map(str::trim).filter(|s| !s.is_empty()) {
                        list.retain(|e| e.source == sf);
                    }
                    Ok(json!({
                        "count": list.len(),
                        "external": list,
                        "note": "用 manage_skill(action=sync, source=..., names=[...]) 导入；overwrite=true 可覆盖本地同名。",
                    }))
                }
                "sync" => {
                    let source = args.get("source").and_then(|v| v.as_str());
                    let names: Option<Vec<String>> = args.get("names").and_then(|v| {
                        v.as_array().map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                    });
                    let overwrite = args
                        .get("overwrite")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let result = app.state::<crate::AppState>().skills.sync_from(
                        source,
                        names.as_deref(),
                        overwrite,
                    )?;
                    let _ = app.emit("skills-changed", ());
                    Ok(result)
                }
                other => bail!("未知 action: {}（enable/disable/scan/sync）", other),
            }
        }
        other => bail!("未知工具: {}", other),
    }
}

fn lookup_cache_tool(app: &AppHandle, args: &Value) -> Result<Value> {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("缺少 action"))?;
    let cache = &app.state::<crate::AppState>().lookup_cache;
    match action {
        "get" => {
            let key = args
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("get 需要 key"))?;
            cache.get(key)
        }
        "put" => {
            let key = args
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("put 需要 key"))?;
            let value = args
                .get("value")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("put 需要 value"))?;
            let source = args.get("source").and_then(Value::as_str);
            let summary = args.get("summary").and_then(Value::as_str);
            let ttl_secs = args.get("ttl_secs").and_then(Value::as_i64);
            cache.put(key, value, source, summary, ttl_secs)
        }
        "list" => {
            let source = args.get("source").and_then(Value::as_str);
            cache.list(source)
        }
        "delete" => {
            let key = args
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("delete 需要 key"))?;
            cache.delete(key)
        }
        other => bail!("未知 action: {}，可用 get/put/list/delete", other),
    }
}

fn string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_argv(args: &Value) -> Result<Vec<String>> {
    match args.get("argv") {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Some(text) = item.as_str() else {
                    bail!(
                        "argv 必须是字符串数组，例如 [\"git\", \"status\"]，不要写成一条命令字符串"
                    );
                };
                out.push(text.to_string());
            }
            Ok(out)
        }
        Some(Value::String(_)) => {
            bail!("argv 必须是字符串数组，例如 [\"git\", \"status\"]，不要把整条命令写成一个字符串")
        }
        _ => bail!("argv 必须是字符串数组"),
    }
}

fn integer_array(args: &Value, key: &str) -> Result<Vec<i32>> {
    let Some(value) = args.get(key) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| anyhow!("{} 必须是整数数组", key))?;
    values
        .iter()
        .map(|value| {
            value
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| anyhow!("{} 包含无效退出码", key))
        })
        .collect()
}

fn string_map(args: &Value, key: &str) -> Result<std::collections::HashMap<String, String>> {
    let Some(value) = args.get(key) else {
        return Ok(Default::default());
    };
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("{} 必须是字符串键值对象", key))?;
    if object.len() > 64 {
        bail!("{} 最多包含 64 项", key);
    }
    object
        .iter()
        .map(|(name, value)| {
            if name.is_empty()
                || name.contains('=')
                || name.contains('\0')
                || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                bail!("{} 包含无效环境变量名: {}", key, name);
            }
            let value = value
                .as_str()
                .ok_or_else(|| anyhow!("{} 的值必须都是字符串", key))?;
            if value.contains('\0') {
                bail!("环境变量 {} 的值包含 NUL 字符", name);
            }
            Ok((name.clone(), value.to_string()))
        })
        .collect()
}

/// Skill 描述本身承担触发语义。这里用连续片段重合做轻量本地召回，
/// 不把完整 Skill 目录永久塞进每一轮上下文。
fn rank_skills(query: &str, skills: Vec<crate::skills::SkillInfo>, limit: usize) -> Vec<Value> {
    let query = normalize_match_text(query);
    let query_chars: Vec<char> = query.chars().collect();
    let mut ranked: Vec<(usize, crate::skills::SkillInfo)> = skills
        .into_iter()
        .filter(|skill| skill.enabled)
        .filter_map(|skill| {
            let candidate = normalize_match_text(&format!("{}{}", skill.name, skill.description));
            let mut score = 0usize;
            for width in 2..=6.min(query_chars.len()) {
                for window in query_chars.windows(width) {
                    let fragment: String = window.iter().collect();
                    if candidate.contains(&fragment) {
                        score += width * width;
                    }
                }
            }
            (score > 0).then_some((score, skill))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    ranked
        .into_iter()
        .take(limit)
        .map(|(score, skill)| {
            json!({
                "name": skill.name,
                "description": skill.description,
                "scope": skill.scope,
                "relevance": score,
            })
        })
        .collect()
}

fn normalize_match_text(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(c))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BulkFileScanIntent {
    target_roots: Vec<String>,
    requires_size_estimates: bool,
    requires_full_metadata: bool,
    direct_scan_override: bool,
}

/// Identify work whose cost grows with an entire drive, many directories, or
/// many files. Capability discovery uses this to make the persistent index the
/// default path instead of merely presenting it beside arbitrary shell tools.
fn detect_bulk_file_scan(query: &str) -> Option<BulkFileScanIntent> {
    let lower = query.to_lowercase();
    let scan_context = [
        "扫描",
        "搜索",
        "查找",
        "筛选",
        "盘点",
        "统计",
        "分析",
        "看看",
        "列出",
        "整理",
        "清理",
        "占用",
        "空间",
        "容量",
        "大文件",
        "重复文件",
        "文件",
        "目录",
        "文件夹",
        "search",
        "scan",
        "find",
        "disk usage",
        "large file",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    let broad_scope = [
        "批量",
        "大量",
        "全部",
        "所有",
        "整个",
        "整体",
        "整盘",
        "全盘",
        "跨目录",
        "多目录",
        "多文件",
        "磁盘",
        "盘符",
        "索引",
        "everything",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    let target_roots = extract_volume_roots(&lower);
    let index_maintenance = lower.contains("索引")
        && (!target_roots.is_empty()
            || ["文件", "目录", "磁盘", "everything"]
                .iter()
                .any(|marker| lower.contains(marker)));
    if (!scan_context && !index_maintenance) || (!broad_scope && target_roots.is_empty()) {
        return None;
    }

    let requires_size_estimates = [
        "占用",
        "空间",
        "容量",
        "大小",
        "大文件",
        "统计",
        "size",
        "usage",
        "largest",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    let requires_full_metadata = [
        "精确",
        "精准",
        "准确",
        "确切",
        "实际大小",
        "逐项扫描",
        "修改时间",
        "最近",
        "最旧",
        "modified",
        "oldest",
        "exact",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    let direct_scan_override = [
        "不用索引",
        "不要索引",
        "不建立索引",
        "拒绝索引",
        "直接扫描",
        "临时扫描",
        "只扫一次",
        "一次性扫描",
        "用powershell",
        "用 powershell",
        "用cmd",
        "用 cmd",
        "用命令扫描",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    Some(BulkFileScanIntent {
        target_roots,
        requires_size_estimates,
        requires_full_metadata,
        direct_scan_override,
    })
}

fn extract_volume_roots(query: &str) -> Vec<String> {
    let chars: Vec<char> = query.chars().collect();
    let mut roots = Vec::new();
    for (index, drive) in chars.iter().copied().enumerate() {
        if !drive.is_ascii_alphabetic() {
            continue;
        }
        let marker = chars[index + 1..]
            .iter()
            .copied()
            .find(|character| !character.is_whitespace());
        if matches!(marker, Some(':' | '盘')) {
            let root = format!("{}:/", drive.to_ascii_uppercase());
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
    }
    roots
}

fn prioritize_index_tools(active_tools: &mut Vec<String>) {
    active_tools.retain(|name| {
        !matches!(
            name.as_str(),
            "scan_desktop" | "run_command" | "run_command_background" | "await_task"
        )
    });
    if !active_tools.iter().any(|name| name == "search_files") {
        active_tools.push("search_files".to_string());
    }
}

fn build_file_scan_policy(
    file_index: &crate::file_index::FileIndex,
    intent: &BulkFileScanIntent,
) -> Result<Value> {
    let coverage = if intent.requires_full_metadata {
        file_index.coverage(&intent.target_roots, true)?
    } else if intent.requires_size_estimates {
        file_index.coverage_for_estimated_sizes(&intent.target_roots)?
    } else {
        file_index.coverage(&intent.target_roots, false)?
    };
    let scope = if intent.target_roots.is_empty() {
        "目标范围".to_string()
    } else {
        intent.target_roots.join("、")
    };
    let next = if intent.direct_scan_override {
        "用户已明确要求不维护索引或只做一次性扫描，可以使用 scan_desktop/commands；仍须检查执行结果，不能把启动当成完成。".to_string()
    } else if coverage.ready {
        if intent.requires_full_metadata {
            format!(
                "批量文件任务必须优先使用 search_files。{} 已有完整元数据索引；精确空间分析下一步调用 action=summarize、accuracy=exact，普通批量筛选调用 action=search，不要改用 PowerShell 全量遍历。",
                scope
            )
        } else if intent.requires_size_estimates {
            format!(
                "批量文件任务必须优先使用 search_files。{} 已有 MFT 估算大小索引；下一步调用 action=summarize、accuracy=fast，并明确告诉用户结果是快速估算及其 size_coverage_percent，不要改用 PowerShell 全量遍历，也不要声称是精确值。",
                scope
            )
        } else {
            format!(
                "批量文件任务必须优先使用 search_files。{} 已有可用索引，下一步调用 action=search，不要改用 PowerShell 全量遍历。",
                scope
            )
        }
    } else if intent.target_roots.is_empty() {
        "批量文件任务必须优先使用 search_files，但当前没有明确目标范围。先向用户确认要分析的盘符或目录；未确认前不要启动索引或全量遍历。".to_string()
    } else if intent.requires_size_estimates && !intent.requires_full_metadata {
        let mut volumes: Vec<String> = intent
            .target_roots
            .iter()
            .filter_map(|root| crate::ntfs_usn::volume_for_path(Path::new(root)))
            .collect();
        volumes.sort();
        volumes.dedup();
        if !volumes.is_empty() {
            format!(
                "{} 尚无快速空间估算索引。请先说明：将针对当前目标卷启动短生命周期、只读的管理员助手，从 NTFS MFT 建立文件路径和逻辑大小估算索引，会弹出 Windows UAC；结果是估算值且不读取文件内容。用户明确同意后，按目标卷逐个调用 action=ntfs_index、volume=<当前卷>、user_confirmed=true；目标卷为 {}。完成后按用户指定的各目标调用 summarize、accuracy=fast。未同意前不要触发 UAC，也不要回退到全盘递归扫描。",
                scope,
                serde_json::to_string(&volumes)?
            )
        } else {
            format!(
                "{} 无法使用 NTFS MFT 快速估算。请说明需要普通逐项元数据扫描，用户同意后调用 action=index；未同意前不要全量遍历。",
                scope
            )
        }
    } else {
        format!(
            "批量文件任务必须优先使用 search_files，但 {} 尚无可用的{}索引。请先提示用户是否开始维护索引：首次建立会后台遍历，之后持续维护文件名、路径、大小和修改时间等元数据，不读取文件内容。用户本轮已明确同意时，调用 action=index、roots={}、user_confirmed=true；否则先询问，不要改用 PowerShell 全量遍历。",
            scope,
            if intent.requires_full_metadata { "精确元数据" } else { "文件名" },
            serde_json::to_string(&intent.target_roots)?
        )
    };
    Ok(json!({
        "intent": "bulk_file_scan",
        "priority_tool": "search_files",
        "target_roots": intent.target_roots,
        "requires_size_estimates": intent.requires_size_estimates,
        "requires_full_metadata": intent.requires_full_metadata,
        "direct_scan_override": intent.direct_scan_override,
        "coverage": coverage,
        "next": next,
    }))
}

fn index_required_result(
    coverage: crate::file_index::IndexCoverage,
    requires_full_metadata: bool,
    reason: &str,
) -> Value {
    json!({
        "ok": false,
        "status": "index_required",
        "index_required": true,
        "requires_full_metadata": requires_full_metadata,
        "coverage": coverage,
        "message": format!(
            "{}。请提示用户开始维护完整文件索引；首次建立会后台遍历所选目录，之后持续维护元数据，不读取文件内容。明确同意后调用 action=index、user_confirmed=true；未同意前不要回退到命令全量扫描。",
            reason
        ),
    })
}

fn ntfs_confirmation_required(volume: &str, purpose: &str) -> Value {
    json!({
        "ok": false,
        "status": "confirmation_required",
        "confirmation_required": true,
        "uac_required": true,
        "volume": volume,
        "message": format!(
            "{}需要启动一次短生命周期、只读的管理员索引助手，并弹出 Windows UAC。请先获得用户明确同意；同意后以 user_confirmed=true 重试。",
            purpose
        ),
    })
}

fn fast_size_index_required_result(
    coverage: crate::file_index::IndexCoverage,
    root: &str,
) -> Value {
    let volume = crate::ntfs_usn::volume_for_path(Path::new(root));
    json!({
        "ok": false,
        "status": "fast_index_required",
        "index_required": true,
        "requires_estimated_sizes": true,
        "uac_required": volume.is_some(),
        "volume": volume,
        "coverage": coverage,
        "message": if let Some(volume) = volume {
            format!(
                "快速空间分析需要为 {} 建立 MFT 估算索引。请说明结果属于快速估算，索引助手只读且会弹出 Windows UAC；用户明确同意后调用 action=ntfs_index、volume={}、user_confirmed=true，完成后再调用 summarize、accuracy=fast。不要直接改用全盘递归扫描。",
                root, volume
            )
        } else {
            "该目标无法使用 NTFS MFT 快速估算。若用户需要空间分析，请说明将逐项读取文件元数据，获得同意后调用 action=index。".to_string()
        },
    })
}

fn text_preview(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        format!("{}…", value.chars().take(max_chars).collect::<String>())
    }
}

fn json_value_or_text(value: &str) -> Value {
    serde_json::from_str(value).unwrap_or_else(|_| json!(value))
}

fn tool_call_search_item(record: &ToolCallRecord) -> Value {
    json!({
        "id": record.id,
        "session": {
            "id": record.session_id,
            "title": record.session_title,
        },
        "request_message": {
            "id": record.request_message_id,
            "content_preview": text_preview(&record.request_content, 180),
        },
        "response_message": {
            "id": record.response_message_id,
            "content_preview": record.response_content.as_deref().map(|value| text_preview(value, 180)),
            "recall": record.response_message_id.filter(|_| {
                record
                    .response_content
                    .as_deref()
                    .is_some_and(|value| value.chars().count() > 180)
            }).map(|id| format!(
                "最终回复已截断。完整正文请调用 recall_session，action=get，id={id}。不要把这个 message_id 传给 get_tool_call_history。"
            )),
        },
        "round_index": record.round_index,
        "call_index": record.call_index,
        "tool_call_id": record.tool_call_id,
        "tool_name": record.tool_name,
        "status": record.status,
        "arguments_preview": text_preview(&record.arguments_json, 400),
        "result_preview": record.result_json.as_deref().map(|value| text_preview(value, 500)),
        "assistant_content_preview": text_preview(&record.assistant_content, 120),
        "has_reasoning_content": !record.reasoning_content.is_empty(),
        "started_at": record.started_at,
        "completed_at": record.completed_at,
    })
}

fn tool_call_detail(record: &ToolCallRecord) -> Value {
    json!({
        "id": record.id,
        "session": {
            "id": record.session_id,
            "title": record.session_title,
        },
        "request_message": {
            "id": record.request_message_id,
            "content": record.request_content,
        },
        "response_message": {
            "id": record.response_message_id,
            "content": record.response_content,
        },
        "round_index": record.round_index,
        "call_index": record.call_index,
        "tool_call_id": record.tool_call_id,
        "tool_name": record.tool_name,
        "status": record.status,
        "arguments": json_value_or_text(&record.arguments_json),
        "result": record.result_json.as_deref().map(json_value_or_text),
        "assistant_content": record.assistant_content,
        "reasoning_content": record.reasoning_content,
        "started_at": record.started_at,
        "completed_at": record.completed_at,
    })
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    fn skill(name: &str, description: &str) -> crate::skills::SkillInfo {
        crate::skills::SkillInfo {
            name: name.to_string(),
            description: description.to_string(),
            enabled: true,
            scope: "external".to_string(),
            source: "local".to_string(),
            synced_from: String::new(),
            path: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn core_exposes_discovery_history_and_skill_loading() {
        let definitions = definitions_for(&core_tool_names());
        let mut names: Vec<_> = definitions
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
            .collect();
        names.sort();
        let mut expected = vec![
            "discover_capabilities",
            "get_tool_call_history",
            "recall_session",
            "load_skill",
            "ask_user",
            "todo_write",
            "workspace_open",
        ];
        expected.sort();
        assert_eq!(names, expected);
    }

    #[test]
    fn tool_history_schema_stays_inside_current_session() {
        let definitions = definitions_for(&["get_tool_call_history"]);
        let tool = &definitions.as_array().unwrap()[0];
        let description = tool
            .pointer("/function/description")
            .and_then(Value::as_str)
            .unwrap();
        assert!(description.contains("当前这次会话"));
        assert!(!description.contains("all_sessions"));
        assert!(tool
            .pointer("/function/parameters/properties/scope")
            .is_none());
    }

    #[test]
    fn categories_activate_only_related_tools() {
        let active = activate_categories(&["files".to_string(), "commands".to_string()]);
        assert!(active.iter().any(|name| name == "read_file"));
        assert!(active.iter().any(|name| name == "run_command"));
        assert!(active.iter().any(|name| name == "lookup_cache"));
        assert!(!active.iter().any(|name| name == "browser_click"));
    }

    #[test]
    fn whole_drive_usage_is_routed_to_persistent_index() {
        let requested_volume = format!("{}:/", 'R');
        let query = format!("看看 {}，整体分析空间使用和目录占用", requested_volume);
        let intent = detect_bulk_file_scan(&query).unwrap();
        assert_eq!(intent.target_roots, vec![requested_volume]);
        assert!(intent.requires_size_estimates);
        assert!(!intent.requires_full_metadata);
        assert!(!intent.direct_scan_override);

        let mut active = activate_categories(&[
            "files".to_string(),
            "commands".to_string(),
            "system".to_string(),
        ]);
        prioritize_index_tools(&mut active);
        assert!(active.iter().any(|name| name == "search_files"));
        assert!(active.iter().any(|name| name == "get_system_info"));
        assert!(!active.iter().any(|name| name == "scan_desktop"));
        assert!(!active.iter().any(|name| name == "run_command"));
        assert!(!active.iter().any(|name| name == "run_command_background"));
        assert!(!active.iter().any(|name| name == "await_task"));
    }

    #[test]
    fn explicit_exact_drive_usage_requires_full_metadata() {
        let requested_volume = format!("{}:/", 'R');
        let intent =
            detect_bulk_file_scan(&format!("精确分析 {} 空间占用", requested_volume)).unwrap();
        assert!(intent.requires_size_estimates);
        assert!(intent.requires_full_metadata);
    }

    #[test]
    fn explicit_one_time_scan_keeps_direct_scan_override() {
        let requested_volume = format!("{}:/", 'R');
        let intent = detect_bulk_file_scan(&format!(
            "不要索引，{} 只扫一次，用 PowerShell 统计",
            requested_volume
        ))
        .unwrap();
        assert_eq!(intent.target_roots, vec![requested_volume]);
        assert!(intent.direct_scan_override);
    }

    #[test]
    fn confirmed_drive_index_maintenance_still_routes_to_file_index() {
        let requested_volume = format!("{}:/", 'R');
        let intent =
            detect_bulk_file_scan(&format!("用户已同意为 {} 建立索引", requested_volume)).unwrap();
        assert_eq!(intent.target_roots, vec![requested_volume]);
        assert!(!intent.direct_scan_override);
    }

    #[test]
    fn search_files_schema_exposes_indexed_usage_summary_and_confirmation() {
        let definitions = definitions_for(&["search_files"]);
        let tool = &definitions.as_array().unwrap()[0];
        let actions = tool
            .pointer("/function/parameters/properties/action/enum")
            .and_then(Value::as_array)
            .unwrap();
        assert!(actions.iter().any(|action| action == "summarize"));
        assert!(tool
            .pointer("/function/parameters/properties/user_confirmed")
            .is_some());
        assert!(tool
            .pointer("/function/parameters/properties/accuracy")
            .is_some());
    }

    #[test]
    fn daily_report_query_recalls_daily_report_skill() {
        let found = rank_skills(
            "帮我把今天的工作写到飞书日报",
            vec![
                skill("daily-report-feishu-base", "写日报、日报、飞书日报"),
                skill("reddit-browse-home-feed", "浏览 Reddit 首页"),
            ],
            5,
        );
        assert_eq!(found[0]["name"], "daily-report-feishu-base");
        assert!(found
            .iter()
            .all(|item| item["name"] != "reddit-browse-home-feed"));
    }

    #[test]
    fn yesterday_daily_report_query_recalls_feishu_skill_not_files() {
        let desc =
            "用户的日报记在飞书多维表格的日报表中。当目标是撰写、查询或参考已提交的日报时使用。";
        let found = rank_skills(
            "帮我写个日报参考昨天我发的",
            vec![
                skill("daily-report-feishu-base", desc),
                skill("desktop-organize", "整理桌面文件时加载"),
                skill("windows-cli", "在 Windows 上用 run_command 执行命令"),
            ],
            5,
        );
        assert_eq!(found[0]["name"], "daily-report-feishu-base");
    }

    #[test]
    fn browser_evaluate_accepts_snapshot_ref_and_structured_args() {
        let definitions = definitions_for(&["browser_evaluate"]);
        let tool = &definitions.as_array().unwrap()[0];
        assert!(tool
            .pointer("/function/parameters/properties/ref")
            .is_some());
        assert!(tool
            .pointer("/function/parameters/properties/args")
            .is_some());
        assert!(tool
            .pointer("/function/description")
            .and_then(Value::as_str)
            .unwrap()
            .contains("不依赖 ref"));
    }

    #[test]
    fn browser_category_exposes_extension_cookie_tool() {
        let active = activate_categories(&["browser".to_string()]);
        assert!(active.iter().any(|name| name == "browser_cookies"));
        let definitions = definitions_for(&["browser_cookies"]);
        let tool = &definitions.as_array().unwrap()[0];
        assert_eq!(
            tool.pointer("/function/name").and_then(Value::as_str),
            Some("browser_cookies")
        );
        assert!(tool
            .pointer("/function/description")
            .and_then(Value::as_str)
            .unwrap()
            .contains("HttpOnly"));
    }

    #[test]
    fn browser_operations_are_added_to_legacy_registry_and_routed_inventory() {
        let path = std::env::temp_dir().join(format!("shiguang-page-ops-{}.db", uuid::Uuid::new_v4()));
        let db = Db::new(&path).unwrap();
        // Simulate a pre-SDK registry with one existing user-maintained definition.
        db.seed_tool_registry(&[crate::db::ToolRegistryEntry {
            name: "browser_type".into(), category: "browser".into(), executor_kind: "builtin".into(),
            executor_config: json!({}), definition: definitions_for(&["browser_type"])[0].clone(),
            enabled: true, source: "builtin".into(), revision: 1, updated_at: crate::db::now_str(),
        }]).unwrap();
        seed_registry(&db).unwrap();
        let routed = registry_definitions_for_categories(&db, &["browser".into()], false);
        for name in crate::browser::operations::TOOL_NAMES {
            assert!(routed.as_array().unwrap().iter().any(|t| t["function"]["name"] == *name), "missing {name}");
        }
        let mut entry = db.tool_registry_entry("browser_action").unwrap().unwrap();
        entry.definition["function"]["description"] = json!("自定义操作描述");
        db.put_tool_registry_entry(&entry).unwrap();
        assert_eq!(seed_registry(&db).unwrap(), 0);
        assert_eq!(db.tool_registry_entry("browser_action").unwrap().unwrap().definition["function"]["description"], "自定义操作描述");
        entry.enabled = false;
        db.put_tool_registry_entry(&entry).unwrap();
        let routed = registry_definitions_for_categories(&db, &["browser".into()], false);
        assert!(!routed.as_array().unwrap().iter().any(|t| t["function"]["name"] == "browser_action"));
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn registry_seed_is_editable_and_not_overwritten() {
        let path = std::env::temp_dir().join(format!(
            "shiguang-tool-registry-{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Db::new(&path).unwrap();
        assert!(seed_registry(&db).unwrap() > 0);
        let mut entry = db.tool_registry_entry("browser_cookies").unwrap().unwrap();
        entry.definition["function"]["description"] = json!("运行时修改后的描述");
        db.put_tool_registry_entry(&entry).unwrap();
        let mut old_agent = db.tool_registry_entry("run_subagent").unwrap().unwrap();
        old_agent.definition["function"]["parameters"]["properties"].as_object_mut().unwrap().remove("role");
        old_agent.definition["function"]["description"] = json!("自定义委托描述");
        db.put_tool_registry_entry(&old_agent).unwrap();
        assert_eq!(seed_registry(&db).unwrap(), 0);
        let tools = registry_definitions(&db, false);
        let agent = tools.as_array().unwrap().iter().find(|tool| tool["function"]["name"] == "run_subagent").unwrap();
        assert!(agent.pointer("/function/parameters/properties/role").is_some());
        assert_eq!(agent["function"]["description"], "自定义委托描述");
        assert!(db.tool_registry_entry("run_subagent").unwrap().unwrap().definition.pointer("/function/parameters/properties/role").is_none());
        let cookie = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| {
                tool.pointer("/function/name").and_then(Value::as_str) == Some("browser_cookies")
            })
            .unwrap();
        assert_eq!(
            cookie
                .pointer("/function/description")
                .and_then(Value::as_str),
            Some("运行时修改后的描述")
        );
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn routed_registry_upgrades_legacy_discovery_and_can_reveal_servers() {
        let path = std::env::temp_dir().join(format!("shiguang-routed-tools-{}.db", uuid::Uuid::new_v4()));
        let db = Db::new(&path).unwrap();
        seed_registry(&db).unwrap();
        let mut discovery = db.tool_registry_entry(DISCOVER_TOOL).unwrap().unwrap();
        discovery.definition["function"]["parameters"]["properties"]["categories"]["items"]["enum"].as_array_mut().unwrap().retain(|v| v != "servers");
        discovery.definition["function"]["description"] = json!("自定义前缀。全部工具已经可用，不必等待激活。");
        discovery.definition["function"]["parameters"]["properties"]["categories"]["description"] = json!("自定义类别说明");
        db.put_tool_registry_entry(&discovery).unwrap();

        let routed = registry_definitions_for_categories(&db, &["files".into(), "commands".into()], false);
        assert!(!routed.as_array().unwrap().iter().any(|t| t["function"]["name"] == "create_server"));
        let upgraded = routed.as_array().unwrap().iter().find(|t| t["function"]["name"] == DISCOVER_TOOL).unwrap();
        assert!(upgraded.pointer("/function/parameters/properties/categories/items/enum").unwrap().as_array().unwrap().contains(&json!("servers")));
        assert!(upgraded["function"]["description"].as_str().unwrap().contains("自定义前缀"));
        assert!(!upgraded["function"]["description"].as_str().unwrap().contains("全部工具已经可用"));
        assert!(upgraded.pointer("/function/parameters/properties/categories/description").unwrap().as_str().unwrap().contains("servers="));
        // Compatibility is applied to the API inventory, not to user-maintained rows.
        assert!(!db.tool_registry_entry(DISCOVER_TOOL).unwrap().unwrap().definition.pointer("/function/parameters/properties/categories/items/enum").unwrap().as_array().unwrap().contains(&json!("servers")));
        let expanded = registry_definitions_for_categories(&db, &["files".into(), "commands".into(), "servers".into()], false);
        for name in ["list_servers", "create_server", "update_server", "delete_server", "read_file", "run_command"] {
            assert!(expanded.as_array().unwrap().iter().any(|t| t["function"]["name"] == name), "missing {name}");
        }
        let mut disabled = db.tool_registry_entry("delete_server").unwrap().unwrap();
        disabled.enabled = false;
        db.put_tool_registry_entry(&disabled).unwrap();
        let filtered = registry_definitions_for_categories(&db, &["servers".into()], false);
        assert!(!filtered.as_array().unwrap().iter().any(|t| t["function"]["name"] == "delete_server"));
        assert!(filtered.as_array().unwrap().iter().any(|t| t["function"]["name"] == "create_server"));
        drop(db); let _ = std::fs::remove_file(path);
    }

    #[test]
    fn routed_registry_preserves_other_runtime_schema_upgrades() {
        let path = std::env::temp_dir().join(format!("shiguang-routed-delegation-{}.db", uuid::Uuid::new_v4()));
        let db = Db::new(&path).unwrap();
        seed_registry(&db).unwrap();
        let mut old_agent = db.tool_registry_entry("run_subagent").unwrap().unwrap();
        old_agent.definition["function"]["parameters"]["properties"].as_object_mut().unwrap().remove("role");
        old_agent.definition["function"]["description"] = json!("自定义委托描述");
        db.put_tool_registry_entry(&old_agent).unwrap();
        let routed = registry_definitions_for_categories(&db, &["delegation".into()], false);
        let tool = routed.as_array().unwrap().iter().find(|t| t["function"]["name"] == "run_subagent").unwrap();
        assert!(tool.pointer("/function/parameters/properties/role").is_some());
        assert_eq!(tool["function"]["description"], "自定义委托描述");
        drop(db); let _ = std::fs::remove_file(path);
    }

    #[test]
    fn lookup_cache_is_defined() {
        let definitions = definitions();
        let found = definitions.as_array().unwrap().iter().any(|tool| {
            tool.pointer("/function/name").and_then(Value::as_str) == Some("lookup_cache")
        });
        assert!(found);
    }

    #[test]
    fn command_tools_expose_await_instead_of_polling() {
        let definitions = definitions_for(&["run_command_background", "await_task", "check_task"]);
        let bg = definitions
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| {
                tool.pointer("/function/name").and_then(Value::as_str)
                    == Some("run_command_background")
            })
            .unwrap();
        assert!(bg
            .pointer("/function/parameters/properties/await")
            .is_some());
        let await_tool = definitions
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| {
                tool.pointer("/function/name").and_then(Value::as_str) == Some("await_task")
            })
            .unwrap();
        let await_desc = await_tool
            .pointer("/function/description")
            .and_then(Value::as_str)
            .unwrap();
        assert!(await_desc.contains("不消耗"));
        assert!(!await_desc.contains("lark-cli"));
        let check_desc = definitions
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| {
                tool.pointer("/function/name").and_then(Value::as_str) == Some("check_task")
            })
            .unwrap()
            .pointer("/function/description")
            .and_then(Value::as_str)
            .unwrap();
        assert!(check_desc.contains("await_task"));
    }

    #[test]
    fn command_tools_expose_preventive_shell_controls() {
        let definitions = definitions_for(&["run_command", "run_command_background"]);
        for tool in definitions.as_array().unwrap() {
            let properties = tool
                .pointer("/function/parameters/properties")
                .and_then(Value::as_object)
                .unwrap();
            for field in [
                "argv",
                "stdin",
                "files",
                "shell",
                "powershell_strict",
                "success_exit_codes",
                "script_args",
                "environment",
            ] {
                assert!(properties.contains_key(field), "missing {}", field);
            }
        }
    }

    #[test]
    fn command_guidance_recovers_windows_cli_pitfalls() {
        let quoting = command_guidance(
            "failed",
            Some(1),
            "positional arguments are not supported (got [项目名称])",
            "cmd",
        );
        assert!(quoting.iter().any(|hint| hint.contains("argv")));

        let json = command_guidance("failed", Some(1), "invalid JSON value near byte 1", "cmd");
        assert!(json.iter().any(|hint| hint.contains("argv")));

        let sub = command_guidance(
            "failed",
            Some(1),
            "unknown subcommand \"whoami\" for \"demo-cli auth\"",
            "cmd",
        );
        assert!(sub.iter().any(|hint| hint.contains("--help")));

        let field = command_guidance(
            "failed",
            Some(1),
            "NOT_FOUND: selected field does not exist",
            "cmd",
        );
        assert!(field.iter().any(|hint| hint.contains("引号")));

        let count = command_guidance(
            "failed",
            Some(1),
            "在此对象上找不到属性 Count",
            "powershell",
        );
        assert!(count.iter().any(|hint| hint.contains("@()")));

        let ps1 = command_guidance(
            "failed",
            Some(1),
            r"C:\Users\user\AppData\Roaming\npm\demo-cli.ps1 执行错误",
            "powershell",
        );
        assert!(ps1.iter().any(|hint| hint.contains("argv")));

        let filter = command_guidance(
            "failed",
            Some(1),
            r#"该类型只支持 "=="、">"、"<"、"empty"、"non_empty""#,
            "cmd",
        );
        assert!(filter.iter().any(|hint| hint.contains("合法运算符")));

        let garbled = command_guidance(
            "done",
            Some(0),
            "| 椤圭洰鍚嶇О | 鍚嶇О |\n| --- | --- |",
            "cmd",
        );
        assert!(garbled.iter().any(|hint| hint.contains("机器可读")));
    }

    #[test]
    fn command_tools_prefer_argv_over_shell_strings() {
        let definitions = definitions_for(&["run_command", "run_command_background"]);
        for tool in definitions.as_array().unwrap() {
            let description = tool
                .pointer("/function/description")
                .and_then(Value::as_str)
                .unwrap();
            assert!(
                description.contains("argv"),
                "missing argv guidance in {}",
                tool.pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap()
            );
            assert!(
                !description.contains("lark-cli"),
                "tool description should not hardcode a specific CLI"
            );
            assert!(tool
                .pointer("/function/parameters/properties/argv")
                .is_some());
            assert!(tool
                .pointer("/function/parameters/properties/stdin")
                .is_some());
            if tool.pointer("/function/name").and_then(Value::as_str) == Some("run_command") {
                assert!(tool
                    .pointer("/function/parameters/properties/files")
                    .is_some());
                assert!(tool
                    .pointer("/function/parameters/properties/json_pointer")
                    .is_some());
                assert!(description.contains("json_pointer") || description.contains("JSON"));
            }
        }
    }

    #[test]
    fn parse_argv_rejects_a_single_command_string() {
        let err = parse_argv(&json!({ "argv": "git status" })).unwrap_err();
        assert!(err.to_string().contains("字符串数组"));
        let ok = parse_argv(&json!({ "argv": ["git", "status"] })).unwrap();
        assert_eq!(ok, vec!["git", "status"]);
        assert!(parse_argv(&json!({})).unwrap().is_empty());
    }
}

#[cfg(test)]
mod tool_history_scope_tests {
    use super::*;
    use crate::db::Db;
    use std::path::Path;

    fn test_db() -> (Db, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "shiguang-tool-history-scope-{}.db",
            uuid::Uuid::new_v4()
        ));
        (Db::new(&path).unwrap(), path)
    }

    fn cleanup(db: Db, path: &Path) {
        drop(db);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    fn insert_call(db: &Db, session_id: i64, tool_name: &str, args: &str) -> i64 {
        let request_id = db
            .save_chat(session_id, "user", &format!("触发 {tool_name}"))
            .unwrap();
        let call_id = db
            .start_tool_call(
                session_id,
                request_id,
                0,
                0,
                &format!("call-{tool_name}-{session_id}"),
                tool_name,
                args,
                "",
                "",
            )
            .unwrap();
        db.finish_tool_call(call_id, "done", r#"{"ok":true}"#)
            .unwrap();
        call_id
    }

    #[test]
    fn search_and_get_stay_inside_the_current_session() {
        let (db, path) = test_db();
        let current = db.current_session_id().unwrap();
        let other = db.create_session().unwrap();

        let current_call = insert_call(&db, current, "browser_status", r#"{"from":"current"}"#);
        let other_call = insert_call(&db, other, "browser_click", r#"{"from":"other"}"#);

        let searched = query_current_session_tool_history(&db, current, &json!({})).unwrap();
        let calls = searched["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], current_call);
        assert_eq!(calls[0]["tool_name"], "browser_status");

        let detail = query_current_session_tool_history(
            &db,
            current,
            &json!({ "action": "get", "id": current_call }),
        )
        .unwrap();
        assert_eq!(detail["id"], current_call);

        let leaked = query_current_session_tool_history(
            &db,
            current,
            &json!({ "action": "get", "id": other_call }),
        );
        assert!(leaked
            .unwrap_err()
            .to_string()
            .contains("未找到工具调用记录"));

        let cross_session =
            query_current_session_tool_history(&db, current, &json!({ "scope": "all_sessions" }));
        assert!(cross_session
            .unwrap_err()
            .to_string()
            .contains("当前这次会话"));

        cleanup(db, &path);
    }
}

fn optional_local_timestamp(args: &Value, key: &str) -> Result<Option<i64>> {
    let Some(raw) = args
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    let naive = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .map(|date| date.and_hms_opt(0, 0, 0).expect("midnight is valid"))
        })
        .map_err(|_| {
            anyhow!(
                "{} 时间格式无效，应为 YYYY-MM-DD 或 YYYY-MM-DD HH:MM:SS",
                key
            )
        })?;
    chrono::Local
        .from_local_datetime(&naive)
        .single()
        .map(|dt| Some(dt.timestamp()))
        .ok_or_else(|| anyhow!("{} 对应的本地时间不存在或有歧义", key))
}

/// 后台任务/命令类工具的统一入口（都走 TaskManager，输出落日志文件）
async fn command_tool(
    app: &AppHandle,
    name: &str,
    args: &Value,
    cancel: &tokio_util::sync::CancellationToken,
    session_id: i64,
) -> Result<Value> {
    let state = app.state::<crate::AppState>();
    let tasks = &state.tasks;
    match name {
        "run_command" => {
            let argv = parse_argv(args)?;
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if argv.is_empty() && command.trim().is_empty() {
                bail!("请提供 argv（推荐，调用外部程序）或 command（cmd / PowerShell 脚本）");
            }
            let stdin = args.get("stdin").and_then(|v| v.as_str());
            let (workdir, input_files) = prepare_workdir_and_files(app, args)?;
            let shell = args.get("shell").and_then(|v| v.as_str());
            let powershell_strict = args
                .get("powershell_strict")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let success_exit_codes = integer_array(args, "success_exit_codes")?;
            let script_args = args.get("script_args").cloned().unwrap_or(Value::Null);
            let environment = string_map(args, "environment")?;
            let timeout = args
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(60)
                .clamp(5, 600);
            let tail_chars = args.get("tail_chars").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let json_pointer = args
                .get("json_pointer")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let info = tasks
                .run_sync(
                    app,
                    crate::tasks::CommandSpec {
                        command,
                        argv: &argv,
                        stdin,
                        label: None,
                        workdir: workdir.as_deref(),
                        shell,
                        powershell_strict,
                        success_exit_codes: &success_exit_codes,
                        script_args: &script_args,
                        environment: &environment,
                    },
                    timeout,
                    cancel,
                )
                .await?;
            Ok(command_result_with_json(
                app,
                tasks,
                &info,
                tail_chars,
                json_pointer,
                input_files,
                workdir.as_deref(),
            )?)
        }
        "run_command_background" => {
            let argv = parse_argv(args)?;
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if argv.is_empty() && command.trim().is_empty() {
                bail!("请提供 argv（推荐，调用外部程序）或 command（cmd / PowerShell 脚本）");
            }
            let stdin = args.get("stdin").and_then(|v| v.as_str());
            let label = args.get("label").and_then(|v| v.as_str());
            let (workdir, input_files) = prepare_workdir_and_files(app, args)?;
            let shell = args.get("shell").and_then(|v| v.as_str());
            let powershell_strict = args
                .get("powershell_strict")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let success_exit_codes = integer_array(args, "success_exit_codes")?;
            let script_args = args.get("script_args").cloned().unwrap_or(Value::Null);
            let environment = string_map(args, "environment")?;
            let notify = args.get("notify").and_then(|v| v.as_bool()).unwrap_or(false);
            let info = tasks.start_command(
                app,
                crate::tasks::CommandSpec {
                    command,
                    argv: &argv,
                    stdin,
                    label,
                    workdir: workdir.as_deref(),
                    shell,
                    powershell_strict,
                    success_exit_codes: &success_exit_codes,
                    script_args: &script_args,
                    environment: &environment,
                },
                // 结束后通知的任务不随聊天停止而结束，用户可在「任务」页或让 AI stop_task 停止。
                if notify { None } else { Some(cancel) },
            )?;
            if notify {
                tasks.set_notify(&info.id, Some(session_id))?;
                let mut result = json!({
                    "task_id": info.id,
                    "pid": info.pid,
                    "status": info.status,
                    "execution_context": command_execution_context(&info),
                    "message": "命令已在后台执行，结束后系统会自动发来【后台通知】并让你继续。现在告诉用户任务已在后台运行、完成后会通知，然后结束本次回复；不要 await_task 或 check_task 等它。",
                });
                attach_input_files(&mut result, &input_files, workdir.as_deref());
                return Ok(result);
            }
            let wait_now = args.get("await").and_then(|v| v.as_bool()).unwrap_or(false);
            if wait_now {
                let timeout = args
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(7200);
                let tail_chars =
                    args.get("tail_chars").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let json_pointer = args
                    .get("json_pointer")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                return awaited_task_result(
                    app,
                    tasks,
                    &info.id,
                    timeout,
                    cancel,
                    tail_chars,
                    json_pointer,
                    input_files,
                    workdir.as_deref(),
                )
                .await;
            }
            let mut result = json!({
                "task_id": info.id,
                "pid": info.pid,
                "status": info.status,
                "execution_context": command_execution_context(&info),
                "message": "命令已在后台执行。长时间任务请立刻 await_task，或下次启动时设 await=true；不要用 check_task 轮询，那会耗尽工具轮次。",
            });
            attach_input_files(&mut result, &input_files, workdir.as_deref());
            Ok(result)
        }
        "await_task" => {
            let id = args
                .get("task_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("缺少 task_id"))?;
            let timeout = args
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(7200);
            let tail_chars = args.get("tail_chars").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let json_pointer = args
                .get("json_pointer")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let notify = args.get("mode").and_then(Value::as_str) == Some("notify");
            // notify：不挂起对话，任务结束后自动通知；已结束则直接返回结果。
            // 普通等待：取消之前的通知，避免拿到结果后又收到重复通知。
            let still_running = tasks.set_notify(id, notify.then_some(session_id))?;
            if notify && still_running {
                return Ok(json!({
                    "task_id": id,
                    "status": "running",
                    "message": "已改为结束后自动通知：任务继续在后台运行，用户停止回复也不会结束它。现在告诉用户完成后会通知，然后结束本次回复。",
                }));
            }
            awaited_task_result(
                app,
                tasks,
                id,
                timeout,
                cancel,
                tail_chars,
                json_pointer,
                Vec::new(),
                None,
            )
            .await
        }
        "check_task" => {
            let id = args
                .get("task_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 task_id"))?;
            let info = tasks.get(id).ok_or_else(|| anyhow!("任务不存在: {}", id))?;
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let tail_chars = args.get("tail_chars").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let json_pointer = args
                .get("json_pointer")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let prefer_json = !json_pointer.is_empty() || pattern.is_none();
            if prefer_json {
                if let Some(mut result) = try_command_json_result(app, tasks, &info, json_pointer)?
                {
                    if let Some(obj) = result.as_object_mut() {
                        obj.insert("label".into(), json!(info.label));
                        obj.insert("started_at".into(), json!(info.started_at));
                        obj.insert("finished_at".into(), json!(info.finished_at));
                        obj.insert("output_mode".into(), json!("json"));
                        obj.insert(
                            "hint".into(),
                            json!(if info.status == "running" {
                                "任务仍在运行。不要反复 check_task；改用 await_task 挂起直到结束。"
                            } else {
                                "任务已结束。stdout 已按 JSON 解析；只要子集时带 json_pointer 再查，不必重跑命令。"
                            }),
                        );
                    }
                    return Ok(result);
                }
            }
            let (mode, output) = match pattern {
                Some(p) => {
                    let hits = tasks.grep(id, p)?;
                    (
                        format!("匹配「{}」的最近 {} 行", p, hits.len()),
                        hits.join("\n"),
                    )
                }
                None => {
                    let t = tasks.tail(id, tail_chars)?;
                    ("末尾片段".to_string(), t.trim().to_string())
                }
            };
            Ok(json!({
                "ok": info.status == "done" || info.status == "running",
                "task_id": info.id,
                "label": info.label,
                "status": info.status,
                "exit_code": info.exit_code,
                "started_at": info.started_at,
                "finished_at": info.finished_at,
                "output_mode": mode,
                "output": output,
                "execution_context": command_execution_context(&info),
                "guidance": command_guidance(&info.status, info.exit_code, &output, &info.shell),
                "log_path": info.log_path,
                "hint": if info.status == "running" {
                    "任务仍在运行。不要反复 check_task；改用 await_task 挂起直到结束。"
                } else {
                    "任务已结束；如需更多输出，用不同 pattern 或更大 tail_chars 再查，完整内容在日志文件中。"
                },
            }))
        }
        "list_tasks" => {
            let list = tasks.list();
            Ok(json!({ "count": list.len(), "tasks": list }))
        }
        "stop_task" => {
            let id = args
                .get("task_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 task_id"))?;
            let info = tasks.stop(app, id)?;
            Ok(json!({ "ok": true, "task_id": info.id, "status": info.status }))
        }
        _ => bail!("未知命令工具: {}", name),
    }
}

fn prepare_workdir_and_files(
    app: &AppHandle,
    args: &Value,
) -> Result<(Option<String>, Vec<String>)> {
    let files = args.get("files");
    let mut workdir = args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    if files.is_some() && workdir.is_none() {
        workdir = Some(
            crate::tempfs::ensure_temp_dir(app)?
                .to_string_lossy()
                .replace('\\', "/"),
        );
    }
    let mut written = Vec::new();
    if let Some(files) = files {
        let dir = std::path::PathBuf::from(
            workdir
                .as_deref()
                .ok_or_else(|| anyhow!("files 需要有效的工作目录"))?,
        );
        std::fs::create_dir_all(&dir)?;
        written = crate::cli_json::write_workdir_files(&dir, files)?;
    }
    Ok((workdir, written))
}

fn attach_input_files(result: &mut Value, input_files: &[String], workdir: Option<&str>) {
    if input_files.is_empty() {
        return;
    }
    if let Some(obj) = result.as_object_mut() {
        obj.insert("input_files".into(), json!(input_files));
        if let Some(dir) = workdir {
            obj.insert("workdir".into(), json!(dir));
        }
    }
}

fn try_command_json_result(
    app: &AppHandle,
    tasks: &crate::tasks::TaskManager,
    info: &crate::tasks::TaskInfo,
    json_pointer: &str,
) -> Result<Option<Value>> {
    let (full_text, source_truncated) = tasks
        .read_decoded(&info.id, crate::cli_json::MAX_JSON_LOG_BYTES)
        .unwrap_or_default();
    if full_text.trim().is_empty() {
        return Ok(None);
    }
    let save_dir = crate::tempfs::ensure_temp_dir(app)?;
    let Some(captured) = crate::cli_json::capture_stdout_json(
        &full_text,
        source_truncated,
        json_pointer,
        &save_dir,
        &info.id,
    )?
    else {
        return Ok(None);
    };
    let output = captured.leftover;
    let guidance = command_guidance(&info.status, info.exit_code, &output, &info.shell);
    let mut result = json!({
        "ok": info.status == "done" || info.status == "running",
        "task_id": info.id,
        "status": info.status,
        "exit_code": info.exit_code,
        "output": output,
        "truncated": false,
        "json_summary": captured.summary,
        "json_chars": captured.chars,
        "execution_context": command_execution_context(info),
        "guidance": guidance,
        "note": captured.note,
        "log_path": info.log_path,
    });
    if let Some(obj) = result.as_object_mut() {
        if let Some(value) = captured.value {
            obj.insert("json".into(), value);
        }
        if let Some(path) = captured.file {
            obj.insert("json_file".into(), json!(path));
        }
        if let Some(err) = captured.pointer_error {
            obj.insert("json_pointer_error".into(), json!(err));
        }
    }
    Ok(Some(result))
}

fn command_result_with_json(
    app: &AppHandle,
    tasks: &crate::tasks::TaskManager,
    info: &crate::tasks::TaskInfo,
    tail_chars: usize,
    json_pointer: &str,
    input_files: Vec<String>,
    workdir: Option<&str>,
) -> Result<Value> {
    if let Some(mut result) = try_command_json_result(app, tasks, info, json_pointer)? {
        attach_input_files(&mut result, &input_files, workdir);
        return Ok(result);
    }
    let (output, truncated) = tasks.head_tail(&info.id, tail_chars).unwrap_or_default();
    let guidance = command_guidance(&info.status, info.exit_code, &output, &info.shell);
    let mut result = json!({
        "ok": info.status == "done",
        "task_id": info.id,
        "status": info.status,
        "exit_code": info.exit_code,
        "output": output.trim(),
        "truncated": truncated,
        "execution_context": command_execution_context(info),
        "guidance": guidance,
        "note": if truncated {
            "输出超长，已截取开头与结尾片段（中间省略）；完整输出在日志文件中，可用 check_task 按 pattern 关键字过滤取需要的部分。"
        } else {
            "以上是完整输出。"
        },
        "log_path": info.log_path,
    });
    attach_input_files(&mut result, &input_files, workdir);
    Ok(result)
}

async fn awaited_task_result(
    app: &AppHandle,
    tasks: &crate::tasks::TaskManager,
    task_id: &str,
    timeout_secs: u64,
    cancel: &tokio_util::sync::CancellationToken,
    tail_chars: usize,
    json_pointer: &str,
    input_files: Vec<String>,
    workdir: Option<&str>,
) -> Result<Value> {
    let (info, wait) = tasks.wait_for(app, task_id, timeout_secs, cancel).await?;
    let mut result = command_result_with_json(
        app,
        tasks,
        &info,
        tail_chars,
        json_pointer,
        input_files,
        workdir,
    )?;
    if let Some(obj) = result.as_object_mut() {
        obj.insert("wait".into(), json!(wait));
        match wait {
            "timeout" if info.status == "running" => {
                obj.insert("ok".into(), json!(false));
                obj.insert("wait_timed_out".into(), json!(true));
                obj.insert(
                    "note".into(),
                    json!("等待超时，任务仍在运行。不要 check_task 轮询；再次 await_task，或 stop_task 结束它。"),
                );
            }
            "cancelled" => {
                obj.insert("ok".into(), json!(false));
                obj.insert("note".into(), json!("用户中断了等待，已尝试停止该任务。"));
            }
            _ => {
                obj.insert(
                    "note".into(),
                    json!("后台任务已结束，对话自动继续。以上是完整结果，不必再 check_task。"),
                );
            }
        }
    }
    Ok(result)
}

fn command_execution_context(info: &crate::tasks::TaskInfo) -> Value {
    let mut context = json!({
        "os": "Windows",
        "shell": info.shell,
        "shell_selection": info.shell_selection,
        "transport": info.transport,
        "success_exit_codes": info.success_exit_codes,
        "python_utf8": true,
        "log_decoding": "UTF-8，失败时回退 GB18030"
    });
    if let Some(obj) = context.as_object_mut() {
        if info.shell == "direct" {
            obj.insert(
                "launch".into(),
                json!("CreateProcess argv，不经过 cmd/PowerShell"),
            );
            obj.insert(
                "quoting".into(),
                json!("每个 argv 项是独立参数；JSON、空格、中文无需转义"),
            );
        } else if info.shell == "powershell" {
            obj.insert(
                "launch".into(),
                json!("Windows PowerShell / NoProfile / NonInteractive"),
            );
            obj.insert(
                "quoting".into(),
                json!("脚本不经过 cmd；$变量、引号和中文保持原样"),
            );
            obj.insert(
                "preflight".into(),
                json!("执行前使用 PowerShell AST Parser 检查完整脚本"),
            );
        } else {
            obj.insert("launch".into(), json!("cmd.exe /d /s /c"));
            obj.insert("code_page".into(), json!(65001));
        }
    }
    context
}

/// 把常见命令失败翻译成模型可直接执行的恢复建议，减少盲目重复同一条命令。
fn command_guidance(
    status: &str,
    exit_code: Option<i32>,
    output: &str,
    shell: &str,
) -> Vec<String> {
    let mut hints = Vec::new();
    if status == "timeout" {
        hints.push("命令已超时终止。确认它是否本应长时间运行；是则改用 run_command_background 并 await=true，否则缩小任务范围。".to_string());
    } else if matches!(status, "failed" | "cancelled") {
        hints.push(format!(
            "命令未成功{}。先根据输出定位根因并修正，再决定是否重试；不要把本次执行当作成功。",
            exit_code
                .map(|code| format!("（退出码 {}）", code))
                .unwrap_or_default()
        ));
    }

    let lower = output.to_lowercase();
    if output.contains("__DH_PS_PARSE_ERROR__") {
        hints.push("PowerShell 执行前语法检查未通过，用户脚本尚未开始运行。按标记后的行列和消息修正脚本正文；不要改用多层引号或临时文件绕过检查。".to_string());
    }
    if output.contains("__DH_PS_RUNTIME_ERROR__") {
        hints.push("PowerShell 严格执行阶段失败。未定义变量、非终止错误和异常已被提升为失败；根据标记后的异常与位置修正根因。只有确认是旧脚本兼容问题时才考虑 powershell_strict=false。".to_string());
    }
    if output.contains("__DH_PS_BOOTSTRAP_ERROR__") {
        hints.push("PowerShell 超长脚本的安全传输引导层失败，用户脚本可能尚未开始运行。检查临时文件访问、执行环境或引导错误；不要改回多层命令行引号。".to_string());
    }
    if output.contains('\u{fffd}') || output.contains("��") {
        hints.push("输出仍含乱码替代字符，不能据此做中文名称匹配。优先让命令输出 JSON（run_command 会自动解析）；必要时查询该 CLI 的编码参数。".to_string());
    }
    if lower.contains("not recognized as an internal or external command")
        || output.contains("不是内部或外部命令")
    {
        hints.push("系统找不到该命令。检查工具是否已安装、可执行文件名是否正确，以及它是否在当前进程的 PATH 中。".to_string());
    }
    if !output.contains("__DH_PS_PARSE_ERROR__")
        && (lower.contains("parsererror") || lower.contains("unexpected token"))
    {
        hints.push(if shell == "powershell" {
            "检测到 PowerShell 解析错误。脚本已通过 EncodedCommand 原样传入，`$变量` 不会被 cmd 展开；请依据报错行检查脚本自身的引号、括号和语法。".to_string()
        } else {
            "检测到 cmd 语法解析错误。若命令实际使用 `$变量`、对象管道或 PowerShell 语法，请把 shell 改为 powershell，并直接传脚本正文，不要再包 powershell -Command。".to_string()
        });
    }
    if lower.contains("access is denied") || output.contains("拒绝访问") {
        hints.push("检测到权限不足。先确认目标和操作范围；只有确实需要提升权限且用户已知情时，才请求授权。".to_string());
    }
    hints.extend(cli_recovery_hints(output, shell));
    hints
}

/// 把 Windows 上调用外部程序的常见失败翻译成下一步，避免模型继续拼命令字符串。
fn cli_recovery_hints(output: &str, shell: &str) -> Vec<String> {
    let mut hints = Vec::new();
    let lower = output.to_lowercase();

    if lower.contains("positional arguments are not supported")
        || lower.contains("invalid json value")
    {
        hints.push(
            "命令行里的 JSON/引号已被 shell 拆坏。改用 argv，把 JSON 作为单独一项或放入 stdin；不要继续改引号重试。".to_string(),
        );
    }
    if lower.contains("unknown subcommand") || lower.contains("unknown command") {
        hints.push(
            "子命令不存在。不要猜测名称；对该程序及其父命令执行 --help，改用帮助里的真实子命令。"
                .to_string(),
        );
    }
    if lower.contains("selected field does not exist")
        || (output.contains("NOT_FOUND") && lower.contains("field"))
    {
        hints.push(
            "指定字段未找到。不要把引号写进标志值；先去掉字段投影，输出全量 JSON 再解析真实字段名。".to_string(),
        );
    }
    if (output.contains("找不到属性") && lower.contains("count"))
        || lower.contains("the property 'count' cannot be found")
        || lower.contains("propertynotfoundstrict")
    {
        hints.push(
            "PowerShell 管道无匹配时返回 $null，严格模式下没有 .Count。先用 `@()` 转成数组：`$hits = @($lines | Where-Object { ... })` 再取 Count；不要关闭 powershell_strict。".to_string(),
        );
    }
    if shell == "powershell" && lower.contains(".ps1") {
        hints.push(
            "PowerShell 可能执行了同名 .ps1 而不是 exe/cmd。改用 argv 直启外部程序，系统按 PATHEXT 解析原生可执行文件。".to_string(),
        );
    }
    if (lower.contains("only support") || output.contains("只支持"))
        && (lower.contains("filter")
            || output.contains("过滤")
            || output.contains("==")
            || output.contains("empty")
            || lower.contains(">="))
    {
        hints.push(
            "该字段类型不支持当前过滤运算符。按报错列出的合法运算符改写，不要换一个同义写法硬试。"
                .to_string(),
        );
    }
    if looks_like_garbled_cli_text(output) {
        hints.push(
            "输出中文已乱码，不能用来做名称匹配。改用该程序的 JSON/机器可读输出（run_command 会自动解析 JSON）；不要把乱码表格当依据，也不必再重定向到文件后 read_file。".to_string(),
        );
    }
    hints
}

/// cmd 把 UTF-8 当系统代码页解码后，常见中文会变成另一组仍合法的汉字。
fn looks_like_garbled_cli_text(output: &str) -> bool {
    ["椤圭洰", "鍚嶇О", "椤圭", "鍚嶇"]
        .iter()
        .any(|marker| output.contains(marker))
}

fn propose_organization(app: &AppHandle, db: &Db, args: &Value) -> Result<Value> {
    let summary = args
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let cats = args
        .get("categories")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if cats.is_empty() {
        bail!("方案为空：categories 不能为空");
    }

    let settings = crate::commands::load_settings(db);
    let root = Path::new(&settings.organize_root);
    let desktop = scanner::desktop_dir()?;

    let mut categories: Vec<PlanCategory> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for c in cats {
        let name = c
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("未分类")
            .to_string();
        let files_raw = c
            .get("files")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut files = Vec::new();
        for f in files_raw {
            let Some(fname) = f.as_str() else { continue };
            // 只保留文件名，防止路径注入
            let base = Path::new(fname)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if base.is_empty() || !seen.insert(base.clone()) {
                continue;
            }
            if base == "desktop.ini" {
                continue;
            }
            if !desktop.join(&base).exists() {
                continue;
            }
            files.push(base);
        }
        if files.is_empty() {
            continue;
        }
        let action = match c.get("action").and_then(|v| v.as_str()) {
            Some("delete") => "delete".to_string(),
            _ => "move".to_string(),
        };
        let target_folder = if action == "delete" {
            String::new()
        } else {
            let safe: String = name
                .chars()
                .map(|ch| if "<>:\"/\\|?*".contains(ch) { '_' } else { ch })
                .collect();
            root.join(safe).to_string_lossy().to_string()
        };
        categories.push(PlanCategory {
            name,
            action,
            target_folder,
            files,
        });
    }

    if categories.is_empty() {
        bail!("方案为空：没有可整理的有效文件");
    }

    db.cancel_pending_plans()?;
    let plan_id = db.insert_plan(&summary, &categories)?;
    let plan = db.get_plan(plan_id)?;
    let _ = app.emit("plan-proposed", &plan);
    let total: usize = categories.iter().map(|c| c.files.len()).sum();
    Ok(json!({
        "plan_id": plan_id,
        "file_count": total,
        "message": "方案已生成并展示给用户，请等待用户在界面上确认或取消，不要重复调用本工具。"
    }))
}

#[allow(dead_code)]
pub fn apply_rule_to_path(db: &Db, path: &Path) -> Result<Option<(String, String)>> {
    rules::apply_to_file(db, path)
}

fn save_browser_recipe(app: &AppHandle, db: &Db, args: &Value) -> Result<Value> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let site = args
        .get("site_pattern")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let goal = args
        .get("goal")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let actions = args
        .get("actions")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("缺少 actions"))?;
    if name.is_empty() || site.is_empty() || goal.is_empty() || actions.is_empty() {
        bail!("名称、站点、目标和至少一个动作不能为空");
    }
    if actions.len() > 12 {
        bail!("浏览器配方最多 12 个动作");
    }
    for action in actions {
        let kind = action.get("action").and_then(Value::as_str).unwrap_or("");
        if !["find_click", "find_type"].contains(&kind)
            || action
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .is_empty()
        {
            bail!("动作只允许带 query 的 find_click 或 find_type");
        }
        if kind == "find_type" && action.get("value").and_then(Value::as_str).is_none() {
            bail!("find_type 需要 value");
        }
    }
    let verification = args
        .get("verification")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let recipe = db.browser_recipe_save(&crate::db::BrowserRecipe {
        id: args.get("id").and_then(Value::as_i64).unwrap_or(0),
        name: name.into(),
        site_pattern: site.into(),
        goal: goal.into(),
        recipe_json: serde_json::to_string(actions)?,
        verification_json: serde_json::to_string(&verification)?,
        success_count: 0,
        failure_count: 0,
        last_used_at: None,
        created_at: String::new(),
        updated_at: String::new(),
    })?;
    let _ = app.emit("browser-recipes-changed", ());
    Ok(json!({ "ok": true, "recipe": recipe }))
}

fn recipe_value(template: &str, vars: &Value) -> Result<String> {
    let mut output = template.to_string();
    let object = vars.as_object();
    for capture in template.match_indices("{{") {
        let start = capture.0 + 2;
        let Some(end) = template[start..].find("}}") else {
            bail!("变量模板未闭合");
        };
        let key = &template[start..start + end];
        let value = object
            .and_then(|map| map.get(key))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("缺少配方变量：{}", key))?;
        let lower = key.to_lowercase();
        if lower.contains("password")
            || lower.contains("cookie")
            || lower.contains("token")
            || lower.contains("secret")
        {
            bail!("配方不接受密码、cookie、token 或密钥变量");
        }
        output = output.replace(&format!("{{{{{key}}}}}"), value);
    }
    Ok(output)
}

async fn run_browser_recipe(app: &AppHandle, db: &Db, args: &Value) -> Result<Value> {
    let id = args
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("缺少 id"))?;
    let recipe = db
        .browser_recipe_get(id)?
        .ok_or_else(|| anyhow!("浏览器配方不存在"))?;
    let state = app.state::<crate::AppState>();
    let info = state.browser.call("info", json!({})).await?;
    let url = info.get("url").and_then(Value::as_str).unwrap_or("");
    if !url
        .to_lowercase()
        .contains(&recipe.site_pattern.to_lowercase())
    {
        db.browser_recipe_mark(id, false)?;
        bail!(
            "配方适用于「{}」，当前页面为「{}」；已停止，请正常快照后处理",
            recipe.site_pattern,
            url
        );
    }
    let actions: Vec<Value> =
        serde_json::from_str(&recipe.recipe_json).map_err(|_| anyhow!("配方动作格式损坏"))?;
    let vars = args.get("variables").cloned().unwrap_or_else(|| json!({}));
    let mut completed = Vec::new();
    for action in actions {
        let kind = action.get("action").and_then(Value::as_str).unwrap_or("");
        let query = recipe_value(
            action.get("query").and_then(Value::as_str).unwrap_or(""),
            &vars,
        )?;
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
            db.browser_recipe_mark(id, false)?;
            bail!(
                "配方步骤「{}」找到 {} 个候选，无法安全选择；已停止，请用 browser_snapshot 接管",
                kind,
                candidates.len()
            );
        }
        let reference = candidates[0]
            .get("ref")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("候选缺少 ref"))?;
        let result = match kind {
            "find_click" => {
                state
                    .browser
                    .call("click", json!({ "ref": reference }))
                    .await?
            }
            "find_type" => {
                let value = recipe_value(
                    action.get("value").and_then(Value::as_str).unwrap_or(""),
                    &vars,
                )?;
                state.browser.call("type", json!({ "ref": reference, "text": value, "clear": action.get("clear").and_then(Value::as_bool).unwrap_or(true) })).await?
            }
            _ => {
                db.browser_recipe_mark(id, false)?;
                bail!("配方含不支持的动作");
            }
        };
        completed.push(json!({ "action": kind, "query": query, "result": result }));
    }
    let verify: Value =
        serde_json::from_str(&recipe.verification_json).unwrap_or_else(|_| json!({}));
    let verified = if let Some(expected) = verify.get("url_contains").and_then(Value::as_str) {
        state
            .browser
            .call("info", json!({}))
            .await?
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(|current| current.contains(expected))
    } else if let Some(query) = verify.get("find_query").and_then(Value::as_str) {
        state
            .browser
            .call("find", json!({ "query": query, "limit": 1 }))
            .await?
            .get("count")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0
    } else {
        true
    };
    db.browser_recipe_mark(id, verified)?;
    if !verified {
        bail!("配方已执行但未通过成功校验；已停止，请用正常浏览器工具核验和处理");
    }
    Ok(json!({ "ok": true, "recipe": recipe.name, "completed": completed, "verified": verified }))
}

async fn browser_tool(app: &AppHandle, name: &str, args: &Value) -> Result<Value> {
    let state = app.state::<crate::AppState>();
    let action = name.trim_start_matches("browser_");
    if action == "status" {
        return Ok(state.browser.status().await);
    }
    if action == "evaluate" {
        let expr = args.get("expression").and_then(Value::as_str).unwrap_or("");
        let has_args = args.get("args").is_some_and(|value| !value.is_null());
        if expr.contains("$args") && !has_args {
            bail!("表达式使用了 $args，请把结构化参数放在 args 里，不要写进 expression");
        }
        if expr.contains("$el") && args.get("ref").and_then(Value::as_i64).is_none() {
            bail!("表达式使用了 $el，请同时传入 browser_snapshot 的 ref");
        }
    }
    if action == "cookies" {
        let has_domains = args
            .get("domains")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty());
        let has_urls = args
            .get("urls")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty());
        if !has_domains && !has_urls {
            bail!("读取 Cookie 时 domains 或 urls 至少提供一项，不能无范围读取整个浏览器");
        }
        let mut params = args.clone();
        params["channel"] = json!("extension");
        return state.browser.call("cookies", params).await;
    }
    if action == "screenshot" {
        let v = state.browser.call("screenshot", args.clone()).await?;
        let b64 = v
            .get("png_base64")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("截图失败：未返回图像数据"))?;
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| anyhow!("截图数据解码失败: {}", e))?;
        let dir = app
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join("screenshots");
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(format!(
            "shot-{}.png",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        std::fs::write(&file, &bytes)?;
        let saved = file.to_string_lossy().replace('\\', "/");
        let settings = crate::commands::load_settings(&state.db);
        let hint = if settings.model_supports_vision {
            "截图已保存。当前对话模型可直接看图，无需再调 read_image；只要文字仍可用 ocr_image。"
        } else {
            "截图已保存。提取文字用 ocr_image（本地免费）；理解画面用 read_image。"
        };
        return Ok(json!({
            "saved": saved,
            "channel": v.get("channel").cloned().unwrap_or(Value::Null),
            "hint": hint,
        }));
    }
    state.browser.call(action, args.clone()).await
}

