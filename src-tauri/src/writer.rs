//! 文本/代码文件的创建与编辑。
//! - 仅允许文本类扩展名（复用 reader 的文本白名单）或无扩展名文件，防止误写 exe/Office 等二进制
//! - 覆盖与编辑前自动备份原文件到应用数据目录 file_backups/
//! - 编辑保持原文件编码（UTF-8 BOM / UTF-16 / GB18030），避免中文 GBK 文件改写后其它程序打开乱码
//! - create：相对路径默认进应用临时目录（temp/），避免中间产物堆到桌面
//! - edit：相对路径默认桌面（改用户既有文件）

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

use crate::tempfs::{self, RelativeBase};

/// 单次写入内容上限（字符），防止模型超长输出失控
const MAX_CONTENT_CHARS: usize = 200_000;

pub fn create_file(
    app: &AppHandle,
    path_raw: &str,
    content: &str,
    overwrite: bool,
) -> Result<Value> {
    let path = tempfs::resolve(app, path_raw, RelativeBase::Temp)?;
    let mut result = create_file_impl(&path, content, overwrite, &backup_dir(app))?;
    if let Some(obj) = result.as_object_mut() {
        obj.insert("in_temp".into(), json!(tempfs::is_under_temp(app, &path)));
        if tempfs::is_under_temp(app, &path) {
            obj.insert(
                "hint".into(),
                json!("已写入应用临时目录。任务收尾时请询问用户是否清理临时文件。"),
            );
        }
    }
    Ok(result)
}

pub fn edit_file(
    app: &AppHandle,
    path_raw: &str,
    mode: &str,
    old_text: Option<&str>,
    new_text: Option<&str>,
    content: Option<&str>,
    all: bool,
) -> Result<Value> {
    let path = tempfs::resolve(app, path_raw, RelativeBase::Desktop)?;
    edit_file_impl(
        &path,
        mode,
        old_text,
        new_text,
        content,
        all,
        &backup_dir(app),
    )
}

fn backup_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("file_backups")
}

/// 仅文本类文件可写：扩展名需在 reader 的文本白名单内；
/// 无扩展名（Makefile/.gitignore/.env 等）也允许——Windows 上无扩展名不可能是可执行文件
fn check_writable(path: &Path) -> Result<()> {
    if path.is_dir() {
        bail!("该路径是文件夹: {}", path.display());
    }
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if name.is_empty() || name.chars().any(|c| "<>:\"|?*".contains(c)) {
        bail!(
            "文件名无效或含非法字符（< > : \" | ? *）: {}",
            path.display()
        );
    }
    match path.extension() {
        None => Ok(()),
        Some(e) => {
            let ext = e.to_string_lossy().to_lowercase();
            if crate::reader::TEXT_EXTS.contains(&ext.as_str()) {
                Ok(())
            } else {
                bail!(
                    "不支持写入 .{} 格式：仅支持文本/代码类文件（txt/md/json/csv/html/css/js/ts/py 等），Office 与二进制格式不支持。",
                    ext
                )
            }
        }
    }
}

/// 把原文件复制到备份目录（文件名加时间戳前缀），返回备份路径
fn backup_file(path: &Path, dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let fname = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unnamed".into());
    let dest = dir.join(format!(
        "{}_{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        fname
    ));
    std::fs::copy(path, &dest)?;
    Ok(dest)
}

fn create_file_impl(
    path: &Path,
    content: &str,
    overwrite: bool,
    backup_dir: &Path,
) -> Result<Value> {
    let chars = content.chars().count();
    if chars > MAX_CONTENT_CHARS {
        bail!(
            "内容过长（{} 字符），上限 {} 字符",
            chars,
            MAX_CONTENT_CHARS
        );
    }
    check_writable(path)?;
    let existed = path.exists();
    if existed && !overwrite {
        bail!(
            "文件已存在: {}。如需覆盖请设 overwrite=true（会自动备份原文件）；要局部修改请用 edit_file。",
            path.display()
        );
    }
    let backup_path = if existed {
        Some(backup_file(path, backup_dir)?)
    } else {
        None
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, encode_new_text(path, content))?;
    Ok(json!({
        "ok": true,
        "path": path.to_string_lossy().replace('\\', "/"),
        "overwritten": existed,
        "chars": chars,
        "backup": backup_path.map(|p| p.to_string_lossy().replace('\\', "/")),
    }))
}

/// Windows PowerShell 5.1 会把无 BOM 的 UTF-8 `.ps1` 当作系统 ANSI 代码页读取。
/// 新建脚本自动写 UTF-8 BOM，使中文字符串不被错解并间接破坏引号/语法；其它
/// 文本仍保持普通 UTF-8，避免改变常规代码文件的预期格式。
fn encode_new_text(path: &Path, text: &str) -> Vec<u8> {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ps1"))
    {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(text.as_bytes());
        bytes
    } else {
        text.as_bytes().to_vec()
    }
}

fn edit_file_impl(
    path: &Path,
    mode: &str,
    old_text: Option<&str>,
    new_text: Option<&str>,
    content: Option<&str>,
    all: bool,
    backup_dir: &Path,
) -> Result<Value> {
    if !path.exists() {
        bail!(
            "文件不存在: {}。要新建文件请用 create_file。",
            path.display()
        );
    }
    check_writable(path)?;
    let raw_bytes = std::fs::read(path)?;
    let enc = detect_encoding(&raw_bytes);
    // UTF-16 天然含 0x00；只对单字节编码做 NUL 嗅探（与 reader 的二进制判定一致）
    if matches!(enc, Encoding::Utf8 | Encoding::Gb18030)
        && raw_bytes.iter().take(8192).any(|&b| b == 0)
    {
        bail!("该文件不是文本类文件，无法编辑: {}", path.display());
    }
    let original = crate::reader::decode_text(&raw_bytes);

    let (edited, replaced) = match mode {
        "replace" => {
            let old = old_text.ok_or_else(|| anyhow!("replace 模式缺少 old_text"))?;
            let new = new_text.ok_or_else(|| anyhow!("replace 模式缺少 new_text"))?;
            apply_replace(&original, old, new, all)?
        }
        "append" => {
            let c = content.ok_or_else(|| anyhow!("append 模式缺少 content"))?;
            (format!("{}{}", original, c), 0)
        }
        "prepend" => {
            let c = content.ok_or_else(|| anyhow!("prepend 模式缺少 content"))?;
            (format!("{}{}", c, original), 0)
        }
        other => bail!("mode 必须是 replace / append / prepend，收到: {}", other),
    };

    let chars = edited.chars().count();
    if chars > MAX_CONTENT_CHARS {
        bail!(
            "编辑后内容过长（{} 字符），上限 {} 字符",
            chars,
            MAX_CONTENT_CHARS
        );
    }
    if edited == original {
        return Ok(json!({
            "ok": true,
            "changed": false,
            "message": "内容没有变化，文件未改动。",
        }));
    }

    let backup_path = backup_file(path, backup_dir)?;
    std::fs::write(path, encode_text(&edited, &enc))?;
    Ok(json!({
        "ok": true,
        "path": path.to_string_lossy().replace('\\', "/"),
        "changed": true,
        "mode": mode,
        "replaced": replaced,
        "chars": chars,
        "backup": backup_path.to_string_lossy().replace('\\', "/"),
    }))
}

/// 精确替换：0 处匹配报错引导先读原文；多处匹配需 all=true，否则报错并告知次数
fn apply_replace(original: &str, old: &str, new: &str, all: bool) -> Result<(String, usize)> {
    if old.is_empty() {
        bail!("old_text 不能为空");
    }
    let count = original.matches(old).count();
    if count == 0 {
        bail!("未找到与 old_text 完全一致的内容。可能缩进/换行有差异，请先用 read_file 查看原文再重试。");
    }
    if count > 1 && !all {
        bail!(
            "old_text 在文件中出现 {} 次。请补充更多上下文使其唯一匹配，或设 all=true 全部替换。",
            count
        );
    }
    let n = if all { count } else { 1 };
    Ok((
        if all {
            original.replace(old, new)
        } else {
            original.replacen(old, new, 1)
        },
        n,
    ))
}

/// 原文件编码：编辑后按原编码写回
enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Gb18030,
}

fn detect_encoding(bytes: &[u8]) -> Encoding {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Encoding::Utf8Bom
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        Encoding::Utf16Le
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Encoding::Utf16Be
    } else if std::str::from_utf8(bytes).is_ok() {
        Encoding::Utf8
    } else {
        Encoding::Gb18030
    }
}

fn encode_text(text: &str, enc: &Encoding) -> Vec<u8> {
    match enc {
        Encoding::Utf8 => text.as_bytes().to_vec(),
        Encoding::Utf8Bom => {
            let mut v = vec![0xEF, 0xBB, 0xBF];
            v.extend_from_slice(text.as_bytes());
            v
        }
        Encoding::Utf16Le => {
            let mut v = vec![0xFF, 0xFE];
            for u in text.encode_utf16() {
                v.extend_from_slice(&u.to_le_bytes());
            }
            v
        }
        Encoding::Utf16Be => {
            let mut v = vec![0xFE, 0xFF];
            for u in text.encode_utf16() {
                v.extend_from_slice(&u.to_be_bytes());
            }
            v
        }
        Encoding::Gb18030 => encoding_rs::GB18030.encode(text).0.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shiguang-writer-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writable_check_blocks_binary_ext() {
        assert!(check_writable(Path::new("D:/x/a.py")).is_ok());
        assert!(check_writable(Path::new("D:/x/a.md")).is_ok());
        assert!(check_writable(Path::new("D:/x/Makefile")).is_ok());
        assert!(check_writable(Path::new("D:/x/.env")).is_ok());
        assert!(check_writable(Path::new("D:/x/a.exe")).is_err());
        assert!(check_writable(Path::new("D:/x/a.docx")).is_err());
    }

    #[test]
    fn replace_requires_unique_match() {
        let (out, n) = apply_replace("a b a", "b", "c", false).unwrap();
        assert_eq!((out.as_str(), n), ("a c a", 1));
        assert!(apply_replace("a b a", "a", "c", false).is_err());
        let (out, n) = apply_replace("a b a", "a", "c", true).unwrap();
        assert_eq!((out.as_str(), n), ("c b c", 2));
        assert!(apply_replace("abc", "x", "y", false).is_err());
        assert!(apply_replace("abc", "", "y", false).is_err());
    }

    #[test]
    fn create_then_edit_roundtrip() {
        let dir = tmp_dir("roundtrip");
        let bkdir = dir.join("bk");
        let file = dir.join("note.txt");

        create_file_impl(&file, "第一行\n第二行\n", false, &bkdir).unwrap();
        // 已存在且不覆盖：报错
        assert!(create_file_impl(&file, "x", false, &bkdir).is_err());

        edit_file_impl(
            &file,
            "replace",
            Some("第二行"),
            Some("改动行"),
            None,
            false,
            &bkdir,
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "第一行\n改动行\n");
        // 编辑产生了备份
        assert_eq!(std::fs::read_dir(&bkdir).unwrap().count(), 1);

        edit_file_impl(&file, "append", None, None, Some("追加"), false, &bkdir).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "第一行\n改动行\n追加"
        );

        edit_file_impl(&file, "prepend", None, None, Some("开头\n"), false, &bkdir).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "开头\n第一行\n改动行\n追加"
        );
    }

    #[test]
    fn new_powershell_script_uses_utf8_bom() {
        let dir = tmp_dir("powershell-bom");
        let file = dir.join("中文脚本.ps1");
        create_file_impl(&file, "Write-Output '中文'", false, &dir.join("bk")).unwrap();
        let bytes = std::fs::read(file).unwrap();
        assert!(bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
        assert_eq!(crate::reader::decode_text(&bytes), "Write-Output '中文'");
    }

    #[test]
    fn gbk_file_keeps_encoding_after_edit() {
        let dir = tmp_dir("gbk");
        let bkdir = dir.join("bk");
        let file = dir.join("old.txt");
        // 构造 GBK 文件
        let (bytes, _, _) = encoding_rs::GB18030.encode("中文标题\n旧内容\n");
        std::fs::write(&file, bytes.as_ref()).unwrap();

        edit_file_impl(
            &file,
            "replace",
            Some("旧内容"),
            Some("新内容"),
            None,
            false,
            &bkdir,
        )
        .unwrap();
        let raw = std::fs::read(&file).unwrap();
        // 仍是 GBK（不是合法 UTF-8），且解码后内容正确
        assert!(std::str::from_utf8(&raw).is_err());
        assert_eq!(crate::reader::decode_text(&raw), "中文标题\n新内容\n");
    }
}
