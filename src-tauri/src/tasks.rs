//! 后台命令任务：命令在后台执行，stdout/stderr 直接写入日志文件，
//! 对话上下文里只放状态与按需截取的输出片段（尾部 / 关键字过滤），
//! 避免大量输出挤占模型上下文窗口。

use anyhow::{anyhow, bail, Result};
use base64::Engine as _;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

/// check_task 默认返回的尾部字符数
const DEFAULT_TAIL_CHARS: usize = 2000;
/// check_task 尾部字符上限
pub const MAX_TAIL_CHARS: usize = 8000;
/// 关键字过滤时最多返回的匹配行数
const MAX_MATCH_LINES: usize = 50;
/// 读日志做过滤时最多从文件末尾回读的字节数（防止超大日志拖慢查询）
const SCAN_BACK_BYTES: u64 = 2 * 1024 * 1024;
/// CREATE_NO_WINDOW：后台命令不弹黑色控制台窗口
pub(crate) const CREATE_NO_WINDOW: u32 = 0x08000000;
const MAX_ARGV_ITEMS: usize = 64;
const MAX_ARGV_ITEM_CHARS: usize = 32_768;
const MAX_STDIN_BYTES: usize = 256 * 1024;

pub struct CommandSpec<'a> {
    pub command: &'a str,
    pub argv: &'a [String],
    pub stdin: Option<&'a str>,
    pub label: Option<&'a str>,
    pub workdir: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub powershell_strict: bool,
    pub success_exit_codes: &'a [i32],
    pub script_args: &'a Value,
    pub environment: &'a HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskInfo {
    pub id: String,
    pub label: String,
    pub command: String,
    /// cmd / powershell / direct；direct 表示 argv 直启、不经 shell。
    pub shell: String,
    /// explicit / legacy_wrapper / inferred / default
    pub shell_selection: String,
    /// cmd / encoded_command / encoded_bootstrap_file / argv
    pub transport: String,
    /// 被视为成功的进程退出码。
    pub success_exit_codes: Vec<i32>,
    /// running / done / failed / cancelled / timeout
    pub status: String,
    pub exit_code: Option<i32>,
    pub pid: u32,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub log_path: String,
}

struct TaskEntry {
    info: TaskInfo,
    success_exit_codes: Vec<i32>,
}

type TaskMap = Arc<Mutex<HashMap<String, TaskEntry>>>;

pub struct TaskManager {
    dir: PathBuf,
    seq: AtomicU64,
    tasks: TaskMap,
}

impl TaskManager {
    pub fn new(app_dir: &Path) -> Self {
        let dir = app_dir.join("tasks");
        let _ = std::fs::create_dir_all(&dir);
        cleanup_stale_powershell_wrappers(&dir);
        Self {
            dir,
            seq: AtomicU64::new(0),
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 启动后台命令，输出实时写入日志文件后立即返回。
    /// 外部程序优先 argv 直启（CreateProcess，不经 shell）；
    /// cmd 命令经 cmd /c；PowerShell 脚本经 UTF-16LE Base64 EncodedCommand。
    pub fn start_command(&self, app: &AppHandle, spec: CommandSpec<'_>) -> Result<TaskInfo> {
        let command = spec.command.trim();
        let argv_mode = !spec.argv.is_empty();
        if !argv_mode && command.is_empty() {
            bail!("请提供 argv（推荐，调用外部程序）或 command（cmd / PowerShell 脚本）");
        }
        let stdin_data = normalize_stdin(spec.stdin)?;
        let display_command = if argv_mode {
            spec.argv.join(" ")
        } else {
            command.to_string()
        };
        let id = format!("t{}", self.seq.fetch_add(1, Ordering::SeqCst) + 1);
        let log_path = self.dir.join(format!("task-{}.log", id));
        let log_file = std::fs::File::create(&log_path)?;
        let stderr_file = log_file.try_clone()?;

        let dir = spec
            .workdir
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| dirs::desktop_dir().unwrap_or_else(|| PathBuf::from(".")));

        let accepted_exit_codes = normalize_success_exit_codes(spec.success_exit_codes)?;
        let mut transient_script = None;
        let (mut cmd, shell_name, shell_selection, transport) = if argv_mode {
            let (program, args) = validate_argv(spec.argv)?;
            let mut cmd = tokio::process::Command::new(&program);
            cmd.args(&args);
            (cmd, "direct", "argv", "argv")
        } else {
            let (shell_kind, executable_command, shell_selection) =
                resolve_shell(command, spec.shell)?;
            let mut transport = "cmd";
            let cmd = match shell_kind {
                ShellKind::Cmd => {
                    let mut cmd = tokio::process::Command::new("cmd");
                    cmd.args(["/d", "/s", "/c", &utf8_command(&executable_command)]);
                    cmd
                }
                ShellKind::PowerShell => {
                    let mut cmd = tokio::process::Command::new("powershell");
                    let wrapper = powershell_wrapper(
                        &executable_command,
                        spec.powershell_strict,
                        spec.script_args,
                    )?;
                    let encoded = encode_powershell_command(&wrapper);
                    if encoded.len() <= 24_000 {
                        transport = "encoded_command";
                        cmd.args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded]);
                    } else {
                        transport = "encoded_bootstrap_file";
                        let script_path = self.dir.join(format!("task-{}.ps1", id));
                        write_utf8_bom(&script_path, &wrapper)?;
                        let bootstrap = encode_powershell_command(powershell_file_bootstrap());
                        cmd.args([
                            "-NoProfile",
                            "-NonInteractive",
                            "-EncodedCommand",
                            &bootstrap,
                        ]);
                        transient_script = Some(script_path);
                    }
                    cmd
                }
            };
            (cmd, shell_kind.as_str(), shell_selection, transport)
        };
        cmd.current_dir(&dir)
            .envs(spec.environment)
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(if stdin_data.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(stderr_file))
            .creation_flags(CREATE_NO_WINDOW);
        if let Some(path) = transient_script.as_ref() {
            // 内部保留变量最后设置，避免被调用方 environment 覆盖。
            cmd.env("DH_PS_WRAPPER_PATH", path);
        }
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                if let Some(path) = transient_script.as_ref() {
                    let _ = std::fs::remove_file(path);
                }
                return Err(anyhow!("启动命令失败: {}", error));
            }
        };
        let pid = child.id().unwrap_or(0);

        let info = TaskInfo {
            id: id.clone(),
            label: spec
                .label
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(&display_command)
                .chars()
                .take(60)
                .collect(),
            command: display_command,
            shell: shell_name.to_string(),
            shell_selection: shell_selection.to_string(),
            transport: transport.to_string(),
            success_exit_codes: accepted_exit_codes.clone(),
            status: "running".to_string(),
            exit_code: None,
            pid,
            started_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            finished_at: None,
            log_path: log_path.to_string_lossy().replace('\\', "/"),
        };
        self.tasks
            .lock()
            .map_err(|e| anyhow!(e.to_string()))?
            .insert(
                id.clone(),
                TaskEntry {
                    info: info.clone(),
                    success_exit_codes: accepted_exit_codes,
                },
            );
        emit_changed(app, &info);

        // 监控协程：独占 child，等进程退出后回填状态。
        // 输出走日志文件而非管道，用 wait() 即可。
        let app2 = app.clone();
        let tasks2 = Arc::clone(&self.tasks);
        let id2 = id.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(data) = stdin_data {
                if let Some(mut pipe) = child.stdin.take() {
                    use tokio::io::AsyncWriteExt;
                    let _ = pipe.write_all(data.as_bytes()).await;
                    let _ = pipe.shutdown().await;
                }
            }
            let result = child.wait().await;
            let (status, code) = match result {
                Ok(s) => {
                    let code = s.code();
                    let accepted = tasks2
                        .lock()
                        .ok()
                        .and_then(|guard| {
                            guard
                                .get(&id2)
                                .map(|entry| entry.success_exit_codes.clone())
                        })
                        .unwrap_or_else(|| vec![0]);
                    (
                        if code.is_some_and(|value| accepted.contains(&value)) {
                            "done"
                        } else {
                            "failed"
                        },
                        code,
                    )
                }
                Err(_) => ("failed", None),
            };
            if let Some(path) = transient_script {
                let _ = std::fs::remove_file(path);
            }
            let finished = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let snapshot = {
                let Ok(mut guard) = tasks2.lock() else { return };
                let Some(entry) = guard.get_mut(&id2) else {
                    return;
                };
                // 用户主动 stop 时状态已被标记为 cancelled，不覆盖
                if entry.info.status == "running" {
                    entry.info.status = status.to_string();
                }
                entry.info.exit_code = code;
                entry.info.finished_at = Some(finished);
                entry.info.clone()
            };
            emit_changed(&app2, &snapshot);
        });
        Ok(info)
    }

    pub fn get(&self, id: &str) -> Option<TaskInfo> {
        self.tasks.lock().ok()?.get(id).map(|e| e.info.clone())
    }

    pub fn list(&self) -> Vec<TaskInfo> {
        let Ok(guard) = self.tasks.lock() else {
            return Vec::new();
        };
        let mut v: Vec<TaskInfo> = guard.values().map(|e| e.info.clone()).collect();
        // id 是 t+自增序号，按序号倒序即最新在前
        v.sort_by_key(|t| {
            std::cmp::Reverse(t.id.trim_start_matches('t').parse::<u64>().unwrap_or(0))
        });
        v.truncate(20);
        v
    }

    /// 停止运行中的任务：taskkill /T 杀整棵进程树（cmd 拉起的子进程一并结束）
    pub fn stop(&self, app: &AppHandle, id: &str) -> Result<TaskInfo> {
        let (pid, already_finished) = {
            let mut guard = self.tasks.lock().map_err(|e| anyhow!(e.to_string()))?;
            let entry = guard
                .get_mut(id)
                .ok_or_else(|| anyhow!("任务不存在: {}", id))?;
            if entry.info.status != "running" {
                (entry.info.pid, true)
            } else {
                entry.info.status = "cancelled".to_string();
                (entry.info.pid, false)
            }
        };
        if !already_finished {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .creation_flags(CREATE_NO_WINDOW)
                .output();
        }
        let info = self.get(id).ok_or_else(|| anyhow!("任务不存在: {}", id))?;
        emit_changed(app, &info);
        Ok(info)
    }

    /// 读取日志尾部 max_chars 个字符（从文件末尾 seek，不整读）
    pub fn tail(&self, id: &str, max_chars: usize) -> Result<String> {
        let info = self.get(id).ok_or_else(|| anyhow!("任务不存在: {}", id))?;
        let cap = if max_chars == 0 {
            DEFAULT_TAIL_CHARS
        } else {
            max_chars.min(MAX_TAIL_CHARS)
        };
        read_tail(Path::new(&info.log_path), cap)
    }

    /// run_command 用：输出不超过 max_chars 时全量返回；超出时返回
    /// 「开头 40% + 省略标记 + 结尾 60%」并标 truncated=true——
    /// 避免 && 串联多条命令时前段命令的输出被整体截掉。
    pub fn head_tail(&self, id: &str, max_chars: usize) -> Result<(String, bool)> {
        let info = self.get(id).ok_or_else(|| anyhow!("任务不存在: {}", id))?;
        let cap = if max_chars == 0 {
            DEFAULT_TAIL_CHARS
        } else {
            max_chars.min(MAX_TAIL_CHARS)
        };
        let path = Path::new(&info.log_path);
        let len = std::fs::metadata(path)?.len();
        // 小文件整读：按真实字符数判断是否需要截断，避免恰好等于 cap 时误判
        if len <= (cap * 4 + 64) as u64 {
            let text = decode_log(&std::fs::read(path)?);
            let count = text.chars().count();
            if count <= cap {
                return Ok((text, false));
            }
            let head: String = text.chars().take(cap * 2 / 5).collect();
            let tail: String = text.chars().skip(count - cap).collect();
            return Ok((join_head_tail(&head, &tail), true));
        }
        let tail = read_tail(path, cap)?;
        let head = read_head(path, cap * 2 / 5)?;
        Ok((join_head_tail(&head, &tail), true))
    }

    /// 读取日志中含 pattern 的最近若干行（最多回读 2MB）
    pub fn grep(&self, id: &str, pattern: &str) -> Result<Vec<String>> {
        let info = self.get(id).ok_or_else(|| anyhow!("任务不存在: {}", id))?;
        let text = read_tail_bytes(Path::new(&info.log_path), SCAN_BACK_BYTES)?;
        let mut hits: Vec<String> = text
            .lines()
            .filter(|l| l.contains(pattern))
            .map(|l| l.chars().take(300).collect())
            .collect();
        if hits.len() > MAX_MATCH_LINES {
            hits = hits.split_off(hits.len() - MAX_MATCH_LINES);
        }
        Ok(hits)
    }

    /// 同步执行命令并等待结束（带超时，超时自动杀进程树），供 run_command 工具使用
    pub async fn run_sync(
        &self,
        app: &AppHandle,
        spec: CommandSpec<'_>,
        timeout_secs: u64,
    ) -> Result<TaskInfo> {
        let info = self.start_command(app, spec)?;
        let id = info.id.clone();
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);
        loop {
            if let Some(cur) = self.get(&id) {
                if cur.status != "running" {
                    return Ok(cur);
                }
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = self.stop(app, &id);
                // stop 标的是 cancelled，这里改记为 timeout；监控协程只覆盖 running，不会冲掉
                if let Ok(mut guard) = self.tasks.lock() {
                    if let Some(entry) = guard.get_mut(&id) {
                        entry.info.status = "timeout".to_string();
                    }
                }
                let cur = self.get(&id).unwrap_or(info);
                let _ = app.emit("task-changed", &cur);
                return Ok(cur);
            }
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    }
}

/// 每个命令都在独立 cmd 子进程内执行，切换代码页不会影响用户自己的终端。
/// UTF-8 优先可统一 cmd 内建命令与大多数现代 CLI；日志读取仍保留 GB18030 回退，
/// 兼容忽略活动代码页的旧程序。
fn utf8_command(command: &str) -> String {
    format!("chcp 65001 >nul & {}", command)
}

fn normalize_stdin(stdin: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = stdin.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if raw.len() > MAX_STDIN_BYTES {
        bail!("stdin 最长 {} 字节", MAX_STDIN_BYTES);
    }
    Ok(Some(raw.to_string()))
}

fn validate_argv(argv: &[String]) -> Result<(String, Vec<String>)> {
    if argv.len() > MAX_ARGV_ITEMS {
        bail!("argv 最多 {} 项", MAX_ARGV_ITEMS);
    }
    let program = argv[0].trim();
    if program.is_empty() {
        bail!("argv 第一项必须是可执行文件名或路径");
    }
    if program.chars().count() > MAX_ARGV_ITEM_CHARS {
        bail!("argv 单项过长");
    }
    if looks_like_unsplit_shell_line(program) {
        bail!(
            "argv 第一项应是程序名，不要把整条命令放进一项。请拆成 [\"git\", \"status\"] 这种形式"
        );
    }
    let args = argv[1..].to_vec();
    for (index, arg) in args.iter().enumerate() {
        if arg.chars().count() > MAX_ARGV_ITEM_CHARS {
            bail!("argv[{}] 过长", index + 1);
        }
    }
    Ok((program.to_string(), args))
}

/// 模型常把整条命令塞进 argv[0]。带空格的真实路径（含 \\ 或 /）放行。
fn looks_like_unsplit_shell_line(program: &str) -> bool {
    let program = program.trim();
    if program.contains('|')
        || program.contains("&&")
        || program.contains("||")
        || program.contains(';')
    {
        return true;
    }
    let has_space = program.contains(' ');
    let looks_like_path = program.contains('\\')
        || program.contains('/')
        || program.ends_with(".exe")
        || program.ends_with(".cmd")
        || program.ends_with(".bat");
    has_space && !looks_like_path
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellKind {
    Cmd,
    PowerShell,
}

impl ShellKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::PowerShell => "powershell",
        }
    }
}

/// 决定实际解释器。显式 shell 最可靠；auto 仍识别旧提示词产生的
/// `powershell ... -Command "..."`，取出脚本后改走 EncodedCommand。
fn resolve_shell(
    command: &str,
    requested: Option<&str>,
) -> Result<(ShellKind, String, &'static str)> {
    let requested = requested.unwrap_or("auto").trim().to_ascii_lowercase();
    match requested.as_str() {
        "cmd" => Ok((ShellKind::Cmd, command.to_string(), "explicit")),
        "powershell" | "pwsh" => Ok((
            ShellKind::PowerShell,
            unwrap_powershell_command(command)
                .unwrap_or(command)
                .to_string(),
            "explicit",
        )),
        "auto" | "" => match unwrap_powershell_command(command) {
            Some(script) => Ok((ShellKind::PowerShell, script.to_string(), "legacy_wrapper")),
            None if looks_like_powershell(command) => {
                Ok((ShellKind::PowerShell, command.to_string(), "inferred"))
            }
            None => Ok((ShellKind::Cmd, command.to_string(), "default")),
        },
        other => bail!("shell 必须是 auto / cmd / powershell，收到: {}", other),
    }
}

/// 只对高置信度 PowerShell 语法自动切换解释器；普通 `$` 文本、正则和外部
/// CLI 参数不会仅凭单个符号触发。存在歧义时仍由 AI 显式指定 shell。
fn looks_like_powershell(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    let starts_with_variable_assignment = command
        .trim_start()
        .strip_prefix('$')
        .and_then(|tail| tail.find('=').map(|pos| tail[..pos].trim()))
        .is_some_and(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '?'))
        });
    starts_with_variable_assignment
        || lower.contains("$_")
        || lower.contains("$psitem")
        || lower.contains("$env:")
        || lower.contains("@(")
        || lower.contains("[system.")
        || lower.contains("[console]::")
        || lower.contains("foreach (")
        || lower.contains("where-object")
        || lower.contains("foreach-object")
        || [
            "get-childitem",
            "get-item",
            "get-content",
            "set-content",
            "test-path",
            "measure-object",
            "select-object",
            "convertto-json",
            "convertfrom-json",
            "write-output",
        ]
        .iter()
        .any(|cmdlet| {
            lower
                .split(['|', ';', '\n', '\r'])
                .map(str::trim_start)
                .any(|segment| {
                    segment.strip_prefix(cmdlet).is_some_and(|tail| {
                        tail.is_empty() || tail.starts_with(char::is_whitespace)
                    })
                })
        })
}

/// 兼容旧调用形态：powershell[-.exe] [flags] -Command <script>。
/// 这里只识别明确包装器，不猜测普通命令是不是 PowerShell 语法。
fn unwrap_powershell_command(command: &str) -> Option<&str> {
    let trimmed = command.trim();
    let lower = trimmed.to_ascii_lowercase();
    let rest = ["powershell.exe", "powershell", "pwsh.exe", "pwsh"]
        .into_iter()
        .find_map(|prefix| {
            lower.strip_prefix(prefix).and_then(|_| {
                trimmed
                    .get(prefix.len()..)
                    .filter(|tail| tail.starts_with(char::is_whitespace))
            })
        })?
        .trim_start();
    let lower_rest = rest.to_ascii_lowercase();
    let marker = "-command";
    let start = lower_rest.find(marker)?;
    if start > 0
        && !lower_rest[..start]
            .chars()
            .last()
            .is_some_and(char::is_whitespace)
    {
        return None;
    }
    let after_marker = start + marker.len();
    if lower_rest
        .get(after_marker..)
        .and_then(|s| s.chars().next())
        .is_some_and(|c| !c.is_whitespace())
    {
        return None;
    }
    let script = rest.get(after_marker..)?.trim();
    if script.len() >= 2 {
        let first = script.as_bytes()[0];
        let last = script.as_bytes()[script.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return script.get(1..script.len() - 1);
        }
    }
    (!script.is_empty()).then_some(script)
}

fn powershell_wrapper(script: &str, strict: bool, script_args: &Value) -> Result<String> {
    let source = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
    let structured_args =
        base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(script_args)?);
    Ok(format!(
        r#"$ProgressPreference = 'SilentlyContinue'
$utf8 = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = $utf8
$OutputEncoding = $utf8
$ErrorActionPreference = 'Stop'
{strict_mode}
$source = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{source}'))
$argsJson = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{structured_args}'))
$DHArgs = ConvertFrom-Json -InputObject $argsJson
$tokens = $null
$parseErrors = $null
[void][System.Management.Automation.Language.Parser]::ParseInput($source, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -gt 0) {{
  foreach ($parseError in $parseErrors) {{
    [Console]::Error.WriteLine(('__DH_PS_PARSE_ERROR__ line={{0}} column={{1}}: {{2}}' -f $parseError.Extent.StartLineNumber, $parseError.Extent.StartColumnNumber, $parseError.Message))
  }}
  exit 2
}}
try {{
  $block = [ScriptBlock]::Create($source)
  $global:LASTEXITCODE = 0
  & $block
  $scriptSucceeded = $?
  $nativeExit = $global:LASTEXITCODE
  if ($nativeExit -ne 0) {{ exit [int]$nativeExit }}
  if (-not $scriptSucceeded) {{
    exit 1
  }}
}} catch {{
  [Console]::Error.WriteLine(('__DH_PS_RUNTIME_ERROR__ {{0}}' -f $_.Exception.Message))
  if ($_.InvocationInfo -and $_.InvocationInfo.PositionMessage) {{ [Console]::Error.WriteLine($_.InvocationInfo.PositionMessage) }}
  exit 1
}}"#,
        strict_mode = if strict {
            "Set-StrictMode -Version 2.0"
        } else {
            "# StrictMode disabled for compatibility"
        },
    ))
}

fn encode_powershell_command(wrapper: &str) -> String {
    let utf16: Vec<u8> = wrapper
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    base64::engine::general_purpose::STANDARD.encode(utf16)
}

fn powershell_file_bootstrap() -> &'static str {
    r#"$ErrorActionPreference = 'Stop'
try {
  $wrapper = [IO.File]::ReadAllText($env:DH_PS_WRAPPER_PATH, [Text.Encoding]::UTF8)
  & ([ScriptBlock]::Create($wrapper))
} catch {
  [Console]::Error.WriteLine(('__DH_PS_BOOTSTRAP_ERROR__ {0}' -f $_.Exception.Message))
  exit 1
}"#
}

fn write_utf8_bom(path: &Path, text: &str) -> Result<()> {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(text.as_bytes());
    std::fs::write(path, bytes)?;
    Ok(())
}

fn cleanup_stale_powershell_wrappers(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("task-") && name.ends_with(".ps1") {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn normalize_success_exit_codes(codes: &[i32]) -> Result<Vec<i32>> {
    if codes.len() > 32 {
        bail!("success_exit_codes 最多 32 个");
    }
    let mut normalized = if codes.is_empty() {
        vec![0]
    } else {
        codes.to_vec()
    };
    normalized.sort_unstable();
    normalized.dedup();
    Ok(normalized)
}

fn emit_changed(app: &AppHandle, info: &TaskInfo) {
    let _ = app.emit("task-changed", info);
}

fn join_head_tail(head: &str, tail: &str) -> String {
    format!(
        "{}\n\n……〔中间输出已省略，完整内容见日志文件〕……\n\n{}",
        head.trim_end(),
        tail.trim_start()
    )
}

/// 从文件末尾回读 max_bytes 并解码成字符串
fn read_tail_bytes(path: &Path, max_bytes: u64) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.take(max_bytes).read_to_end(&mut buf)?;
    Ok(decode_log(&buf))
}

/// 从文件开头读 max_chars 个字符（按字节 4 倍上限读取后取头部字符）
fn read_head(path: &Path, max_chars: usize) -> Result<String> {
    use std::io::Read;
    let f = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    f.take((max_chars * 4 + 64) as u64).read_to_end(&mut buf)?;
    let text = decode_log(&buf);
    Ok(text.chars().take(max_chars).collect())
}

/// 命令日志解码：中文 Windows 的 cmd 默认输出 GBK，直接按 UTF-8 lossy 解会
/// 出现「@」之类的乱码符号（GBK 第二字节落在 ASCII 区）。先按 UTF-8 试；
/// 失败时若去掉前 1~3 字节（从日志中间 seek 可能切在多字节字符中间）即为合法
/// UTF-8，说明是截断所致而非 GBK，走 lossy；否则按 GB18030 解。
fn decode_log(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    match String::from_utf8(bytes.to_vec()) {
        Ok(s) => s,
        Err(_) => {
            let mid_cut = (1..=3usize)
                .any(|n| bytes.len() > n + 8 && std::str::from_utf8(&bytes[n..]).is_ok());
            if mid_cut {
                String::from_utf8_lossy(bytes).into_owned()
            } else {
                encoding_rs::GB18030.decode(bytes).0.into_owned()
            }
        }
    }
}

fn read_tail(path: &Path, max_chars: usize) -> Result<String> {
    // 字符数按字节 4 倍上限回读，再从结果里取尾部字符
    let text = read_tail_bytes(path, (max_chars * 4 + 64) as u64)?;
    let count = text.chars().count();
    if count <= max_chars {
        Ok(text)
    } else {
        Ok(text.chars().skip(count - max_chars).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        decode_log, encode_powershell_command, looks_like_powershell,
        looks_like_unsplit_shell_line, normalize_success_exit_codes, powershell_file_bootstrap,
        powershell_wrapper, resolve_shell, unwrap_powershell_command, utf8_command, validate_argv,
        write_utf8_bom, ShellKind,
    };
    use base64::Engine as _;

    #[test]
    fn command_switches_child_code_page_to_utf8() {
        assert_eq!(utf8_command("echo hello"), "chcp 65001 >nul & echo hello");
    }

    #[test]
    fn argv_rejects_unsplit_command_line_but_allows_paths_with_spaces() {
        let err = validate_argv(&["git status".to_string()]).unwrap_err();
        assert!(err.to_string().contains("拆成"));
        assert!(looks_like_unsplit_shell_line("git status -sb"));
        assert!(looks_like_unsplit_shell_line("tool | more"));
        assert!(!looks_like_unsplit_shell_line(
            r"C:\Program Files\Git\cmd\git.exe"
        ));
        let (program, args) =
            validate_argv(&["git".into(), "status".into(), "-sb".into()]).unwrap();
        assert_eq!(program, "git");
        assert_eq!(args, vec!["status", "-sb"]);
    }

    #[test]
    fn auto_unwraps_legacy_powershell_command_without_losing_variables() {
        let original =
            "powershell -NoProfile -Command \"$dirs=@('甲','乙'); foreach($d in $dirs){$d}\"";
        let (shell, script, selection) = resolve_shell(original, None).unwrap();
        assert_eq!(shell, ShellKind::PowerShell);
        assert_eq!(selection, "legacy_wrapper");
        assert_eq!(script, "$dirs=@('甲','乙'); foreach($d in $dirs){$d}");
        assert_eq!(
            unwrap_powershell_command("echo powershell -Command x"),
            None
        );
    }

    #[test]
    fn explicit_powershell_keeps_raw_multiline_script() {
        let script = "$dirs = @('中文')\nforeach ($d in $dirs) { $d }";
        let (shell, resolved, selection) = resolve_shell(script, Some("powershell")).unwrap();
        assert_eq!(shell, ShellKind::PowerShell);
        assert_eq!(selection, "explicit");
        assert_eq!(resolved, script);
    }

    #[test]
    fn auto_detects_high_confidence_powershell_but_not_plain_dollar_text() {
        assert!(looks_like_powershell(
            "$dirs=@('A'); foreach ($d in $dirs) {$d}"
        ));
        assert!(looks_like_powershell("$value = 1; Write-Output $value"));
        assert!(looks_like_powershell("Get-ChildItem C:/ | Measure-Object"));
        assert!(!looks_like_powershell("echo price=$5"));
        assert!(!looks_like_powershell("npm view package-$tag"));
        assert!(!looks_like_powershell("tool.exe --get-item value"));
        let (_, _, selection) = resolve_shell("Get-ChildItem C:/", None).unwrap();
        assert_eq!(selection, "inferred");
    }

    #[test]
    fn encoded_command_roundtrips_static_wrapper_and_chinese_source() {
        let script = "$dirs=@('中文'); foreach($d in $dirs){ '目录=' + $d }";
        let wrapper = powershell_wrapper(script, true, &serde_json::Value::Null).unwrap();
        let encoded = encode_powershell_command(&wrapper);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let decoded = String::from_utf16(&units).unwrap();
        assert_eq!(decoded, wrapper);
        assert!(decoded.starts_with("$ProgressPreference = 'SilentlyContinue'"));
        assert!(decoded.contains("[Console]::OutputEncoding"));
        assert!(decoded.contains("Parser]::ParseInput"));
        assert!(decoded.contains("Set-StrictMode"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn structured_script_args_preserve_quotes_spaces_and_chinese() {
        use std::os::windows::process::CommandExt;

        let args = serde_json::json!({
            "path": "C:/目录/有 空格/'单引号'/\"双引号\"",
            "items": ["甲", "乙"]
        });
        let wrapper =
            powershell_wrapper("$DHArgs.path; $DHArgs.items -join ','", true, &args).unwrap();
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                &encode_powershell_command(&wrapper),
            ])
            .creation_flags(super::CREATE_NO_WINDOW)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", decode_log(&output.stderr));
        let stdout = decode_log(&output.stdout);
        assert!(stdout.contains("C:/目录/有 空格/'单引号'/\"双引号\""));
        assert!(stdout.contains("甲,乙"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn encoded_bootstrap_runs_long_wrapper_file_and_preserves_unicode() {
        use std::os::windows::process::CommandExt;

        let path = std::env::temp_dir().join(format!(
            "shiguang-powershell-bootstrap-{}.ps1",
            std::process::id()
        ));
        let long_text = "长脚本内容".repeat(5000);
        let script = format!("$value = '{}'; $value.Length", long_text);
        let wrapper = powershell_wrapper(&script, true, &serde_json::Value::Null).unwrap();
        assert!(encode_powershell_command(&wrapper).len() > 24_000);
        write_utf8_bom(&path, &wrapper).unwrap();
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                &encode_powershell_command(powershell_file_bootstrap()),
            ])
            .env("DH_PS_WRAPPER_PATH", &path)
            .creation_flags(super::CREATE_NO_WINDOW)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(path);
        assert!(output.status.success(), "{}", decode_log(&output.stderr));
        assert_eq!(
            decode_log(&output.stdout).trim(),
            long_text.chars().count().to_string()
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn encoded_command_executes_variables_and_chinese_in_windows_powershell() {
        use std::os::windows::process::CommandExt;

        let script = "$dirs=@('甲','乙'); foreach($d in $dirs){ '目录=' + $d }";
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                &encode_powershell_command(
                    &powershell_wrapper(script, true, &serde_json::Value::Null).unwrap(),
                ),
            ])
            .creation_flags(super::CREATE_NO_WINDOW)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "PowerShell failed: {}",
            decode_log(&output.stderr)
        );
        let stdout = decode_log(&output.stdout);
        assert!(
            output.stderr.is_empty(),
            "unexpected PowerShell auxiliary stream: {}",
            decode_log(&output.stderr)
        );
        assert!(stdout.contains("目录=甲"), "unexpected stdout: {}", stdout);
        assert!(stdout.contains("目录=乙"), "unexpected stdout: {}", stdout);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn powershell_preflight_rejects_syntax_before_execution() {
        use std::os::windows::process::CommandExt;

        let script = "Write-Output '不应执行'; foreach ($d in ) { $d }";
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                &encode_powershell_command(
                    &powershell_wrapper(script, true, &serde_json::Value::Null).unwrap(),
                ),
            ])
            .creation_flags(super::CREATE_NO_WINDOW)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!decode_log(&output.stdout).contains("不应执行"));
        assert!(decode_log(&output.stderr).contains("__DH_PS_PARSE_ERROR__"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn powershell_strict_mode_rejects_uninitialized_variables() {
        use std::os::windows::process::CommandExt;

        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                &encode_powershell_command(
                    &powershell_wrapper("Write-Output $missing", true, &serde_json::Value::Null)
                        .unwrap(),
                ),
            ])
            .creation_flags(super::CREATE_NO_WINDOW)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(decode_log(&output.stderr).contains("__DH_PS_RUNTIME_ERROR__"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn powershell_wrapper_propagates_native_failure_exit_code() {
        use std::os::windows::process::CommandExt;

        let wrapper =
            powershell_wrapper("cmd.exe /d /c exit 7", true, &serde_json::Value::Null).unwrap();
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                &encode_powershell_command(&wrapper),
            ])
            .creation_flags(super::CREATE_NO_WINDOW)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(7));
    }

    #[test]
    fn success_exit_codes_are_normalized_and_bounded() {
        assert_eq!(normalize_success_exit_codes(&[]).unwrap(), vec![0]);
        assert_eq!(
            normalize_success_exit_codes(&[7, 0, 7, 1]).unwrap(),
            vec![0, 1, 7]
        );
        assert!(normalize_success_exit_codes(&vec![0; 33]).is_err());
    }

    #[test]
    fn decodes_gbk_cmd_output() {
        // 中文 cmd 的 GBK 输出："目录" 的 GBK 编码
        let gbk = encoding_rs::GB18030
            .encode(" 目录 C:\\Users 中文路径\r\n")
            .0
            .into_owned();
        assert!(std::str::from_utf8(&gbk).is_err());
        let s = decode_log(&gbk);
        assert!(s.contains("目录"), "GBK 解码失败: {}", s);
        assert!(s.contains("中文路径"), "GBK 解码失败: {}", s);
    }

    #[test]
    fn utf8_mid_cut_uses_lossy_not_gbk() {
        // 从多字节字符中间 seek 截断的 UTF-8：不应误判成 GBK
        let full = "输出结果：成功完成编译".as_bytes();
        let cut = &full[2..]; // 切在「输」字中间
        assert!(std::str::from_utf8(cut).is_err());
        let s = decode_log(cut);
        assert!(s.contains("出结果：成功完成编译"), "UTF-8 截断误判: {}", s);
    }

    #[test]
    fn ascii_and_utf8_passthrough() {
        assert_eq!(decode_log(b"hello world\n"), "hello world\n");
        assert_eq!(decode_log("正常中文 UTF-8".as_bytes()), "正常中文 UTF-8");
    }
}
