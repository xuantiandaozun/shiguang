//! 命令行参数与事件解析：Claude Code（stream-json 常驻）与 Codex（exec --json 每轮一进程）。
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

pub fn codex_args(mode: &str, resume: Option<&str>, mcp_exe: &str, run_id: &str, data_dir: &str) -> Vec<String> {
    let sandbox = match mode {
        "plan" => "read-only",
        "full" => "danger-full-access",
        _ => "workspace-write",
    };
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
            out.push(Parsed::Error(clip(msg, 600)));
        }
        _ => {}
    }
    out
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
        assert!(args.contains(&"sandbox_mode=\"workspace-write\"".to_string()));
        assert_eq!(args.last().unwrap(), "-");
        let plan = codex_args("plan", None, "x", "ar-1", "d");
        assert_eq!(plan[1], "--json");
        assert!(plan.contains(&"sandbox_mode=\"read-only\"".to_string()));
    }

    #[test]
    fn npm_shim_entry_is_found() {
        let shim = r#"endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%"  "%dp0%\node_modules\@openai\codex\bin\codex.js" %*"#;
        assert_eq!(npm_shim_entry(shim).as_deref(), Some(r"node_modules\@openai\codex\bin\codex.js"));
        assert_eq!(npm_shim_entry("@echo off"), None);
    }
}
