//! 聊天消息里点文件名：在资源管理器中打开所在目录并选中该文件。
//! AI 给的常是相对路径（temp/xx.png、只有文件名），依次在临时目录、应用数据、
//! 当前工作区、桌面、下载、文档、视频、图片、用户目录下找；末尾粘了标点时逐步去掉再试。

use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

#[tauri::command]
pub fn reveal_in_folder(app: AppHandle, path: String) -> Result<String, String> {
    let found = locate(&app, &path).ok_or_else(|| format!("找不到文件：{}", path.trim()))?;
    let display = found.to_string_lossy().replace('/', "\\");
    let mut cmd = std::process::Command::new("explorer");
    if found.is_file() {
        cmd.arg(format!("/select,{display}"));
    } else {
        cmd.arg(&display);
    }
    cmd.spawn().map_err(|e| format!("打开资源管理器失败: {e}"))?;
    Ok(display)
}

fn locate(app: &AppHandle, raw: &str) -> Option<PathBuf> {
    let text = normalize(raw)?;
    let temp = crate::tempfs::temp_dir(app);
    let mut bases = vec![temp.clone()];
    if let Ok(dir) = app.path().app_data_dir() {
        bases.push(dir);
    }
    {
        let state = app.state::<crate::AppState>();
        if let Some(ws) = state
            .db
            .current_session_id()
            .ok()
            .and_then(|sid| crate::workspaces::current(&state.db, sid))
        {
            bases.push(PathBuf::from(ws.root));
        }
    }
    bases.extend(
        [
            dirs::desktop_dir(),
            dirs::download_dir(),
            dirs::document_dir(),
            dirs::video_dir(),
            dirs::picture_dir(),
            dirs::home_dir(),
        ]
        .into_iter()
        .flatten(),
    );
    let chars: Vec<char> = text.chars().collect();
    for cut in 0..=chars.len().saturating_sub(1).min(12) {
        let candidate: String = chars[..chars.len() - cut].iter().collect();
        if let Some(found) = resolve_one(candidate.trim(), &temp, &bases) {
            return Some(found);
        }
    }
    // 文件已被移走但目录还在：打开所在目录。
    let parent = Path::new(&text).parent()?;
    (parent.is_absolute() && parent.is_dir()).then(|| parent.to_path_buf())
}

/// 去掉引号/反引号与 file:/// 前缀，展开 ~ 和 %VAR%，统一为反斜杠。
fn normalize(raw: &str) -> Option<String> {
    let mut text = raw
        .trim()
        .trim_matches(|c| matches!(c, '"' | '\'' | '`' | '“' | '”'))
        .to_string();
    if let Some(rest) = text.strip_prefix("file:///") {
        text = percent_decode(rest);
    }
    if let Some(rest) = text.strip_prefix('~') {
        text = format!("{}{}", dirs::home_dir()?.to_string_lossy(), rest);
    }
    let mut guard = 0;
    while let Some(start) = text.find('%') {
        guard += 1;
        let Some(len) = text[start + 1..].find('%') else { break };
        let Ok(value) = std::env::var(&text[start + 1..start + 1 + len]) else { break };
        text = format!("{}{}{}", &text[..start], value, &text[start + 2 + len..]);
        if guard > 4 {
            break;
        }
    }
    let text = text.replace('/', "\\");
    (!text.is_empty() && text.len() <= 1024).then_some(text)
}

fn resolve_one(candidate: &str, temp: &Path, bases: &[PathBuf]) -> Option<PathBuf> {
    if candidate.is_empty() {
        return None;
    }
    let p = Path::new(candidate);
    if p.is_absolute() {
        return p.exists().then(|| p.to_path_buf());
    }
    let rel = candidate.trim_start_matches(".\\");
    for prefix in ["temp\\", "临时\\"] {
        if let Some(rest) = rel.strip_prefix(prefix) {
            let p = temp.join(rest);
            if p.exists() {
                return Some(p);
            }
        }
    }
    bases.iter().map(|b| b.join(rel)).find(|p| p.exists())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_quotes_and_file_urls() {
        assert_eq!(normalize("`C:/a/b.png`").unwrap(), "C:\\a\\b.png");
        assert_eq!(normalize("file:///C:/a%20b/c.txt").unwrap(), "C:\\a b\\c.txt");
    }

    #[test]
    fn resolves_relative_against_bases_and_trims_trailing_text() {
        let dir = std::env::temp_dir().join(format!("reveal-test-{}", std::process::id()));
        let temp = dir.join("temp");
        std::fs::create_dir_all(temp.join("shots")).unwrap();
        std::fs::write(temp.join("shots").join("a.png"), b"x").unwrap();
        let bases = vec![temp.clone()];
        assert!(resolve_one("temp\\shots\\a.png", &temp, &bases).is_some());
        assert!(resolve_one("shots\\a.png", &temp, &bases).is_some());
        assert!(resolve_one("shots\\b.png", &temp, &bases).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
