//! AI 工作临时目录：`%APPDATA%/com.deskhelper.win/temp/`
//! - 中间产物、草稿、脚本输出等一律落这里，禁止堆到桌面
//! - 设置页可一键清空；任务结束后由 AI 主动询问是否清理

use anyhow::{bail, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

const TEMP_DIR_NAME: &str = "temp";

/// 从 app_data 根拼出临时目录路径（不创建）
pub fn temp_dir_in(app_data: &Path) -> PathBuf {
    app_data.join(TEMP_DIR_NAME)
}

pub fn temp_dir(app: &AppHandle) -> PathBuf {
    let app_data = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."));
    temp_dir_in(&app_data)
}

pub fn ensure_temp_dir(app: &AppHandle) -> Result<PathBuf> {
    let dir = temp_dir(app);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn temp_path_display(app: &AppHandle) -> String {
    temp_dir(app).to_string_lossy().replace('\\', "/")
}

/// 路径是否落在应用临时目录下（用于 create 结果标记等）
pub fn is_under_temp(app: &AppHandle, path: &Path) -> bool {
    let temp = temp_dir(app);
    path.starts_with(&temp)
}

#[derive(Debug, Clone, Serialize)]
pub struct TempInfo {
    pub path: String,
    pub file_count: u64,
    pub dir_count: u64,
    pub total_bytes: u64,
}

fn walk_stats(dir: &Path) -> (u64, u64, u64) {
    let mut files = 0u64;
    let mut dirs = 0u64;
    let mut bytes = 0u64;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0, 0);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dirs += 1;
            let (f, d, b) = walk_stats(&path);
            files += f;
            dirs += d;
            bytes += b;
        } else {
            files += 1;
            bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    (files, dirs, bytes)
}

pub fn info(app: &AppHandle) -> Result<TempInfo> {
    let dir = ensure_temp_dir(app)?;
    let (file_count, dir_count, total_bytes) = walk_stats(&dir);
    Ok(TempInfo {
        path: dir.to_string_lossy().replace('\\', "/"),
        file_count,
        dir_count,
        total_bytes,
    })
}

/// 清空临时目录内全部内容（保留目录本身），返回清理前的统计
pub fn clear(app: &AppHandle) -> Result<TempInfo> {
    let dir = ensure_temp_dir(app)?;
    let before = info(app)?;
    let entries = std::fs::read_dir(&dir)?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(before)
}

/// 解析用户/AI 给出的路径：
/// - 绝对路径原样使用
/// - `temp/` / `临时/` 前缀 → 应用临时目录
/// - `desktop/` / `桌面/` 前缀 → 桌面
/// - 其余相对路径：由 `relative_base` 决定（create 默认临时目录，读/编辑默认桌面）
pub fn resolve(app: &AppHandle, raw: &str, relative_base: RelativeBase) -> Result<PathBuf> {
    let raw = raw.trim().trim_matches('"');
    if raw.is_empty() {
        bail!("path 不能为空");
    }
    let p = Path::new(raw);
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    let s = raw.replace('\\', "/");
    if let Some(rest) = strip_dir_prefix(&s, &["temp/", "临时/"]) {
        if rest.is_empty() {
            bail!("请给出临时目录下的具体文件名，例如 temp/draft.txt");
        }
        return Ok(ensure_temp_dir(app)?.join(rest));
    }
    if let Some(rest) = strip_dir_prefix(&s, &["desktop/", "桌面/"]) {
        if rest.is_empty() {
            bail!("请给出桌面上的具体文件名，例如 desktop/备忘录.txt");
        }
        return Ok(crate::organizer::scanner::desktop_dir()?.join(rest));
    }
    match relative_base {
        RelativeBase::Temp => Ok(ensure_temp_dir(app)?.join(p)),
        RelativeBase::Desktop => Ok(crate::organizer::scanner::desktop_dir()?.join(p)),
    }
}

#[derive(Debug, Clone, Copy)]
pub enum RelativeBase {
    /// create_file：相对路径默认进临时目录，避免污染桌面
    Temp,
    /// read / edit：相对路径默认桌面（用户桌面上的既有文件）
    Desktop,
}

fn strip_dir_prefix<'a>(s: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    for p in prefixes {
        if let Some(rest) = s.strip_prefix(p) {
            return Some(rest);
        }
        // 也接受不带斜杠的「整目录名」作为非法，上面已处理 empty
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_prefixes() {
        assert_eq!(strip_dir_prefix("temp/a.txt", &["temp/", "临时/"]), Some("a.txt"));
        assert_eq!(strip_dir_prefix("临时/x", &["temp/", "临时/"]), Some("x"));
        assert_eq!(strip_dir_prefix("desktop/a", &["desktop/", "桌面/"]), Some("a"));
        assert_eq!(strip_dir_prefix("a.txt", &["temp/", "临时/"]), None);
    }
}
