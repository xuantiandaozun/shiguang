//! 把随安装包带上的浏览器扩展解压到安装目录（与 exe 同级的 browser-extension）。

use anyhow::{anyhow, Result};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

const ZIP_NAME: &str = "browser-extension.zip";
const DIR_NAME: &str = "browser-extension";
const MARKER: &str = ".shiguang-pack";

#[derive(Debug, Clone, serde::Serialize)]
pub struct ExtensionInfo {
    pub path: String,
    pub ready: bool,
}

pub fn ensure_unpacked(app: &AppHandle) -> Result<PathBuf> {
    let dest = install_dir_dest(app).unwrap_or_else(dev_extension_dir);
    let zip = find_zip(app);
    if dest.join("manifest.json").is_file() {
        // 程序升级带来新扩展时重新解压（按压缩包内容比对），用户在浏览器里点「重新加载」即可生效
        if let Some(zip) = zip.as_ref().filter(|z| is_stale(z, &dest)) {
            if let Err(e) = extract_zip(zip, &dest) {
                log::warn!("更新浏览器扩展目录失败（继续使用旧版）: {e:#}");
            }
        }
        return Ok(dest);
    }
    if let Some(zip) = zip {
        extract_zip(&zip, &dest)?;
        return Ok(dest);
    }
    let dev = dev_extension_dir();
    if dev.join("manifest.json").is_file() {
        return Ok(dev);
    }
    Err(anyhow!(
        "未找到浏览器扩展。安装包应把扩展解压到安装目录的 {} 文件夹",
        DIR_NAME
    ))
}

pub fn info(app: &AppHandle) -> ExtensionInfo {
    let path = ensure_unpacked(app)
        .unwrap_or_else(|_| install_dir_dest(app).unwrap_or_else(dev_extension_dir));
    ExtensionInfo {
        ready: path.join("manifest.json").is_file(),
        path: path.to_string_lossy().replace('\\', "/"),
    }
}

fn install_dir_dest(app: &AppHandle) -> Option<PathBuf> {
    exe_dir(app).map(|d| d.join(DIR_NAME))
}

fn exe_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path().resource_dir().ok().or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    })
}

fn find_zip(app: &AppHandle) -> Option<PathBuf> {
    let mut cands = Vec::new();
    if let Ok(dir) = app.path().resource_dir() {
        cands.push(dir.join(ZIP_NAME));
    }
    if let Ok(p) = app
        .path()
        .resolve(ZIP_NAME, tauri::path::BaseDirectory::Resource)
    {
        cands.push(p);
    }
    if let Some(dir) = exe_dir(app) {
        cands.push(dir.join(ZIP_NAME));
    }
    cands.into_iter().find(|p| p.is_file())
}

fn dev_extension_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(DIR_NAME)
}

pub fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let file = File::open(zip_path).map_err(|e| anyhow!("打开扩展压缩包失败: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| anyhow!("解析扩展压缩包失败: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| anyhow!("读取压缩包条目失败: {e}"))?;
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        if rel.as_os_str().is_empty() || entry.is_dir() {
            continue;
        }
        let out = dest.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        let mut f = File::create(&out)?;
        f.write_all(&bytes)?;
    }
    if !dest.join("manifest.json").is_file() {
        anyhow::bail!("压缩包里没有 manifest.json，无法作为浏览器扩展使用");
    }
    let _ = std::fs::write(dest.join(MARKER), fingerprint(zip_path));
    Ok(())
}

fn fingerprint(zip_path: &Path) -> String {
    use sha2::{Digest, Sha256};
    std::fs::read(zip_path)
        .map(|b| format!("{:x}", Sha256::digest(&b)))
        .unwrap_or_default()
}

/// 解压目录不是由这个压缩包解出来的（或是旧版本留下的）
fn is_stale(zip_path: &Path, dest: &Path) -> bool {
    let current = std::fs::read_to_string(dest.join(MARKER)).unwrap_or_default();
    let fp = fingerprint(zip_path);
    !fp.is_empty() && current.trim() != fp
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, content) in entries {
            w.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            w.write_all(content.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn extracts_manifest_and_rejects_zip_slip() {
        let dir = std::env::temp_dir().join(format!("shiguang-ext-{}", uuid::Uuid::new_v4()));
        let zip_path = dir.join("ext.zip");
        let dest = dir.join("out");
        std::fs::create_dir_all(&dir).unwrap();
        let bytes = make_zip(&[
            ("manifest.json", r#"{"name":"拾光浏览器桥"}"#),
            ("background.js", "console.log(1)"),
            ("../escape.txt", "nope"),
        ]);
        std::fs::write(&zip_path, bytes).unwrap();
        extract_zip(&zip_path, &dest).unwrap();
        assert!(dest.join("manifest.json").is_file());
        assert!(dest.join("background.js").is_file());
        assert!(!dir.join("escape.txt").is_file());
        assert!(!is_stale(&zip_path, &dest));
        // 新版压缩包：判定为过期并覆盖
        std::fs::write(&zip_path, make_zip(&[("manifest.json", r#"{"version":"2"}"#)])).unwrap();
        assert!(is_stale(&zip_path, &dest));
        extract_zip(&zip_path, &dest).unwrap();
        assert!(std::fs::read_to_string(dest.join("manifest.json")).unwrap().contains("\"2\""));
        assert!(!is_stale(&zip_path, &dest));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
