use anyhow::{anyhow, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct FileItem {
    pub name: String,
    /// 相对被扫描根目录的路径（顶层项与 name 相同）
    pub path: String,
    pub is_dir: bool,
    pub ext: String,
    pub size_kb: u64,
    pub modified: String,
    /// 1 = 根目录直属项，2 = 第一层子文件夹内，依此类推
    pub depth: usize,
}

pub fn desktop_dir() -> Result<PathBuf> {
    dirs::desktop_dir().ok_or_else(|| anyhow!("无法定位桌面目录"))
}

/// 扫描桌面顶层（不进入子文件夹）
pub fn scan_desktop(limit: usize, skip_dir: Option<String>) -> Result<Vec<FileItem>> {
    let dir = desktop_dir()?;
    scan_path(&dir, 1, limit, skip_dir)
}

/// 扫描任意目录。max_depth=1 只列顶层；>1 时深入子文件夹读取内容。
/// skip_dir 仅在根层级按文件夹名跳过（用于排除整理根目录）。
pub fn scan_path(
    dir: &Path,
    max_depth: usize,
    limit: usize,
    skip_dir: Option<String>,
) -> Result<Vec<FileItem>> {
    if !dir.is_dir() {
        return Err(anyhow!("目录不存在或不可访问: {}", dir.display()));
    }
    let mut items = Vec::new();
    scan_level(dir, Path::new(""), 1, max_depth.max(1), &skip_dir, &mut items, limit);
    Ok(items)
}

fn scan_level(
    dir: &Path,
    rel: &Path,
    depth: usize,
    max_depth: usize,
    skip_dir: &Option<String>,
    items: &mut Vec<FileItem>,
    limit: usize,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut level: Vec<(PathBuf, FileItem)> = Vec::new();
    for entry in rd {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "desktop.ini" {
            continue;
        }
        if depth == 1 {
            if let Some(skip) = skip_dir {
                if &name == skip {
                    continue;
                }
            }
        }
        let Ok(ft) = entry.file_type() else { continue };
        // 跳过符号链接/联接点，避免递归循环
        if ft.is_symlink() {
            continue;
        }
        let is_dir = ft.is_dir();
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let modified = meta
            .modified()
            .ok()
            .map(|t| {
                let dt: chrono::DateTime<chrono::Local> = t.into();
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            })
            .unwrap_or_default();
        let child_rel = if rel.as_os_str().is_empty() {
            name.clone()
        } else {
            format!("{}/{}", rel.to_string_lossy().replace('\\', "/"), name)
        };
        level.push((
            path,
            FileItem {
                name,
                path: child_rel,
                is_dir,
                ext,
                size_kb: if is_dir { 0 } else { meta.len() / 1024 },
                modified,
                depth,
            },
        ));
    }
    level.sort_by(|a, b| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase()));
    for (fs_path, item) in level {
        if items.len() >= limit {
            return;
        }
        let descend = item.is_dir && depth < max_depth;
        let child_rel = PathBuf::from(&item.path);
        items.push(item);
        if descend {
            scan_level(&fs_path, &child_rel, depth + 1, max_depth, skip_dir, items, limit);
        }
    }
}
