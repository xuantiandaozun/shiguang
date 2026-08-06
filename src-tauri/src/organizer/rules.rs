use crate::db::{Db, Rule};
use anyhow::Result;
use regex::Regex;
use std::path::{Path, PathBuf};

pub fn rule_matches(rule: &Rule, file_name: &str, ext: &str) -> bool {
    match rule.match_type.as_str() {
        "ext" => rule.pattern.split(',').any(|p| {
            let p = p.trim().trim_start_matches('.').to_lowercase();
            !p.is_empty() && p == ext.to_lowercase()
        }),
        "keyword" => rule.pattern.split(',').any(|p| {
            let p = p.trim().to_lowercase();
            !p.is_empty() && file_name.to_lowercase().contains(&p)
        }),
        "regex" => Regex::new(&rule.pattern)
            .map(|r| r.is_match(file_name))
            .unwrap_or(false),
        _ => false,
    }
}

/// 若文件命中任一启用且已审核的规则，则移动到规则目标目录并记录日志。
/// 返回 Some((文件名, 目标路径)) 表示发生了移动。
pub fn apply_to_file(db: &Db, path: &Path) -> Result<Option<(String, String)>> {
    let Some(name) = path.file_name().map(|s| s.to_string_lossy().to_string()) else {
        return Ok(None);
    };
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let rules = db.list_active_rules()?;
    let Some(rule) = rules.into_iter().find(|r| rule_matches(r, &name, &ext)) else {
        return Ok(None);
    };
    let target_dir = PathBuf::from(&rule.target_folder);
    let dst = crate::organizer::executor::unique_dest(&target_dir, &name);
    crate::organizer::executor::move_path(path, &dst)?;
    let batch = format!("auto-{}", uuid::Uuid::new_v4());
    db.insert_log(
        &batch,
        "auto-move",
        &path.to_string_lossy(),
        &dst.to_string_lossy(),
    )?;
    Ok(Some((name, dst.to_string_lossy().to_string())))
}
