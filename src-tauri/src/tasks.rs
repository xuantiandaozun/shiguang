//! 后台命令任务：命令在后台执行，stdout/stderr 直接写入日志文件，
//! 对话上下文里只放状态与按需截取的输出片段（尾部 / 关键字过滤），
//! 避免大量输出挤占模型上下文窗口。

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
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

#[derive(Debug, Clone, Serialize)]
pub struct TaskInfo {
    pub id: String,
    pub label: String,
    pub command: String,
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
        Self {
            dir,
            seq: AtomicU64::new(0),
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 启动后台命令（Windows cmd /c），输出实时写入日志文件后立即返回。
    /// 监控协程等待进程结束后更新状态并广播 task-changed。
    pub fn start_command(
        &self,
        app: &AppHandle,
        command: &str,
        label: Option<&str>,
        workdir: Option<&str>,
    ) -> Result<TaskInfo> {
        let command = command.trim();
        if command.is_empty() {
            bail!("命令不能为空");
        }
        let id = format!("t{}", self.seq.fetch_add(1, Ordering::SeqCst) + 1);
        let log_path = self.dir.join(format!("task-{}.log", id));
        let log_file = std::fs::File::create(&log_path)?;
        let stderr_file = log_file.try_clone()?;

        let dir = workdir
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| dirs::desktop_dir().unwrap_or_else(|| PathBuf::from(".")));

        let mut cmd = tokio::process::Command::new("cmd");
        cmd.args(["/c", command])
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(stderr_file))
            .creation_flags(CREATE_NO_WINDOW);
        let mut child = cmd.spawn().map_err(|e| anyhow!("启动命令失败: {}", e))?;
        let pid = child.id().unwrap_or(0);

        let info = TaskInfo {
            id: id.clone(),
            label: label
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(command)
                .chars()
                .take(60)
                .collect(),
            command: command.to_string(),
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
            .insert(id.clone(), TaskEntry { info: info.clone() });
        emit_changed(app, &info);

        // 监控协程：独占 child，等进程退出后回填状态。
        // 输出走日志文件而非管道，用 wait() 即可。
        let app2 = app.clone();
        let tasks2 = Arc::clone(&self.tasks);
        let id2 = id.clone();
        tauri::async_runtime::spawn(async move {
            let result = child.wait().await;
            let (status, code) = match result {
                Ok(s) => {
                    let code = s.code();
                    (if s.success() { "done" } else { "failed" }, code)
                }
                Err(_) => ("failed", None),
            };
            let finished = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let snapshot = {
                let Ok(mut guard) = tasks2.lock() else { return };
                let Some(entry) = guard.get_mut(&id2) else { return };
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
            let entry = guard.get_mut(id).ok_or_else(|| anyhow!("任务不存在: {}", id))?;
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
        command: &str,
        workdir: Option<&str>,
        timeout_secs: u64,
    ) -> Result<TaskInfo> {
        let info = self.start_command(app, command, None, workdir)?;
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
            let mid_cut = (1..=3usize).any(|n| {
                bytes.len() > n + 8 && std::str::from_utf8(&bytes[n..]).is_ok()
            });
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
    use super::decode_log;

    #[test]
    fn decodes_gbk_cmd_output() {
        // 中文 cmd 的 GBK 输出："目录" 的 GBK 编码
        let gbk = encoding_rs::GB18030.encode(" 目录 C:\\Users 中文路径\r\n").0.into_owned();
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
