//! 文件内容读取与属性查询。
//! - 文本文件（含编码探测：UTF-8 / UTF-16 / GB18030）
//! - Office 文档（docx/xlsx/pptx，按 zip+XML 提取纯文本）
//! - PDF 文档（提取文本层；扫描件无文本层时提示走 read_image）
//! - 大文件分页读取：默认只返回开头，offset 续读或 full 读完整（有安全上限）
//! - 文件属性：大小/创建/修改/访问时间/只读/隐藏等 Windows 属性字段

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const DEFAULT_MAX_CHARS: usize = 4000;
const MAX_PAGE_CHARS: usize = 20_000;
const FULL_READ_CAP: usize = 100_000;
const RAW_READ_CAP: u64 = 20 * 1024 * 1024;

/// 文本类扩展名白名单（writer 模块复用：只有这些格式允许创建/编辑）
pub const TEXT_EXTS: &[&str] = &[
    "txt",
    "text",
    "md",
    "markdown",
    "log",
    "json",
    "jsonl",
    "csv",
    "tsv",
    "xml",
    "yaml",
    "yml",
    "toml",
    "ini",
    "conf",
    "cfg",
    "env",
    "properties",
    "svg",
    "html",
    "htm",
    "css",
    "js",
    "mjs",
    "cjs",
    "jsx",
    "ts",
    "tsx",
    "vue",
    "rs",
    "py",
    "java",
    "c",
    "h",
    "cpp",
    "hpp",
    "cs",
    "go",
    "php",
    "rb",
    "swift",
    "kt",
    "sql",
    "sh",
    "bat",
    "ps1",
    "cmd",
    "lua",
    "r",
    "ipynb",
    "lock",
    "gitignore",
    "gitattributes",
    "editorconfig",
    "dockerignore",
    "reg",
    "vbs",
];

#[derive(Debug, Serialize)]
pub struct ReadResult {
    pub path: String,
    pub format: String,
    pub offset: usize,
    pub total_chars: usize,
    pub returned_chars: usize,
    pub truncated: bool,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FileInfo {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub ext: String,
    pub type_desc: String,
    pub size_bytes: u64,
    pub size_readable: String,
    pub created: String,
    pub modified: String,
    pub accessed: String,
    pub readonly: bool,
    pub hidden: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_count: Option<usize>,
}

enum FileClass {
    Text,
    Unknown,
    Office(&'static str),
    LegacyOffice,
    Pdf,
    Image,
    Binary,
}

fn classify(ext: &str) -> FileClass {
    match ext {
        "docx" | "docm" => FileClass::Office("docx"),
        "xlsx" | "xlsm" => FileClass::Office("xlsx"),
        "pptx" | "pptm" => FileClass::Office("pptx"),
        "doc" | "xls" | "ppt" => FileClass::LegacyOffice,
        "pdf" => FileClass::Pdf,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff" | "heic"
        | "avif" => FileClass::Image,
        e if TEXT_EXTS.contains(&e) => FileClass::Text,
        // 已知二进制扩展直接归类，其余按内容嗅探
        "zip" | "7z" | "rar" | "tar" | "gz" | "exe" | "msi" | "dll" | "com" | "bin" | "dat"
        | "mp3" | "wav" | "flac" | "m4a" | "mp4" | "mkv" | "avi" | "mov" | "wmv" | "iso"
        | "lnk" | "url" | "db" | "sqlite" => FileClass::Binary,
        _ => FileClass::Unknown,
    }
}

/// 绝对路径直接使用；`temp/`/`临时/` → 应用临时目录；`desktop/`/`桌面/` 或其余相对路径 → 桌面。
/// 不存在则报错。
pub fn resolve_path(app: &tauri::AppHandle, raw: &str) -> Result<PathBuf> {
    let full = crate::tempfs::resolve(app, raw, crate::tempfs::RelativeBase::Desktop)?;
    if !full.exists() {
        bail!("路径不存在: {}", full.display());
    }
    Ok(full)
}

pub fn read_file(
    path: &Path,
    offset: usize,
    max_chars: Option<usize>,
    full: bool,
) -> Result<ReadResult> {
    if path.is_dir() {
        bail!(
            "该路径是文件夹，请用 scan_desktop 查看其内容: {}",
            path.display()
        );
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let (format, text) = match classify(&ext) {
        FileClass::Text | FileClass::Unknown => {
            let bytes = read_head(path, RAW_READ_CAP)?;
            // 前 8KB 含 NUL 视为二进制
            if bytes.iter().take(8192).any(|&b| b == 0) {
                bail!("该文件不是文本类文件，无法读取内容。请使用 get_file_info 查询大小、时间等属性。");
            }
            ("text".to_string(), decode_text(&bytes))
        }
        FileClass::Office(kind) => (kind.to_string(), extract_office(path, kind)?),
        FileClass::Pdf => ("pdf".to_string(), extract_pdf(path)?),
        FileClass::LegacyOffice => {
            bail!("旧版 .doc/.xls/.ppt 格式暂不支持读取内容。可另存为新版格式后再读，或用 get_file_info 查看属性。")
        }
        FileClass::Image => {
            bail!("图片不能用 read_file 读取：提取文字请用 ocr_image，理解画面请用 read_image；查属性用 get_file_info。")
        }
        FileClass::Binary => {
            bail!("该文件不是文本类文件，无法读取内容。请使用 get_file_info 查询大小、时间等属性。")
        }
    };
    Ok(paginate(path, &format, &text, offset, max_chars, full))
}

fn paginate(
    path: &Path,
    format: &str,
    text: &str,
    offset: usize,
    max_chars: Option<usize>,
    full: bool,
) -> ReadResult {
    let total = text.chars().count();
    let cap = if full {
        FULL_READ_CAP
    } else {
        max_chars
            .unwrap_or(DEFAULT_MAX_CHARS)
            .clamp(1, MAX_PAGE_CHARS)
    };
    let start = offset.min(total);
    let content: String = text.chars().skip(start).take(cap).collect();
    let returned = content.chars().count();
    let next = start + returned;
    let truncated = next < total;
    let hint = if truncated {
        Some(if full {
            format!(
                "内容超过安全上限 {} 字符，已截断。可用 offset={} 继续分段读取。",
                FULL_READ_CAP, next
            )
        } else {
            format!(
                "仅显示前 {} 字符（共 {} 字符）。用 offset={} 继续读取，或设 full=true 读取完整内容（上限 {} 字符）。",
                returned, total, next, FULL_READ_CAP
            )
        })
    } else {
        None
    };
    ReadResult {
        path: path.to_string_lossy().replace('\\', "/"),
        format: format.to_string(),
        offset: start,
        total_chars: total,
        returned_chars: returned,
        truncated,
        content,
        hint,
    }
}

fn read_head(path: &Path, cap: u64) -> Result<Vec<u8>> {
    let f = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    f.take(cap).read_to_end(&mut buf)?;
    Ok(buf)
}

/// 字节解码为文本：UTF-8 BOM / UTF-16 BOM / UTF-8，失败回退 GB18030（writer 复用做编辑前解码）
pub fn decode_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return utf16_lossy(&bytes[2..], false);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return utf16_lossy(&bytes[2..], true);
    }
    match String::from_utf8(bytes.to_vec()) {
        Ok(s) => s,
        // 中文 Windows 常见 GBK/GB18030 编码的文本文件
        Err(_) => encoding_rs::GB18030.decode(bytes).0.into_owned(),
    }
}

fn utf16_lossy(bytes: &[u8], big_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| {
            if big_endian {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

// ---------- Office (zip + xml) ----------

fn extract_office(path: &Path, kind: &str) -> Result<String> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|_| anyhow!("无法解析该 Office 文档（文件可能已损坏）"))?;
    match kind {
        "docx" => extract_docx(&mut zip),
        "xlsx" => extract_xlsx(&mut zip),
        "pptx" => extract_pptx(&mut zip),
        _ => bail!("不支持的 Office 类型: {}", kind),
    }
}

fn read_zip_entry<R: Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<String> {
    let f = zip
        .by_name(name)
        .map_err(|_| anyhow!("文档中未找到 {}", name))?;
    let mut buf = Vec::new();
    f.take(RAW_READ_CAP).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn list_entries<R: Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    prefix: &str,
    suffix: &str,
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for i in 0..zip.len() {
        if let Ok(f) = zip.by_index(i) {
            let n = f.name();
            if n.starts_with(prefix) && n.ends_with(suffix) {
                names.push(n.to_string());
            }
        }
    }
    names.sort_by_key(|n| entry_number(n));
    names
}

fn entry_number(name: &str) -> u32 {
    name.chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

fn extract_docx<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>) -> Result<String> {
    let xml = read_zip_entry(zip, "word/document.xml")?;
    let xml = xml
        .replace("</w:p>", "\n")
        .replace("</w:tr>", "\n")
        .replace("</w:tc>", "\t")
        .replace("<w:tab/>", "\t")
        .replace("<w:br/>", "\n");
    Ok(collapse_blank(&decode_entities(&strip_tags(&xml))))
}

fn extract_xlsx<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>) -> Result<String> {
    let shared = read_zip_entry(zip, "xl/sharedStrings.xml")
        .map(|xml| parse_shared_strings(&xml))
        .unwrap_or_default();
    let sheets = list_entries(zip, "xl/worksheets/sheet", ".xml");
    if sheets.is_empty() {
        bail!("文档中未找到工作表数据");
    }
    let mut out = String::new();
    for (idx, name) in sheets.iter().enumerate().take(20) {
        let xml = read_zip_entry(zip, name)?;
        out.push_str(&format!("【工作表{}】\n", idx + 1));
        out.push_str(&parse_sheet(&xml, &shared));
        out.push('\n');
    }
    Ok(out)
}

fn extract_pptx<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>) -> Result<String> {
    let slides = list_entries(zip, "ppt/slides/slide", ".xml");
    if slides.is_empty() {
        bail!("文档中未找到幻灯片数据");
    }
    let mut out = String::new();
    for (idx, name) in slides.iter().enumerate().take(100) {
        let xml = read_zip_entry(zip, name)?;
        out.push_str(&format!("【第{}页】\n", idx + 1));
        for t in extract_tag_texts(&xml, "a:t") {
            let t = decode_entities(&t);
            if !t.trim().is_empty() {
                out.push_str(t.trim());
                out.push('\n');
            }
        }
        out.push('\n');
    }
    Ok(out)
}

fn parse_shared_strings(xml: &str) -> Vec<String> {
    blocks(xml, "<si>", "</si>")
        .iter()
        .map(|si| decode_entities(&extract_tag_texts(si, "t").concat()))
        .collect()
}

fn parse_sheet(xml: &str, shared: &[String]) -> String {
    let mut out = String::new();
    for row in blocks(xml, "<row", "</row>") {
        let mut cells: Vec<String> = Vec::new();
        for cell in cell_blocks(row) {
            let (tag, body) = split_open_tag(cell);
            let value = match attr_value(tag, "t").as_deref() {
                Some("s") => first_tag(body, "v")
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .and_then(|i| shared.get(i).cloned())
                    .unwrap_or_default(),
                Some("inlineStr") => first_tag(body, "t").unwrap_or_default(),
                _ => first_tag(body, "v").unwrap_or_default(),
            };
            cells.push(decode_entities(&value));
        }
        if cells.iter().any(|c| !c.trim().is_empty()) {
            out.push_str(&cells.join("\t"));
            out.push('\n');
        }
    }
    out
}

/// 扫描 open 前缀到第一个 close 的块，跳过自闭合标签（<row .../> 这类）
fn blocks<'a>(xml: &'a str, open: &str, close: &str) -> Vec<&'a str> {
    let mut res = Vec::new();
    let mut pos = 0;
    while let Some(i) = xml[pos..].find(open) {
        let start = pos + i;
        let Some(gt) = xml[start..].find('>') else {
            break;
        };
        if xml[start..start + gt + 1].ends_with("/>") {
            pos = start + gt + 1;
            continue;
        }
        let body_start = start + gt + 1;
        let Some(c) = xml[body_start..].find(close) else {
            break;
        };
        let end = body_start + c + close.len();
        res.push(&xml[start..end]);
        pos = end;
    }
    res
}

fn cell_blocks(row: &str) -> Vec<&str> {
    let mut v = blocks(row, "<c ", "</c>");
    v.extend(blocks(row, "<c>", "</c>"));
    v
}

fn split_open_tag(s: &str) -> (&str, &str) {
    match s.find('>') {
        Some(i) => (&s[..=i], &s[i + 1..]),
        None => (s, ""),
    }
}

fn attr_value(tag: &str, name: &str) -> Option<String> {
    let pat = format!("{}=\"", name);
    let i = tag.find(&pat)?;
    let rest = &tag[i + pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn first_tag(xml: &str, tag: &str) -> Option<String> {
    extract_tag_texts(xml, tag).into_iter().next()
}

/// 提取所有 <tag ...>内容</tag> 的文本
fn extract_tag_texts(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut res = Vec::new();
    let mut pos = 0;
    while let Some(i) = xml[pos..].find(&open) {
        let start = pos + i;
        let Some(gt) = xml[start..].find('>') else {
            break;
        };
        if xml[start..start + gt + 1].ends_with("/>") {
            pos = start + gt + 1;
            continue;
        }
        let body_start = start + gt + 1;
        let Some(c) = xml[body_start..].find(&close) else {
            break;
        };
        res.push(xml[body_start..body_start + c].to_string());
        pos = body_start + c + close.len();
    }
    res
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn collapse_blank(s: &str) -> String {
    let re = regex::Regex::new(r"\n{3,}").expect("valid regex");
    re.replace_all(s.trim(), "\n\n").to_string()
}

// ---------- PDF ----------

/// 提取 PDF 纯文本（按页拼接）。扫描件/图片型 PDF 没有文本层，
/// 提取结果为空时给出明确提示，而不是静默返回空白。
fn extract_pdf(path: &Path) -> Result<String> {
    let text = pdf_extract::extract_text(path)
        .map_err(|e| anyhow!("PDF 解析失败（文件可能损坏或加密）: {}", e))?;
    let text = collapse_blank(&text);
    if text.is_empty() {
        bail!("该 PDF 没有可提取的文本层（可能是扫描件/图片型 PDF）。可将页面截图后用 read_image 识别。")
    }
    Ok(text)
}

// ---------- 文件属性 ----------

pub fn file_info(path: &Path) -> Result<FileInfo> {
    let meta = std::fs::metadata(path)?;
    let is_dir = meta.is_dir();
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let ext = if is_dir {
        String::new()
    } else {
        path.extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    let fmt_time = |t: std::io::Result<std::time::SystemTime>| {
        t.ok()
            .map(|t| {
                let dt: chrono::DateTime<chrono::Local> = t.into();
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            })
            .unwrap_or_else(|| "未知".to_string())
    };
    let child_count = if is_dir {
        std::fs::read_dir(path).ok().map(|rd| rd.count())
    } else {
        None
    };
    Ok(FileInfo {
        path: path.to_string_lossy().replace('\\', "/"),
        name,
        kind: if is_dir {
            "文件夹".into()
        } else {
            "文件".into()
        },
        type_desc: type_desc(&ext, is_dir),
        ext,
        size_bytes: if is_dir { 0 } else { meta.len() },
        size_readable: if is_dir {
            "-".into()
        } else {
            human_size(meta.len())
        },
        created: fmt_time(meta.created()),
        modified: fmt_time(meta.modified()),
        accessed: fmt_time(meta.accessed()),
        readonly: meta.permissions().readonly(),
        hidden: is_hidden(path, &meta),
        child_count,
    })
}

#[cfg(windows)]
fn is_hidden(_path: &Path, meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // FILE_ATTRIBUTE_HIDDEN = 0x2
    meta.file_attributes() & 0x2 != 0
}

#[cfg(not(windows))]
fn is_hidden(path: &Path, _meta: &std::fs::Metadata) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().starts_with('.'))
        .unwrap_or(false)
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{} B", bytes)
    } else if b < KB.powi(2) {
        format!("{:.1} KB", b / KB)
    } else if b < KB.powi(3) {
        format!("{:.1} MB", b / KB.powi(2))
    } else {
        format!("{:.2} GB", b / KB.powi(3))
    }
}

fn type_desc(ext: &str, is_dir: bool) -> String {
    if is_dir {
        return "文件夹".into();
    }
    match ext {
        "txt" | "log" => "文本文档".into(),
        "md" | "markdown" => "Markdown 文档".into(),
        "doc" | "docx" | "docm" => "Word 文档".into(),
        "xls" | "xlsx" | "xlsm" => "Excel 工作簿".into(),
        "ppt" | "pptx" | "pptm" => "PowerPoint 演示文稿".into(),
        "pdf" => "PDF 文档".into(),
        "zip" | "7z" | "rar" | "tar" | "gz" => "压缩文件".into(),
        "exe" | "msi" | "com" => "应用程序".into(),
        "dll" => "应用程序扩展".into(),
        "lnk" => "快捷方式".into(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "svg" | "tif" | "tiff" => {
            "图片".into()
        }
        "mp3" | "wav" | "flac" | "m4a" => "音频文件".into(),
        "mp4" | "mkv" | "avi" | "mov" | "wmv" => "视频文件".into(),
        "json" | "jsonl" => "JSON 文件".into(),
        "csv" | "tsv" => "表格数据文件".into(),
        "" => "文件".into(),
        other => format!("{} 文件", other.to_uppercase()),
    }
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
    fn docx_extracts_paragraphs() {
        let document = r#"<?xml version="1.0"?><w:document><w:body><w:p><w:r><w:t>你好</w:t></w:r><w:r><w:t>世界</w:t></w:r></w:p><w:p><w:r><w:t>第二段 &amp; 符号</w:t></w:r></w:p></w:body></w:document>"#;
        let bytes = make_zip(&[("word/document.xml", document)]);
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let text = extract_docx(&mut zip).unwrap();
        assert_eq!(text, "你好世界\n第二段 & 符号");
    }

    #[test]
    fn xlsx_extracts_cells() {
        let shared = r#"<sst><si><t>姓名</t></si><si><t>张三</t></si></sst>"#;
        let sheet = r#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>42</v></c></row><row r="2"><c r="A2" t="s"><v>1</v></c><c r="B2"/></row></sheetData></worksheet>"#;
        let bytes = make_zip(&[
            ("xl/sharedStrings.xml", shared),
            ("xl/worksheets/sheet1.xml", sheet),
        ]);
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let text = extract_xlsx(&mut zip).unwrap();
        assert!(text.contains("【工作表1】"));
        assert!(text.contains("姓名\t42"));
        assert!(text.contains("张三"));
    }

    #[test]
    fn decodes_gbk_text() {
        // "你好" 的 GBK 编码
        assert_eq!(decode_text(&[0xC4, 0xE3, 0xBA, 0xC3]), "你好");
    }

    #[test]
    fn paginates_by_chars() {
        let text = "一二三四五六七八九十";
        let p1 = paginate(Path::new("a.txt"), "text", text, 0, Some(4), false);
        assert_eq!(p1.content, "一二三四");
        assert!(p1.truncated);
        let p2 = paginate(Path::new("a.txt"), "text", text, 4, Some(4), false);
        assert_eq!(p2.content, "五六七八");
        let p3 = paginate(Path::new("a.txt"), "text", text, 8, Some(4), false);
        assert_eq!(p3.content, "九十");
        assert!(!p3.truncated);
    }
}
