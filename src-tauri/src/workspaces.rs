//! 工作区：固定项目的路径、部署方式、验证命令、配套工具和规则。
//! 只存仓库里读不到的信息；项目说明文件和 git 状态在打开时现读。
use crate::db::{Db, WorkspaceRow};
use anyhow::{bail, Result};
use serde_json::{json, Map, Value};
use std::path::Path;

const LIST_FIELDS: &[&str] = &["aliases", "verify_commands", "rules", "notes", "links"];
const TEXT_FIELDS: &[&str] = &["summary", "deploy", "tools", "default_agent", "agent_mode"];
const MAX_ITEMS: usize = 30;
const MAX_TEXT: usize = 1200;
pub const AGENT_KINDS: &[&str] = &["claude_code", "codex", "cursor", "pi"];

fn clip(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

/// 只保留已知字段，去空白、限长；links 允许字符串或 {label,url}。
pub fn normalize_data(input: &Value) -> Result<Value> {
    let mut out = Map::new();
    for key in TEXT_FIELDS {
        if let Some(v) = input.get(*key) {
            let text = v.as_str().map(|s| clip(s, MAX_TEXT)).unwrap_or_default();
            if !text.is_empty() {
                out.insert((*key).into(), json!(text));
            }
        }
    }
    if let Some(agent) = out.get("default_agent").and_then(Value::as_str) {
        if !AGENT_KINDS.contains(&agent) {
            bail!("default_agent 只能是 claude_code、codex、cursor 或 pi");
        }
    }
    if let Some(mode) = out.get("agent_mode").and_then(Value::as_str) {
        if !matches!(mode, "plan" | "edit" | "full") {
            bail!("agent_mode 只能是 plan、edit 或 full");
        }
    }
    for key in LIST_FIELDS {
        let Some(v) = input.get(*key) else { continue };
        let items: Vec<Value> = match v {
            Value::String(s) => s.lines().map(|l| json!(clip(l, MAX_TEXT))).collect(),
            Value::Array(a) => a
                .iter()
                .filter_map(|item| match item {
                    Value::String(s) => Some(json!(clip(s, MAX_TEXT))),
                    Value::Object(o) if *key == "links" => {
                        let url = o.get("url").and_then(Value::as_str).map(|s| clip(s, 500)).unwrap_or_default();
                        let label = o.get("label").and_then(Value::as_str).map(|s| clip(s, 80)).unwrap_or_default();
                        (!url.is_empty()).then(|| json!({"label": label, "url": url}))
                    }
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let items: Vec<Value> = items
            .into_iter()
            .filter(|v| v.as_str().map_or(true, |s| !s.is_empty()))
            .take(MAX_ITEMS)
            .collect();
        if !items.is_empty() {
            out.insert((*key).into(), Value::Array(items));
        }
    }
    Ok(Value::Object(out))
}

pub fn text<'a>(ws: &'a WorkspaceRow, key: &str) -> &'a str {
    ws.data.get(key).and_then(Value::as_str).unwrap_or("")
}

pub fn list(ws: &WorkspaceRow, key: &str) -> Vec<String> {
    ws.data
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(o) => {
                        let url = o.get("url").and_then(Value::as_str).unwrap_or("");
                        let label = o.get("label").and_then(Value::as_str).unwrap_or("");
                        Some(if label.is_empty() { url.to_string() } else { format!("{label}：{url}") })
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 在用户消息里按名称/别名找工作区：取最长命中；两个工作区并列最长时视为歧义。
pub fn match_in_text(workspaces: &[WorkspaceRow], message: &str) -> Option<i64> {
    let haystack = message.to_lowercase();
    let mut best: Option<(usize, i64)> = None;
    let mut tie = false;
    for ws in workspaces {
        let mut needles = list(ws, "aliases");
        needles.push(ws.name.clone());
        let longest = needles
            .iter()
            .map(|n| n.trim().to_lowercase())
            .filter(|n| n.chars().count() >= 2 && haystack.contains(n.as_str()))
            .map(|n| n.chars().count())
            .max();
        let Some(len) = longest else { continue };
        match best {
            Some((best_len, _)) if len < best_len => {}
            Some((best_len, id)) if len == best_len && id != ws.id => tie = true,
            _ => {
                best = Some((len, ws.id));
                tie = false;
            }
        }
    }
    if tie { None } else { best.map(|(_, id)| id) }
}

/// 工作区目录：按 id 排序保持稳定，作为独立 system 消息不破坏前缀缓存。
pub fn catalog_block(workspaces: &[WorkspaceRow]) -> Option<String> {
    if workspaces.is_empty() {
        return None;
    }
    let mut sorted: Vec<&WorkspaceRow> = workspaces.iter().collect();
    sorted.sort_by_key(|w| w.id);
    let mut out = String::from("<workspaces>\n用户登记的工作区（固定项目）。提到名称或别名时就是指它；本轮已绑定的工作区会在末尾给出摘要，需要 git 状态和项目说明时调用 workspace_open。\n");
    for ws in sorted {
        let aliases = list(ws, "aliases");
        let alias = if aliases.is_empty() { String::new() } else { format!("（别名：{}）", aliases.join("、")) };
        let summary = text(ws, "summary");
        out.push_str(&format!("- {}{alias}：{} — {}\n", ws.name, if summary.is_empty() { "未写说明" } else { summary }, ws.root));
    }
    out.push_str("</workspaces>");
    Some(out)
}

/// 本轮末尾的当前工作区摘要。
pub fn brief(ws: &WorkspaceRow) -> String {
    let mut lines = vec![format!("当前工作区：{}", ws.name), format!("根目录：{}", ws.root)];
    for (label, key) in [("说明", "summary"), ("部署", "deploy"), ("配套工具", "tools")] {
        let value = text(ws, key);
        if !value.is_empty() {
            lines.push(format!("{label}：{value}"));
        }
    }
    for (label, key) in [("验证命令", "verify_commands"), ("相关链接", "links")] {
        let items = list(ws, key);
        if !items.is_empty() {
            lines.push(format!("{label}：{}", items.join("；")));
        }
    }
    let rules = list(ws, "rules");
    if !rules.is_empty() {
        lines.push(format!("必须遵守：\n{}", rules.iter().map(|r| format!("  - {r}")).collect::<Vec<_>>().join("\n")));
    }
    let notes = list(ws, "notes");
    if !notes.is_empty() {
        let recent: Vec<_> = notes.iter().rev().take(8).rev().map(|n| format!("  - {n}")).collect();
        lines.push(format!("经验：\n{}", recent.join("\n")));
    }
    let agent = text(ws, "default_agent");
    if !agent.is_empty() {
        lines.push(format!("默认外派：{agent}"));
    }
    lines.push("未给 workdir 的 run_command 默认在根目录执行。多文件代码修改优先 delegate_to_agent 外派；git 状态和项目说明用 workspace_open 读取。".into());
    format!("\n\n<current_workspace>\n{}\n</current_workspace>", lines.join("\n"))
}

pub fn git(root: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args).current_dir(root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(crate::tasks::CREATE_NO_WINDOW);
    }
    let out = cmd.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn doc_excerpts(root: &Path, max_docs: usize, max_chars: usize) -> Vec<Value> {
    ["AGENT.md", "AGENTS.md", "CLAUDE.md", "README.md"]
        .iter()
        .filter_map(|name| {
            let text = std::fs::read_to_string(root.join(name)).ok()?;
            let total = text.chars().count();
            Some(json!({"file": name, "excerpt": text.chars().take(max_chars).collect::<String>(), "truncated": total > max_chars}))
        })
        .take(max_docs)
        .collect()
}

/// 打开工作区时现读：git 分支、未提交改动、最近提交、远端，项目说明开头。
pub fn live_context(root: &str) -> Value {
    let root = Path::new(root);
    if !root.is_dir() {
        return json!({"error": "根目录不存在或无法访问"});
    }
    let status = git(root, &["status", "--porcelain"]).unwrap_or_default();
    let changed: Vec<&str> = status.lines().take(40).collect();
    json!({
        "git": {
            "branch": git(root, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "remote": git(root, &["remote", "get-url", "origin"]),
            "uncommitted": changed,
            "uncommitted_total": status.lines().count(),
            "recent_commits": git(root, &["log", "--oneline", "-5"]).map(|s| s.lines().map(str::to_string).collect::<Vec<_>>()),
        },
        "docs": doc_excerpts(root, 2, 1500),
    })
}

/// 从文件夹推断工作区草稿：名称、验证命令、部署方式、框架。只给建议，不保存。
pub fn detect(root: &str) -> Result<Value> {
    let dir = Path::new(root.trim());
    if !dir.is_dir() {
        bail!("文件夹不存在：{root}");
    }
    let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut verify = Vec::new();
    let mut hints = Vec::new();
    let mut deploy = Vec::new();
    if let Ok(raw) = std::fs::read_to_string(dir.join("package.json")) {
        let pkg: Value = serde_json::from_str(&raw).unwrap_or_default();
        let pm = if dir.join("pnpm-lock.yaml").exists() { "pnpm" }
            else if dir.join("yarn.lock").exists() { "yarn" }
            else if dir.join("bun.lockb").exists() || dir.join("bun.lock").exists() { "bun" }
            else { "npm" };
        if let Some(scripts) = pkg.get("scripts").and_then(Value::as_object) {
            for script in ["lint", "typecheck", "test", "build"] {
                if scripts.contains_key(script) {
                    verify.push(format!("{pm} run {script}"));
                }
            }
        }
        let deps = |key: &str| pkg.get(key).and_then(Value::as_object).map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
        let all: Vec<String> = deps("dependencies").into_iter().chain(deps("devDependencies")).collect();
        for (dep, label) in [("nuxt", "Nuxt"), ("next", "Next.js"), ("vite", "Vite"), ("react", "React"), ("vue", "Vue"), ("@tauri-apps/api", "Tauri"), ("astro", "Astro"), ("svelte", "Svelte")] {
            if all.iter().any(|d| d == dep) {
                hints.push(label.to_string());
            }
        }
    }
    if dir.join("Cargo.toml").exists() {
        verify.push("cargo check".into());
        hints.push("Rust".into());
    }
    if dir.join("pyproject.toml").exists() || dir.join("requirements.txt").exists() {
        hints.push("Python".into());
    }
    if dir.join("go.mod").exists() {
        verify.push("go build ./...".into());
        hints.push("Go".into());
    }
    if dir.join("vercel.json").exists() || dir.join(".vercel").is_dir() {
        deploy.push("Vercel（关联 Git 时 push 即部署）");
    }
    if dir.join("netlify.toml").exists() {
        deploy.push("Netlify");
    }
    if dir.join("Dockerfile").exists() {
        deploy.push("Docker 镜像");
    }
    let workflows: Vec<String> = std::fs::read_dir(dir.join(".github").join("workflows"))
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    if !workflows.is_empty() {
        deploy.push("GitHub Actions");
    }
    Ok(json!({
        "name": name,
        "root": dir.to_string_lossy(),
        "summary": hints.join(" + "),
        "deploy": deploy.join("；"),
        "verify_commands": verify,
        "git": {
            "remote": git(dir, &["remote", "get-url", "origin"]),
            "branch": git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
        },
        "github_workflows": workflows,
        "docs": doc_excerpts(dir, 2, 800),
        "note": "这是推断出的草稿：别名、规则、配套工具、部署后果需要和用户确认后再 workspace_save。",
    }))
}

/// 返回会话当前绑定的工作区。
pub fn current(db: &Db, session_id: i64) -> Option<WorkspaceRow> {
    let id = db.session_workspace(session_id).ok().flatten()?;
    db.workspace_get(id).ok().flatten()
}

/// 按 id / 名称 / 别名找工作区。
pub fn resolve(db: &Db, key: &Value) -> Result<WorkspaceRow> {
    if let Some(id) = key.as_i64() {
        return db.workspace_get(id)?.ok_or_else(|| anyhow::anyhow!("工作区 {id} 不存在"));
    }
    let name = key.as_str().map(str::trim).unwrap_or("");
    if name.is_empty() {
        bail!("需要工作区名称");
    }
    if let Some(ws) = db.workspace_by_name(name)? {
        return Ok(ws);
    }
    let all = db.workspace_list()?;
    let lower = name.to_lowercase();
    let hits: Vec<_> = all
        .iter()
        .filter(|w| list(w, "aliases").iter().any(|a| a.trim().to_lowercase() == lower))
        .collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!("没有名为「{name}」的工作区；可用 workspace_list 查看"),
        _ => bail!("「{name}」对应多个工作区，请用名称"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(id: i64, name: &str, aliases: &[&str]) -> WorkspaceRow {
        WorkspaceRow {
            id,
            name: name.into(),
            root: format!("D:/{name}"),
            data: json!({"aliases": aliases}),
            last_used_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn matches_longest_alias_and_rejects_ties() {
        let all = vec![ws(1, "博客", &["blog", "blog.zhoudw.vip"]), ws(2, "拾光", &["deskhelper"])];
        assert_eq!(match_in_text(&all, "帮我改一下博客的缓存"), Some(1));
        assert_eq!(match_in_text(&all, "看看 blog.zhoudw.vip 首页"), Some(1));
        assert_eq!(match_in_text(&all, "今天天气"), None);
        let tie = vec![ws(1, "前端", &["web"]), ws(2, "后端", &["api"])];
        assert_eq!(match_in_text(&tie, "web 和 api 一起改"), None);
    }

    #[test]
    fn normalize_keeps_known_fields_only() {
        let data = normalize_data(&json!({
            "aliases": ["blog", " ", "blok"], "summary": " Nuxt ", "secret": "x",
            "links": [{"label":"站点","url":"https://a"}, {"label":"空"}], "rules": "push 前问我\n",
        })).unwrap();
        assert_eq!(data["aliases"], json!(["blog", "blok"]));
        assert_eq!(data["summary"], "Nuxt");
        assert!(data.get("secret").is_none());
        assert_eq!(data["links"], json!([{"label":"站点","url":"https://a"}]));
        assert_eq!(data["rules"], json!(["push 前问我"]));
        assert!(normalize_data(&json!({"default_agent":"gpt"})).is_err());
    }

    #[test]
    fn catalog_is_stable_regardless_of_input_order() {
        let a = vec![ws(2, "乙", &[]), ws(1, "甲", &[])];
        let b = vec![ws(1, "甲", &[]), ws(2, "乙", &[])];
        assert_eq!(catalog_block(&a), catalog_block(&b));
        assert!(catalog_block(&[]).is_none());
    }
}

// ---------- AI 工具 ----------

pub const TOOL_NAMES: &[&str] = &["workspace_list", "workspace_open", "workspace_detect", "workspace_save"];

// 工具定义在 plugins/<插件>/tools.json。

fn preview(name: &str, root: &str, data: &Value, appended: &[String]) -> String {
    let mut lines = vec![format!("名称：{name}"), format!("根目录：{root}")];
    let stub = WorkspaceRow { id: 0, name: name.into(), root: root.into(), data: data.clone(), last_used_at: None, created_at: String::new(), updated_at: String::new() };
    for (label, key) in [("别名", "aliases"), ("验证命令", "verify_commands"), ("链接", "links")] {
        let items = list(&stub, key);
        if !items.is_empty() {
            lines.push(format!("{label}：{}", items.join("、")));
        }
    }
    for (label, key) in [("说明", "summary"), ("部署", "deploy"), ("配套工具", "tools"), ("默认外派", "default_agent"), ("外派权限", "agent_mode")] {
        let v = text(&stub, key);
        if !v.is_empty() {
            lines.push(format!("{label}：{v}"));
        }
    }
    let rules = list(&stub, "rules");
    if !rules.is_empty() {
        lines.push(format!("规则：\n{}", rules.iter().map(|r| format!("· {r}")).collect::<Vec<_>>().join("\n")));
    }
    if !appended.is_empty() {
        lines.push(format!("新增经验：\n{}", appended.iter().map(|r| format!("· {r}")).collect::<Vec<_>>().join("\n")));
    }
    lines.join("\n")
}

fn summary_json(ws: &WorkspaceRow, current: Option<i64>) -> Value {
    json!({
        "id": ws.id, "name": ws.name, "root": ws.root,
        "aliases": list(ws, "aliases"), "summary": text(ws, "summary"),
        "default_agent": text(ws, "default_agent"),
        "current": current == Some(ws.id), "last_used_at": ws.last_used_at,
    })
}

/// 合并更新字段与追加经验，返回 (合并后数据, 追加的经验)。
fn merge_fields(existing: Option<&Value>, args: &Value) -> Result<(Value, Vec<String>)> {
    let mut merged = existing.cloned().unwrap_or_else(|| json!({}));
    let patch = normalize_data(args)?;
    if let (Some(base), Some(patch)) = (merged.as_object_mut(), patch.as_object()) {
        for (k, v) in patch {
            base.insert(k.clone(), v.clone());
        }
    }
    let appended: Vec<String> = args
        .get("append_notes")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(|s| clip(s, MAX_TEXT)).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    if !appended.is_empty() {
        if let Some(obj) = merged.as_object_mut() {
            let notes = obj.entry("notes").or_insert_with(|| json!([]));
            if let Some(arr) = notes.as_array_mut() {
                arr.extend(appended.iter().map(|s| json!(s)));
                let overflow = arr.len().saturating_sub(MAX_ITEMS);
                arr.drain(..overflow);
            }
        }
    }
    Ok((merged, appended))
}

pub async fn execute(
    app: &tauri::AppHandle,
    name: &str,
    args: &Value,
    cancel: &tokio_util::sync::CancellationToken,
    session_id: i64,
) -> Result<Value> {
    use tauri::{Emitter, Manager};
    let state = app.state::<crate::AppState>();
    let db = &state.db;
    match name {
        "workspace_list" => {
            let current = db.session_workspace(session_id)?;
            let items: Vec<Value> = db.workspace_list()?.iter().map(|w| summary_json(w, current)).collect();
            Ok(json!({"workspaces": items}))
        }
        "workspace_open" => {
            let ws = resolve(db, args.get("workspace").unwrap_or(&Value::Null))?;
            db.set_session_workspace(session_id, Some(ws.id))?;
            db.workspace_touch(ws.id)?;
            let _ = app.emit("workspaces-changed", ws.id);
            Ok(json!({
                "workspace": {"id": ws.id, "name": ws.name, "root": ws.root, "fields": ws.data},
                "live": live_context(&ws.root),
                "note": "已绑定到当前会话。未提交改动若与本次任务无关，外派或提交前先告诉用户。",
            }))
        }
        "workspace_detect" => detect(args.get("root").and_then(Value::as_str).unwrap_or("")),
        "workspace_save" => {
            let ws_name = args
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("需要工作区名称"))?;
            let existing = db.workspace_by_name(ws_name)?;
            let root = args
                .get("root")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| existing.as_ref().map(|w| w.root.clone()))
                .ok_or_else(|| anyhow::anyhow!("新建工作区需要 root（项目根目录绝对路径）"))?;
            if !Path::new(&root).is_dir() {
                bail!("根目录不存在：{root}");
            }
            let (merged, appended) = merge_fields(existing.as_ref().map(|w| &w.data), args)?;
            let verb = if existing.is_some() { "更新" } else { "保存" };
            let prompt = json!({"questions":[{
                "id": "save",
                "header": "工作区",
                "question": format!("{verb}工作区「{ws_name}」？"),
                "details": preview(ws_name, &root, &merged, &appended),
                "options": [{"label": verb}, {"label": "先不要"}],
            }]});
            let outcome = crate::ask_user::execute(app, &prompt, cancel).await?;
            let answer = outcome.pointer("/answers/0");
            let accepted = answer
                .and_then(|a| a.get("selected"))
                .and_then(Value::as_array)
                .is_some_and(|s| s.iter().any(|v| v.as_str() == Some(verb)));
            let remark = answer.and_then(|a| a.get("custom")).and_then(Value::as_str).unwrap_or("");
            if !accepted {
                return Ok(json!({"saved": false, "user_remark": remark, "note": "用户没有同意保存。按备注调整后可再提议，不要重复弹同样的内容。"}));
            }
            let saved = db.workspace_save(existing.map(|w| w.id).unwrap_or(0), ws_name, &root, &merged)?;
            db.set_session_workspace(session_id, Some(saved.id))?;
            let _ = app.emit("workspaces-changed", saved.id);
            Ok(json!({"saved": true, "workspace": summary_json(&saved, Some(saved.id)), "user_remark": remark}))
        }
        _ => bail!("未知工作区工具 {name}"),
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    #[test]
    fn merge_overrides_given_fields_and_appends_notes() {
        let existing = json!({"summary":"旧","rules":["a"],"notes":["n1"]});
        let (merged, appended) = merge_fields(Some(&existing), &json!({"summary":"新","append_notes":["n2"]})).unwrap();
        assert_eq!(merged["summary"], "新");
        assert_eq!(merged["rules"], json!(["a"]));
        assert_eq!(merged["notes"], json!(["n1", "n2"]));
        assert_eq!(appended, vec!["n2".to_string()]);
    }
}
