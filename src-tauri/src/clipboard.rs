//! 从系统剪贴板取出文件路径或图片，落到聊天附件能用的绝对路径。
//! 资源管理器 Ctrl+C 的文件走 CF_HDROP（保留原路径）；截图等无路径内容写入 temp/pasted/。

use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::AppHandle;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClipboardImport {
    pub paths: Vec<String>,
    pub skipped_dirs: u32,
}

pub fn import_attachments(
    app: &AppHandle,
    include_image: bool,
) -> Result<ClipboardImport, String> {
    #[cfg(windows)]
    {
        win::import(app, include_image)
    }
    #[cfg(not(windows))]
    {
        let _ = (app, include_image);
        Err("当前系统不支持从剪贴板读取文件".into())
    }
}

pub fn save_pasted_bytes(
    app: &AppHandle,
    filename: &str,
    data: &[u8],
) -> Result<String, String> {
    if data.is_empty() {
        return Err("粘贴内容为空".into());
    }
    let dir = pasted_dir(app)?;
    let path = unique_in(&dir, filename);
    std::fs::write(&path, data).map_err(|e| format!("保存粘贴文件失败: {e}"))?;
    Ok(display_path(&path))
}

fn pasted_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = crate::tempfs::ensure_temp_dir(app)
        .map_err(|e| e.to_string())?
        .join("pasted");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn sanitize_filename(name: &str) -> String {
    let trimmed = name.trim().trim_start_matches('.');
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if out.chars().count() >= 120 {
            break;
        }
        match ch {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    let out = out.trim().to_string();
    if out.is_empty() {
        "paste.bin".into()
    } else {
        out
    }
}

fn unique_in(dir: &Path, name: &str) -> PathBuf {
    let safe = sanitize_filename(name);
    let candidate = dir.join(&safe);
    if !candidate.exists() {
        return candidate;
    }
    let stem = Path::new(&safe)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "paste".into());
    let ext = Path::new(&safe)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for i in 1..1000 {
        let p = dir.join(format!("{stem}-{i}{ext}"));
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{stem}-{}{ext}", uuid::Uuid::new_v4().simple()))
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn screenshot_name(ext: &str) -> String {
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    format!("截图-{ts}.{ext}")
}

#[cfg(windows)]
mod win {
    use super::{display_path, pasted_dir, screenshot_name, unique_in, ClipboardImport};
    use std::path::Path;
    use std::time::Duration;
    use tauri::AppHandle;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows_sys::Win32::UI::Shell::DragQueryFileW;

    const CF_DIB: u32 = 8;
    const CF_HDROP: u32 = 15;

    struct ClipboardGuard;
    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }

    fn open_clipboard() -> Result<ClipboardGuard, String> {
        for _ in 0..12 {
            let ok = unsafe { OpenClipboard(std::ptr::null_mut()) };
            if ok != 0 {
                return Ok(ClipboardGuard);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err("无法打开剪贴板，请再试一次".into())
    }

    pub fn import(app: &AppHandle, include_image: bool) -> Result<ClipboardImport, String> {
        let _guard = open_clipboard()?;
        let (paths, skipped_dirs) = read_hdrop();
        if !paths.is_empty() || skipped_dirs > 0 {
            return Ok(ClipboardImport {
                paths,
                skipped_dirs,
            });
        }
        if !include_image {
            return Ok(ClipboardImport::default());
        }
        if let Some(image) = read_image_bytes() {
            let ext = if image.starts_with(b"\x89PNG") {
                "png"
            } else {
                "bmp"
            };
            let dir = pasted_dir(app)?;
            let path = unique_in(&dir, &screenshot_name(ext));
            std::fs::write(&path, image).map_err(|e| format!("保存剪贴板图片失败: {e}"))?;
            return Ok(ClipboardImport {
                paths: vec![display_path(&path)],
                skipped_dirs: 0,
            });
        }
        Ok(ClipboardImport::default())
    }

    fn read_hdrop() -> (Vec<String>, u32) {
        let handle = unsafe { GetClipboardData(CF_HDROP) };
        if handle.is_null() {
            return (Vec::new(), 0);
        }
        let count = unsafe { DragQueryFileW(handle, 0xFFFF_FFFF, std::ptr::null_mut(), 0) };
        let mut paths = Vec::new();
        let mut skipped_dirs = 0u32;
        for i in 0..count {
            let needed = unsafe { DragQueryFileW(handle, i, std::ptr::null_mut(), 0) };
            if needed == 0 {
                continue;
            }
            let mut buf = vec![0u16; needed as usize + 1];
            let written = unsafe { DragQueryFileW(handle, i, buf.as_mut_ptr(), buf.len() as u32) };
            if written == 0 {
                continue;
            }
            let raw = String::from_utf16_lossy(&buf[..written as usize]);
            let path = Path::new(raw.trim());
            if !path.exists() {
                continue;
            }
            if path.is_dir() {
                skipped_dirs += 1;
                continue;
            }
            paths.push(display_path(path));
        }
        (paths, skipped_dirs)
    }

    fn read_image_bytes() -> Option<Vec<u8>> {
        if let Some(png) = clipboard_format_bytes("PNG") {
            if png.starts_with(b"\x89PNG") {
                return Some(png);
            }
        }
        let dib = unsafe { GetClipboardData(CF_DIB) };
        if dib.is_null() {
            return None;
        }
        let raw = hglobal_bytes(dib)?;
        dib_to_bmp(&raw)
    }

    fn clipboard_format_bytes(name: &str) -> Option<Vec<u8>> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let fmt = unsafe { RegisterClipboardFormatW(wide.as_ptr()) };
        if fmt == 0 {
            return None;
        }
        let handle = unsafe { GetClipboardData(fmt) };
        if handle.is_null() {
            return None;
        }
        hglobal_bytes(handle)
    }

    fn hglobal_bytes(handle: windows_sys::Win32::Foundation::HANDLE) -> Option<Vec<u8>> {
        unsafe {
            let ptr = GlobalLock(handle);
            if ptr.is_null() {
                return None;
            }
            let size = GlobalSize(handle);
            let data = if size == 0 {
                None
            } else {
                Some(std::slice::from_raw_parts(ptr as *const u8, size).to_vec())
            };
            GlobalUnlock(handle);
            data
        }
    }

    /// CF_DIB 是无文件头的 DIB，补上 BITMAPFILEHEADER 才能当 .bmp 用。
    fn dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
        if dib.len() < 40 {
            return None;
        }
        let header_size = u32::from_le_bytes(dib[0..4].try_into().ok()?) as usize;
        if header_size < 40 || header_size > dib.len() {
            return None;
        }
        let bit_count = u16::from_le_bytes(dib[14..16].try_into().ok()?);
        let compression = u32::from_le_bytes(dib[16..20].try_into().ok()?);
        let clr_used = u32::from_le_bytes(dib[32..36].try_into().ok()?);
        let palette = if bit_count <= 8 {
            let colors = if clr_used == 0 {
                1u32 << bit_count
            } else {
                clr_used
            };
            colors.saturating_mul(4) as usize
        } else if compression == 3 && header_size == 40 {
            12
        } else {
            clr_used.saturating_mul(4) as usize
        };
        let off_bits = (14 + header_size + palette) as u32;
        if off_bits as usize > 14 + dib.len() {
            return None;
        }
        let file_size = (14 + dib.len()) as u32;
        let mut bmp = Vec::with_capacity(14 + dib.len());
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&file_size.to_le_bytes());
        bmp.extend_from_slice(&[0u8; 4]);
        bmp.extend_from_slice(&off_bits.to_le_bytes());
        bmp.extend_from_slice(dib);
        Some(bmp)
    }
}
