//! 工具执行器。builtin 由内核按工具名执行（llm::tools::execute），这里处理其余类型。
//!
//! | kind | 配置 | 权限 |
//! |---|---|---|
//! | builtin | 无 | 仅内置插件 |
//! | alias | target, args（默认参数） | tools:<target> 或 tools:* |
//! | browser_action | action, extension_only | browser |
//! | command | argv（模板）, stdin=json/none, timeout_seconds, cwd | command |
//! | http | method, url（模板）, headers, body=args/none, timeout_seconds | net |
//! | mcp | server, tool | mcp |
//! | script | file, function | script |
use super::{Plugin, ToolSpec};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio_util::sync::CancellationToken;

pub const KINDS: &[&str] = &["builtin", "alias", "browser_action", "command", "http", "mcp", "script"];

const MAX_OUTPUT_CHARS: usize = 200_000;

pub fn required_permission(kind: &str) -> Option<&'static str> {
    match kind {
        "browser_action" => Some("browser"),
        "command" => Some("command"),
        "http" => Some("net"),
        "mcp" => Some("mcp"),
        "script" => Some("script"),
        _ => None,
    }
}

/// 插件是否可以调用某个工具（alias 目标、脚本里的 sg.callTool）。内置插件不受限。
pub fn may_call_tool(plugin: &Plugin, tool: &str) -> bool {
    plugin.builtin
        || plugin.manifest.permissions.iter().any(|p| p == "tools:*" || p.strip_prefix("tools:") == Some(tool))
}

/// 执行非 builtin 工具。builtin 返回 None，交给内核。
pub async fn execute(
    app: &AppHandle,
    spec: &ToolSpec,
    args: &Value,
    cancel: &CancellationToken,
    session_id: i64,
) -> Option<Result<Value>> {
    let kind = spec.executor_kind().to_string();
    if kind == "builtin" {
        return None;
    }
    let snapshot = super::snapshot();
    let Some(plugin) = snapshot.plugin(&spec.plugin).cloned() else {
        return Some(Err(anyhow!("工具 {} 所属插件 {} 已不存在", spec.name, spec.plugin)));
    };
    let config = &spec.executor;
    let result = match kind.as_str() {
        "alias" => alias(app, &plugin, config, args, cancel, session_id).await,
        "browser_action" => browser_action(app, config, args).await,
        "command" => command(&plugin, config, args, cancel).await,
        "http" => http(config, args, cancel).await,
        "mcp" => {
            let server = config.get("server").and_then(Value::as_str).unwrap_or_default();
            let tool = config.get("tool").and_then(Value::as_str).unwrap_or(&spec.name);
            super::mcp::call(&plugin, server, tool, args, cancel).await
        }
        "script" => {
            let file = config.get("file").and_then(Value::as_str).unwrap_or_default().to_string();
            let function = config.get("function").and_then(Value::as_str).unwrap_or("run").to_string();
            super::script::run_tool(app.clone(), plugin, file, function, args.clone(), session_id).await
        }
        other => Err(anyhow!("工具 {} 的执行器 {other} 尚未受支持", spec.name)),
    };
    Some(result)
}

async fn alias(
    app: &AppHandle,
    plugin: &Plugin,
    config: &Value,
    args: &Value,
    cancel: &CancellationToken,
    session_id: i64,
) -> Result<Value> {
    let target = config.get("target").and_then(Value::as_str).ok_or_else(|| anyhow!("alias 执行器缺少 target"))?;
    if !may_call_tool(plugin, target) {
        bail!("插件 {} 没有调用 {target} 的权限（permissions 需包含 tools:{target}）", plugin.manifest.id);
    }
    let mut merged = config.get("args").cloned().unwrap_or_else(|| json!({}));
    if let (Some(base), Some(extra)) = (merged.as_object_mut(), args.as_object()) {
        for (k, v) in extra {
            base.insert(k.clone(), v.clone());
        }
    }
    Box::pin(crate::llm::tools::execute(app, target, &merged, cancel, session_id)).await
}

async fn browser_action(app: &AppHandle, config: &Value, args: &Value) -> Result<Value> {
    let action = config.get("action").and_then(Value::as_str).ok_or_else(|| anyhow!("browser_action 执行器缺少 action"))?;
    let mut params = args.clone();
    if config.get("extension_only").and_then(Value::as_bool).unwrap_or(true) {
        params["channel"] = json!("extension");
    }
    let state = app.state::<crate::AppState>();
    state.browser.call(action, params).await
}

/// 模板替换：{plugin_dir}、{args_json}、{args.字段}。
pub fn render_template(template: &str, plugin: &Plugin, args: &Value, url_encode: bool) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let key = &after[..end];
        let value = match key {
            "plugin_dir" => Some(plugin.dir.display().to_string()),
            "args_json" => Some(args.to_string()),
            _ => key.strip_prefix("args.").map(|field| match args.get(field) {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            }),
        };
        match value {
            Some(v) if url_encode && key != "plugin_dir" => out.push_str(&url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()),
            Some(v) => out.push_str(&v),
            None => {
                out.push('{');
                out.push_str(key);
                out.push('}');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn parse_output(text: &str) -> Value {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return value;
    }
    let clipped: String = trimmed.chars().take(MAX_OUTPUT_CHARS).collect();
    json!({ "output": clipped, "truncated": clipped.len() < trimmed.len() })
}

async fn command(plugin: &Plugin, config: &Value, args: &Value, cancel: &CancellationToken) -> Result<Value> {
    let argv: Vec<String> = config
        .get("argv")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("command 执行器缺少 argv"))?
        .iter()
        .filter_map(Value::as_str)
        .map(|item| render_template(item, plugin, args, false))
        .collect();
    let (program, rest) = argv.split_first().ok_or_else(|| anyhow!("command 执行器 argv 为空"))?;
    let cwd = match config.get("cwd").and_then(Value::as_str) {
        Some(rel) => super::resolve_inside(&plugin.dir, rel, plugin.builtin)?,
        None => plugin.dir.clone(),
    };
    let timeout = config.get("timeout_seconds").and_then(Value::as_u64).unwrap_or(60).clamp(1, 3600);
    let send_stdin = config.get("stdin").and_then(Value::as_str).unwrap_or("json") == "json";
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(rest)
        .current_dir(&cwd)
        .stdin(if send_stdin { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .env("SHIGUANG_PLUGIN_ID", &plugin.manifest.id)
        .env("SHIGUANG_PLUGIN_DIR", &plugin.dir);
    #[cfg(windows)]
    cmd.creation_flags(crate::tasks::CREATE_NO_WINDOW);
    let mut child = cmd.spawn().with_context(|| format!("启动 {program} 失败"))?;
    if send_stdin {
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(args.to_string().as_bytes()).await?;
        }
    }
    let output = tokio::select! {
        _ = cancel.cancelled() => bail!("已取消"),
        _ = tokio::time::sleep(Duration::from_secs(timeout)) => bail!("命令超过 {timeout} 秒未结束，已终止"),
        output = child.wait_with_output() => output?,
    };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        bail!(
            "命令退出码 {}：{}",
            output.status.code().unwrap_or(-1),
            stderr.trim().chars().take(4000).collect::<String>()
        );
    }
    Ok(parse_output(&stdout))
}

async fn http(config: &Value, args: &Value, cancel: &CancellationToken) -> Result<Value> {
    let snapshot_plugin = Plugin {
        manifest: serde_json::from_value(json!({"id":"_","name":"_"}))?,
        dir: Default::default(),
        builtin: false,
        enabled: true,
    };
    let url_template = config.get("url").and_then(Value::as_str).ok_or_else(|| anyhow!("http 执行器缺少 url"))?;
    let url = render_template(url_template, &snapshot_plugin, args, true);
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        bail!("http 执行器只支持 http/https 地址");
    }
    let method = config.get("method").and_then(Value::as_str).unwrap_or("GET").to_ascii_uppercase();
    let timeout = config.get("timeout_seconds").and_then(Value::as_u64).unwrap_or(30).clamp(1, 300);
    let client = reqwest::Client::builder().timeout(Duration::from_secs(timeout)).build()?;
    let mut request = client.request(reqwest::Method::from_bytes(method.as_bytes())?, &url);
    if let Some(headers) = config.get("headers").and_then(Value::as_object) {
        for (k, v) in headers {
            if let Some(v) = v.as_str() {
                request = request.header(k, render_template(v, &snapshot_plugin, args, false));
            }
        }
    }
    if config.get("body").and_then(Value::as_str) == Some("args") {
        request = request.json(args);
    }
    let response = tokio::select! {
        _ = cancel.cancelled() => bail!("已取消"),
        response = request.send() => response?,
    };
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        bail!("HTTP {}：{}", status.as_u16(), text.chars().take(2000).collect::<String>());
    }
    Ok(parse_output(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(permissions: &[&str]) -> Plugin {
        Plugin {
            manifest: serde_json::from_value(json!({"id":"p","name":"P","permissions":permissions})).unwrap(),
            dir: std::path::PathBuf::from("D:/plugins/p"),
            builtin: false,
            enabled: true,
        }
    }

    #[test]
    fn templates_fill_args_and_encode_urls() {
        let p = plugin(&[]);
        let args = json!({"q":"a b&c","n":3});
        assert_eq!(render_template("x?q={args.q}&n={args.n}", &p, &args, true), "x?q=a+b%26c&n=3");
        assert_eq!(render_template("{plugin_dir}/s.py {args.q} {unknown}", &p, &args, false), "D:/plugins/p/s.py a b&c {unknown}");
    }

    #[test]
    fn tool_permissions_are_explicit() {
        assert!(!may_call_tool(&plugin(&[]), "run_command"));
        assert!(may_call_tool(&plugin(&["tools:run_command"]), "run_command"));
        assert!(may_call_tool(&plugin(&["tools:*"]), "anything"));
    }
}
