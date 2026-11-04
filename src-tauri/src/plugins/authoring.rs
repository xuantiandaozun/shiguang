//! 拾光自己新增/修改用户插件的流程：暂存 → 校验 → 确认（可先预览）→ 存版本快照 → 生效。
//! 内置插件只读；任何修改都能回滚到之前的版本。
use super::{build_snapshot, Dirs, PLUGIN_FILE};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

const MAX_FILE_BYTES: usize = 512 * 1024;
const MAX_FILES: usize = 200;
const KEEP_VERSIONS: usize = 20;

fn user_root() -> Result<PathBuf> {
    super::host().dirs().user.ok_or_else(|| anyhow!("用户插件目录尚未初始化"))
}

fn history_root(id: &str) -> Result<PathBuf> {
    Ok(user_root()?.join(".history").join(id))
}

fn ensure_editable(id: &str) -> Result<()> {
    if !super::valid_plugin_id(id) {
        bail!("插件 id 只能包含小写字母、数字、- 和 _");
    }
    if let Some(builtin) = super::host().dirs().builtin {
        if builtin.join(id).join(PLUGIN_FILE).is_file() {
            bail!("{id} 是内置插件，只读。请新建一个用户插件（换一个 id）来扩展或替代它。");
        }
    }
    Ok(())
}

fn check_rel(rel: &str) -> Result<()> {
    let p = Path::new(rel);
    if rel.is_empty() || p.is_absolute() || rel.contains(':') || rel.contains('\\') {
        bail!("文件路径必须是插件内的相对路径（用 /）：{rel}");
    }
    if p.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        bail!("文件路径不能包含 .. 或 .：{rel}");
    }
    if rel.split('/').any(|seg| seg.starts_with('.')) {
        bail!("不能写隐藏文件：{rel}");
    }
    Ok(())
}

pub fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn list_files(root: &Path, dir: &Path, out: &mut Vec<Value>) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        if path.is_dir() {
            list_files(root, &path, out)?;
        } else {
            out.push(json!({"path": rel, "bytes": entry.metadata().map(|m| m.len()).unwrap_or(0)}));
        }
    }
    Ok(())
}

/// 校验一个插件目录：清单、工具、脚本语法、界面入口。返回问题列表（空表示通过）。
pub fn validate(dir: &Path, expected_id: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let root = match dir.parent() {
        Some(root) => root.to_path_buf(),
        None => return vec!["目录不合法".into()],
    };
    let builtin = super::host().dirs().builtin;
    let snapshot = build_snapshot(&Dirs { builtin: builtin.clone(), user: Some(root) }, &Default::default(), false, 0);
    let Some(plugin) = snapshot.plugin(expected_id).filter(|p| !p.builtin) else {
        let mut found = false;
        for e in &snapshot.errors {
            if e.path.starts_with(&dir.display().to_string()) || e.plugin == expected_id {
                problems.push(e.message.clone());
                found = true;
            }
        }
        if !found {
            problems.push(format!("{PLUGIN_FILE} 缺失，或其中的 id 不是 {expected_id}"));
        }
        return problems;
    };
    for e in snapshot.errors.iter().filter(|e| e.plugin == expected_id) {
        problems.push(format!("{}: {}", e.path, e.message));
    }
    let manifest = &plugin.manifest;
    // 脚本：hooks 与 script 执行器引用的文件必须存在且能通过语法检查。
    let mut scripts: Vec<String> = manifest.hooks.iter().map(|h| h.script.clone()).collect();
    for tool in snapshot.tools.iter().filter(|t| t.plugin == expected_id) {
        if tool.executor_kind() == "script" {
            if let Some(file) = tool.executor.get("file").and_then(Value::as_str) {
                scripts.push(file.to_string());
            }
        }
    }
    scripts.sort();
    scripts.dedup();
    for file in scripts {
        match super::script::read_script(plugin, &file) {
            Ok(source) => {
                if let Err(e) = syntax_check(&source) {
                    problems.push(format!("{file} 语法错误：{e}"));
                }
            }
            Err(e) => problems.push(format!("{e:#}")),
        }
    }
    // 界面入口：entry 文件存在，view 是合法 JSON。
    for (slot, item) in ui_items(&manifest.ui) {
        for (field, must_json) in [("entry", false), ("view", true)] {
            let Some(rel) = item.get(field).and_then(Value::as_str) else { continue };
            let rel = rel.split(['?', '#']).next().unwrap_or(rel);
            match super::resolve_inside(&plugin.dir, rel, false) {
                Ok(path) if path.is_file() => {
                    if must_json {
                        if let Err(e) = std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|t| serde_json::from_str::<Value>(&t).map_err(Into::into)) {
                            problems.push(format!("ui.{slot} 的 {rel} 不是合法 JSON：{e}"));
                        }
                    }
                }
                Ok(_) => problems.push(format!("ui.{slot} 引用的 {rel} 不存在")),
                Err(e) => problems.push(e.to_string()),
            }
            if item.get("component").is_some() {
                problems.push(format!("ui.{slot} 的 component 只供内置插件使用，用户插件请用 entry 或 view"));
            }
        }
    }
    if let Some(rel) = &manifest.settings {
        if let Err(e) = snapshot.read_json(expected_id, rel) {
            problems.push(format!("settings：{e:#}"));
        }
    }
    problems
}

/// ui 里所有带 entry/view/component 的挂载项：(挂载点, 配置)。
pub fn ui_items(ui: &Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let Some(obj) = ui.as_object() else { return out };
    for (slot, value) in obj {
        let items: Vec<Value> = match value {
            Value::Array(items) => items.clone(),
            Value::Object(_) => vec![value.clone()],
            _ => continue,
        };
        for item in items {
            if item.get("entry").is_some() || item.get("view").is_some() || item.get("component").is_some() {
                out.push((slot.clone(), item));
            }
        }
    }
    out
}

fn syntax_check(source: &str) -> Result<()> {
    let rt = rquickjs::Runtime::new().map_err(|e| anyhow!("{e}"))?;
    let ctx = rquickjs::Context::full(&rt).map_err(|e| anyhow!("{e}"))?;
    ctx.with(|js| {
        use rquickjs::CatchResultExt;
        js.globals().set("__src", source.to_string()).map_err(|e| anyhow!("{e}"))?;
        js.eval::<(), _>("new Function('module','exports','sg', __src)").catch(&js).map_err(|e| anyhow!("{e}"))
    })
}

fn timestamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S%3f").to_string()
}

/// 把当前版本存进历史，返回版本号。插件不存在时返回 None。
fn snapshot_version(id: &str, reason: &str) -> Result<Option<String>> {
    let live = user_root()?.join(id);
    if !live.join(PLUGIN_FILE).is_file() {
        return Ok(None);
    }
    let version = timestamp();
    let dst = history_root(id)?.join(&version);
    copy_dir(&live, &dst)?;
    std::fs::write(dst.join(".version.json"), json!({"reason": reason, "at": crate::db::now_str()}).to_string())?;
    prune_history(id)?;
    Ok(Some(version))
}

fn prune_history(id: &str) -> Result<()> {
    let root = history_root(id)?;
    let mut versions = list_versions_raw(&root);
    while versions.len() > KEEP_VERSIONS {
        let oldest = versions.remove(0);
        let _ = std::fs::remove_dir_all(root.join(oldest));
    }
    Ok(())
}

fn list_versions_raw(root: &Path) -> Vec<String> {
    let mut versions: Vec<String> = std::fs::read_dir(root)
        .map(|it| it.flatten().filter(|e| e.path().is_dir()).filter_map(|e| e.file_name().to_str().map(str::to_string)).collect())
        .unwrap_or_default();
    versions.sort();
    versions
}

fn replace_live(id: &str, from: &Path) -> Result<()> {
    let live = user_root()?.join(id);
    if live.exists() {
        std::fs::remove_dir_all(&live).with_context(|| format!("清理旧版本 {}", live.display()))?;
    }
    copy_dir(from, &live)
}

async fn confirm(app: &AppHandle, cancel: &CancellationToken, header: &str, question: String, details: String, options: &[&str]) -> Result<(String, String)> {
    let prompt = json!({"questions":[{
        "id": "plugin",
        "header": header,
        "question": question,
        "details": details,
        "options": options.iter().map(|o| json!({"label": o})).collect::<Vec<_>>(),
    }]});
    let outcome = crate::ask_user::execute(app, &prompt, cancel).await?;
    let answer = outcome.pointer("/answers/0");
    let selected = answer
        .and_then(|a| a.get("selected"))
        .and_then(Value::as_array)
        .and_then(|s| s.first())
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let remark = answer.and_then(|a| a.get("custom")).and_then(Value::as_str).unwrap_or("").to_string();
    Ok((selected, remark))
}

fn emit_preview(app: &AppHandle, id: &str) {
    crate::windows::open_main(app);
    let _ = app.emit("plugin-preview", json!({"id": id}));
}

/// plugin_write：files 为 {相对路径: 内容 | null(删除)}。
pub async fn write(app: &AppHandle, args: &Value, cancel: &CancellationToken) -> Result<Value> {
    let id = args.get("plugin_id").and_then(Value::as_str).unwrap_or_default().trim().to_string();
    ensure_editable(&id)?;
    let summary = args.get("summary").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let files = args.get("files").and_then(Value::as_object).ok_or_else(|| anyhow!("files 必须是 {{路径: 内容}} 对象"))?;
    if files.is_empty() {
        bail!("files 不能为空");
    }
    let root = user_root()?;
    let live = root.join(&id);
    let stage_root = root.join(".staging").join(uuid::Uuid::new_v4().to_string());
    let stage = stage_root.join(&id);
    let result = async {
        if live.is_dir() {
            copy_dir(&live, &stage)?;
        } else {
            std::fs::create_dir_all(&stage)?;
        }
        let mut changes = Vec::new();
        for (rel, content) in files {
            check_rel(rel)?;
            let path = stage.join(rel);
            match content {
                Value::Null => {
                    if path.is_file() {
                        std::fs::remove_file(&path)?;
                        changes.push(format!("删除 {rel}"));
                    }
                }
                Value::String(text) => {
                    if text.len() > MAX_FILE_BYTES {
                        bail!("{rel} 超过 512KB");
                    }
                    let existed = path.is_file();
                    let same = existed && std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str());
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&path, text)?;
                    if !same {
                        changes.push(format!("{} {rel}（{} 字节）", if existed { "修改" } else { "新增" }, text.len()));
                    }
                }
                _ => bail!("{rel} 的内容必须是字符串或 null"),
            }
        }
        let mut all = Vec::new();
        list_files(&stage, &stage, &mut all)?;
        if all.len() > MAX_FILES {
            bail!("插件文件超过 {MAX_FILES} 个");
        }
        if changes.is_empty() {
            return Ok(json!({"applied": false, "note": "内容与现有版本相同，没有变化。"}));
        }
        let problems = validate(&stage, &id);
        if !problems.is_empty() {
            return Ok(json!({"applied": false, "valid": false, "problems": problems, "note": "校验没通过，插件没有改动。修正后重新提交。规范见 docs/PLUGINS.md，或用 plugin_read 读取 core 插件的 PLUGIN_GUIDE.md。"}));
        }
        let manifest: Value = serde_json::from_str(&std::fs::read_to_string(stage.join(PLUGIN_FILE))?)?;
        let name = manifest["name"].as_str().unwrap_or(&id).to_string();
        let permissions = manifest["permissions"].as_array().map(|p| p.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("、")).unwrap_or_default();
        let has_ui = manifest.get("ui").is_some_and(|ui| !ui.is_null());
        let verb = if live.is_dir() { "修改" } else { "新增" };
        let details = format!(
            "{summary}\n\n权限：{}\n改动：\n- {}",
            if permissions.is_empty() { "无".to_string() } else { permissions },
            changes.join("\n- ")
        );
        let mut options = vec!["应用", "放弃"];
        if has_ui {
            options.insert(1, "先预览再决定");
        }
        let (choice, remark) = confirm(app, cancel, "插件", format!("{verb}插件「{name}」（{id}）？"), details.clone(), &options).await?;
        if choice != "应用" && choice != "先预览再决定" {
            return Ok(json!({"applied": false, "user_remark": remark, "note": "用户没有同意。按备注调整后可再提交，不要重复弹同样的内容。"}));
        }
        let version = snapshot_version(&id, &format!("{verb}前：{summary}"))?;
        replace_live(&id, &stage)?;
        super::host().reload();
        if choice == "先预览再决定" {
            emit_preview(app, &id);
            let (keep, remark2) = confirm(app, cancel, "插件预览", format!("已在主窗口打开「{name}」预览，保留这次改动吗？"), details, &["保留", "撤销"]).await?;
            if keep != "保留" {
                match &version {
                    Some(v) => restore(&id, v)?,
                    None => {
                        std::fs::remove_dir_all(root.join(&id))?;
                    }
                }
                super::host().reload();
                return Ok(json!({"applied": false, "reverted": true, "user_remark": remark2, "note": "用户预览后撤销了改动，已恢复原样。"}));
            }
        }
        let snapshot = super::snapshot();
        let errors: Vec<_> = snapshot.errors.iter().filter(|e| e.plugin == id).map(|e| e.message.clone()).collect();
        Ok(json!({
            "applied": true,
            "plugin_id": id,
            "previous_version": version,
            "user_remark": remark,
            "tools": snapshot.tools.iter().filter(|t| t.plugin == id).map(|t| t.name.clone()).collect::<Vec<_>>(),
            "load_errors": errors,
            "note": "已生效（工具从下一轮起可用，界面已刷新）。如需撤销用 plugin_rollback。"
        }))
    }
    .await;
    let _ = std::fs::remove_dir_all(&stage_root);
    result
}

fn restore(id: &str, version: &str) -> Result<()> {
    let src = history_root(id)?.join(version);
    if !src.join(PLUGIN_FILE).is_file() {
        bail!("版本 {version} 不存在");
    }
    let tmp = user_root()?.join(".staging").join(uuid::Uuid::new_v4().to_string());
    copy_dir(&src, &tmp)?;
    let _ = std::fs::remove_file(tmp.join(".version.json"));
    let result = replace_live(id, &tmp);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

pub fn versions(id: &str) -> Result<Vec<Value>> {
    let root = history_root(id)?;
    Ok(list_versions_raw(&root)
        .into_iter()
        .rev()
        .map(|v| {
            let meta: Value = std::fs::read_to_string(root.join(&v).join(".version.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
            json!({"version": v, "reason": meta["reason"], "at": meta["at"]})
        })
        .collect())
}

pub async fn rollback(app: &AppHandle, args: &Value, cancel: &CancellationToken) -> Result<Value> {
    let id = args.get("plugin_id").and_then(Value::as_str).unwrap_or_default().to_string();
    ensure_editable(&id)?;
    let Some(version) = args.get("version").and_then(Value::as_str) else {
        return Ok(json!({"plugin_id": id, "versions": versions(&id)?, "note": "指定 version 才会回滚。"}));
    };
    let (choice, remark) = confirm(app, cancel, "插件", format!("把插件 {id} 回滚到 {version}？"), "当前版本会先存进历史，之后还能再恢复。".into(), &["回滚", "先不要"]).await?;
    if choice != "回滚" {
        return Ok(json!({"rolled_back": false, "user_remark": remark}));
    }
    let saved = snapshot_version(&id, &format!("回滚到 {version} 前"))?;
    restore(&id, version)?;
    super::host().reload();
    Ok(json!({"rolled_back": true, "plugin_id": id, "version": version, "saved_current_as": saved}))
}

/// 界面里用户本人点的回滚，不再确认。
pub fn rollback_direct(id: &str, version: &str) -> Result<Value> {
    ensure_editable(id)?;
    let saved = snapshot_version(id, &format!("回滚到 {version} 前"))?;
    restore(id, version)?;
    super::host().reload();
    Ok(json!({"rolled_back": true, "plugin_id": id, "version": version, "saved_current_as": saved}))
}

pub async fn delete(app: &AppHandle, args: &Value, cancel: &CancellationToken) -> Result<Value> {
    let id = args.get("plugin_id").and_then(Value::as_str).unwrap_or_default().to_string();
    ensure_editable(&id)?;
    let live = user_root()?.join(&id);
    if !live.is_dir() {
        bail!("用户插件 {id} 不存在");
    }
    let (choice, remark) = confirm(app, cancel, "插件", format!("删除插件 {id}？"), "删除前会存进历史，可用 plugin_rollback 恢复。".into(), &["删除", "先不要"]).await?;
    if choice != "删除" {
        return Ok(json!({"deleted": false, "user_remark": remark}));
    }
    let saved = snapshot_version(&id, "删除前")?;
    std::fs::remove_dir_all(&live)?;
    super::host().reload();
    Ok(json!({"deleted": true, "saved_as": saved}))
}

pub fn read(args: &Value) -> Result<Value> {
    let id = args.get("plugin_id").and_then(Value::as_str).unwrap_or_default();
    let snapshot = super::snapshot();
    let plugin = snapshot.plugin(id).ok_or_else(|| anyhow!("插件 {id} 不存在"))?;
    match args.get("path").and_then(Value::as_str).filter(|p| !p.is_empty()) {
        None => {
            let mut files = Vec::new();
            list_files(&plugin.dir, &plugin.dir, &mut files)?;
            Ok(json!({"plugin_id": id, "builtin": plugin.builtin, "dir": plugin.dir, "files": files}))
        }
        Some(rel) => {
            check_rel(rel)?;
            let path = plugin.dir.join(rel);
            let text = std::fs::read_to_string(&path).with_context(|| format!("读取 {rel}"))?;
            let clipped: String = text.chars().take(60_000).collect();
            Ok(json!({"plugin_id": id, "path": rel, "content": clipped, "truncated": clipped.len() < text.len()}))
        }
    }
}

pub fn list() -> Value {
    let snapshot = super::snapshot();
    json!({
        "user_dir": super::host().dirs().user,
        "safe_mode": super::host().safe_mode(),
        "plugins": snapshot.plugins.iter().map(|p| json!({
            "id": p.manifest.id,
            "name": p.manifest.name,
            "version": p.manifest.version,
            "description": p.manifest.description,
            "builtin": p.builtin,
            "enabled": p.enabled,
            "permissions": p.manifest.permissions,
            "tools": snapshot.tools.iter().filter(|t| t.plugin == p.manifest.id).map(|t| t.name.clone()).collect::<Vec<_>>(),
            "ui_slots": ui_items(&p.manifest.ui).into_iter().map(|(s, _)| s).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "errors": snapshot.errors,
    })
}

pub fn set_enabled(app: &AppHandle, id: &str, enabled: bool) -> Result<Value> {
    let snapshot = super::snapshot();
    let plugin = snapshot.plugin(id).ok_or_else(|| anyhow!("插件 {id} 不存在"))?;
    if plugin.builtin && id == "core" && !enabled {
        bail!("core 插件不能停用");
    }
    let state = tauri::Manager::state::<crate::AppState>(app);
    let mut overrides = super::host().overrides();
    overrides.insert(id.to_string(), enabled);
    super::settings::save_overrides(&state.db, &overrides)?;
    super::host().set_override(id, enabled);
    Ok(json!({"plugin_id": id, "enabled": enabled}))
}

/// 内核执行入口（plugins 插件里的工具）。
pub async fn execute(app: &AppHandle, name: &str, args: &Value, cancel: &CancellationToken) -> Result<Value> {
    match name {
        "plugin_list" => Ok(list()),
        "plugin_read" => read(args),
        "plugin_write" => write(app, args, cancel).await,
        "plugin_rollback" => rollback(app, args, cancel).await,
        "plugin_delete" => delete(app, args, cancel).await,
        "plugin_enable" => {
            let id = args.get("plugin_id").and_then(Value::as_str).unwrap_or_default();
            let enabled = args.get("enabled").and_then(Value::as_bool).unwrap_or(true);
            set_enabled(app, id, enabled)
        }
        "plugin_preview" => {
            let id = args.get("plugin_id").and_then(Value::as_str).unwrap_or_default();
            super::snapshot().plugin(id).ok_or_else(|| anyhow!("插件 {id} 不存在"))?;
            emit_preview(app, id);
            Ok(json!({"opened": true}))
        }
        _ => bail!("未知插件工具 {name}"),
    }
}

pub const TOOL_NAMES: &[&str] = &["plugin_list", "plugin_read", "plugin_write", "plugin_rollback", "plugin_delete", "plugin_enable", "plugin_preview"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_are_strict() {
        assert!(check_rel("ui/index.html").is_ok());
        assert!(check_rel("../x").is_err());
        assert!(check_rel("a\\b").is_err());
        assert!(check_rel(".history/x").is_err());
        assert!(check_rel("/x").is_err());
    }

    #[test]
    fn syntax_check_catches_errors() {
        assert!(syntax_check("module.exports.run = (a) => a;").is_ok());
        assert!(syntax_check("function (").is_err());
    }
}

#[cfg(test)]
mod example_tests {
    use super::*;

    /// docs/plugin-examples/quick-notes 作为用户插件：能通过校验，脚本工具能完整跑通。
    #[test]
    fn quick_notes_example_validates_and_runs() {
        let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/plugin-examples/quick-notes");
        let root = std::env::temp_dir().join(format!("sg-example-{}", uuid::Uuid::new_v4()));
        let dir = root.join("quick-notes");
        copy_dir(&example, &dir).unwrap();
        let problems = validate(&dir, "quick-notes");
        assert!(problems.is_empty(), "{problems:?}");

        let snapshot = build_snapshot(&Dirs { builtin: super::super::host().dirs().builtin, user: Some(root.clone()) }, &Default::default(), false, 1);
        let plugin = snapshot.plugin("quick-notes").unwrap().clone();
        assert!(snapshot.tools_for_category("notes").contains(&"notes_add".to_string()));
        let source = super::super::script::read_script(&plugin, "notes.js").unwrap();
        let ctx = super::super::script::HostCtx { app: None, plugin, session_id: 0, data_dir: Some(root.join(".data")) };
        let run = |f: &str, args: Value| super::super::script::run_blocking(ctx.clone(), &source, "notes.js", f, &args, super::super::script::HOOK_TIMEOUT);
        let added = run("add", json!({"text": "买牛奶"})).unwrap();
        assert_eq!(added["saved"], true);
        let listed = run("list", json!({})).unwrap();
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["items"][0]["text"], "买牛奶");
        let id = listed["items"][0]["id"].as_str().unwrap().to_string();
        assert!(run("add", json!({"text": "  "})).is_err());
        assert_eq!(run("remove", json!({"id": id})).unwrap()["count"], 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn validation_reports_broken_plugins() {
        let root = std::env::temp_dir().join(format!("sg-broken-{}", uuid::Uuid::new_v4()));
        let dir = root.join("broken");
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::write(dir.join(PLUGIN_FILE), r#"{"id":"broken","name":"坏","permissions":[],"tools":["tools.json"],"ui":{"tabs":[{"id":"x","view":"ui/missing.json"}]}}"#).unwrap();
        std::fs::write(dir.join("tools.json"), r#"[{"name":"b_run","description":"x","executor":{"kind":"script","file":"x.js"}}]"#).unwrap();
        let problems = validate(&dir, "broken");
        assert!(problems.iter().any(|p| p.contains("script")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("missing.json")), "{problems:?}");
        let _ = std::fs::remove_dir_all(root);
    }
}
