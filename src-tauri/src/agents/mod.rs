//! 外派：把工作区里的编码任务交给 Claude Code / Codex CLI，用各自的订阅额度。
//! 拾光负责交代任务、回答智能体的提问、核对 git 结果；提交和 push 由拾光在用户确认后执行。
//! 智能体通过 shiguang-mcp（SHIGUANG_AGENT_RUN 外派模式）的 ask_main / report_checkpoint 反问主 AI。
pub mod adapters;
#[cfg(test)]
mod e2e_tests;

use crate::db::AgentRunRow;
use adapters::Parsed;
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub const KINDS: &[&str] = crate::workspaces::AGENT_KINDS;
const MAX_EXCHANGES: u64 = 12;
const IDLE_SECS: u64 = 30 * 60;
const DEFAULT_AWAIT_SECS: u64 = 1200;
const MAX_AWAIT_SECS: u64 = 3600;
const SETTINGS_KEY: &str = "code_agents";

pub fn label(kind: &str) -> &'static str {
    match kind {
        "claude_code" => "Claude Code",
        "codex" => "Codex",
        _ => "智能体",
    }
}

fn binary(kind: &str) -> &'static str {
    match kind {
        "claude_code" => "claude",
        _ => "codex",
    }
}

// ---------- 探测与配置 ----------

#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub kind: String,
    pub label: String,
    pub path: Option<String>,
    pub version: Option<String>,
    pub enabled: bool,
    pub available: bool,
    pub path_override: String,
}

#[derive(Debug, Clone)]
struct Launcher {
    program: PathBuf,
    prefix: Vec<String>,
    display: String,
}

fn config(db: &crate::db::Db) -> Value {
    db.get_setting(SETTINGS_KEY)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({}))
}

pub fn set_config(db: &crate::db::Db, kind: &str, enabled: bool, path: &str) -> Result<()> {
    if !KINDS.contains(&kind) {
        bail!("未知智能体 {kind}");
    }
    let mut cfg = config(db);
    cfg[kind] = json!({"enabled": enabled, "path": path.trim()});
    db.set_setting(SETTINGS_KEY, &cfg.to_string())?;
    *VERSIONS.lock().unwrap() = None;
    Ok(())
}

fn enabled(db: &crate::db::Db, kind: &str) -> bool {
    config(db).pointer(&format!("/{kind}/enabled")).and_then(Value::as_bool).unwrap_or(true)
}

fn launcher(db: &crate::db::Db, kind: &str) -> Option<Launcher> {
    let override_path = config(db).pointer(&format!("/{kind}/path")).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let resolved = if override_path.is_empty() { crate::tasks::resolve_program(binary(kind)) } else { override_path };
    let path = PathBuf::from(&resolved);
    if !path.is_file() {
        return None;
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if ext == "cmd" || ext == "bat" {
        let text = std::fs::read_to_string(&path).ok()?;
        let dir = path.parent()?;
        let entry = dir.join(adapters::npm_shim_entry(&text)?);
        let node = if dir.join("node.exe").is_file() { dir.join("node.exe") } else { PathBuf::from(crate::tasks::resolve_program("node")) };
        if !entry.is_file() || !node.is_file() {
            return None;
        }
        return Some(Launcher { program: node, prefix: vec![entry.to_string_lossy().to_string()], display: resolved });
    }
    Some(Launcher { program: path, prefix: Vec::new(), display: resolved })
}

static VERSIONS: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);

fn command(l: &Launcher) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(&l.program);
    cmd.args(&l.prefix);
    #[cfg(windows)]
    cmd.creation_flags(crate::tasks::CREATE_NO_WINDOW);
    cmd
}

async fn version(l: &Launcher) -> Option<String> {
    if let Some(v) = VERSIONS.lock().unwrap().as_ref().and_then(|m| m.get(&l.display).cloned()) {
        return v;
    }
    let mut cmd = command(l);
    cmd.arg("--version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(std::time::Duration::from_secs(15), cmd.output()).await.ok()?.ok();
    let v = out
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string())
        .filter(|s| !s.is_empty());
    VERSIONS.lock().unwrap().get_or_insert_with(HashMap::new).insert(l.display.clone(), v.clone());
    v
}

pub async fn detect(app: &AppHandle) -> Vec<AgentInfo> {
    let db = &app.state::<crate::AppState>().db;
    let mut out = Vec::new();
    for kind in KINDS {
        let l = launcher(db, kind);
        let version = match &l {
            Some(l) => version(l).await,
            None => None,
        };
        out.push(AgentInfo {
            kind: kind.to_string(),
            label: label(kind).into(),
            path: l.as_ref().map(|l| l.display.clone()),
            available: l.is_some() && version.is_some(),
            version,
            enabled: enabled(db, kind),
            path_override: config(db).pointer(&format!("/{kind}/path")).and_then(Value::as_str).unwrap_or("").to_string(),
        });
    }
    out
}

fn mcp_exe() -> Result<String> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| anyhow!("找不到程序目录"))?;
    let path = dir.join(if cfg!(windows) { "shiguang-mcp.exe" } else { "shiguang-mcp" });
    if !path.is_file() {
        bail!("找不到 {}，外派需要它让智能体回问拾光；请重新打包或从发布目录启动", path.display());
    }
    Ok(path.to_string_lossy().to_string())
}

fn data_dir(app: &AppHandle) -> String {
    app.path().app_data_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
}

// ---------- 运行时 ----------

#[derive(Default)]
pub struct AgentHub {
    runs: Mutex<HashMap<String, Arc<Handle>>>,
}

struct Handle {
    tx: watch::Sender<u64>,
    /// 读改写 DB 记录时串行化
    write: Mutex<()>,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    stdin: Option<Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>>,
    proc_cancel: Option<CancellationToken>,
    generation: u64,
    pending: Option<Pending>,
    awaiting: u32,
}

struct Pending {
    qid: String,
    answer: Option<String>,
}

impl AgentHub {
    fn handle(&self, id: &str) -> Arc<Handle> {
        self.runs
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_insert_with(|| {
                let (tx, _) = watch::channel(0);
                Arc::new(Handle { tx, write: Mutex::new(()), inner: Mutex::new(Inner::default()) })
            })
            .clone()
    }

    /// 应用退出：结束所有外派进程。
    pub fn shutdown(&self) {
        for h in self.runs.lock().unwrap().values() {
            if let Some(c) = &h.inner.lock().unwrap().proc_cancel {
                c.cancel();
            }
        }
    }
}

fn handle(app: &AppHandle, id: &str) -> Arc<Handle> {
    app.state::<crate::AppState>().agents.handle(id)
}

fn load(app: &AppHandle, id: &str) -> Result<AgentRunRow> {
    app.state::<crate::AppState>().db.agent_run_get(id)?.ok_or_else(|| anyhow!("外派任务 {id} 不存在"))
}

fn update(app: &AppHandle, id: &str, f: impl FnOnce(&mut AgentRunRow)) -> Result<AgentRunRow> {
    let h = handle(app, id);
    let row = {
        let _guard = h.write.lock().unwrap();
        let mut row = load(app, id)?;
        f(&mut row);
        app.state::<crate::AppState>().db.agent_run_update(id, &row.status, &row.data)?;
        row
    };
    h.tx.send_modify(|v| *v += 1);
    let _ = app.emit("agent-run-changed", id);
    Ok(row)
}

fn event(app: &AppHandle, id: &str, kind: &str, content: &str) {
    let _ = app.state::<crate::AppState>().db.agent_run_event(id, kind, content);
    let _ = app.emit("agent-run-changed", id);
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max).collect::<String>()) }
}

fn git_ok(root: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args).current_dir(root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(crate::tasks::CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| anyhow!("无法运行 git：{e}"))?;
    if !out.status.success() {
        bail!("git {} 失败：{}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn work_root(run: &AgentRunRow) -> PathBuf {
    run.data
        .pointer("/worktree/path")
        .and_then(Value::as_str)
        .filter(|_| run.data.pointer("/worktree/state").and_then(Value::as_str) == Some("active"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&run.root))
}

// ---------- 开始外派 ----------

fn contract(run_id: &str, ws: Option<&crate::db::WorkspaceRow>, root: &Path, mode: &str) -> String {
    let mut lines = vec![
        format!("你是被「拾光」（用户的桌面助手）外派来的编码智能体，外派编号 {run_id}。工作目录：{}", root.display()),
        "规则：".into(),
        "1. 只做下面交代的任务，不顺手改无关的东西。".into(),
        "2. 不要 git commit / push / reset / stash，也不要丢弃已有改动；提交由拾光在用户确认后执行。".into(),
        "3. 需求不清、需要扩大改动范围、要动线上数据、要装新依赖、或者需要线上/浏览器实测结果时，调用 MCP 工具 ask_main 提问并等回答，不要猜。".into(),
        "4. 任务里约定了关键节点的，到了就调用 report_checkpoint 汇报并等回复。".into(),
        "5. 结束时用中文简短总结：改了哪些文件、为什么、怎么验证的、还有什么风险或没做完的。".into(),
    ];
    if mode == "plan" {
        lines.push("6. 本次是只读方案模式：只分析并给出修改方案，不要改文件。".into());
    }
    if let Some(ws) = ws {
        lines.push(format!("\n项目：{}", ws.name));
        for (label, key) in [("说明", "summary"), ("部署", "deploy")] {
            let v = crate::workspaces::text(ws, key);
            if !v.is_empty() {
                lines.push(format!("{label}：{v}"));
            }
        }
        let verify = crate::workspaces::list(ws, "verify_commands");
        if !verify.is_empty() {
            lines.push(format!("验证命令：{}", verify.join("；")));
        }
        let rules = crate::workspaces::list(ws, "rules");
        if !rules.is_empty() {
            lines.push(format!("项目规则：\n{}", rules.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n")));
        }
        let notes = crate::workspaces::list(ws, "notes");
        if !notes.is_empty() {
            lines.push(format!("已知经验：\n{}", notes.iter().rev().take(8).rev().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n")));
        }
    }
    lines.join("\n")
}

fn pick_agent(app: &AppHandle, requested: Option<&str>, ws: Option<&crate::db::WorkspaceRow>) -> Result<String> {
    let db = &app.state::<crate::AppState>().db;
    let usable = |k: &str| enabled(db, k) && launcher(db, k).is_some();
    if let Some(k) = requested.filter(|s| !s.is_empty() && *s != "auto") {
        if !KINDS.contains(&k) {
            bail!("agent 只能是 claude_code 或 codex");
        }
        if !usable(k) {
            bail!("{} 不可用：未安装、未在 PATH 中或已在工作区页停用", label(k));
        }
        return Ok(k.into());
    }
    if let Some(k) = ws.map(|w| crate::workspaces::text(w, "default_agent")).filter(|k| !k.is_empty() && usable(k)) {
        return Ok(k.into());
    }
    KINDS.iter().find(|k| usable(k)).map(|k| k.to_string()).ok_or_else(|| anyhow!("没有可用的编码智能体（需要安装 Claude Code 或 Codex CLI）"))
}

pub async fn start(app: &AppHandle, args: &Value, session_id: i64) -> Result<Value> {
    let state = app.state::<crate::AppState>();
    let db = &state.db;
    let task = args.get("task").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("需要 task：写清要改什么、范围、背景"))?;
    let ws = match args.get("workspace").filter(|v| !v.is_null()) {
        Some(key) => Some(crate::workspaces::resolve(db, key)?),
        None if args.get("root").and_then(Value::as_str).is_none() => crate::workspaces::current(db, session_id),
        None => None,
    };
    let root = args.get("root").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
        .or_else(|| ws.as_ref().map(|w| w.root.clone()))
        .ok_or_else(|| anyhow!("需要工作区（workspace）或项目根目录（root）"))?;
    let root_path = PathBuf::from(&root);
    if !root_path.is_dir() {
        bail!("项目目录不存在：{root}");
    }
    let agent = pick_agent(app, args.get("agent").and_then(Value::as_str), ws.as_ref())?;
    let mode = args.get("mode").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
        .or_else(|| ws.as_ref().map(|w| crate::workspaces::text(w, "agent_mode").to_string()).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "edit".into());
    if !matches!(mode.as_str(), "plan" | "edit" | "full") {
        bail!("mode 只能是 plan、edit、full");
    }
    let isolation = args.get("isolation").and_then(Value::as_str).unwrap_or("none");
    let is_repo = crate::workspaces::git(&root_path, &["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true");
    if isolation == "worktree" && !is_repo {
        bail!("isolation=worktree 需要 git 仓库");
    }
    let base_head = if is_repo { crate::workspaces::git(&root_path, &["rev-parse", "HEAD"]) } else { None };
    let dirty: Vec<String> = if is_repo {
        crate::workspaces::git(&root_path, &["status", "--porcelain"]).unwrap_or_default().lines().map(str::to_string).collect()
    } else {
        Vec::new()
    };
    let allow_dirty = args.get("allow_dirty").and_then(Value::as_bool).unwrap_or(false);
    if !dirty.is_empty() && mode != "plan" && isolation != "worktree" && !allow_dirty {
        return Ok(json!({
            "status": "needs_confirmation",
            "uncommitted": dirty.iter().take(20).collect::<Vec<_>>(),
            "uncommitted_total": dirty.len(),
            "note": "项目里有未提交的改动，外派后会和智能体的改动混在一起。先告诉用户并 ask_user：继续（带 allow_dirty=true 重试）、用 isolation=worktree 隔离、或先让用户处理。",
        }));
    }
    let id = format!("ar-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let worktree = if isolation == "worktree" { Some(create_worktree(app, &root_path, &id)?) } else { None };
    let title = args.get("title").and_then(Value::as_str).map(|s| clip(s, 60)).filter(|s| !s.is_empty())
        .unwrap_or_else(|| clip(task.lines().next().unwrap_or(task), 40));
    let acceptance = args.get("acceptance").and_then(Value::as_str).map(str::trim).unwrap_or("");
    let verify = ws.as_ref().map(|w| crate::workspaces::list(w, "verify_commands")).unwrap_or_default();
    let mut data = json!({
        "task": task, "acceptance": acceptance,
        "workspace_name": ws.as_ref().map(|w| w.name.clone()),
        "base_head": base_head, "is_repo": is_repo,
        "verify_commands": verify, "exchanges": 0, "turns": 0, "cost_usd": 0.0,
    });
    let effective_root = worktree.as_ref().map(|(p, _)| p.clone()).unwrap_or_else(|| root_path.clone());
    if let Some((path, branch)) = &worktree {
        data["worktree"] = json!({"path": path.to_string_lossy(), "branch": branch, "state": "active"});
    }
    let row = AgentRunRow {
        id: id.clone(), agent: agent.clone(), workspace_id: ws.as_ref().map(|w| w.id), session_id,
        title, root: root.clone(), mode: mode.clone(), status: "running".into(), data,
        created_at: String::new(), updated_at: String::new(),
    };
    db.agent_run_create(&row)?;
    if let Some(w) = &ws {
        let _ = db.workspace_touch(w.id);
    }
    let mut message = contract(&id, ws.as_ref(), &effective_root, &mode);
    message.push_str(&format!("\n\n## 任务\n{task}"));
    if !acceptance.is_empty() {
        message.push_str(&format!("\n\n## 验收标准\n{acceptance}"));
    }
    event(app, &id, "main", &format!("{task}{}", if acceptance.is_empty() { String::new() } else { format!("\n\n验收标准：{acceptance}") }));
    let _ = crate::windows::show_agent_run(app, &id);
    if let Err(e) = send_turn(app, &id, &message).await {
        update(app, &id, |r| {
            r.status = "error".into();
            r.data["error"] = json!(e.to_string());
        })?;
        return Err(e);
    }
    Ok(json!({"run_id": id, "agent": label(&agent), "mode": mode, "root": effective_root.to_string_lossy(), "status": "running"}))
}

// ---------- 一轮对话 ----------

async fn send_turn(app: &AppHandle, id: &str, message: &str) -> Result<()> {
    let run = update(app, id, |r| {
        r.status = "running".into();
        r.data["turns"] = json!(r.data["turns"].as_u64().unwrap_or(0) + 1);
        r.data["question"] = Value::Null;
        r.data["error"] = Value::Null;
    })?;
    match run.agent.as_str() {
        "claude_code" => claude_turn(app, &run, message).await,
        _ => codex_turn(app, &run, message).await,
    }
}

async fn write_line(stdin: &Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>, line: &str) -> std::io::Result<()> {
    let mut guard = stdin.lock().await;
    guard.write_all(line.as_bytes()).await?;
    guard.write_all(b"\n").await?;
    guard.flush().await
}

async fn claude_turn(app: &AppHandle, run: &AgentRunRow, message: &str) -> Result<()> {
    let h = handle(app, &run.id);
    let line = adapters::claude_user_line(message);
    let live = h.inner.lock().unwrap().stdin.clone();
    if let Some(stdin) = live {
        if write_line(&stdin, &line).await.is_ok() {
            return Ok(());
        }
        h.inner.lock().unwrap().stdin = None;
    }
    let db = &app.state::<crate::AppState>().db;
    let l = launcher(db, "claude_code").ok_or_else(|| anyhow!("找不到 Claude Code（claude 命令）"))?;
    let mcp = adapters::claude_mcp_config(&mcp_exe()?, &run.id, &data_dir(app));
    let resume = run.data.get("external_session").and_then(Value::as_str);
    let mut cmd = command(&l);
    cmd.args(adapters::claude_args(&run.mode, resume, &mcp))
        .current_dir(work_root(run))
        .env("MCP_TOOL_TIMEOUT", "1800000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| anyhow!("启动 Claude Code 失败：{e}"))?;
    let stdin = Arc::new(tokio::sync::Mutex::new(child.stdin.take().ok_or_else(|| anyhow!("无法写入 Claude Code"))?));
    write_line(&stdin, &line).await.map_err(|e| anyhow!("写入 Claude Code 失败：{e}"))?;
    let cancel = CancellationToken::new();
    let generation = {
        let mut inner = h.inner.lock().unwrap();
        inner.stdin = Some(stdin);
        inner.proc_cancel = Some(cancel.clone());
        inner.generation += 1;
        inner.generation
    };
    tauri::async_runtime::spawn(supervise(app.clone(), run.id.clone(), child, cancel, generation, true));
    Ok(())
}

async fn codex_turn(app: &AppHandle, run: &AgentRunRow, message: &str) -> Result<()> {
    let h = handle(app, &run.id);
    let db = &app.state::<crate::AppState>().db;
    let l = launcher(db, "codex").ok_or_else(|| anyhow!("找不到 Codex（codex 命令）"))?;
    let resume = run.data.get("external_session").and_then(Value::as_str);
    let mut cmd = command(&l);
    cmd.args(adapters::codex_args(&run.mode, resume, &mcp_exe()?, &run.id, &data_dir(app)))
        .current_dir(work_root(run))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| anyhow!("启动 Codex 失败：{e}"))?;
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("无法写入 Codex"))?;
    stdin.write_all(message.as_bytes()).await?;
    drop(stdin);
    let cancel = CancellationToken::new();
    let generation = {
        let mut inner = h.inner.lock().unwrap();
        inner.proc_cancel = Some(cancel.clone());
        inner.generation += 1;
        inner.generation
    };
    tauri::async_runtime::spawn(supervise(app.clone(), run.id.clone(), child, cancel, generation, false));
    Ok(())
}

async fn supervise(app: AppHandle, id: String, mut child: tokio::process::Child, cancel: CancellationToken, generation: u64, claude: bool) {
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    if let Some(stderr) = child.stderr.take() {
        let tail = stderr_tail.clone();
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut t = tail.lock().unwrap();
                t.push_str(&line);
                t.push('\n');
                let len = t.chars().count();
                if len > 4000 {
                    *t = t.chars().skip(len - 4000).collect();
                }
            }
        });
    }
    if let Some(stdout) = child.stdout.take() {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    let _ = child.kill().await;
                    break;
                }
                line = lines.next_line() => match line {
                    Ok(Some(line)) => {
                        let parsed = if claude { adapters::parse_claude(&line) } else { adapters::parse_codex(&line) };
                        for p in parsed {
                            on_parsed(&app, &id, p, generation).await;
                        }
                    }
                    _ => break,
                }
            }
        }
    }
    let status = child.wait().await.ok();
    let h = handle(&app, &id);
    {
        let mut inner = h.inner.lock().unwrap();
        if inner.generation == generation {
            inner.stdin = None;
            inner.proc_cancel = None;
            if let Some(p) = inner.pending.take() {
                // 进程没了，未答的问题作废
                drop(p);
            }
        }
    }
    let Ok(run) = load(&app, &id) else { return };
    if !matches!(run.status.as_str(), "running" | "asking") {
        return;
    }
    if cancel.is_cancelled() {
        return;
    }
    let ok = status.is_some_and(|s| s.success());
    if !claude && ok {
        finish_turn(&app, &id).await;
        return;
    }
    let tail = stderr_tail.lock().unwrap().trim().to_string();
    let reason = if tail.is_empty() { format!("{} 进程意外退出（{:?}）", label(&run.agent), status.and_then(|s| s.code())) } else { clip(&tail, 1500) };
    event(&app, &id, "error", &reason);
    let _ = update(&app, &id, |r| {
        r.status = "error".into();
        r.data["error"] = json!(reason);
    });
    notify_background(&app, &id);
}

async fn on_parsed(app: &AppHandle, id: &str, p: Parsed, generation: u64) {
    match p {
        Parsed::Session(s) => {
            let _ = update(app, id, |r| {
                if r.data.get("external_session").and_then(Value::as_str) != Some(s.as_str()) {
                    r.data["external_session"] = json!(s);
                }
            });
        }
        Parsed::Text(t) => {
            event(app, id, "text", &t);
            let _ = update(app, id, |r| r.data["last_message"] = json!(clip(&t, 4000)));
        }
        Parsed::Tool { name, detail } => {
            // 回问拾光的 MCP 调用另有 ask/checkpoint 事件，不重复显示
            if !name.contains("shiguang") {
                event(app, id, "tool", &json!({"name": name, "detail": detail}).to_string());
            }
        }
        Parsed::ToolFailed(detail) => event(app, id, "tool_error", &detail),
        Parsed::Error(msg) => {
            event(app, id, "error", &msg);
            let _ = update(app, id, |r| r.data["last_error"] = json!(msg));
        }
        Parsed::TurnDone { cost_usd, is_error, text } => {
            if let Some(c) = cost_usd {
                let _ = update(app, id, |r| r.data["cost_usd"] = json!(c));
            }
            if is_error {
                let reason = text.unwrap_or_else(|| "本轮出错".into());
                event(app, id, "error", &reason);
                let _ = update(app, id, |r| {
                    r.status = "error".into();
                    r.data["error"] = json!(clip(&reason, 1500));
                });
                notify_background(app, id);
            } else {
                finish_turn(app, id).await;
                schedule_idle_close(app.clone(), id.to_string(), generation);
            }
        }
    }
}

/// 一轮结束：拾光自己算 git 改动，不只听智能体的总结。
async fn finish_turn(app: &AppHandle, id: &str) {
    let Ok(run) = load(app, id) else { return };
    let result = git_result(&run);
    let summary = match result.get("changed_total").and_then(Value::as_u64) {
        Some(0) => "本轮结束，没有文件改动".to_string(),
        Some(n) => format!("本轮结束，{n} 个文件有改动"),
        None => "本轮结束".to_string(),
    };
    event(app, id, "result", &format!("{summary}{}", if result["head_changed"] == json!(true) { "；注意：智能体自己提交了 commit" } else { "" }));
    let _ = update(app, id, |r| {
        r.status = "idle".into();
        r.data["result"] = result;
        r.data["question"] = Value::Null;
    });
    notify_background(app, id);
}

fn git_result(run: &AgentRunRow) -> Value {
    if run.data.get("is_repo").and_then(Value::as_bool) != Some(true) {
        return json!({"note": "不是 git 仓库，无法统计改动"});
    }
    let root = work_root(run);
    let status = crate::workspaces::git(&root, &["status", "--porcelain"]).unwrap_or_default();
    let changed: Vec<&str> = status.lines().collect();
    let head = crate::workspaces::git(&root, &["rev-parse", "HEAD"]);
    let base = run.data.get("base_head").and_then(Value::as_str);
    let head_changed = matches!((base, head.as_deref()), (Some(b), Some(h)) if b != h);
    let mut out = json!({
        "changed_files": changed.iter().take(60).collect::<Vec<_>>(),
        "changed_total": changed.len(),
        "diff_stat": crate::workspaces::git(&root, &["diff", "--stat", "HEAD"]).map(|s| clip(&s, 3000)),
        "head_changed": head_changed,
    });
    if head_changed {
        if let Some(b) = base {
            out["new_commits"] = json!(crate::workspaces::git(&root, &["log", "--oneline", &format!("{b}..HEAD")]));
        }
    }
    out
}

fn schedule_idle_close(app: AppHandle, id: String, generation: u64) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(IDLE_SECS)).await;
        let h = handle(&app, &id);
        let still_idle = load(&app, &id).map(|r| r.status == "idle").unwrap_or(false);
        let mut inner = h.inner.lock().unwrap();
        if still_idle && inner.generation == generation {
            if let Some(c) = inner.proc_cancel.take() {
                c.cancel();
            }
            inner.stdin = None;
        }
    });
}

/// 没有人在 await 时，把提问/结束写进会话，提醒主对话。
fn notify_background(app: &AppHandle, id: &str) {
    let h = handle(app, id);
    if h.inner.lock().unwrap().awaiting > 0 {
        return;
    }
    let Ok(run) = load(app, id) else { return };
    let state = app.state::<crate::AppState>();
    if state.chat_busy.load(std::sync::atomic::Ordering::SeqCst) || run.session_id == 0 {
        return;
    }
    let who = label(&run.agent);
    let line = match run.status.as_str() {
        "asking" => format!("外派任务「{}」（{who}）在等回答：{}。可以说「回复外派 {}」或直接在运行窗口回答。", run.title, run.data.pointer("/question/text").and_then(Value::as_str).unwrap_or(""), run.id),
        "idle" => format!("外派任务「{}」（{who}）本轮结束：{}", run.title, clip(run.data.get("last_message").and_then(Value::as_str).unwrap_or(""), 300)),
        "error" => format!("外派任务「{}」（{who}）出错：{}", run.title, clip(run.data.get("error").and_then(Value::as_str).unwrap_or(""), 300)),
        _ => return,
    };
    let _ = state.db.save_chat(run.session_id, "system", &line);
    let _ = app.emit("subagent-chat", json!({"session_id": run.session_id, "content": line}));
}

// ---------- 智能体回问（MCP 桥） ----------

pub fn mcp_ask(app: &AppHandle, id: &str, kind: &str, payload: &Value) -> Result<Value> {
    let run = load(app, id)?;
    if run.status != "running" {
        bail!("外派任务当前状态为 {}，不能提问", run.status);
    }
    let exchanges = run.data.get("exchanges").and_then(Value::as_u64).unwrap_or(0) + 1;
    if exchanges > MAX_EXCHANGES {
        event(app, id, "system", "已达到沟通上限，智能体被要求自行判断");
        return Ok(json!({"answer": format!("本次外派已沟通 {MAX_EXCHANGES} 次，达到上限。请按现有信息自行判断；需要用户决定的事写进最终总结，不要再提问。")}));
    }
    let text = match kind {
        "checkpoint" => {
            let get = |k: &str| payload.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
            let mut parts = vec![format!("【关键节点】{}", get("milestone"))];
            for (label, key) in [("已完成", "done"), ("证据", "evidence"), ("下一步", "next")] {
                let v = get(key);
                if !v.is_empty() {
                    parts.push(format!("{label}：{v}"));
                }
            }
            parts.join("\n")
        }
        _ => {
            let q = payload.get("question").and_then(Value::as_str).unwrap_or("").trim().to_string();
            let ctx = payload.get("context").and_then(Value::as_str).unwrap_or("").trim().to_string();
            if q.is_empty() {
                bail!("question 不能为空");
            }
            if ctx.is_empty() { q } else { format!("{q}\n（背景：{ctx}）") }
        }
    };
    let qid = uuid::Uuid::new_v4().simple().to_string();
    handle(app, id).inner.lock().unwrap().pending = Some(Pending { qid: qid.clone(), answer: None });
    event(app, id, if kind == "checkpoint" { "checkpoint" } else { "ask" }, &text);
    update(app, id, |r| {
        r.status = "asking".into();
        r.data["exchanges"] = json!(exchanges);
        r.data["question"] = json!({"qid": qid, "kind": kind, "text": text});
    })?;
    notify_background(app, id);
    Ok(json!({"qid": qid}))
}

pub fn mcp_poll(app: &AppHandle, id: &str, qid: &str) -> Result<Value> {
    let h = handle(app, id);
    let mut inner = h.inner.lock().unwrap();
    match &inner.pending {
        Some(p) if p.qid == qid => {
            if let Some(answer) = p.answer.clone() {
                inner.pending = None;
                return Ok(json!({"answer": answer}));
            }
            Ok(json!({"pending": true}))
        }
        _ => {
            drop(inner);
            let status = load(app, id).map(|r| r.status).unwrap_or_default();
            if matches!(status.as_str(), "cancelled" | "error" | "closed") {
                Ok(json!({"cancelled": true}))
            } else {
                Ok(json!({"answer": "这个问题已失效（可能被新的指示取代）。请按最新指示继续。"}))
            }
        }
    }
}

// ---------- 主 AI / 用户 回复 ----------

/// from：main=主 AI，user=用户（运行窗口）。
pub async fn reply(app: &AppHandle, id: &str, content: &str, from: &str) -> Result<Value> {
    let content = content.trim();
    if content.is_empty() {
        bail!("回复内容不能为空");
    }
    let run = load(app, id)?;
    let who = if from == "user" { "user" } else { "main" };
    match run.status.as_str() {
        "asking" => {
            let h = handle(app, id);
            let answered = {
                let mut inner = h.inner.lock().unwrap();
                match inner.pending.as_mut() {
                    Some(p) if p.answer.is_none() => {
                        p.answer = Some(if from == "user" { format!("（用户回答）{content}") } else { content.to_string() });
                        true
                    }
                    _ => false,
                }
            };
            if answered {
                event(app, id, who, content);
                update(app, id, |r| {
                    r.status = "running".into();
                    r.data["question"] = Value::Null;
                })?;
                return Ok(json!({"run_id": id, "status": "running", "note": "已答复，智能体继续执行；用 await_agent 等它下一次提问或本轮结束。"}));
            }
            // 进程已不在（如应用重启），作为新一轮发送
            event(app, id, who, content);
            send_turn(app, id, content).await?;
        }
        "idle" | "error" | "cancelled" => {
            event(app, id, who, content);
            send_turn(app, id, content).await?;
        }
        "running" => {
            let live = handle(app, id).inner.lock().unwrap().stdin.clone();
            match (run.agent.as_str(), live) {
                ("claude_code", Some(stdin)) => {
                    write_line(&stdin, &adapters::claude_user_line(&format!("（插话）{content}"))).await?;
                    event(app, id, who, content);
                }
                _ => bail!("智能体正在执行，等它提问或本轮结束后再回复（await_agent）"),
            }
        }
        other => bail!("外派任务已{}，不能继续", if other == "closed" { "收尾" } else { other }),
    }
    Ok(json!({"run_id": id, "status": "running", "note": "已发送，智能体在同一会话里继续；用 await_agent 等结果。"}))
}

pub fn stop(app: &AppHandle, id: &str) -> Result<Value> {
    let run = load(app, id)?;
    let h = handle(app, id);
    {
        let mut inner = h.inner.lock().unwrap();
        if let Some(c) = inner.proc_cancel.take() {
            c.cancel();
        }
        inner.stdin = None;
        inner.pending = None;
    }
    if matches!(run.status.as_str(), "running" | "asking") {
        event(app, id, "system", "已中断");
        update(app, id, |r| {
            r.status = "cancelled".into();
            r.data["question"] = Value::Null;
        })?;
        return Ok(json!({"run_id": id, "status": "cancelled"}));
    }
    Ok(json!({"run_id": id, "status": run.status, "note": "本轮已不在执行，已关闭智能体进程；仍可 agent_reply 续接原会话。"}))
}

// ---------- 等待与汇报 ----------

struct AwaitGuard(Arc<Handle>);
impl Drop for AwaitGuard {
    fn drop(&mut self) {
        let mut inner = self.0.inner.lock().unwrap();
        inner.awaiting = inner.awaiting.saturating_sub(1);
    }
}

pub async fn await_run(app: &AppHandle, id: &str, timeout_secs: u64, cancel: &CancellationToken) -> Result<Value> {
    let h = handle(app, id);
    h.inner.lock().unwrap().awaiting += 1;
    let _guard = AwaitGuard(h.clone());
    let mut rx = h.tx.subscribe();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs.clamp(1, MAX_AWAIT_SECS));
    loop {
        let run = load(app, id)?;
        if run.status != "running" {
            return Ok(view_for_main(&run));
        }
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = stop(app, id);
                bail!("已中断（外派任务一并停止）");
            }
            _ = tokio::time::sleep_until(deadline) => return Ok(view_for_main(&load(app, id)?)),
            changed = rx.changed() => { if changed.is_err() { tokio::time::sleep(std::time::Duration::from_millis(500)).await; } }
        }
    }
}

fn view_for_main(run: &AgentRunRow) -> Value {
    let d = &run.data;
    let mut v = json!({
        "run_id": run.id, "agent": label(&run.agent), "status": run.status, "title": run.title,
        "workspace": d.get("workspace_name"), "mode": run.mode,
        "last_message": d.get("last_message").and_then(Value::as_str).map(|s| clip(s, 3000)),
        "exchanges": d.get("exchanges"),
    });
    let note = match run.status.as_str() {
        "asking" => {
            v["question"] = d.pointer("/question/text").cloned().unwrap_or(Value::Null);
            v["question_kind"] = d.pointer("/question/kind").cloned().unwrap_or(Value::Null);
            "智能体在等你回答。能凭工作区事实、线上实测、你掌握的上下文回答就用 agent_reply；关键节点核对证据后明确是否继续及范围。涉及 push、删除、扩大范围、线上数据、花钱的决定先 ask_user，不能替用户批准。".to_string()
        }
        "idle" => {
            v["result"] = d.get("result").cloned().unwrap_or(Value::Null);
            v["verify_commands"] = d.get("verify_commands").cloned().unwrap_or(Value::Null);
            let mut n = "本轮结束。result 是拾光自己统计的 git 改动；按 verify_commands 自己验证，必要时看 diff。需要修改用 agent_reply 在同一会话追问。提交/push 前先征得用户同意，由你执行 git。".to_string();
            if d.pointer("/result/head_changed") == Some(&json!(true)) {
                n.push_str("注意：智能体违反约定自己提交了 commit（见 new_commits），告诉用户。");
            }
            if d.pointer("/worktree/state").and_then(Value::as_str) == Some("active") {
                v["worktree"] = d.get("worktree").cloned().unwrap_or(Value::Null);
                n.push_str("改动在隔离 worktree 里，用户确认后 finish_agent_worktree apply 合回原项目，不要就 discard。");
            }
            n
        }
        "error" => {
            v["error"] = d.get("error").cloned().unwrap_or(Value::Null);
            "出错了。能修复的（补信息、换说法）用 agent_reply 在原会话续接；额度用尽、未登录等告诉用户。".to_string()
        }
        "running" => "仍在运行。再次 await_agent 等待，不要轮询。".to_string(),
        "cancelled" => "已中断。需要继续可 agent_reply 续接原会话。".to_string(),
        _ => String::new(),
    };
    v["note"] = json!(note);
    v
}

pub fn run_view(app: &AppHandle, id: &str, after: i64) -> Result<Value> {
    let run = load(app, id)?;
    let events = app.state::<crate::AppState>().db.agent_run_events(id, after, 2000)?;
    Ok(json!({"run": run, "events": events, "agent_label": label(&run.agent)}))
}

// ---------- worktree 隔离 ----------

fn create_worktree(app: &AppHandle, root: &Path, id: &str) -> Result<(PathBuf, String)> {
    let base = app.path().app_data_dir()?.join("worktrees");
    std::fs::create_dir_all(&base)?;
    let path = base.join(id);
    let branch = format!("shiguang/{id}");
    git_ok(root, &["worktree", "add", "-b", &branch, &path.to_string_lossy(), "HEAD"])?;
    Ok((path, branch))
}

pub async fn finish_worktree(app: &AppHandle, id: &str, action: &str) -> Result<Value> {
    let run = load(app, id)?;
    let wt = run.data.get("worktree").cloned().ok_or_else(|| anyhow!("这个外派没有使用 worktree"))?;
    if wt.get("state").and_then(Value::as_str) != Some("active") {
        bail!("worktree 已处理过");
    }
    if matches!(run.status.as_str(), "running" | "asking") {
        bail!("智能体还在执行，先等它结束或 stop_agent");
    }
    stop_process(app, id).await;
    let path = PathBuf::from(wt.get("path").and_then(Value::as_str).unwrap_or(""));
    let branch = wt.get("branch").and_then(Value::as_str).unwrap_or("").to_string();
    let root = PathBuf::from(&run.root);
    let mut applied_files = Vec::new();
    if action == "apply" {
        git_ok(&path, &["add", "-A"])?;
        let files = git_ok(&path, &["diff", "--cached", "--name-only"])?;
        applied_files = files.lines().map(str::to_string).collect();
        if !applied_files.is_empty() {
            let mut cmd = std::process::Command::new("git");
            cmd.args(["diff", "--cached", "--binary"]).current_dir(&path);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(crate::tasks::CREATE_NO_WINDOW);
            }
            let patch = cmd.output()?.stdout;
            let patch_file = crate::tempfs::ensure_temp_dir(app)?.join(format!("{id}.patch"));
            std::fs::write(&patch_file, &patch)?;
            git_ok(&root, &["apply", "--3way", "--whitespace=nowarn", &patch_file.to_string_lossy()])
                .map_err(|e| anyhow!("合回原项目失败（worktree 保留未删）：{e}"))?;
            let _ = std::fs::remove_file(&patch_file);
        }
    } else if action != "discard" {
        bail!("action 只能是 apply 或 discard");
    }
    // Windows 上刚结束的进程可能还占着目录，稍等重试；删不掉也不影响已合回的结果。
    let mut removed = false;
    for _ in 0..10 {
        if git_ok(&root, &["worktree", "remove", "--force", &path.to_string_lossy()]).is_ok() {
            removed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    if !removed {
        let _ = git_ok(&root, &["worktree", "prune"]);
    }
    let _ = git_ok(&root, &["branch", "-D", &branch]);
    let state = if action == "apply" { "applied" } else { "discarded" };
    event(app, id, "system", if action == "apply" { "worktree 改动已合回原项目" } else { "worktree 已丢弃" });
    update(app, id, |r| {
        r.status = "closed".into();
        r.data["worktree"]["state"] = json!(state);
    })?;
    Ok(json!({
        "run_id": id, "worktree": state, "applied_files": applied_files, "worktree_removed": removed,
        "note": if action == "apply" { "改动已在原项目里（未提交）。验证后征得用户同意再提交。" } else { "" },
    }))
}

/// 结束智能体进程并等它真正退出（最多约 5 秒），以便删除它占用的目录。
async fn stop_process(app: &AppHandle, id: &str) {
    let h = handle(app, id);
    let cancel = {
        let mut inner = h.inner.lock().unwrap();
        inner.stdin = None;
        inner.proc_cancel.clone()
    };
    let Some(cancel) = cancel else { return };
    cancel.cancel();
    for _ in 0..50 {
        if h.inner.lock().unwrap().proc_cancel.is_none() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

// ---------- AI 工具 ----------

pub const TOOL_NAMES: &[&str] = &["delegate_to_agent", "await_agent", "agent_reply", "stop_agent", "list_agents", "finish_agent_worktree"];

pub fn definitions() -> Vec<Value> {
    vec![
        json!({"type":"function","function":{
            "name":"delegate_to_agent",
            "description":"把工作区里的代码修改外派给 Claude Code 或 Codex（用户自己的订阅）。适合多文件改动、需要读懂项目再动手的任务；查数据、浏览器、跑 CLI、一两行小改仍自己做。task 写清目标、范围、背景和不要做的事；acceptance 写可核对的验收标准。智能体不能提交/推送，过程中会通过你回问（status=asking），你回答后它继续。默认挂起等到它提问或本轮结束。",
            "parameters":{"type":"object","properties":{
                "task":{"type":"string","description":"任务说明：要改什么、为什么、范围、已知事实"},
                "acceptance":{"type":"string","description":"验收标准，如「npm run build 通过；响应头没有 Set-Cookie」"},
                "workspace":{"description":"工作区名称/别名/id；省略时用当前会话绑定的工作区"},
                "root":{"type":"string","description":"没有登记工作区时直接给项目根目录"},
                "agent":{"type":"string","enum":["auto","claude_code","codex"],"description":"默认 auto：工作区默认 → Claude Code → Codex"},
                "mode":{"type":"string","enum":["plan","edit","full"],"description":"plan 只出方案不改文件；edit 改文件跑命令（默认）；full 跳过智能体自身的确认"},
                "isolation":{"type":"string","enum":["none","worktree"],"description":"worktree：在独立 git worktree 里改，结束后 finish_agent_worktree 合回或丢弃"},
                "allow_dirty":{"type":"boolean","description":"项目有未提交改动时，用户同意后才设为 true"},
                "title":{"type":"string","description":"运行窗口标题（可选）"},
                "await":{"type":"boolean","description":"默认 true；false 则立即返回 run_id"},
                "timeout_secs":{"type":"integer","description":"最长等待秒数，默认 1200"}
            },"required":["task"]}
        }}),
        json!({"type":"function","function":{
            "name":"await_agent",
            "description":"挂起等待外派任务的下一个提问/关键节点，或本轮结束。不要轮询。",
            "parameters":{"type":"object","properties":{"run_id":{"type":"string"},"timeout_secs":{"type":"integer"}},"required":["run_id"]}
        }}),
        json!({"type":"function","function":{
            "name":"agent_reply",
            "description":"回复外派智能体：status=asking 时作为对它问题的回答；本轮已结束（idle/error/cancelled）时作为同一会话的追问或纠偏。不能替用户批准 push、删除、扩大范围。默认发送后继续等待。",
            "parameters":{"type":"object","properties":{
                "run_id":{"type":"string"}, "content":{"type":"string"},
                "await":{"type":"boolean","description":"默认 true"}, "timeout_secs":{"type":"integer"}
            },"required":["run_id","content"]}
        }}),
        json!({"type":"function","function":{
            "name":"stop_agent",
            "description":"中断外派任务（结束智能体进程）。之后仍可 agent_reply 续接原会话。",
            "parameters":{"type":"object","properties":{"run_id":{"type":"string"}},"required":["run_id"]}
        }}),
        json!({"type":"function","function":{
            "name":"list_agents",
            "description":"列出可用的编码智能体（是否安装、版本）和最近的外派任务。",
            "parameters":{"type":"object","properties":{}}
        }}),
        json!({"type":"function","function":{
            "name":"finish_agent_worktree",
            "description":"处理隔离 worktree 里的改动：apply 合回原项目（未提交），discard 丢弃。apply 前先给用户看改动并征得同意。",
            "parameters":{"type":"object","properties":{"run_id":{"type":"string"},"action":{"type":"string","enum":["apply","discard"]}},"required":["run_id","action"]}
        }}),
    ]
}

fn timeout_arg(args: &Value) -> u64 {
    args.get("timeout_secs").and_then(Value::as_u64).unwrap_or(DEFAULT_AWAIT_SECS)
}

pub async fn execute(app: &AppHandle, name: &str, args: &Value, cancel: &CancellationToken, session_id: i64) -> Result<Value> {
    let run_id = || args.get("run_id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("需要 run_id"));
    match name {
        "delegate_to_agent" => {
            let started = start(app, args, session_id).await?;
            let Some(id) = started.get("run_id").and_then(Value::as_str).map(str::to_string) else { return Ok(started) };
            if args.get("await").and_then(Value::as_bool) == Some(false) {
                let mut v = started;
                v["note"] = json!("已在后台运行。需要结果时 await_agent；它提问或结束时会在会话里提醒。");
                return Ok(v);
            }
            await_run(app, &id, timeout_arg(args), cancel).await
        }
        "await_agent" => await_run(app, run_id()?, timeout_arg(args), cancel).await,
        "agent_reply" => {
            let id = run_id()?;
            let sent = reply(app, id, args.get("content").and_then(Value::as_str).unwrap_or(""), "main").await?;
            if args.get("await").and_then(Value::as_bool) == Some(false) {
                return Ok(sent);
            }
            await_run(app, id, timeout_arg(args), cancel).await
        }
        "stop_agent" => stop(app, run_id()?),
        "list_agents" => {
            let agents = detect(app).await;
            let runs: Vec<Value> = app.state::<crate::AppState>().db.agent_run_list(10)?.iter().map(|r| json!({
                "run_id": r.id, "agent": label(&r.agent), "title": r.title, "status": r.status, "updated_at": r.updated_at,
            })).collect();
            Ok(json!({"agents": agents, "recent_runs": runs}))
        }
        "finish_agent_worktree" => finish_worktree(app, run_id()?, args.get("action").and_then(Value::as_str).unwrap_or("")).await,
        _ => bail!("未知外派工具 {name}"),
    }
}
