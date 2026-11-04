//! 工具调用钩子。插件在 plugin.json 的 hooks 里声明脚本：
//! - before_tool：输入 {tool, args, session_id}，返回 {block:"原因"} 拦截，或 {args:{...}} 改写参数，或 null 放行。
//! - after_tool：输入 {tool, args, result, session_id}，返回 {result:...} 替换结果，或 {note:"..."} 追加提示，或 null。
//! 钩子出错只记日志，不影响工具本身。用户插件需要 hooks 权限。
use super::script::{self, HostCtx, HOOK_TIMEOUT};
use serde_json::{json, Value};
use tauri::AppHandle;

pub enum Before {
    Allow(Value),
    Block(String),
}

fn matching(event: &str, tool: &str) -> Vec<(super::Plugin, super::manifest::HookSpec)> {
    let snapshot = super::snapshot();
    let mut out = Vec::new();
    for plugin in snapshot.enabled_plugins() {
        if !plugin.builtin && !plugin.manifest.permissions.iter().any(|p| p == "hooks") {
            continue;
        }
        for hook in &plugin.manifest.hooks {
            if hook.event == event && (hook.tools.is_empty() || hook.tools.iter().any(|t| t == tool)) {
                out.push((plugin.clone(), hook.clone()));
            }
        }
    }
    out
}

async fn run(app: &AppHandle, plugin: super::Plugin, hook: super::manifest::HookSpec, input: Value, session_id: i64) -> Option<Value> {
    let source = match script::read_script(&plugin, &hook.script) {
        Ok(source) => source,
        Err(e) => {
            log::warn!("插件 {} 钩子脚本读取失败: {e:#}", plugin.manifest.id);
            return None;
        }
    };
    let label = format!("{}/{}", plugin.manifest.id, hook.script);
    let ctx = HostCtx::new(app.clone(), plugin, session_id);
    let result = tauri::async_runtime::spawn_blocking(move || script::run_blocking(ctx, &source, &label, &hook.function, &input, HOOK_TIMEOUT)).await;
    match result {
        Ok(Ok(value)) if !value.is_null() => Some(value),
        Ok(Ok(_)) => None,
        Ok(Err(e)) => {
            log::warn!("插件钩子出错: {e:#}");
            None
        }
        Err(e) => {
            log::warn!("插件钩子线程异常: {e}");
            None
        }
    }
}

pub async fn before_tool(app: &AppHandle, tool: &str, args: &Value, session_id: i64) -> Before {
    let hooks = matching("before_tool", tool);
    let mut args = args.clone();
    for (plugin, hook) in hooks {
        let id = plugin.manifest.id.clone();
        let input = json!({"tool": tool, "args": args, "session_id": session_id});
        let Some(out) = run(app, plugin, hook, input, session_id).await else { continue };
        if let Some(reason) = out.get("block").and_then(Value::as_str) {
            return Before::Block(format!("插件 {id} 拦截了这次调用：{reason}"));
        }
        if let Some(new_args) = out.get("args").filter(|v| v.is_object()) {
            args = new_args.clone();
        }
    }
    Before::Allow(args)
}

pub async fn after_tool(app: &AppHandle, tool: &str, args: &Value, result: Value, session_id: i64) -> Value {
    let hooks = matching("after_tool", tool);
    let mut result = result;
    for (plugin, hook) in hooks {
        let id = plugin.manifest.id.clone();
        let input = json!({"tool": tool, "args": args, "result": result, "session_id": session_id});
        let Some(out) = run(app, plugin, hook, input, session_id).await else { continue };
        if let Some(replaced) = out.get("result") {
            result = replaced.clone();
        }
        if let Some(note) = out.get("note").and_then(Value::as_str) {
            if let Some(obj) = result.as_object_mut() {
                let notes = obj.entry("plugin_notes").or_insert_with(|| json!([]));
                if let Some(list) = notes.as_array_mut() {
                    list.push(json!(format!("[{id}] {note}")));
                }
            }
        }
    }
    result
}

/// 快速判断是否有任何钩子，避免每次工具调用都遍历。
pub fn any_hooks() -> bool {
    super::snapshot().enabled_plugins().any(|p| !p.manifest.hooks.is_empty())
}
