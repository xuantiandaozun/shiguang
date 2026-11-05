//! 找 ffmpeg：设置里的路径 → 应用数据目录 tools/ffmpeg → PATH → 按设置里的地址下载（.zip / .tgz）。

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const EXE: &str = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };

fn works(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let mut cmd = Command::new(path);
    cmd.arg("-version").stdout(Stdio::null()).stderr(Stdio::null());
    super::render::hide_window(&mut cmd);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

fn on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(EXE)).find(|p| works(p))
}

/// 不下载，只看本机现有的
pub fn find_local(configured: &str, tools_dir: &Path) -> Option<PathBuf> {
    let configured = configured.trim().trim_matches('"');
    if !configured.is_empty() && works(Path::new(configured)) {
        return Some(PathBuf::from(configured));
    }
    let managed = tools_dir.join("ffmpeg").join(EXE);
    if works(&managed) {
        return Some(managed);
    }
    on_path()
}

/// 找不到就依次尝试下载地址。progress(已下载字节, 总字节)。
pub async fn ensure(
    configured: &str,
    tools_dir: &Path,
    urls: &[String],
    progress: impl Fn(u64, u64),
) -> Result<PathBuf> {
    if let Some(p) = find_local(configured, tools_dir) {
        return Ok(p);
    }
    if urls.is_empty() {
        bail!("没有找到 ffmpeg，也没有配置下载地址。请在「插件 → 录屏」设置里填写 ffmpeg.exe 路径");
    }
    let dest_dir = tools_dir.join("ffmpeg");
    std::fs::create_dir_all(&dest_dir)?;
    let mut errors = Vec::new();
    for url in urls {
        match download(url, &dest_dir, &progress).await {
            Ok(p) if works(&p) => return Ok(p),
            Ok(p) => errors.push(format!("{url}：下载的文件无法运行（{}）", p.display())),
            Err(e) => errors.push(format!("{url}：{e:#}")),
        }
    }
    bail!("准备 ffmpeg 失败：\n{}\n可以手动下载 ffmpeg，并在「插件 → 录屏」设置里填写 ffmpeg.exe 的路径", errors.join("\n"))
}

async fn download(url: &str, dest_dir: &Path, progress: &impl Fn(u64, u64)) -> Result<PathBuf> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(900))
        .build()?;
    let resp = client.get(url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let archive = dest_dir.join("download.part");
    let mut file = std::fs::File::create(&archive)?;
    let mut got = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk)?;
        got += chunk.len() as u64;
        progress(got, total);
    }
    drop(file);
    let lower = url.to_lowercase();
    let out = dest_dir.join(EXE);
    let tmp = dest_dir.join(format!("{EXE}.tmp"));
    let res = tokio::task::spawn_blocking({
        let (archive, tmp) = (archive.clone(), tmp.clone());
        move || {
            if lower.ends_with(".zip") {
                extract_zip(&archive, &tmp)
            } else if lower.ends_with(".tgz") || lower.ends_with(".tar.gz") {
                extract_tgz(&archive, &tmp)
            } else {
                // 直接是可执行文件
                std::fs::copy(&archive, &tmp).map(|_| ()).map_err(Into::into)
            }
        }
    })
    .await
    .map_err(|e| anyhow!("解压线程异常：{e}"))?;
    let _ = std::fs::remove_file(&archive);
    res?;
    std::fs::rename(&tmp, &out).context("保存 ffmpeg")?;
    Ok(out)
}

fn is_target(name: &str) -> bool {
    name.replace('\\', "/").rsplit('/').next() == Some(EXE)
}

fn extract_zip(archive: &Path, to: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.is_file() && is_target(entry.name()) {
            let mut out = std::fs::File::create(to)?;
            std::io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
    }
    bail!("压缩包里没有 {EXE}")
}

fn extract_tgz(archive: &Path, to: &Path) -> Result<()> {
    let gz = flate2::read::GzDecoder::new(std::fs::File::open(archive)?);
    let mut tar = tar::Archive::new(gz);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.to_string_lossy().to_string();
        if is_target(&name) {
            let mut out = std::fs::File::create(to)?;
            std::io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
    }
    bail!("压缩包里没有 {EXE}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_matches_nested_names() {
        assert!(is_target("package/ffmpeg.exe") || !cfg!(windows));
        assert!(!is_target("package/ffprobe.exe"));
    }

    #[test]
    fn tgz_extracts_executable() {
        let dir = std::env::temp_dir().join(format!("sg-ff-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("a.tgz");
        {
            let gz = flate2::write::GzEncoder::new(std::fs::File::create(&archive).unwrap(), flate2::Compression::fast());
            let mut b = tar::Builder::new(gz);
            let data = b"hello";
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_cksum();
            b.append_data(&mut h, format!("package/{EXE}"), &data[..]).unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        let out = dir.join("out");
        extract_tgz(&archive, &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(dir);
    }
}
