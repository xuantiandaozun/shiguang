//! 命令行参数与事件解析：Claude Code（stream-json 常驻）；Codex（exec --json）、Cursor（-p stream-json）、Pi（--mode json）每轮一进程。
//! 纯函数，便于测试；进程管理在 mod.rs。
use serde_json::{json, Value};

/// 统一后的事件。
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    /// 外部会话 id（Claude session_id / Codex thread_id），用于续接。
    Session(String),
    Text(String),
    Tool { name: String, detail: String },
    ToolFailed(String),
    /// Claude 一轮结束（进程仍在）。
    TurnDone { cost_usd: Option<f64>, is_error: bool, text: Option<String> },
    Error(String),
    /// 不影响结果的过程提示（如网络重连），界面淡色显示，不算出错。
    Notice(String),
}

/// 禁止外派智能体自己提交、推送或丢弃改动；提交由拾光在用户确认后执行。
const DENIED_COMMANDS: &[&str] = &["git commit", "git push", "git reset --hard", "git clean", "git checkout --", "git restore", "git rebase", "git stash"];

pub fn claude_args(mode: &str, resume: Option<&str>, mcp_config: &str) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let permission = match mode {
        "plan" => "plan",
        "full" => "bypassPermissions",
        _ => "acceptEdits",
    };
    args.extend(["--permission-mode".into(), permission.into()]);
    if mode != "plan" {
        // edit：改文件自动通过；命令行工具交给 Claude 自己的权限判断之外再放行常用读写，
        // 但始终拒绝提交/推送/丢改动。Windows 上 Claude 用 PowerShell 工具，两种写法都加。
        args.push("--allowedTools".into());
        args.extend(["Bash", "PowerShell", "mcp__shiguang__ask_main", "mcp__shiguang__report_checkpoint"].iter().map(|s| s.to_string()));
        args.push("--disallowedTools".into());
        for cmd in DENIED_COMMANDS {
            args.push(format!("Bash({cmd}:*)"));
            args.push(format!("PowerShell({cmd}:*)"));
        }
    } else {
        args.push("--allowedTools".into());
        args.extend(["mcp__shiguang__ask_main", "mcp__shiguang__report_checkpoint"].iter().map(|s| s.to_string()));
    }
    args.extend(["--mcp-config".into(), mcp_config.into()]);
    if let Some(id) = resume.filter(|s| !s.is_empty()) {
        args.extend(["--resume".into(), id.into()]);
    }
    args
}

pub fn claude_mcp_config(mcp_exe: &str, run_id: &str, data_dir: &str) -> String {
    json!({"mcpServers": {"shiguang": {
        "type": "stdio",
        "command": mcp_exe,
        "args": [],
        "env": {"SHIGUANG_AGENT_RUN": run_id, "SHIGUANG_DATA_DIR": data_dir},
    }}})
    .to_string()
}

/// 一条用户消息（stream-json 输入）。
pub fn claude_user_line(text: &str) -> String {
    json!({"type": "user", "message": {"role": "user", "content": text}}).to_string()
}

fn toml_literal(s: &str) -> String {
    // TOML 字面量字符串不能含单引号；含时退回基本字符串并转义。
    if s.contains('\'') {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        format!("'{s}'")
    }
}

/// Codex 沙箱。Windows 上 workspace-write 沙箱会拒绝改项目里已有的文件、也断网（实测），
/// 智能体只能来回求助，所以 Windows 上 edit 不开沙箱；禁止提交靠约定 + 拾光每轮核对 HEAD。
pub fn codex_sandbox(mode: &str) -> &'static str {
    match mode {
        "plan" => "read-only",
        "full" => "danger-full-access",
        _ if cfg!(windows) => "danger-full-access",
        _ => "workspace-write",
    }
}

pub fn codex_args(mode: &str, resume: Option<&str>, mcp_exe: &str, run_id: &str, data_dir: &str) -> Vec<String> {
    let sandbox = codex_sandbox(mode);
    let mut args: Vec<String> = vec!["exec".into()];
    if let Some(id) = resume.filter(|s| !s.is_empty()) {
        args.extend(["resume".into(), id.into()]);
    }
    args.extend(["--json".into(), "--skip-git-repo-check".into()]);
    for cfg in [
        format!("sandbox_mode=\"{sandbox}\""),
        "approval_policy=\"never\"".to_string(),
        format!("mcp_servers.shiguang.command={}", toml_literal(mcp_exe)),
        format!(
            "mcp_servers.shiguang.env={{SHIGUANG_AGENT_RUN={}, SHIGUANG_DATA_DIR={}}}",
            toml_literal(run_id),
            toml_literal(data_dir)
        ),
        "mcp_servers.shiguang.tool_timeout_sec=1800".to_string(),
        "mcp_servers.shiguang.startup_timeout_sec=30".to_string(),
        // approval_policy=never 会自动拒绝需要审批的 MCP 调用；回问拾光的两个工具预先放行。
        "mcp_servers.shiguang.default_tools_approval_mode=\"approve\"".to_string(),
        "mcp_servers.shiguang.tools.ask_main.approval_mode=\"approve\"".to_string(),
        "mcp_servers.shiguang.tools.report_checkpoint.approval_mode=\"approve\"".to_string(),
    ] {
        args.extend(["-c".into(), cfg]);
    }
    args.push("-".into());
    args
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// 工具输入里挑最能说明「在干什么」的字段。
fn tool_detail(input: &Value) -> String {
    for key in ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt"] {
        if let Some(v) = input.get(key).and_then(Value::as_str) {
            return clip(v, 300);
        }
    }
    clip(&input.to_string(), 200)
}

pub fn parse_claude(line: &str) -> Vec<Parsed> {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    let mut out = Vec::new();
    // 只看主线程；子代理内部的消息带 parent_tool_use_id。
    if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
        return out;
    }
    match v.get("type").and_then(Value::as_str).unwrap_or("") {
        "system" => {
            if v.get("subtype").and_then(Value::as_str) == Some("init") {
                if let Some(id) = v.get("session_id").and_then(Value::as_str) {
                    out.push(Parsed::Session(id.into()));
                }
            }
        }
        "assistant" => {
            for block in v.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let text = block.get("text").and_then(Value::as_str).unwrap_or("").trim();
                        if !text.is_empty() {
                            out.push(Parsed::Text(text.into()));
                        }
                    }
                    Some("tool_use") => out.push(Parsed::Tool {
                        name: block.get("name").and_then(Value::as_str).unwrap_or("tool").into(),
                        detail: tool_detail(block.get("input").unwrap_or(&Value::Null)),
                    }),
                    _ => {}
                }
            }
        }
        "user" => {
            for block in v.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
                if block.get("type").and_then(Value::as_str) == Some("tool_result")
                    && block.get("is_error").and_then(Value::as_bool) == Some(true)
                {
                    let content = match block.get("content") {
                        Some(Value::String(s)) => s.clone(),
                        Some(other) => other.to_string(),
                        None => String::new(),
                    };
                    out.push(Parsed::ToolFailed(clip(&content, 400)));
                }
            }
        }
        "result" => {
            if let Some(id) = v.get("session_id").and_then(Value::as_str) {
                out.push(Parsed::Session(id.into()));
            }
            let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false)
                || v.get("subtype").and_then(Value::as_str).is_some_and(|s| s.starts_with("error"));
            out.push(Parsed::TurnDone {
                cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
                is_error,
                text: v.get("result").and_then(Value::as_str).map(str::to_string),
            });
        }
        _ => {}
    }
    out
}

pub fn parse_codex(line: &str) -> Vec<Parsed> {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    let mut out = Vec::new();
    match v.get("type").and_then(Value::as_str).unwrap_or("") {
        "thread.started" => {
            if let Some(id) = v.get("thread_id").and_then(Value::as_str) {
                out.push(Parsed::Session(id.into()));
            }
        }
        "item.completed" => {
            let item = v.get("item").unwrap_or(&Value::Null);
            match item.get("type").and_then(Value::as_str).unwrap_or("") {
                "agent_message" => {
                    let text = item.get("text").and_then(Value::as_str).unwrap_or("").trim();
                    if !text.is_empty() {
                        out.push(Parsed::Text(text.into()));
                    }
                }
                "command_execution" => {
                    let cmd = item.get("command").and_then(Value::as_str).unwrap_or("");
                    let code = item.get("exit_code").and_then(Value::as_i64);
                    out.push(Parsed::Tool { name: "命令".into(), detail: clip(cmd, 300) });
                    if code.is_some_and(|c| c != 0) {
                        let output = item.get("aggregated_output").and_then(Value::as_str).unwrap_or("");
                        out.push(Parsed::ToolFailed(format!("退出码 {}：{}", code.unwrap_or(-1), clip(output, 300))));
                    }
                }
                "file_change" => {
                    let paths: Vec<String> = item
                        .get("changes")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|c| {
                            let path = c.get("path").and_then(Value::as_str)?;
                            let kind = c.get("kind").and_then(Value::as_str).unwrap_or("");
                            Some(if kind.is_empty() { path.to_string() } else { format!("{kind} {path}") })
                        })
                        .collect();
                    out.push(Parsed::Tool { name: "改文件".into(), detail: clip(&paths.join("，"), 400) });
                }
                "mcp_tool_call" => {
                    let server = item.get("server").and_then(Value::as_str).unwrap_or("");
                    let tool = item.get("tool").and_then(Value::as_str).unwrap_or("");
                    out.push(Parsed::Tool { name: format!("mcp__{server}__{tool}"), detail: String::new() });
                }
                "web_search" => out.push(Parsed::Tool {
                    name: "搜索".into(),
                    detail: clip(item.get("query").and_then(Value::as_str).unwrap_or(""), 200),
                }),
                "error" => out.push(Parsed::Error(clip(item.get("message").and_then(Value::as_str).unwrap_or("未知错误"), 600))),
                _ => {}
            }
        }
        "turn.failed" => {
            let msg = v.pointer("/error/message").and_then(Value::as_str).unwrap_or("本轮失败");
            out.push(Parsed::Error(clip(msg, 600)));
        }
        "error" => {
            let msg = v.get("message").and_then(Value::as_str).unwrap_or("出错");
            if is_transient(msg) {
                out.push(Parsed::Notice(network_notice(msg)));
            } else {
                out.push(Parsed::Error(clip(msg, 600)));
            }
        }
        _ => {}
    }
    out
}

/// Codex 连 OpenAI 时的自动重连 / 改走 HTTPS：它自己会恢复，不是任务出错。
fn is_transient(msg: &str) -> bool {
    let m = msg.trim_start();
    m.starts_with("Reconnecting") || m.starts_with("Falling back from WebSockets") || m.contains("stream disconnected - retrying")
}

fn network_notice(msg: &str) -> String {
    let m = msg.trim();
    if m.starts_with("Falling back") {
        "网络：WebSocket 连不上，已改用 HTTPS 继续".into()
    } else {
        format!("网络重连中：{}", clip(m, 120))
    }
}

// ---------- Cursor CLI（cursor-agent -p --output-format stream-json） ----------

pub fn cursor_args(mode: &str, resume: Option<&str>, message: &str) -> Vec<String> {
    let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--trust"].iter().map(|s| s.to_string()).collect();
    if mode == "plan" {
        args.extend(["--mode".into(), "plan".into()]);
    } else {
        // --force：命令直接执行不逐条确认；Cursor 的沙箱在 Windows 上同样会挡正常改动，关掉
        args.extend(["--force".into(), "--sandbox".into(), "disabled".into()]);
    }
    if let Some(id) = resume.filter(|s| !s.is_empty()) {
        args.extend(["--resume".into(), id.into()]);
    }
    args.push(message.into());
    args
}

pub fn parse_cursor(line: &str) -> Vec<Parsed> {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    let mut out = Vec::new();
    let session = || v.get("session_id").and_then(Value::as_str).map(|s| Parsed::Session(s.into()));
    match v.get("type").and_then(Value::as_str).unwrap_or("") {
        "system" => out.extend(session()),
        "assistant" => {
            for block in v.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    let text = block.get("text").and_then(Value::as_str).unwrap_or("").trim();
                    if !text.is_empty() {
                        out.push(Parsed::Text(text.into()));
                    }
                }
            }
        }
        "tool_call" => {
            // {"subtype":"started"|"completed","tool_call":{"shellToolCall":{"args":{...},"result":{...}}}}
            let Some((key, call)) = v.get("tool_call").and_then(Value::as_object).and_then(|o| o.iter().next()) else { return out };
            let name = match key.as_str() {
                "function" => call.get("name").and_then(Value::as_str).unwrap_or("工具").to_string(),
                k => cursor_tool_name(k),
            };
            match v.get("subtype").and_then(Value::as_str) {
                Some("started") => {
                    let args = call.get("args").or_else(|| call.get("arguments")).unwrap_or(&Value::Null);
                    let args = match args {
                        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
                        other => other.clone(),
                    };
                    out.push(Parsed::Tool { name, detail: tool_detail(&args) });
                }
                Some("completed") => {
                    let result = call.get("result").unwrap_or(&Value::Null);
                    for bad in ["error", "failure", "rejected"] {
                        if let Some(e) = result.get(bad) {
                            let text = e
                                .get("error")
                                .or_else(|| e.get("message"))
                                .or_else(|| e.get("stderr"))
                                .and_then(Value::as_str)
                                .map(str::to_string)
                                .unwrap_or_else(|| e.to_string());
                            out.push(Parsed::ToolFailed(format!("{name}：{}", clip(&text, 400))));
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        "result" => {
            out.extend(session());
            let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false)
                || v.get("subtype").and_then(Value::as_str).is_some_and(|s| s != "success");
            out.push(Parsed::TurnDone { cost_usd: None, is_error, text: v.get("result").and_then(Value::as_str).map(str::to_string) });
        }
        "error" => {
            let msg = v.get("message").or_else(|| v.get("error")).and_then(Value::as_str).unwrap_or("出错");
            out.push(Parsed::Error(clip(msg, 600)));
        }
        _ => {}
    }
    out
}

fn cursor_tool_name(key: &str) -> String {
    match key.trim_end_matches("ToolCall") {
        "shell" => "命令".into(),
        "edit" | "write" | "delete" => "改文件".into(),
        "read" => "读文件".into(),
        "grep" | "glob" | "ls" => "查找".into(),
        other => other.to_string(),
    }
}

// ---------- Pi（pi --mode json） ----------

/// 会话直接用外派编号：--session-id 不存在就创建、存在就续接，不用另外记 id。
pub fn pi_args(mode: &str, session_id: &str) -> Vec<String> {
    let mut args: Vec<String> = vec!["--mode".into(), "json".into(), "--session-id".into(), session_id.into()];
    if mode == "plan" {
        args.extend(["--tools".into(), "read,grep,find,ls".into()]);
    }
    // 任务正文从 stdin 传（Pi 把 stdin 拼在第一条提示前面）
    args.extend(["--".into(), "按上面的说明执行。".into()]);
    args
}

pub fn parse_pi(line: &str) -> Vec<Parsed> {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    let mut out = Vec::new();
    match v.get("type").and_then(Value::as_str).unwrap_or("") {
        "message_end" => {
            let msg = v.get("message").unwrap_or(&Value::Null);
            if msg.get("role").and_then(Value::as_str) != Some("assistant") {
                return out;
            }
            for block in msg.get("content").and_then(Value::as_array).into_iter().flatten() {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    let text = block.get("text").and_then(Value::as_str).unwrap_or("").trim();
                    if !text.is_empty() {
                        out.push(Parsed::Text(text.into()));
                    }
                }
            }
            if msg.get("stopReason").and_then(Value::as_str) == Some("error") {
                let e = msg.get("errorMessage").and_then(Value::as_str).unwrap_or("模型调用出错");
                out.push(Parsed::Error(clip(e, 600)));
            }
        }
        "tool_execution_start" => out.push(Parsed::Tool {
            name: match v.get("toolName").and_then(Value::as_str).unwrap_or("工具") {
                "bash" | "powershell" => "命令".into(),
                "edit" | "write" => "改文件".into(),
                "read" => "读文件".into(),
                other => other.into(),
            },
            detail: tool_detail(v.get("args").unwrap_or(&Value::Null)),
        }),
        "tool_execution_end" => {
            if v.get("isError").and_then(Value::as_bool) == Some(true) {
                let text: Vec<&str> = v
                    .pointer("/result/content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c.get("text").and_then(Value::as_str))
                    .collect();
                let name = v.get("toolName").and_then(Value::as_str).unwrap_or("工具");
                out.push(Parsed::ToolFailed(format!("{name}：{}", clip(&text.join("
"), 400))));
            }
        }
        "auto_retry_start" => {
            let e = v.get("errorMessage").and_then(Value::as_str).unwrap_or("");
            out.push(Parsed::Notice(format!("模型调用失败，自动重试 {}/{}：{}", v["attempt"], v["maxAttempts"], clip(e, 120))));
        }
        "auto_retry_end" => {
            if v.get("success").and_then(Value::as_bool) == Some(false) {
                out.push(Parsed::Error(clip(v.get("finalError").and_then(Value::as_str).unwrap_or("重试失败"), 600)));
            }
        }
        _ => {}
    }
    out
}

/// Windows 系统代理（注册表 ProxyServer）转成代理 URL：`host:port` 或 `http=..;https=..` 两种写法。
pub fn proxy_url(server: &str) -> Option<String> {
    let s = server.trim();
    let pick = if s.contains('=') {
        let parts: Vec<(&str, &str)> = s.split(';').filter_map(|p| p.split_once('=')).map(|(k, v)| (k.trim(), v.trim())).collect();
        ["https", "http"].iter().find_map(|want| parts.iter().find(|(k, _)| k.eq_ignore_ascii_case(want)).map(|(_, v)| *v))?
    } else {
        s.split(';').next()?.trim()
    };
    if pick.is_empty() {
        return None;
    }
    Some(if pick.contains("://") { pick.to_string() } else { format!("http://{pick}") })
}

/// npm 全局命令在 Windows 是 .cmd 包装：解析出 `node <入口.js>`，避免参数经 cmd.exe 转义。
pub fn npm_shim_entry(shim_text: &str) -> Option<String> {
    let marker = "\"%dp0%\\";
    let mut rest = shim_text;
    while let Some(start) = rest.find(marker) {
        let after = &rest[start + marker.len()..];
        let end = after.find('"')?;
        let rel = &after[..end];
        if rel.to_ascii_lowercase().ends_with(".js") || rel.to_ascii_lowercase().ends_with(".mjs") || rel.to_ascii_lowercase().ends_with(".cjs") {
            return Some(rel.to_string());
        }
        rest = &after[end..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_events_from_real_stream_shape() {
        let init = r#"{"type":"system","subtype":"init","session_id":"s1","tools":[]}"#;
        assert_eq!(parse_claude(init), vec![Parsed::Session("s1".into())]);
        let assistant = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"改好了"},{"type":"tool_use","name":"Edit","input":{"file_path":"a.ts","old_string":"x"}}]},"parent_tool_use_id":null,"session_id":"s1"}"#;
        assert_eq!(parse_claude(assistant), vec![
            Parsed::Text("改好了".into()),
            Parsed::Tool { name: "Edit".into(), detail: "a.ts".into() },
        ]);
        let sub = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"sub"}]},"parent_tool_use_id":"t1"}"#;
        assert!(parse_claude(sub).is_empty());
        let failed = r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"denied"}]}}"#;
        assert_eq!(parse_claude(failed), vec![Parsed::ToolFailed("denied".into())]);
        let result = r#"{"type":"result","subtype":"success","is_error":false,"result":"PONG","session_id":"s1","total_cost_usd":0.08}"#;
        assert_eq!(parse_claude(result), vec![
            Parsed::Session("s1".into()),
            Parsed::TurnDone { cost_usd: Some(0.08), is_error: false, text: Some("PONG".into()) },
        ]);
    }

    #[test]
    fn codex_events_from_real_stream_shape() {
        assert_eq!(parse_codex(r#"{"type":"thread.started","thread_id":"t-1"}"#), vec![Parsed::Session("t-1".into())]);
        assert_eq!(
            parse_codex(r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"PONG"}}"#),
            vec![Parsed::Text("PONG".into())]
        );
        let cmd = r#"{"type":"item.completed","item":{"type":"command_execution","command":"npm run build","exit_code":1,"aggregated_output":"boom"}}"#;
        assert_eq!(parse_codex(cmd), vec![
            Parsed::Tool { name: "命令".into(), detail: "npm run build".into() },
            Parsed::ToolFailed("退出码 1：boom".into()),
        ]);
        assert_eq!(parse_codex(r#"{"type":"turn.failed","error":{"message":"quota"}}"#), vec![Parsed::Error("quota".into())]);
        assert!(parse_codex("not json").is_empty());
    }

    #[test]
    fn codex_reconnects_are_notices_not_errors() {
        let r = parse_codex(r#"{"type":"error","message":"Reconnecting... 2/5 (request timed out)"}"#);
        assert!(matches!(&r[..], [Parsed::Notice(m)] if m.contains("2/5")));
        let r = parse_codex(r#"{"type":"error","message":"Falling back from WebSockets to HTTPS transport. request timed out"}"#);
        assert!(matches!(&r[..], [Parsed::Notice(_)]));
        assert!(matches!(&parse_codex(r#"{"type":"error","message":"quota exceeded"}"#)[..], [Parsed::Error(_)]));
    }

    #[test]
    fn cursor_events() {
        assert_eq!(parse_cursor(r#"{"type":"system","subtype":"init","session_id":"c1","model":"x"}"#), vec![Parsed::Session("c1".into())]);
        assert_eq!(
            parse_cursor(r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"好的"}]},"session_id":"c1"}"#),
            vec![Parsed::Text("好的".into())]
        );
        assert_eq!(
            parse_cursor(r#"{"type":"tool_call","subtype":"started","call_id":"1","tool_call":{"shellToolCall":{"args":{"command":"npm test"}}}}"#),
            vec![Parsed::Tool { name: "命令".into(), detail: "npm test".into() }]
        );
        let failed = r#"{"type":"tool_call","subtype":"completed","tool_call":{"editToolCall":{"args":{"path":"a.ts"},"result":{"error":{"error":"denied"}}}}}"#;
        assert_eq!(parse_cursor(failed), vec![Parsed::ToolFailed("改文件：denied".into())]);
        assert!(parse_cursor(r#"{"type":"tool_call","subtype":"completed","tool_call":{"readToolCall":{"args":{},"result":{"success":{}}}}}"#).is_empty());
        assert_eq!(parse_cursor(r#"{"type":"result","subtype":"success","is_error":false,"result":"完成","session_id":"c1"}"#), vec![
            Parsed::Session("c1".into()),
            Parsed::TurnDone { cost_usd: None, is_error: false, text: Some("完成".into()) },
        ]);
        let args = cursor_args("edit", Some("c1"), "做事");
        assert!(args.contains(&"--force".to_string()) && args.windows(2).any(|w| w == ["--resume", "c1"]));
        assert_eq!(args.last().unwrap(), "做事");
        assert!(cursor_args("plan", None, "看看").windows(2).any(|w| w == ["--mode", "plan"]));
    }

    #[test]
    fn pi_events() {
        let end = r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"..."},{"type":"text","text":"改好了"},{"type":"toolCall","id":"t","name":"edit","arguments":{}}],"stopReason":"toolUse"}}"#;
        assert_eq!(parse_pi(end), vec![Parsed::Text("改好了".into())]);
        assert!(parse_pi(r#"{"type":"message_end","message":{"role":"user","content":"hi"}}"#).is_empty());
        assert_eq!(
            parse_pi(r#"{"type":"tool_execution_start","toolCallId":"1","toolName":"bash","args":{"command":"ls"}}"#),
            vec![Parsed::Tool { name: "命令".into(), detail: "ls".into() }]
        );
        let bad = r#"{"type":"tool_execution_end","toolCallId":"1","toolName":"edit","result":{"content":[{"type":"text","text":"no match"}]},"isError":true}"#;
        assert_eq!(parse_pi(bad), vec![Parsed::ToolFailed("edit：no match".into())]);
        let err = r#"{"type":"message_end","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"401"}}"#;
        assert_eq!(parse_pi(err), vec![Parsed::Error("401".into())]);
        assert!(matches!(
            &parse_pi(r#"{"type":"auto_retry_start","attempt":1,"maxAttempts":3,"delayMs":2000,"errorMessage":"529"}"#)[..],
            [Parsed::Notice(_)]
        ));
        let args = pi_args("plan", "ar-1");
        assert!(args.windows(2).any(|w| w == ["--session-id", "ar-1"]) && args.contains(&"read,grep,find,ls".to_string()));
    }

    #[test]
    fn claude_edit_mode_denies_commits_and_plan_mode_is_read_only() {
        let edit = claude_args("edit", Some("s1"), "{}");
        assert!(edit.windows(2).any(|w| w == ["--permission-mode", "acceptEdits"]));
        assert!(edit.contains(&"Bash(git push:*)".to_string()));
        assert!(edit.contains(&"PowerShell(git commit:*)".to_string()));
        assert!(edit.windows(2).any(|w| w == ["--resume", "s1"]));
        let plan = claude_args("plan", None, "{}");
        assert!(plan.windows(2).any(|w| w == ["--permission-mode", "plan"]));
        assert!(!plan.contains(&"Bash".to_string()));
        assert!(!plan.contains(&"--resume".to_string()));
    }

    #[test]
    fn codex_args_put_resume_first_and_quote_windows_paths() {
        let args = codex_args("edit", Some("t-1"), r"C:\app\shiguang-mcp.exe", "ar-1", r"C:\data");
        assert_eq!(&args[..3], ["exec", "resume", "t-1"]);
        assert!(args.contains(&r"mcp_servers.shiguang.command='C:\app\shiguang-mcp.exe'".to_string()));
        assert!(args.contains(&format!("sandbox_mode=\"{}\"", codex_sandbox("edit"))));
        assert_eq!(args.last().unwrap(), "-");
        let plan = codex_args("plan", None, "x", "ar-1", "d");
        assert_eq!(plan[1], "--json");
        assert!(plan.contains(&"sandbox_mode=\"read-only\"".to_string()));
    }

    #[test]
    fn system_proxy_strings() {
        assert_eq!(proxy_url("127.0.0.1:7897").as_deref(), Some("http://127.0.0.1:7897"));
        assert_eq!(proxy_url("http=1.2.3.4:80;https=1.2.3.4:443").as_deref(), Some("http://1.2.3.4:443"));
        assert_eq!(proxy_url("socks5://127.0.0.1:1080").as_deref(), Some("socks5://127.0.0.1:1080"));
        assert_eq!(proxy_url(" "), None);
    }

    #[test]
    fn npm_shim_entry_is_found() {
        let shim = r#"endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%"  "%dp0%\node_modules\@openai\codex\bin\codex.js" %*"#;
        assert_eq!(npm_shim_entry(shim).as_deref(), Some(r"node_modules\@openai\codex\bin\codex.js"));
        assert_eq!(npm_shim_entry("@echo off"), None);
    }
}
