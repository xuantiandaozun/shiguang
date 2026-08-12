use crate::db::Db;
use anyhow::{ensure, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct ExecResult {
    pub plan_id: i64,
    pub batch_id: String,
    pub moved: u64,
    pub deleted: u64,
    pub skipped: u64,
}

pub fn execute_plan(db: &Db, plan_id: i64) -> Result<ExecResult> {
    let plan = db.get_plan(plan_id)?;
    ensure!(
        plan.status == "pending",
        "方案状态为「{}」，无法重复执行",
        plan.status
    );
    let desktop = crate::organizer::scanner::desktop_dir()?;
    let batch_id = uuid::Uuid::new_v4().to_string();
    let mut moved = 0u64;
    let mut deleted = 0u64;
    let mut skipped = 0u64;

    for cat in &plan.categories {
        let is_delete = cat.action == "delete";
        if !is_delete {
            std::fs::create_dir_all(Path::new(&cat.target_folder))?;
        }
        for name in &cat.files {
            let src = desktop.join(name);
            if !src.exists() {
                skipped += 1;
                continue;
            }
            if is_delete {
                match trash::delete(&src) {
                    Ok(_) => {
                        db.insert_log(&batch_id, "delete", &src.to_string_lossy(), "回收站")?;
                        deleted += 1;
                    }
                    Err(e) => {
                        log::warn!("删除到回收站失败 {}: {}", src.display(), e);
                        skipped += 1;
                    }
                }
                continue;
            }
            let dst = unique_dest(Path::new(&cat.target_folder), name);
            match move_path(&src, &dst) {
                Ok(_) => {
                    db.insert_log(
                        &batch_id,
                        "move",
                        &src.to_string_lossy(),
                        &dst.to_string_lossy(),
                    )?;
                    moved += 1;
                }
                Err(e) => {
                    log::warn!("移动失败 {} -> {}: {}", src.display(), dst.display(), e);
                    skipped += 1;
                }
            }
        }
    }

    db.set_plan_status(plan_id, "executed", Some(&batch_id))?;
    Ok(ExecResult {
        plan_id,
        batch_id,
        moved,
        deleted,
        skipped,
    })
}

pub fn undo_batch(db: &Db, batch_id: &str) -> Result<u64> {
    let logs = db.logs_for_batch(batch_id)?;
    let mut count = 0u64;
    for log in logs.iter().filter(|l| !l.undone) {
        // 删除项在系统回收站里，无法自动还原，需用户从回收站手动恢复
        if log.op_type == "delete" {
            continue;
        }
        let src = Path::new(&log.src_path);
        let dst = Path::new(&log.dst_path);
        if !dst.exists() {
            continue;
        }
        let back = if src.exists() {
            unique_dest(src.parent().unwrap_or(Path::new(".")), &file_name(src))
        } else {
            src.to_path_buf()
        };
        if let Err(e) = move_path(dst, &back) {
            log::warn!(
                "撤销移动失败 {} -> {}: {}",
                dst.display(),
                back.display(),
                e
            );
            continue;
        }
        count += 1;
    }
    db.mark_batch_undone(batch_id)?;
    Ok(count)
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

pub fn move_path(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p)?;
    }
    match std::fs::rename(src, dst) {
        Ok(_) => Ok(()),
        Err(_) => {
            // 跨卷移动时 rename 会失败，退化为复制+删除（仅文件）
            std::fs::copy(src, dst)?;
            std::fs::remove_file(src)
        }
    }
}

pub fn unique_dest(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let p = Path::new(name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| name.to_string());
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for i in 1..1000 {
        let c = dir.join(format!("{} ({}){}", stem, i, ext));
        if !c.exists() {
            return c;
        }
    }
    dir.join(format!("{}-{}{}", stem, uuid::Uuid::new_v4(), ext))
}
