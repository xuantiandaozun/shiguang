//! Protocol and client for the short-lived elevated NTFS read helper.
//!
//! The helper accepts one read-only request, writes one authenticated response,
//! then exits. It has no file deletion or arbitrary command capability.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::Manager;

pub const PROTOCOL_VERSION: u32 = 2;
const REQUEST_DIR_NAME: &str = "ntfs-helper";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperRequest {
    pub version: u32,
    pub request_id: String,
    pub action: HelperAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HelperAction {
    Probe {
        volume: String,
    },
    CatchUp {
        checkpoint: crate::ntfs_usn::JournalCheckpoint,
    },
    MftSnapshot {
        volume: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperResponse {
    pub version: u32,
    pub request_id: String,
    pub result: Option<HelperResult>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HelperResult {
    Probe {
        result: crate::ntfs_usn::CatchUpResult,
    },
    CatchUp {
        result: ResolvedCatchUpResult,
    },
    MftSnapshot {
        result: MftSnapshotResult,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MftSnapshotResult {
    pub checkpoint: crate::ntfs_usn::JournalCheckpoint,
    pub snapshot_file: String,
    pub record_count: u64,
    pub estimated_size_count: u64,
    pub missing_size_count: u64,
    pub catch_up: ResolvedCatchUpResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResolvedCatchUpResult {
    Changes {
        checkpoint: crate::ntfs_usn::JournalCheckpoint,
        changes: Vec<ResolvedUsnChange>,
        unresolved: usize,
    },
    RebuildRequired {
        volume: String,
        reason: String,
    },
    Unavailable {
        volume: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedUsnChange {
    pub path: String,
    pub reason: u32,
    pub is_directory: bool,
}

pub fn helper_request_dir(app_dir: &Path) -> PathBuf {
    app_dir.join(REQUEST_DIR_NAME)
}

pub fn request_path(dir: &Path, request_id: &str) -> PathBuf {
    dir.join(format!("request-{}.json", request_id))
}

pub fn response_path(dir: &Path, request_id: &str) -> PathBuf {
    dir.join(format!("response-{}.json", request_id))
}

pub fn validate_request_path(path: &Path) -> Result<(PathBuf, String)> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("请求文件名无效"))?;
    let request_id = file_name
        .strip_prefix("request-")
        .and_then(|name| name.strip_suffix(".json"))
        .ok_or_else(|| anyhow!("请求文件名不符合协议"))?;
    uuid::Uuid::parse_str(request_id).context("请求 ID 不是有效 UUID")?;
    let parent = path.parent().ok_or_else(|| anyhow!("请求路径没有父目录"))?;
    if parent.file_name().and_then(|name| name.to_str()) != Some(REQUEST_DIR_NAME) {
        return Err(anyhow!("请求只能位于 {} 目录", REQUEST_DIR_NAME));
    }
    let app_dir = parent
        .parent()
        .ok_or_else(|| anyhow!("请求目录不在应用数据目录内"))?;
    if app_dir.file_name().and_then(|name| name.to_str()) != Some("com.deskhelper.win") {
        return Err(anyhow!("请求目录不属于拾光应用数据"));
    }
    Ok((parent.to_path_buf(), request_id.to_string()))
}

pub fn execute_request(request: HelperRequest) -> HelperResponse {
    execute_request_in_dir(request, None)
}

pub fn execute_request_in_dir(
    request: HelperRequest,
    request_dir: Option<&Path>,
) -> HelperResponse {
    if request.version != PROTOCOL_VERSION {
        return HelperResponse {
            version: PROTOCOL_VERSION,
            request_id: request.request_id,
            result: None,
            error: Some("helper 协议版本不兼容".into()),
        };
    }
    let result = match request.action {
        HelperAction::Probe { volume } => {
            if !is_canonical_volume(&volume) {
                return protocol_error(
                    request.request_id,
                    "卷路径必须是规范盘符根路径，格式为“盘符:/”",
                );
            }
            HelperResult::Probe {
                result: crate::ntfs_usn::checkpoint(Path::new(&volume)),
            }
        }
        HelperAction::CatchUp { checkpoint } => {
            if !is_canonical_volume(&checkpoint.volume) {
                return protocol_error(
                    request.request_id,
                    "检查点卷路径必须是规范盘符根路径，格式为“盘符:/”",
                );
            }
            let result = match crate::ntfs_usn::read_since(&checkpoint) {
                crate::ntfs_usn::CatchUpResult::Changes {
                    checkpoint,
                    changes,
                } => {
                    let volume = checkpoint.volume.clone();
                    let (resolved, unresolved) =
                        crate::ntfs_usn::resolve_change_paths(&volume, changes);
                    ResolvedCatchUpResult::Changes {
                        checkpoint,
                        changes: resolved
                            .into_iter()
                            .map(|(change, path)| ResolvedUsnChange {
                                path: path.to_string_lossy().replace('\\', "/"),
                                reason: change.reason,
                                is_directory: change.is_directory(),
                            })
                            .collect(),
                        unresolved,
                    }
                }
                crate::ntfs_usn::CatchUpResult::RebuildRequired { volume, reason } => {
                    ResolvedCatchUpResult::RebuildRequired { volume, reason }
                }
                crate::ntfs_usn::CatchUpResult::Unavailable { volume, reason } => {
                    ResolvedCatchUpResult::Unavailable { volume, reason }
                }
            };
            HelperResult::CatchUp { result }
        }
        HelperAction::MftSnapshot { volume } => {
            if !is_canonical_volume(&volume) {
                return protocol_error(
                    request.request_id,
                    "卷路径必须是规范盘符根路径，格式为“盘符:/”",
                );
            }
            let Some(dir) = request_dir else {
                return protocol_error(request.request_id, "MFT 快照缺少受控输出目录");
            };
            let snapshot_file = format!("snapshot-{}.db", request.request_id);
            let snapshot_path = dir.join(&snapshot_file);
            match write_mft_snapshot(&volume, &snapshot_path) {
                Ok((checkpoint, record_count, estimated_size_count, missing_size_count)) => {
                    let catch_up = resolve_catch_up(&checkpoint);
                    HelperResult::MftSnapshot {
                        result: MftSnapshotResult {
                            checkpoint,
                            snapshot_file,
                            record_count,
                            estimated_size_count,
                            missing_size_count,
                            catch_up,
                        },
                    }
                }
                Err(error) => {
                    return protocol_error(request.request_id, &format!("MFT 快照失败: {error:#}"));
                }
            }
        }
    };
    HelperResponse {
        version: PROTOCOL_VERSION,
        request_id: request.request_id,
        result: Some(result),
        error: None,
    }
}

fn resolve_catch_up(checkpoint: &crate::ntfs_usn::JournalCheckpoint) -> ResolvedCatchUpResult {
    match crate::ntfs_usn::read_since(checkpoint) {
        crate::ntfs_usn::CatchUpResult::Changes {
            checkpoint,
            changes,
        } => {
            let volume = checkpoint.volume.clone();
            let (resolved, unresolved) = crate::ntfs_usn::resolve_change_paths(&volume, changes);
            ResolvedCatchUpResult::Changes {
                checkpoint,
                changes: resolved
                    .into_iter()
                    .map(|(change, path)| ResolvedUsnChange {
                        path: path.to_string_lossy().replace('\\', "/"),
                        reason: change.reason,
                        is_directory: change.is_directory(),
                    })
                    .collect(),
                unresolved,
            }
        }
        crate::ntfs_usn::CatchUpResult::RebuildRequired { volume, reason } => {
            ResolvedCatchUpResult::RebuildRequired { volume, reason }
        }
        crate::ntfs_usn::CatchUpResult::Unavailable { volume, reason } => {
            ResolvedCatchUpResult::Unavailable { volume, reason }
        }
    }
}

fn write_mft_snapshot(
    volume: &str,
    snapshot_path: &Path,
) -> Result<(crate::ntfs_usn::JournalCheckpoint, u64, u64, u64)> {
    if snapshot_path.exists() {
        return Err(anyhow!("MFT 快照文件已存在"));
    }
    let mut conn = rusqlite::Connection::open(snapshot_path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;
         CREATE TABLE records(
           file_reference INTEGER PRIMARY KEY,
           parent_reference INTEGER NOT NULL,
           name TEXT NOT NULL,
           extension TEXT NOT NULL,
           file_attributes INTEGER NOT NULL,
           estimated_size_bytes INTEGER,
           size_known INTEGER NOT NULL
         );",
    )?;
    let tx = conn.transaction()?;
    let mut insert = tx.prepare_cached(
        "INSERT OR REPLACE INTO records(
           file_reference,parent_reference,name,extension,file_attributes,
           estimated_size_bytes,size_known
         ) VALUES(?1,?2,?3,?4,?5,?6,?7)",
    )?;
    let mut record_count = 0u64;
    let mut estimated_size_count = 0u64;
    let mut missing_size_count = 0u64;
    let checkpoint = crate::ntfs_usn::enumerate_mft(volume, |record| {
        let extension = Path::new(&record.name)
            .extension()
            .map(|value| value.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let is_directory = record.file_attributes & 0x10 != 0;
        if !is_directory {
            if record.estimated_size_bytes.is_some() {
                estimated_size_count += 1;
            } else {
                missing_size_count += 1;
            }
        }
        insert.execute(rusqlite::params![
            record.file_reference as i64,
            record.parent_reference as i64,
            record.name,
            extension,
            record.file_attributes as i64,
            record.estimated_size_bytes.map(|size| size as i64),
            record.estimated_size_bytes.is_some() as i64,
        ])?;
        record_count += 1;
        Ok(())
    })?;
    drop(insert);
    tx.commit()?;
    Ok((
        checkpoint,
        record_count,
        estimated_size_count,
        missing_size_count,
    ))
}

fn is_canonical_volume(volume: &str) -> bool {
    crate::ntfs_usn::volume_for_path(Path::new(volume)).as_deref() == Some(volume)
        && volume.len() == 3
}

fn protocol_error(request_id: String, message: &str) -> HelperResponse {
    HelperResponse {
        version: PROTOCOL_VERSION,
        request_id,
        result: None,
        error: Some(message.into()),
    }
}

#[cfg(windows)]
pub async fn probe_elevated(
    app: &tauri::AppHandle,
    volume: &str,
) -> Result<crate::ntfs_usn::CatchUpResult> {
    match run_elevated(
        app,
        HelperAction::Probe {
            volume: volume.to_string(),
        },
    )
    .await?
    .result
    {
        HelperResult::Probe { result } => Ok(result),
        _ => Err(anyhow!("NTFS helper 返回了错误的动作类型")),
    }
}

#[cfg(windows)]
pub async fn catch_up_elevated(
    app: &tauri::AppHandle,
    checkpoint: crate::ntfs_usn::JournalCheckpoint,
) -> Result<ResolvedCatchUpResult> {
    match run_elevated(app, HelperAction::CatchUp { checkpoint })
        .await?
        .result
    {
        HelperResult::CatchUp { result } => Ok(result),
        _ => Err(anyhow!("NTFS helper 返回了错误的动作类型")),
    }
}

#[cfg(windows)]
pub async fn mft_snapshot_elevated(
    app: &tauri::AppHandle,
    volume: &str,
) -> Result<(MftSnapshotResult, PathBuf)> {
    let elevated = run_elevated(
        app,
        HelperAction::MftSnapshot {
            volume: volume.to_string(),
        },
    )
    .await?;
    match elevated.result {
        HelperResult::MftSnapshot { result } => {
            let expected = format!("snapshot-{}.db", elevated.request_id);
            if result.snapshot_file != expected {
                return Err(anyhow!("NTFS helper 返回了无效的快照文件名"));
            }
            let path = elevated.dir.join(&result.snapshot_file);
            if !path.is_file() {
                return Err(anyhow!("NTFS helper 未生成 MFT 快照文件"));
            }
            Ok((result, path))
        }
        _ => Err(anyhow!("NTFS helper 返回了错误的动作类型")),
    }
}

#[cfg(windows)]
struct ElevatedResult {
    result: HelperResult,
    dir: PathBuf,
    request_id: String,
}

#[cfg(windows)]
async fn run_elevated(app: &tauri::AppHandle, action: HelperAction) -> Result<ElevatedResult> {
    let app_dir = app.path().app_data_dir().context("无法定位应用数据目录")?;
    let dir = helper_request_dir(&app_dir);
    std::fs::create_dir_all(&dir)?;
    let request_id = uuid::Uuid::new_v4().to_string();
    let request_file = request_path(&dir, &request_id);
    let response_file = response_path(&dir, &request_id);
    let snapshot_file = dir.join(format!("snapshot-{}.db", request_id));
    let request = HelperRequest {
        version: PROTOCOL_VERSION,
        request_id: request_id.clone(),
        action,
    };
    std::fs::write(&request_file, serde_json::to_vec(&request)?)?;
    let launch_result =
        locate_helper_exe().and_then(|helper| launch_elevated(&helper, &request_file));
    if let Err(error) = launch_result {
        cleanup_request_files(&request_file, &response_file);
        let _ = std::fs::remove_file(&snapshot_file);
        return Err(error);
    }

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    let response = loop {
        if response_file.exists() {
            let bytes = std::fs::read(&response_file)?;
            break serde_json::from_slice::<HelperResponse>(&bytes)
                .context("解析提权 helper 响应失败")?;
        }
        if tokio::time::Instant::now() >= deadline {
            cleanup_request_files(&request_file, &response_file);
            let _ = std::fs::remove_file(&snapshot_file);
            return Err(anyhow!("等待 NTFS 提权助手超时；可能取消了 UAC 授权"));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    };
    cleanup_request_files(&request_file, &response_file);
    if response.version != PROTOCOL_VERSION || response.request_id != request_id {
        let _ = std::fs::remove_file(&snapshot_file);
        return Err(anyhow!("NTFS helper 响应身份校验失败"));
    }
    if let Some(error) = response.error {
        let _ = std::fs::remove_file(&snapshot_file);
        return Err(anyhow!(error));
    }
    let result = response
        .result
        .ok_or_else(|| anyhow!("NTFS helper 没有返回结果"))?;
    Ok(ElevatedResult {
        result,
        dir,
        request_id,
    })
}

#[cfg(not(windows))]
pub async fn probe_elevated(
    _app: &tauri::AppHandle,
    _volume: &str,
) -> Result<crate::ntfs_usn::CatchUpResult> {
    Err(anyhow!("提权 NTFS helper 仅支持 Windows"))
}

#[cfg(not(windows))]
pub async fn catch_up_elevated(
    _app: &tauri::AppHandle,
    _checkpoint: crate::ntfs_usn::JournalCheckpoint,
) -> Result<ResolvedCatchUpResult> {
    Err(anyhow!("提权 NTFS helper 仅支持 Windows"))
}

#[cfg(not(windows))]
pub async fn mft_snapshot_elevated(
    _app: &tauri::AppHandle,
    _volume: &str,
) -> Result<(MftSnapshotResult, PathBuf)> {
    Err(anyhow!("提权 NTFS helper 仅支持 Windows"))
}

fn cleanup_request_files(request: &Path, response: &Path) {
    let _ = std::fs::remove_file(request);
    let _ = std::fs::remove_file(response);
}

#[cfg(windows)]
fn locate_helper_exe() -> Result<PathBuf> {
    let current = std::env::current_exe().context("无法定位拾光程序路径")?;
    let sibling = current
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("shiguang-index-helper.exe");
    if sibling.is_file() {
        Ok(sibling)
    } else {
        Err(anyhow!(
            "未找到 NTFS 索引助手: {}。开发环境请先运行 npm run sidecar:dev",
            sibling.display()
        ))
    }
}

#[cfg(windows)]
fn launch_elevated(helper: &Path, request: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let operation: Vec<u16> = "runas".encode_utf16().chain(Some(0)).collect();
    let file: Vec<u16> = helper.as_os_str().encode_wide().chain(Some(0)).collect();
    let parameters = format!("--request \"{}\"", request.display());
    let parameters: Vec<u16> = parameters.encode_utf16().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            std::ptr::null(),
            SW_HIDE,
        )
    };
    if result as isize <= 32 {
        return Err(anyhow!(
            "无法启动 NTFS 提权助手（ShellExecute 状态 {}），可能取消了 UAC",
            result as isize
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_path_must_be_scoped_and_uuid_named() {
        let id = uuid::Uuid::new_v4().to_string();
        let valid = PathBuf::from(format!(
            r"C:\Users\me\AppData\Roaming\com.deskhelper.win\ntfs-helper\request-{}.json",
            id
        ));
        assert_eq!(validate_request_path(&valid).unwrap().1, id);
        assert!(validate_request_path(Path::new(r"C:\temp\request-bad.json")).is_err());
    }

    #[test]
    fn protocol_rejects_unknown_version() {
        let response = execute_request(HelperRequest {
            version: 999,
            request_id: "x".into(),
            action: HelperAction::Probe {
                volume: "C:/".into(),
            },
        });
        assert!(response.result.is_none());
        assert!(response.error.is_some());
    }

    #[test]
    fn protocol_rejects_noncanonical_volume() {
        let response = execute_request(HelperRequest {
            version: PROTOCOL_VERSION,
            request_id: "x".into(),
            action: HelperAction::CatchUp {
                checkpoint: crate::ntfs_usn::JournalCheckpoint {
                    volume: r"C:\Windows".into(),
                    journal_id: 1,
                    next_usn: 2,
                },
            },
        });
        assert!(response.result.is_none());
        assert!(response.error.is_some());
    }
}
