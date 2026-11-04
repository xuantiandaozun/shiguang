//! 真实 CLI 端到端（默认忽略，会消耗订阅额度）：
//!   cargo test --manifest-path src-tauri/Cargo.toml --lib agents::e2e_tests -- --ignored --nocapture
//! 用真实 claude / codex + 真实 shiguang-mcp.exe（外派模式）+ 假的拾光桥，验证：
//! 参数被接受、ask_main 回问往返、同一会话追问、禁止 commit。
use super::adapters::{self, Parsed};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

fn mcp_exe() -> PathBuf {
    if let Ok(p) = std::env::var("SHIGUANG_MCP_EXE") {
        return PathBuf::from(p);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("debug").join("shiguang-mcp.exe")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").args(args).current_dir(dir).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn temp_repo() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shiguang-agent-e2e-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init"]);
    dir
}

/// 假的拾光桥：agent_ask 记下问题，agent_poll 立即给出答案。
async fn fake_bridge(data_dir: &Path, answer: &'static str) -> Arc<Mutex<Vec<Value>>> {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    std::fs::create_dir_all(data_dir).unwrap();
    std::fs::write(data_dir.join("mcp-bridge.json"), json!({"port": port, "token": "t"}).to_string()).unwrap();
    let log = asked.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                let (head_end, len) = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 { return; }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
                        let len = head.lines().find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())).unwrap_or(0);
                        break (i + 4, len);
                    }
                };
                while buf.len() < head_end + len {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 { return; }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let req: Value = serde_json::from_slice(&buf[head_end..head_end + len]).unwrap_or_default();
                let resp = match req.get("action").and_then(Value::as_str) {
                    Some("agent_ask") => {
                        log.lock().unwrap().push(req.clone());
                        json!({"qid": "q1"})
                    }
                    Some("agent_poll") => json!({"answer": answer}),
                    _ => json!({"error": "unexpected"}),
                };
                let body = resp.to_string();
                let _ = sock.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
            });
        }
    });
    asked
}

const ASK_TASK: &str = "这是一次自动化测试。第一步：调用 MCP 工具 ask_main，问题写「a.txt 应该改成什么单词？」。第二步：把 a.txt 的全部内容改成回答里给出的那个单词（只有这个单词）。不要做其它任何事，完成后回复 DONE。";
const COMMIT_TASK: &str = "这是一次自动化测试：请运行 git add -A 然后 git commit -m e2e 提交改动。如果命令被拒绝，不要绕过，直接回复 REFUSED。";

async fn read_until_done(lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>, claude: bool, events: &mut Vec<Parsed>) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
    loop {
        let line = tokio::time::timeout_at(deadline, lines.next_line()).await.expect("等待智能体超时").unwrap();
        let Some(line) = line else { return };
        let parsed = if claude { adapters::parse_claude(&line) } else { adapters::parse_codex(&line) };
        let done = parsed.iter().any(|p| matches!(p, Parsed::TurnDone { .. }));
        for p in parsed {
            eprintln!("  {p:?}");
            events.push(p);
        }
        if done {
            return;
        }
    }
}

#[tokio::test]
#[ignore]
async fn claude_code_round_trip() {
    let exe = mcp_exe();
    assert!(exe.is_file(), "先构建 shiguang-mcp：{}", exe.display());
    let repo = temp_repo();
    let data_dir = repo.join(".data");
    std::fs::write(repo.join(".git").join("info").join("exclude"), ".data/\n").unwrap();
    let asked = fake_bridge(&data_dir, "WORLD").await;
    let mcp = adapters::claude_mcp_config(&exe.to_string_lossy(), "ar-e2e", &data_dir.to_string_lossy());
    let program = crate::tasks::resolve_program("claude");
    let mut child = tokio::process::Command::new(program)
        .args(adapters::claude_args("edit", None, &mcp))
        .current_dir(&repo)
        .env("MCP_TOOL_TIMEOUT", "600000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut events = Vec::new();

    stdin.write_all(format!("{}\n", adapters::claude_user_line(ASK_TASK)).as_bytes()).await.unwrap();
    stdin.flush().await.unwrap();
    read_until_done(&mut lines, true, &mut events).await;
    assert!(events.iter().any(|e| matches!(e, Parsed::Session(_))), "应拿到 session_id");
    assert_eq!(asked.lock().unwrap().len(), 1, "应通过 ask_main 回问一次");
    assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap().trim(), "WORLD");

    // 同一进程追问：提交应被拒绝
    let head = git(&repo, &["rev-parse", "HEAD"]);
    stdin.write_all(format!("{}\n", adapters::claude_user_line(COMMIT_TASK)).as_bytes()).await.unwrap();
    stdin.flush().await.unwrap();
    read_until_done(&mut lines, true, &mut events).await;
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head, "外派智能体不应能提交");
    drop(stdin);
    let _ = child.kill().await;
    let _ = std::fs::remove_dir_all(&repo);
}

async fn codex_turn(repo: &Path, data_dir: &Path, resume: Option<&str>, prompt: &str, events: &mut Vec<Parsed>) {
    let exe = mcp_exe();
    let shim = PathBuf::from(crate::tasks::resolve_program("codex"));
    let text = std::fs::read_to_string(&shim).unwrap();
    let entry = shim.parent().unwrap().join(adapters::npm_shim_entry(&text).unwrap());
    let node = crate::tasks::resolve_program("node");
    let mut child = tokio::process::Command::new(node)
        .arg(entry)
        .args(adapters::codex_args("edit", resume, &exe.to_string_lossy(), "ar-e2e", &data_dir.to_string_lossy()))
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(prompt.as_bytes()).await.unwrap();
    drop(stdin);
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
    while let Some(line) = tokio::time::timeout_at(deadline, lines.next_line()).await.expect("Codex 超时").unwrap() {
        for p in adapters::parse_codex(&line) {
            eprintln!("  {p:?}");
            events.push(p);
        }
    }
    let status = child.wait().await.unwrap();
    assert!(status.success(), "codex 退出码 {status:?}");
}

#[tokio::test]
#[ignore]
async fn codex_round_trip() {
    assert!(mcp_exe().is_file());
    let repo = temp_repo();
    let data_dir = repo.join(".data");
    std::fs::write(repo.join(".git").join("info").join("exclude"), ".data/\n").unwrap();
    let asked = fake_bridge(&data_dir, "WORLD").await;
    let mut events = Vec::new();
    codex_turn(&repo, &data_dir, None, ASK_TASK, &mut events).await;
    let thread = events.iter().find_map(|e| if let Parsed::Session(s) = e { Some(s.clone()) } else { None }).expect("应拿到 thread_id");
    assert_eq!(asked.lock().unwrap().len(), 1, "应通过 ask_main 回问一次");
    assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap().trim(), "WORLD");

    // 续接同一会话
    let mut more = Vec::new();
    codex_turn(&repo, &data_dir, Some(&thread), "这是自动化测试：上一轮你把 a.txt 改成了什么？只回复那个单词。", &mut more).await;
    assert!(more.iter().any(|e| matches!(e, Parsed::Text(t) if t.contains("WORLD"))), "续接后应记得上一轮");

    // Codex 没有命令黑名单，靠 workspace-write 沙箱（.git 只读）阻止提交；拾光另外用 head_changed 兜底。
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let mut commit = Vec::new();
    codex_turn(&repo, &data_dir, Some(&thread), COMMIT_TASK, &mut commit).await;
    eprintln!("HEAD changed: {}", git(&repo, &["rev-parse", "HEAD"]) != head);
    let _ = std::fs::remove_dir_all(&repo);
}
