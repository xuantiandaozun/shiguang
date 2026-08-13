//! Lightweight persistent file-name index used by the AI file search tool.
//!
//! This deliberately indexes metadata only. File contents are a separate, much
//! more expensive concern. Re-indexing is transactional, so searches keep using
//! the previous complete snapshot until the replacement is ready.

use anyhow::{anyhow, Context, Result};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection};
use serde::Serialize;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    pub running: bool,
    pub roots: Vec<String>,
    pub scanned: u64,
    pub indexed: u64,
    pub skipped: u64,
    pub current_path: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error: Option<String>,
    pub watching: bool,
    pub watched_roots: Vec<String>,
    pub incremental_updates: u64,
    pub last_change_at: Option<String>,
    pub watch_error: Option<String>,
    pub usn_status: String,
    pub usn_changes_replayed: u64,
    pub usn_last_replay_at: Option<String>,
    pub usn_error: Option<String>,
}

impl Default for IndexStatus {
    fn default() -> Self {
        Self {
            running: false,
            roots: vec![],
            scanned: 0,
            indexed: 0,
            skipped: 0,
            current_path: String::new(),
            started_at: String::new(),
            finished_at: None,
            error: None,
            watching: false,
            watched_roots: vec![],
            incremental_updates: 0,
            last_change_at: None,
            watch_error: None,
            usn_status: "not_checked".into(),
            usn_changes_replayed: 0,
            usn_last_replay_at: None,
            usn_error: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct SearchQuery {
    pub roots: Vec<String>,
    pub text: Option<String>,
    pub extensions: Vec<String>,
    pub kind: Option<String>,
    pub min_size_bytes: Option<u64>,
    pub max_size_bytes: Option<u64>,
    pub modified_after: Option<i64>,
    pub modified_before: Option<i64>,
    pub sort: String,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexedFile {
    pub path: String,
    pub name: String,
    pub extension: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub modified_unix: i64,
}

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub items: Vec<IndexedFile>,
    pub returned: usize,
    pub truncated: bool,
    pub indexed_roots: Vec<IndexedRoot>,
    pub index_status: IndexStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexedRoot {
    pub path: String,
    pub indexed_at: String,
    pub item_count: u64,
    /// full = exact size/time metadata is available; estimated = fast MFT name
    /// graph plus best-effort logical sizes; names_only = legacy fast MFT graph;
    /// unknown = index created by an older version and should be rebuilt before
    /// metadata-sensitive analysis.
    pub metadata_level: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexCoverage {
    pub ready: bool,
    pub requested_roots: Vec<String>,
    pub covered_roots: Vec<String>,
    pub missing_roots: Vec<String>,
    pub insufficient_metadata_roots: Vec<String>,
    pub indexed_roots: Vec<IndexedRoot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectoryUsage {
    pub path: String,
    pub name: String,
    pub size_bytes: u64,
    pub file_count: u64,
    pub directory_count: u64,
    pub missing_size_count: u64,
}

#[derive(Debug, Serialize)]
pub struct UsageSummary {
    pub root: String,
    /// `exact` comes from a normal metadata scan; `estimated` comes from raw
    /// MFT `$DATA` lengths and may omit special records or alternate streams.
    pub accuracy: String,
    pub total_size_bytes: u64,
    pub file_count: u64,
    pub sized_file_count: u64,
    pub missing_size_count: u64,
    pub size_coverage_percent: f64,
    pub directory_count: u64,
    pub items: Vec<DirectoryUsage>,
    pub returned: usize,
    pub truncated: bool,
    pub indexed_roots: Vec<IndexedRoot>,
    pub index_status: IndexStatus,
}

pub struct FileIndex {
    db_path: PathBuf,
    status: Arc<Mutex<IndexStatus>>,
    cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    watch_tx: mpsc::Sender<WatchMessage>,
    usn_recovering: Arc<AtomicBool>,
}

enum WatchMessage {
    Reconfigure(Vec<PathBuf>),
    Event(notify::Result<Event>),
    Shutdown,
}

impl Drop for FileIndex {
    fn drop(&mut self) {
        let _ = self.watch_tx.send(WatchMessage::Shutdown);
    }
}

impl FileIndex {
    pub fn new(app_dir: &Path) -> Result<Self> {
        let db_path = app_dir.join("file_index.db");
        init_db(&db_path)?;
        let status = Arc::new(Mutex::new(IndexStatus::default()));
        let watch_tx = spawn_index_watcher(db_path.clone(), status.clone());
        let existing_roots = load_indexed_root_paths(&db_path).unwrap_or_default();
        let _ = watch_tx.send(WatchMessage::Reconfigure(existing_roots));
        Ok(Self {
            db_path,
            status,
            cancel: Arc::new(Mutex::new(None)),
            watch_tx,
            usn_recovering: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn status(&self) -> IndexStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn stop(&self) -> bool {
        let guard = self.cancel.lock().unwrap();
        if let Some(cancel) = guard.as_ref() {
            cancel.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub fn start(&self, roots: Vec<PathBuf>) -> Result<IndexStatus> {
        if self.status().running || self.usn_recovering.load(Ordering::Relaxed) {
            return Err(anyhow!("已有文件索引任务正在运行"));
        }
        let mut valid = Vec::new();
        for root in roots {
            let canonical = root
                .canonicalize()
                .with_context(|| format!("索引路径不存在或不可访问: {}", root.display()))?;
            if !canonical.is_dir() {
                return Err(anyhow!("索引路径不是目录: {}", canonical.display()));
            }
            if !valid.contains(&canonical) {
                valid.push(canonical);
            }
        }
        if valid.is_empty() {
            return Err(anyhow!("至少需要一个索引目录"));
        }
        valid = minimize_roots(valid);
        let journal_starts = capture_journal_checkpoints(&valid);

        let cancel = Arc::new(AtomicBool::new(false));
        *self.cancel.lock().unwrap() = Some(cancel.clone());
        let root_names: Vec<String> = valid.iter().map(|p| display_path(p)).collect();
        let initial = IndexStatus {
            running: true,
            roots: root_names,
            started_at: crate::db::now_str(),
            ..IndexStatus::default()
        };
        *self.status.lock().unwrap() = initial.clone();
        // Avoid incremental writes competing with the full replacement
        // transaction. The previous completed index remains queryable.
        let _ = self.watch_tx.send(WatchMessage::Reconfigure(vec![]));

        let db_path = self.db_path.clone();
        let status = self.status.clone();
        let cancel_slot = self.cancel.clone();
        let watch_tx = self.watch_tx.clone();
        std::thread::Builder::new()
            .name("file-indexer".into())
            .spawn(move || {
                let result = build_index(&db_path, &valid, &status, &cancel);
                let usn_result = if result.is_ok() {
                    replay_and_store_checkpoints(&db_path, journal_starts)
                } else {
                    Ok(ReplaySummary::default())
                };
                {
                    let mut current = status.lock().unwrap();
                    current.running = false;
                    current.finished_at = Some(crate::db::now_str());
                    current.current_path.clear();
                    if let Err(e) = result {
                        current.error = Some(e.to_string());
                    }
                    apply_replay_status(&mut current, usn_result);
                }
                *cancel_slot.lock().unwrap() = None;
                // Both commit and rollback leave a complete set of roots in the
                // database, so restore watching from that authoritative state.
                let roots = load_indexed_root_paths(&db_path).unwrap_or_default();
                let _ = watch_tx.send(WatchMessage::Reconfigure(roots));
            })
            .context("启动文件索引线程失败")?;
        Ok(initial)
    }

    /// Replay changes that happened while the application was not running, then
    /// restore live directory watching. This is intentionally background work:
    /// the previous complete index remains searchable throughout recovery.
    pub fn recover_usn_async(&self) {
        if self
            .usn_recovering
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let roots = load_indexed_root_paths(&self.db_path).unwrap_or_default();
        if roots.is_empty() {
            self.usn_recovering.store(false, Ordering::Relaxed);
            let _ = self.watch_tx.send(WatchMessage::Reconfigure(vec![]));
            return;
        }
        let _ = self.watch_tx.send(WatchMessage::Reconfigure(vec![]));
        {
            let mut status = self.status.lock().unwrap();
            status.usn_status = "checking".into();
            status.usn_error = None;
        }
        let db_path = self.db_path.clone();
        let status = self.status.clone();
        let recovering = self.usn_recovering.clone();
        let watch_tx = self.watch_tx.clone();
        std::thread::Builder::new()
            .name("file-usn-recovery".into())
            .spawn(move || {
                let checkpoints = load_journal_checkpoints(&db_path).unwrap_or_default();
                let result = if checkpoints.is_empty() {
                    Err(anyhow!("当前索引还没有 USN 检查点；下次完整索引后启用"))
                } else {
                    replay_and_store_checkpoints(&db_path, checkpoints)
                };
                apply_replay_status(&mut status.lock().unwrap(), result);
                recovering.store(false, Ordering::Relaxed);
                let roots = load_indexed_root_paths(&db_path).unwrap_or_default();
                let _ = watch_tx.send(WatchMessage::Reconfigure(roots));
            })
            .expect("failed to spawn USN recovery thread");
    }

    /// Explicit UAC-assisted synchronization. This is never called at startup;
    /// the user must approve the elevation prompt for each manual sync.
    pub async fn sync_usn_elevated(
        &self,
        app: &tauri::AppHandle,
        volume: &str,
    ) -> Result<IndexStatus> {
        if self
            .usn_recovering
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(anyhow!("USN 同步或文件索引任务正在运行"));
        }
        let result = self.sync_usn_elevated_inner(app, volume).await;
        self.usn_recovering.store(false, Ordering::Relaxed);
        match result {
            Ok(summary) => {
                apply_replay_status(&mut self.status.lock().unwrap(), Ok(summary));
                Ok(self.status())
            }
            Err(error) => {
                let message = error.to_string();
                apply_replay_status(&mut self.status.lock().unwrap(), Err(error));
                Err(anyhow!(message))
            }
        }
    }

    async fn sync_usn_elevated_inner(
        &self,
        app: &tauri::AppHandle,
        volume: &str,
    ) -> Result<ReplaySummary> {
        let indexed = load_indexed_root_paths(&self.db_path)?;
        if !indexed
            .iter()
            .any(|root| crate::ntfs_usn::volume_for_path(root).as_deref() == Some(volume))
        {
            return Err(anyhow!("{} 还没有文件索引，请先执行 index", volume));
        }
        let checkpoint = load_journal_checkpoints(&self.db_path)?
            .into_iter()
            .find(|checkpoint| checkpoint.volume == volume);
        let Some(checkpoint) = checkpoint else {
            return match crate::ntfs_helper::probe_elevated(app, volume).await? {
                crate::ntfs_usn::CatchUpResult::Changes { checkpoint, .. } => {
                    save_journal_checkpoint(&self.db_path, &checkpoint)?;
                    Ok(ReplaySummary {
                        status: "active".into(),
                        errors: vec!["已建立 USN 基线；后续同步将只读取本次之后的变化".into()],
                        ..ReplaySummary::default()
                    })
                }
                crate::ntfs_usn::CatchUpResult::RebuildRequired { reason, .. }
                | crate::ntfs_usn::CatchUpResult::Unavailable { reason, .. } => {
                    Err(anyhow!(reason))
                }
            };
        };

        match crate::ntfs_helper::catch_up_elevated(app, checkpoint).await? {
            crate::ntfs_helper::ResolvedCatchUpResult::Changes {
                checkpoint,
                changes,
                unresolved,
            } => {
                let applied =
                    apply_resolved_usn_changes(&self.db_path, &checkpoint.volume, changes)?;
                if unresolved > 0 {
                    Ok(ReplaySummary {
                        status: "rebuild_required".into(),
                        changes: applied,
                        errors: vec![format!(
                            "有 {} 条 USN 记录无法还原路径，未推进检查点",
                            unresolved
                        )],
                    })
                } else {
                    save_journal_checkpoint(&self.db_path, &checkpoint)?;
                    Ok(ReplaySummary {
                        status: "active".into(),
                        changes: applied,
                        ..ReplaySummary::default()
                    })
                }
            }
            crate::ntfs_helper::ResolvedCatchUpResult::RebuildRequired { reason, .. } => {
                Ok(ReplaySummary {
                    status: "rebuild_required".into(),
                    errors: vec![reason],
                    ..ReplaySummary::default()
                })
            }
            crate::ntfs_helper::ResolvedCatchUpResult::Unavailable { reason, .. } => {
                Err(anyhow!(reason))
            }
        }
    }

    /// Build a whole-volume name index from the NTFS MFT, then replay changes
    /// that occurred during enumeration. The elevated helper writes only a
    /// short-lived SQLite snapshot; this process performs the final import.
    pub async fn rebuild_ntfs_elevated(
        &self,
        app: &tauri::AppHandle,
        volume: &str,
    ) -> Result<IndexStatus> {
        if self.status().running
            || self
                .usn_recovering
                .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(anyhow!("已有文件索引或 USN 同步任务正在运行"));
        }
        let _ = self.watch_tx.send(WatchMessage::Reconfigure(vec![]));
        {
            let mut status = self.status.lock().unwrap();
            status.running = true;
            status.roots = vec![volume.to_string()];
            status.scanned = 0;
            status.indexed = 0;
            status.skipped = 0;
            status.current_path = format!("正在枚举 {} 的 NTFS MFT", volume);
            status.started_at = crate::db::now_str();
            status.finished_at = None;
            status.error = None;
            status.usn_status = "mft_building".into();
            status.usn_error = None;
        }

        let result = self.rebuild_ntfs_elevated_inner(app, volume).await;
        self.usn_recovering.store(false, Ordering::Relaxed);
        let roots = load_indexed_root_paths(&self.db_path).unwrap_or_default();
        let _ = self.watch_tx.send(WatchMessage::Reconfigure(roots));
        let mut status = self.status.lock().unwrap();
        status.running = false;
        status.finished_at = Some(crate::db::now_str());
        match result {
            Ok((summary, records, missing_sizes)) => {
                status.scanned = records;
                status.indexed = records;
                status.skipped = missing_sizes;
                status.current_path.clear();
                apply_replay_status(&mut status, Ok(summary));
                Ok(status.clone())
            }
            Err(error) => {
                let message = error.to_string();
                status.error = Some(message.clone());
                apply_replay_status(&mut status, Err(error));
                Err(anyhow!(message))
            }
        }
    }

    async fn rebuild_ntfs_elevated_inner(
        &self,
        app: &tauri::AppHandle,
        volume: &str,
    ) -> Result<(ReplaySummary, u64, u64)> {
        let (snapshot, snapshot_path) =
            crate::ntfs_helper::mft_snapshot_elevated(app, volume).await?;
        let import_result =
            import_mft_snapshot(&self.db_path, &snapshot_path, volume, &snapshot.checkpoint);
        let _ = std::fs::remove_file(&snapshot_path);
        import_result?;

        let summary = match snapshot.catch_up {
            crate::ntfs_helper::ResolvedCatchUpResult::Changes {
                checkpoint,
                changes,
                unresolved,
            } => {
                let applied = apply_resolved_usn_changes(&self.db_path, volume, changes)?;
                if unresolved == 0 {
                    save_journal_checkpoint(&self.db_path, &checkpoint)?;
                    ReplaySummary {
                        status: "active".into(),
                        changes: applied,
                        ..ReplaySummary::default()
                    }
                } else {
                    ReplaySummary {
                        status: "rebuild_required".into(),
                        changes: applied,
                        errors: vec![format!(
                            "MFT 枚举期间有 {} 条 USN 记录无法还原路径，保留起始检查点",
                            unresolved
                        )],
                    }
                }
            }
            crate::ntfs_helper::ResolvedCatchUpResult::RebuildRequired { reason, .. } => {
                ReplaySummary {
                    status: "rebuild_required".into(),
                    errors: vec![reason],
                    ..ReplaySummary::default()
                }
            }
            crate::ntfs_helper::ResolvedCatchUpResult::Unavailable { reason, .. } => {
                ReplaySummary {
                    status: "unavailable".into(),
                    errors: vec![reason],
                    ..ReplaySummary::default()
                }
            }
        };
        Ok((summary, snapshot.record_count, snapshot.missing_size_count))
    }

    pub fn indexed_roots(&self) -> Result<Vec<IndexedRoot>> {
        let conn = Connection::open(&self.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT root, indexed_at, item_count, metadata_level
               FROM indexed_roots ORDER BY root COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(IndexedRoot {
                path: row.get(0)?,
                indexed_at: row.get(1)?,
                item_count: row.get::<_, i64>(2)?.max(0) as u64,
                metadata_level: row.get(3)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn coverage(
        &self,
        requested_roots: &[String],
        require_full_metadata: bool,
    ) -> Result<IndexCoverage> {
        self.coverage_with_level(
            requested_roots,
            if require_full_metadata {
                "full"
            } else {
                "names_only"
            },
        )
    }

    pub fn coverage_for_estimated_sizes(
        &self,
        requested_roots: &[String],
    ) -> Result<IndexCoverage> {
        self.coverage_with_level(requested_roots, "estimated")
    }

    fn coverage_with_level(
        &self,
        requested_roots: &[String],
        required_level: &str,
    ) -> Result<IndexCoverage> {
        let indexed_roots = self.indexed_roots()?;
        let requested: Vec<String> = requested_roots
            .iter()
            .map(|root| normalize_input_path(root))
            .filter(|root| !root.is_empty())
            .collect();
        let targets: Vec<String> = if requested.is_empty() {
            indexed_roots.iter().map(|root| root.path.clone()).collect()
        } else {
            requested.clone()
        };
        let mut covered_roots = Vec::new();
        let mut missing_roots = Vec::new();
        let mut insufficient_metadata_roots = Vec::new();
        for target in &targets {
            let covering = indexed_roots
                .iter()
                .filter(|indexed| path_covers(&indexed.path, target))
                .max_by_key(|indexed| normalize_input_path(&indexed.path).chars().count());
            match covering {
                Some(indexed)
                    if !metadata_level_satisfies(&indexed.metadata_level, required_level) =>
                {
                    insufficient_metadata_roots.push(target.clone());
                }
                Some(_) => covered_roots.push(target.clone()),
                None => missing_roots.push(target.clone()),
            }
        }
        let ready = !targets.is_empty()
            && missing_roots.is_empty()
            && insufficient_metadata_roots.is_empty();
        Ok(IndexCoverage {
            ready,
            requested_roots: requested,
            covered_roots,
            missing_roots,
            insufficient_metadata_roots,
            indexed_roots,
        })
    }

    pub fn search(&self, query: SearchQuery) -> Result<SearchResult> {
        let conn = Connection::open(&self.db_path)?;
        let mut clauses: Vec<String> = Vec::new();
        let mut values: Vec<SqlValue> = Vec::new();

        if !query.roots.is_empty() {
            let mut root_parts = Vec::new();
            for root in &query.roots {
                let normalized = normalize_input_path(root);
                root_parts.push("(path = ? OR path LIKE ?)".to_string());
                values.push(SqlValue::Text(normalized.clone()));
                values.push(SqlValue::Text(format!(
                    "{}/%",
                    normalized.trim_end_matches('/')
                )));
            }
            clauses.push(format!("({})", root_parts.join(" OR ")));
        }
        if let Some(text) = query.text.filter(|s| !s.is_empty()) {
            clauses.push("(name LIKE ? COLLATE NOCASE OR path LIKE ? COLLATE NOCASE)".into());
            let pattern = format!("%{}%", escape_like(&text));
            values.push(SqlValue::Text(pattern.clone()));
            values.push(SqlValue::Text(pattern));
        }
        if !query.extensions.is_empty() {
            clauses.push(format!(
                "extension IN ({})",
                vec!["?"; query.extensions.len()].join(",")
            ));
            for ext in query.extensions {
                values.push(SqlValue::Text(
                    ext.trim().trim_start_matches('.').to_ascii_lowercase(),
                ));
            }
        }
        match query.kind.as_deref() {
            Some("file") => clauses.push("is_dir = 0".into()),
            Some("dir") | Some("directory") => clauses.push("is_dir = 1".into()),
            Some(other) => return Err(anyhow!("kind 仅支持 file 或 directory，收到: {}", other)),
            None => {}
        }
        for (column, value) in [
            ("size_bytes >= ?", query.min_size_bytes.map(|v| v as i64)),
            ("size_bytes <= ?", query.max_size_bytes.map(|v| v as i64)),
            ("modified >= ?", query.modified_after),
            ("modified <= ?", query.modified_before),
        ] {
            if let Some(value) = value {
                clauses.push(column.into());
                values.push(SqlValue::Integer(value));
            }
        }

        let order = match query.sort.as_str() {
            "size_desc" => "size_bytes DESC, path COLLATE NOCASE",
            "size_asc" => "size_bytes ASC, path COLLATE NOCASE",
            "modified_desc" => "modified DESC, path COLLATE NOCASE",
            "modified_asc" => "modified ASC, path COLLATE NOCASE",
            "name_desc" => "name COLLATE NOCASE DESC",
            "name_asc" | "" => "name COLLATE NOCASE ASC",
            other => return Err(anyhow!("不支持的排序方式: {}", other)),
        };
        let limit = query.limit.clamp(1, 500);
        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let sql = format!(
            "SELECT path,name,extension,is_dir,size_bytes,modified FROM files{} ORDER BY {} LIMIT {}",
            where_sql,
            order,
            limit + 1
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values), |row| {
            Ok(IndexedFile {
                path: row.get(0)?,
                name: row.get(1)?,
                extension: row.get(2)?,
                is_dir: row.get::<_, i64>(3)? != 0,
                size_bytes: row.get::<_, i64>(4)?.max(0) as u64,
                modified_unix: row.get(5)?,
            })
        })?;
        let mut items: Vec<IndexedFile> = rows.filter_map(|r| r.ok()).collect();
        let truncated = items.len() > limit;
        items.truncate(limit);
        Ok(SearchResult {
            returned: items.len(),
            items,
            truncated,
            indexed_roots: self.indexed_roots()?,
            index_status: self.status(),
        })
    }

    /// Summarize indexed file sizes by the first child below `root`. This is the
    /// indexed equivalent of an expensive recursive per-directory disk scan.
    pub fn summarize_usage(
        &self,
        root: &str,
        limit: usize,
        require_exact: bool,
    ) -> Result<UsageSummary> {
        let root = normalize_input_path(root);
        if root.is_empty() {
            return Err(anyhow!("汇总目录不能为空"));
        }
        let coverage = if require_exact {
            self.coverage(std::slice::from_ref(&root), true)?
        } else {
            self.coverage_for_estimated_sizes(std::slice::from_ref(&root))?
        };
        if !coverage.ready {
            return Err(anyhow!(if require_exact {
                "目标目录尚无精确元数据索引"
            } else {
                "目标目录尚无可用于空间估算的 MFT 大小索引"
            }));
        }
        let actual_level = coverage
            .indexed_roots
            .iter()
            .filter(|indexed| path_covers(&indexed.path, &root))
            .max_by_key(|indexed| normalize_input_path(&indexed.path).chars().count())
            .map(|indexed| indexed.metadata_level.as_str())
            .unwrap_or("estimated");

        let prefix = format!("{}/", root.trim_end_matches('/'));
        let child_start = prefix.chars().count() as i64 + 1;
        let pattern = format!("{}%", escape_literal_like(&prefix));
        let conn = Connection::open(&self.db_path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let (total_size_bytes, file_count, sized_file_count, directory_count): (
            i64,
            i64,
            i64,
            i64,
        ) = conn.query_row(
            "SELECT COALESCE(SUM(CASE WHEN is_dir=0 THEN size_bytes ELSE 0 END),0),
                        COALESCE(SUM(CASE WHEN is_dir=0 THEN 1 ELSE 0 END),0),
                        COALESCE(SUM(CASE WHEN is_dir=0 AND size_known=1 THEN 1 ELSE 0 END),0),
                        COALESCE(SUM(CASE WHEN is_dir=1 THEN 1 ELSE 0 END),0)
                   FROM files WHERE path LIKE ?1 ESCAPE '\\'",
            params![pattern],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let cap = limit.clamp(1, 100);
        let sql = r#"
            WITH scoped AS (
              SELECT CASE
                       WHEN instr(substr(path, ?1), '/') > 0
                         THEN substr(substr(path, ?1), 1, instr(substr(path, ?1), '/') - 1)
                       ELSE substr(path, ?1)
                     END AS segment,
                     is_dir,
                     size_bytes,
                     size_known
                FROM files
               WHERE path LIKE ?2 ESCAPE '\'
            )
            SELECT segment,
                   COALESCE(SUM(CASE WHEN is_dir=0 THEN size_bytes ELSE 0 END),0) AS total_size,
                   COALESCE(SUM(CASE WHEN is_dir=0 THEN 1 ELSE 0 END),0) AS file_count,
                   COALESCE(SUM(CASE WHEN is_dir=1 THEN 1 ELSE 0 END),0) AS directory_count,
                   COALESCE(SUM(CASE WHEN is_dir=0 AND size_known=0 THEN 1 ELSE 0 END),0) AS missing_size_count
              FROM scoped
             WHERE segment <> ''
             GROUP BY segment
             ORDER BY total_size DESC, segment COLLATE NOCASE
             LIMIT ?3
        "#;
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params![child_start, pattern, (cap + 1) as i64], |row| {
            let name: String = row.get(0)?;
            Ok(DirectoryUsage {
                path: format!("{}{}", prefix, name),
                name,
                size_bytes: row.get::<_, i64>(1)?.max(0) as u64,
                file_count: row.get::<_, i64>(2)?.max(0) as u64,
                directory_count: row.get::<_, i64>(3)?.max(0) as u64,
                missing_size_count: row.get::<_, i64>(4)?.max(0) as u64,
            })
        })?;
        let mut items: Vec<DirectoryUsage> = rows.filter_map(|row| row.ok()).collect();
        let truncated = items.len() > cap;
        items.truncate(cap);
        let missing_size_count = file_count.saturating_sub(sized_file_count);
        Ok(UsageSummary {
            root,
            accuracy: if actual_level == "full" {
                "exact".into()
            } else {
                "estimated".into()
            },
            total_size_bytes: total_size_bytes.max(0) as u64,
            file_count: file_count.max(0) as u64,
            sized_file_count: sized_file_count.max(0) as u64,
            missing_size_count: missing_size_count.max(0) as u64,
            size_coverage_percent: if file_count <= 0 {
                100.0
            } else {
                sized_file_count.max(0) as f64 * 100.0 / file_count as f64
            },
            directory_count: directory_count.max(0) as u64,
            returned: items.len(),
            items,
            truncated,
            indexed_roots: coverage.indexed_roots,
            index_status: self.status(),
        })
    }
}

#[derive(Default)]
struct ReplaySummary {
    status: String,
    changes: u64,
    errors: Vec<String>,
}

fn capture_journal_checkpoints(roots: &[PathBuf]) -> Vec<crate::ntfs_usn::JournalCheckpoint> {
    let mut volumes = std::collections::HashSet::new();
    let mut checkpoints = Vec::new();
    for root in roots {
        let Some(volume) = crate::ntfs_usn::volume_for_path(root) else {
            continue;
        };
        if !volumes.insert(volume) {
            continue;
        }
        if let crate::ntfs_usn::CatchUpResult::Changes { checkpoint, .. } =
            crate::ntfs_usn::checkpoint(root)
        {
            checkpoints.push(checkpoint);
        }
    }
    checkpoints
}

fn replay_and_store_checkpoints(
    db_path: &Path,
    checkpoints: Vec<crate::ntfs_usn::JournalCheckpoint>,
) -> Result<ReplaySummary> {
    if checkpoints.is_empty() {
        return Ok(ReplaySummary {
            status: "unavailable".into(),
            errors: vec!["没有可用的 NTFS USN Journal；继续使用实时文件监听".into()],
            ..ReplaySummary::default()
        });
    }
    let mut summary = ReplaySummary {
        status: "active".into(),
        ..ReplaySummary::default()
    };
    for checkpoint in checkpoints {
        match crate::ntfs_usn::read_since(&checkpoint) {
            crate::ntfs_usn::CatchUpResult::Changes {
                checkpoint: next,
                changes,
            } => {
                let (applied, unresolved) = apply_usn_changes(db_path, &next.volume, changes)?;
                summary.changes += applied;
                if unresolved > 0 {
                    summary.status = "rebuild_required".into();
                    summary.errors.push(format!(
                        "{} 有 {} 条 USN 记录无法还原路径，需要完整重建该卷索引",
                        next.volume, unresolved
                    ));
                } else {
                    save_journal_checkpoint(db_path, &next)?;
                }
            }
            crate::ntfs_usn::CatchUpResult::RebuildRequired { volume, reason } => {
                summary.status = "rebuild_required".into();
                summary.errors.push(format!("{}: {}", volume, reason));
            }
            crate::ntfs_usn::CatchUpResult::Unavailable { volume, reason } => {
                if summary.status != "rebuild_required" {
                    summary.status = "unavailable".into();
                }
                summary.errors.push(format!("{}: {}", volume, reason));
            }
        }
    }
    Ok(summary)
}

fn apply_replay_status(status: &mut IndexStatus, result: Result<ReplaySummary>) {
    match result {
        Ok(summary) => {
            status.usn_status = if summary.status.is_empty() {
                "not_configured".into()
            } else {
                summary.status
            };
            status.usn_changes_replayed += summary.changes;
            status.usn_last_replay_at = Some(crate::db::now_str());
            status.usn_error = if summary.errors.is_empty() {
                None
            } else {
                Some(summary.errors.join("；"))
            };
        }
        Err(error) => {
            status.usn_status = "unavailable".into();
            status.usn_error = Some(error.to_string());
        }
    }
}

fn load_journal_checkpoints(db_path: &Path) -> Result<Vec<crate::ntfs_usn::JournalCheckpoint>> {
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT volume,journal_id,next_usn FROM usn_checkpoints ORDER BY volume COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |row| {
        let journal_text: String = row.get(1)?;
        Ok((
            row.get::<_, String>(0)?,
            journal_text,
            row.get::<_, i64>(2)?,
        ))
    })?;
    Ok(rows
        .filter_map(|row| row.ok())
        .filter_map(|(volume, journal_id, next_usn)| {
            journal_id
                .parse::<u64>()
                .ok()
                .map(|journal_id| crate::ntfs_usn::JournalCheckpoint {
                    volume,
                    journal_id,
                    next_usn,
                })
        })
        .collect())
}

fn save_journal_checkpoint(
    db_path: &Path,
    checkpoint: &crate::ntfs_usn::JournalCheckpoint,
) -> Result<()> {
    let conn = Connection::open(db_path)?;
    conn.execute(
        "INSERT INTO usn_checkpoints(volume,journal_id,next_usn,updated_at)
         VALUES(?1,?2,?3,?4)
         ON CONFLICT(volume) DO UPDATE SET journal_id=excluded.journal_id,
         next_usn=excluded.next_usn,updated_at=excluded.updated_at",
        params![
            checkpoint.volume,
            checkpoint.journal_id.to_string(),
            checkpoint.next_usn,
            crate::db::now_str(),
        ],
    )?;
    Ok(())
}

fn apply_usn_changes(
    db_path: &Path,
    volume: &str,
    changes: Vec<crate::ntfs_usn::UsnChange>,
) -> Result<(u64, usize)> {
    const USN_REASON_FILE_DELETE: u32 = 0x0000_0200;
    const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;

    let roots = load_indexed_root_paths(db_path)?;
    let volume_path = PathBuf::from(volume);
    let covers_whole_volume = roots
        .iter()
        .any(|root| normalize_input_path(&display_path(root)) == normalize_input_path(volume));
    let (resolved, unresolved) = crate::ntfs_usn::resolve_change_paths(volume, changes);
    let critical_unresolved = if covers_whole_volume { unresolved } else { 0 };
    let mut conn = Connection::open(db_path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let tx = conn.transaction()?;
    let mut applied = 0u64;
    for (change, path) in resolved {
        if !path.starts_with(&volume_path) {
            continue;
        }
        let Some(root) = indexed_root_for_path(&roots, &path) else {
            continue;
        };
        let root_text = display_path(root);
        let path_text = display_path(&path);
        if change.reason & (USN_REASON_FILE_DELETE | USN_REASON_RENAME_OLD_NAME) != 0 {
            let prefix = format!("{}/%", path_text.trim_end_matches('/'));
            let removed = tx.execute(
                "DELETE FROM files WHERE path=?1 OR path LIKE ?2",
                params![path_text, prefix],
            )? as u64;
            adjust_root_count(&tx, &root_text, -(removed as i64))?;
            applied += removed;
        } else if path.exists() {
            applied += upsert_incremental_path(&tx, &root_text, &path, change.is_directory())?;
        }
    }
    tx.commit()?;
    Ok((applied, critical_unresolved))
}

fn apply_resolved_usn_changes(
    db_path: &Path,
    volume: &str,
    changes: Vec<crate::ntfs_helper::ResolvedUsnChange>,
) -> Result<u64> {
    const USN_REASON_FILE_DELETE: u32 = 0x0000_0200;
    const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;

    let roots = load_indexed_root_paths(db_path)?;
    let volume_path = PathBuf::from(volume);
    let mut conn = Connection::open(db_path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let tx = conn.transaction()?;
    let mut applied = 0u64;
    for change in changes {
        let path = PathBuf::from(&change.path);
        if !path.starts_with(&volume_path) {
            continue;
        }
        let Some(root) = indexed_root_for_path(&roots, &path) else {
            continue;
        };
        let root_text = display_path(root);
        let path_text = display_path(&path);
        if change.reason & (USN_REASON_FILE_DELETE | USN_REASON_RENAME_OLD_NAME) != 0 {
            let prefix = format!("{}/%", path_text.trim_end_matches('/'));
            let removed = tx.execute(
                "DELETE FROM files WHERE path=?1 OR path LIKE ?2",
                params![path_text, prefix],
            )? as u64;
            adjust_root_count(&tx, &root_text, -(removed as i64))?;
            applied += removed;
        } else if path.exists() {
            applied += upsert_incremental_path(&tx, &root_text, &path, change.is_directory)?;
        }
    }
    tx.commit()?;
    Ok(applied)
}

fn import_mft_snapshot(
    db_path: &Path,
    snapshot_path: &Path,
    volume: &str,
    checkpoint: &crate::ntfs_usn::JournalCheckpoint,
) -> Result<()> {
    let mut conn = Connection::open(db_path)?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    let snapshot_text = snapshot_path.to_string_lossy().to_string();
    conn.execute("ATTACH DATABASE ?1 AS mft_snapshot", params![snapshot_text])?;
    let transaction_result = (|| -> Result<()> {
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM files WHERE root=?1", params![volume])?;
        tx.execute("DELETE FROM indexed_roots WHERE root=?1", params![volume])?;
        tx.execute(
            r#"
            WITH RECURSIVE tree(frn,path,parent,name,extension,is_dir,size_bytes,size_known) AS (
              SELECT r.file_reference, ?1, ?1, r.name, r.extension,
                     (r.file_attributes & 16) != 0,
                     COALESCE(r.estimated_size_bytes,0), r.size_known
                FROM mft_snapshot.records r
               WHERE r.file_reference = r.parent_reference
              UNION ALL
              SELECT r.file_reference, ?1 || r.name, ?1, r.name, r.extension,
                     (r.file_attributes & 16) != 0,
                     COALESCE(r.estimated_size_bytes,0), r.size_known
                FROM mft_snapshot.records r
               WHERE r.file_reference != r.parent_reference
                 AND NOT EXISTS (
                   SELECT 1 FROM mft_snapshot.records p
                    WHERE p.file_reference = r.parent_reference
                 )
              UNION ALL
              SELECT child.file_reference,
                     CASE WHEN parent.path = ?1
                          THEN ?1 || child.name
                          ELSE parent.path || '/' || child.name END,
                     parent.path, child.name, child.extension,
                     (child.file_attributes & 16) != 0,
                     COALESCE(child.estimated_size_bytes,0), child.size_known
                FROM tree parent
                JOIN mft_snapshot.records child
                  ON child.parent_reference = parent.frn
                 AND child.file_reference != child.parent_reference
            )
            INSERT OR REPLACE INTO files(
              path,root,parent,name,extension,is_dir,size_bytes,size_known,modified
            )
            SELECT path,?1,parent,name,extension,is_dir,size_bytes,size_known,0
              FROM tree WHERE path != ?1
            "#,
            params![volume],
        )?;
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM files WHERE root=?1",
            params![volume],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO indexed_roots(root,indexed_at,item_count,metadata_level)
             VALUES(?1,?2,?3,'estimated')",
            params![volume, crate::db::now_str(), count],
        )?;
        tx.execute(
            "INSERT INTO usn_checkpoints(volume,journal_id,next_usn,updated_at)
             VALUES(?1,?2,?3,?4)
             ON CONFLICT(volume) DO UPDATE SET journal_id=excluded.journal_id,
             next_usn=excluded.next_usn,updated_at=excluded.updated_at",
            params![
                checkpoint.volume,
                checkpoint.journal_id.to_string(),
                checkpoint.next_usn,
                crate::db::now_str(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    })();
    let _ = conn.execute_batch("DETACH DATABASE mft_snapshot");
    transaction_result
}

fn minimize_roots(mut roots: Vec<PathBuf>) -> Vec<PathBuf> {
    roots.sort_by_key(|path| path.components().count());
    let mut kept: Vec<PathBuf> = Vec::new();
    for root in roots {
        if !kept.iter().any(|parent| root.starts_with(parent)) {
            kept.push(root);
        }
    }
    kept
}

fn load_indexed_root_paths(db_path: &Path) -> Result<Vec<PathBuf>> {
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare("SELECT root FROM indexed_roots ORDER BY length(root)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(minimize_roots(
        rows.filter_map(|row| row.ok()).map(PathBuf::from).collect(),
    ))
}

fn spawn_index_watcher(
    db_path: PathBuf,
    status: Arc<Mutex<IndexStatus>>,
) -> mpsc::Sender<WatchMessage> {
    let (tx, rx) = mpsc::channel();
    let event_tx = tx.clone();
    std::thread::Builder::new()
        .name("file-index-watcher".into())
        .spawn(move || {
            let mut watcher =
                match notify::recommended_watcher(move |event: notify::Result<Event>| {
                    let _ = event_tx.send(WatchMessage::Event(event));
                }) {
                    Ok(watcher) => watcher,
                    Err(e) => {
                        status.lock().unwrap().watch_error =
                            Some(format!("启动文件变化监听失败: {}", e));
                        return;
                    }
                };
            let mut watched: Vec<PathBuf> = Vec::new();
            let mut pending_events: Vec<Event> = Vec::new();
            loop {
                let message = rx.recv_timeout(std::time::Duration::from_millis(250));
                match message {
                    Ok(WatchMessage::Reconfigure(roots)) => {
                        flush_incremental_events(&db_path, &watched, &mut pending_events, &status);
                        for root in watched.drain(..) {
                            let _ = watcher.unwatch(&root);
                        }
                        let mut errors = Vec::new();
                        for root in minimize_roots(roots) {
                            match watcher.watch(&root, RecursiveMode::Recursive) {
                                Ok(()) => watched.push(root),
                                Err(e) => errors.push(format!("{}: {}", root.display(), e)),
                            }
                        }
                        let mut current = status.lock().unwrap();
                        current.watching = !watched.is_empty();
                        current.watched_roots =
                            watched.iter().map(|root| display_path(root)).collect();
                        current.watch_error = if errors.is_empty() {
                            None
                        } else {
                            Some(format!("部分目录无法监听: {}", errors.join("；")))
                        };
                    }
                    Ok(WatchMessage::Event(Ok(event))) => {
                        if status.lock().unwrap().running || !is_index_event(&event.kind) {
                            continue;
                        }
                        pending_events.push(event);
                    }
                    Ok(WatchMessage::Event(Err(e))) => {
                        log::debug!("文件索引监听事件错误: {}", e);
                        status.lock().unwrap().watch_error =
                            Some(format!("文件变化监听事件错误: {}", e));
                    }
                    Ok(WatchMessage::Shutdown) => {
                        flush_incremental_events(&db_path, &watched, &mut pending_events, &status);
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        flush_incremental_events(&db_path, &watched, &mut pending_events, &status)
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .expect("failed to spawn file index watcher");
    tx
}

fn is_index_event(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
    )
}

fn flush_incremental_events(
    db_path: &Path,
    roots: &[PathBuf],
    pending: &mut Vec<Event>,
    status: &Arc<Mutex<IndexStatus>>,
) {
    if pending.is_empty() {
        return;
    }
    let events = std::mem::take(pending);
    match apply_incremental_events(db_path, roots, &events) {
        Ok(changed) if changed > 0 => {
            let mut current = status.lock().unwrap();
            current.incremental_updates += changed;
            current.last_change_at = Some(crate::db::now_str());
        }
        Ok(_) => {}
        Err(e) => {
            log::warn!("增量文件索引更新失败: {}", e);
            status.lock().unwrap().watch_error = Some(format!("最近一次增量更新失败: {}", e));
        }
    }
}

fn apply_incremental_events(db_path: &Path, roots: &[PathBuf], events: &[Event]) -> Result<u64> {
    let mut conn = Connection::open(db_path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let tx = conn.transaction()?;
    let mut changed = 0u64;
    for event in events {
        for path in &event.paths {
            let Some(root) = indexed_root_for_path(roots, path) else {
                continue;
            };
            let root_text = display_path(root);
            let path_text = display_path(path);
            if is_index_database_path(db_path, &path_text) {
                continue;
            }
            if path.exists() {
                // A newly created or renamed directory may already contain files by
                // the time the event arrives. Index its small subtree once so those
                // children cannot be missed; ordinary directory metadata changes do
                // not trigger a recursive scan.
                let recursive = path.is_dir()
                    && matches!(event.kind, EventKind::Create(_) | EventKind::Any)
                    || path.is_dir()
                        && matches!(
                            event.kind,
                            EventKind::Modify(notify::event::ModifyKind::Name(_))
                        );
                changed += upsert_incremental_path(&tx, &root_text, path, recursive)?;
            } else {
                let prefix = format!("{}/%", path_text.trim_end_matches('/'));
                let removed = tx.execute(
                    "DELETE FROM files WHERE path=?1 OR path LIKE ?2",
                    params![path_text, prefix],
                )? as u64;
                adjust_root_count(&tx, &root_text, -(removed as i64))?;
                changed += removed;
            }
        }
    }
    tx.commit()?;
    Ok(changed)
}

fn indexed_root_for_path<'a>(roots: &'a [PathBuf], path: &Path) -> Option<&'a PathBuf> {
    roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
}

fn upsert_incremental_path(
    tx: &rusqlite::Transaction<'_>,
    root: &str,
    path: &Path,
    recursive: bool,
) -> Result<u64> {
    let mut queue = VecDeque::from([path.to_path_buf()]);
    let mut changed = 0u64;
    let mut inserted = 0i64;
    while let Some(current) = queue.pop_front() {
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let path_text = display_path(&current);
        let Some(name) = current.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        let parent = current.parent().map(display_path).unwrap_or_default();
        let extension = current
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let is_dir = metadata.is_dir();
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        let was_inserted = tx.execute(
            "INSERT OR IGNORE INTO files(path,root,parent,name,extension,is_dir,size_bytes,size_known,modified)
             VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8)",
            params![
                path_text,
                root,
                parent,
                name,
                extension,
                is_dir as i64,
                if is_dir { 0 } else { metadata.len() as i64 },
                modified,
            ],
        )?;
        if was_inserted == 0 {
            tx.execute(
                "UPDATE files SET root=?2,parent=?3,name=?4,extension=?5,is_dir=?6,size_bytes=?7,size_known=1,modified=?8 WHERE path=?1",
                params![
                    path_text,
                    root,
                    parent,
                    name,
                    extension,
                    is_dir as i64,
                    if is_dir { 0 } else { metadata.len() as i64 },
                    modified,
                ],
            )?;
        } else {
            inserted += 1;
        }
        changed += 1;

        if recursive && is_dir && !is_reparse_point(&metadata) {
            let entries = match std::fs::read_dir(&current) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            queue.extend(entries.filter_map(|entry| entry.ok().map(|entry| entry.path())));
        }
    }
    adjust_root_count(tx, root, inserted)?;
    Ok(changed)
}

fn adjust_root_count(tx: &rusqlite::Transaction<'_>, root: &str, delta: i64) -> Result<()> {
    tx.execute(
        "UPDATE indexed_roots SET item_count=max(0,item_count+?2) WHERE root=?1",
        params![root, delta],
    )?;
    Ok(())
}

fn is_index_database_path(db_path: &Path, candidate: &str) -> bool {
    let index_db = display_path(db_path);
    candidate == index_db
        || candidate == format!("{}-wal", index_db)
        || candidate == format!("{}-shm", index_db)
}

fn init_db(path: &Path) -> Result<()> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        r#"
        PRAGMA journal_mode=WAL;
        PRAGMA synchronous=NORMAL;
        CREATE TABLE IF NOT EXISTS files(
          path TEXT PRIMARY KEY,
          root TEXT NOT NULL,
          parent TEXT NOT NULL,
          name TEXT NOT NULL,
          extension TEXT NOT NULL DEFAULT '',
          is_dir INTEGER NOT NULL,
          size_bytes INTEGER NOT NULL DEFAULT 0,
          size_known INTEGER NOT NULL DEFAULT 0,
          modified INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_files_root ON files(root);
        CREATE INDEX IF NOT EXISTS idx_files_name ON files(name COLLATE NOCASE);
        CREATE INDEX IF NOT EXISTS idx_files_extension ON files(extension);
        CREATE INDEX IF NOT EXISTS idx_files_size ON files(size_bytes DESC);
        CREATE INDEX IF NOT EXISTS idx_files_modified ON files(modified DESC);
        CREATE TABLE IF NOT EXISTS indexed_roots(
          root TEXT PRIMARY KEY,
          indexed_at TEXT NOT NULL,
          item_count INTEGER NOT NULL,
          metadata_level TEXT NOT NULL DEFAULT 'unknown'
        );
        CREATE TABLE IF NOT EXISTS usn_checkpoints(
          volume TEXT PRIMARY KEY,
          journal_id TEXT NOT NULL,
          next_usn INTEGER NOT NULL,
          updated_at TEXT NOT NULL
        );
        "#,
    )?;
    let has_metadata_level = {
        let mut stmt = conn.prepare("PRAGMA table_info(indexed_roots)")?;
        let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
        let found = columns
            .filter_map(|column| column.ok())
            .any(|name| name == "metadata_level");
        found
    };
    if !has_metadata_level {
        conn.execute(
            "ALTER TABLE indexed_roots ADD COLUMN metadata_level TEXT NOT NULL DEFAULT 'unknown'",
            [],
        )?;
    }
    let has_size_known = {
        let mut stmt = conn.prepare("PRAGMA table_info(files)")?;
        let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
        let found = columns
            .filter_map(|column| column.ok())
            .any(|name| name == "size_known");
        found
    };
    if !has_size_known {
        conn.execute(
            "ALTER TABLE files ADD COLUMN size_known INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
        // Existing full indexes already contain sizes collected from metadata.
        conn.execute(
            "UPDATE files SET size_known=1 WHERE root IN (
               SELECT root FROM indexed_roots WHERE metadata_level='full'
             )",
            [],
        )?;
    }
    Ok(())
}

fn build_index(
    db_path: &Path,
    roots: &[PathBuf],
    status: &Arc<Mutex<IndexStatus>>,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut conn = Connection::open(db_path)?;
    let index_db = display_path(db_path);
    let index_wal = format!("{}-wal", index_db);
    let index_shm = format!("{}-shm", index_db);
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let tx = conn.transaction()?;
    for root in roots {
        let root_text = display_path(root);
        tx.execute("DELETE FROM files WHERE root=?1", params![root_text])?;
        tx.execute(
            "DELETE FROM indexed_roots WHERE root=?1",
            params![root_text],
        )?;
    }
    let mut insert = tx.prepare_cached(
        "INSERT OR REPLACE INTO files(path,root,parent,name,extension,is_dir,size_bytes,size_known,modified)
         VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8)",
    )?;

    for root in roots {
        let root_text = display_path(root);
        let mut root_count = 0u64;
        let mut pending_count = 0u64;
        let mut queue = VecDeque::from([root.clone()]);
        while let Some(dir) = queue.pop_front() {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!("文件索引已取消，保留之前的完整索引"));
            }
            {
                let mut s = status.lock().unwrap();
                s.current_path = display_path(&dir);
            }
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(_) => {
                    status.lock().unwrap().skipped += 1;
                    continue;
                }
            };
            for entry in entries {
                if cancel.load(Ordering::Relaxed) {
                    return Err(anyhow!("文件索引已取消，保留之前的完整索引"));
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => {
                        status.lock().unwrap().skipped += 1;
                        continue;
                    }
                };
                let path = entry.path();
                let path_text = display_path(&path);
                // A whole-drive index includes the application's data folder.
                // Exclude this index and its WAL sidecars so it cannot report
                // its own transient growth as a large user file.
                if path_text == index_db || path_text == index_wal || path_text == index_shm {
                    continue;
                }
                let metadata = match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(_) => {
                        status.lock().unwrap().skipped += 1;
                        continue;
                    }
                };
                let is_dir = metadata.is_dir();
                if is_dir && !is_reparse_point(&metadata) {
                    queue.push_back(path.clone());
                }
                let name = entry.file_name().to_string_lossy().to_string();
                let extension = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                insert.execute(params![
                    path_text,
                    root_text,
                    display_path(&dir),
                    name,
                    extension,
                    is_dir as i64,
                    if is_dir { 0 } else { metadata.len() as i64 },
                    modified,
                ])?;
                root_count += 1;
                pending_count += 1;
                // Updating UI-visible progress for every file becomes expensive
                // on large volumes. Publish it in small batches instead.
                if pending_count >= 256 {
                    let mut s = status.lock().unwrap();
                    s.scanned += pending_count;
                    s.indexed += pending_count;
                    pending_count = 0;
                }
            }
        }
        if pending_count > 0 {
            let mut s = status.lock().unwrap();
            s.scanned += pending_count;
            s.indexed += pending_count;
        }
        tx.execute(
            "INSERT INTO indexed_roots(root,indexed_at,item_count,metadata_level)
             VALUES(?1,?2,?3,'full')",
            params![root_text, crate::db::now_str(), root_count as i64],
        )?;
    }
    drop(insert);
    tx.commit()?;
    Ok(())
}

fn display_path(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    // canonicalize() on Windows commonly returns verbatim paths such as
    // \\?\C:\foo. Keep those for traversal but never expose/store that prefix.
    if let Some(rest) = normalized.strip_prefix("//?/UNC/") {
        format!("//{}", rest)
    } else if let Some(rest) = normalized.strip_prefix("//?/") {
        rest.to_string()
    } else {
        normalized
    }
}

fn normalize_input_path(path: &str) -> String {
    let normalized = path.trim().replace('\\', "/");
    if normalized.len() == 3 && normalized.as_bytes()[1] == b':' && normalized.ends_with('/') {
        normalized
    } else {
        normalized.trim_end_matches('/').to_string()
    }
}

fn metadata_level_satisfies(actual: &str, required: &str) -> bool {
    fn rank(level: &str) -> u8 {
        match level {
            "full" => 2,
            "estimated" => 1,
            "names_only" | "unknown" => 0,
            _ => 0,
        }
    }
    rank(actual) >= rank(required)
}

fn escape_like(text: &str) -> String {
    // SQLite LIKE has no escape character unless explicitly declared. Treating
    // user wildcards as wildcards is useful for an Everything-like search.
    text.to_string()
}

fn escape_literal_like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn path_covers(indexed_root: &str, target: &str) -> bool {
    let indexed = normalize_input_path(indexed_root).to_lowercase();
    let target = normalize_input_path(target).to_lowercase();
    target == indexed || target.starts_with(&format!("{}/", indexed.trim_end_matches('/')))
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_and_searches_metadata() {
        let base =
            std::env::temp_dir().join(format!("shiguang-index-test-{}", uuid::Uuid::new_v4()));
        let root = base.join("root");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs").join("report.pdf"), vec![1u8; 2048]).unwrap();
        std::fs::write(root.join("note.txt"), b"hello").unwrap();
        let index = FileIndex::new(&base).unwrap();
        index.start(vec![root.clone()]).unwrap();
        for _ in 0..100 {
            if !index.status().running {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(index.status().error.is_none(), "{:?}", index.status());
        let found = index
            .search(SearchQuery {
                extensions: vec!["pdf".into()],
                min_size_bytes: Some(1024),
                sort: "size_desc".into(),
                limit: 10,
                ..SearchQuery::default()
            })
            .unwrap();
        assert_eq!(found.items.len(), 1);
        assert_eq!(found.items[0].name, "report.pdf");
        let roots = index.indexed_roots().unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].metadata_level, "full");

        let summary = index
            .summarize_usage(&display_path(&root), 10, true)
            .unwrap();
        assert_eq!(summary.accuracy, "exact");
        assert_eq!(summary.total_size_bytes, 2053);
        assert_eq!(summary.file_count, 2);
        let docs = summary
            .items
            .iter()
            .find(|item| item.name == "docs")
            .unwrap();
        assert_eq!(docs.size_bytes, 2048);
        assert_eq!(docs.file_count, 1);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn watcher_tracks_create_modify_and_remove() {
        let base =
            std::env::temp_dir().join(format!("shiguang-watch-test-{}", uuid::Uuid::new_v4()));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let index = FileIndex::new(&base).unwrap();
        index.start(vec![root.clone()]).unwrap();
        for _ in 0..200 {
            let status = index.status();
            if !status.running && status.watching {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(index.status().watching, "{:?}", index.status());

        let added = root.join("created-later.log");
        std::fs::write(&added, b"first").unwrap();
        assert!(wait_for_match(&index, "created-later", true));

        std::fs::write(&added, vec![7u8; 4096]).unwrap();
        for _ in 0..200 {
            let result = search_text(&index, "created-later");
            if result.items.first().map(|item| item.size_bytes) == Some(4096) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            search_text(&index, "created-later").items[0].size_bytes,
            4096
        );

        std::fs::remove_file(&added).unwrap();
        assert!(wait_for_match(&index, "created-later", false));
        drop(index);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn imports_mft_parent_graph_into_paths() {
        let base = std::env::temp_dir().join(format!("shiguang-mft-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let index_db = base.join("file_index.db");
        let snapshot_db = base.join("snapshot.db");
        init_db(&index_db).unwrap();
        let snapshot = Connection::open(&snapshot_db).unwrap();
        snapshot
            .execute_batch(
                "CREATE TABLE records(
                   file_reference INTEGER PRIMARY KEY,
                   parent_reference INTEGER NOT NULL,
                   name TEXT NOT NULL,
                   extension TEXT NOT NULL,
                   file_attributes INTEGER NOT NULL,
                   estimated_size_bytes INTEGER,
                   size_known INTEGER NOT NULL
                 );
                 INSERT INTO records VALUES(5,5,'.','',16,0,1);
                 INSERT INTO records VALUES(10,5,'Users','',16,0,1);
                 INSERT INTO records VALUES(11,10,'report.pdf','pdf',0,4096,1);",
            )
            .unwrap();
        drop(snapshot);
        let checkpoint = crate::ntfs_usn::JournalCheckpoint {
            volume: "C:/".into(),
            journal_id: 7,
            next_usn: 9,
        };
        import_mft_snapshot(&index_db, &snapshot_db, "C:/", &checkpoint).unwrap();
        let conn = Connection::open(&index_db).unwrap();
        let path: String = conn
            .query_row(
                "SELECT path FROM files WHERE name='report.pdf'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(path, "C:/Users/report.pdf");
        let metadata_level: String = conn
            .query_row(
                "SELECT metadata_level FROM indexed_roots WHERE root='C:/'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(metadata_level, "estimated");
        let summary = FileIndex::new(&base)
            .unwrap()
            .summarize_usage("C:/", 10, false)
            .unwrap();
        assert_eq!(summary.accuracy, "estimated");
        assert_eq!(summary.total_size_bytes, 4096);
        assert_eq!(summary.missing_size_count, 0);
        assert_eq!(
            load_journal_checkpoints(&index_db).unwrap(),
            vec![checkpoint]
        );
        drop(conn);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn legacy_index_roots_are_marked_unknown_until_rebuilt() {
        let base =
            std::env::temp_dir().join(format!("shiguang-index-migrate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let db_path = base.join("index.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE indexed_roots(
               root TEXT PRIMARY KEY,
               indexed_at TEXT NOT NULL,
               item_count INTEGER NOT NULL
             );
             INSERT INTO indexed_roots VALUES('D:/','2026-01-01 00:00:00',1);",
        )
        .unwrap();
        drop(conn);
        init_db(&db_path).unwrap();
        let conn = Connection::open(&db_path).unwrap();
        let metadata_level: String = conn
            .query_row(
                "SELECT metadata_level FROM indexed_roots WHERE root='D:/'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(metadata_level, "unknown");
        drop(conn);
        let _ = std::fs::remove_dir_all(base);
    }

    fn wait_for_match(index: &FileIndex, text: &str, expected: bool) -> bool {
        for _ in 0..200 {
            if (!search_text(index, text).items.is_empty()) == expected {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    fn search_text(index: &FileIndex, text: &str) -> SearchResult {
        index
            .search(SearchQuery {
                text: Some(text.into()),
                sort: "name_asc".into(),
                limit: 10,
                ..SearchQuery::default()
            })
            .unwrap()
    }
}
