//! 常用办公文档：Word / Excel / PDF / PPT 的结构化读取与编辑，以及借助本机 Office/WPS/LibreOffice 的格式转换。
//! 纯 Rust 实现读写，不要求装 Office、不依赖 Python；改已有文件前自动备份。

pub mod convert;
pub mod docx;
pub mod md;
pub mod pdf;
pub mod xlsx;
pub mod xml;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tauri::AppHandle;

pub const TOOL_NAMES: &[&str] = &["office_read", "word_create", "word_edit", "excel_edit", "pdf_edit", "office_convert"];

/// OOXML 包（zip）：保持条目顺序读入内存，按需替换后整体写出
#[derive(Default)]
pub struct Package {
    entries: Vec<(String, Vec<u8>)>,
}

impl Package {
    pub fn open(path: &Path) -> Result<Package> {
        let file = std::fs::File::open(path).with_context(|| format!("打开文件失败：{}", path.display()))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|_| anyhow!("不是有效的 Office 文档（可能是旧版 .doc/.xls 或已加密）：{}", path.display()))?;
        let mut entries = Vec::with_capacity(zip.len());
        for i in 0..zip.len() {
            let mut f = zip.by_index(i)?;
            if f.is_dir() {
                continue;
            }
            let mut buf = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut buf)?;
            entries.push((f.name().to_string(), buf));
        }
        Ok(Package { entries })
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(n, _)| n.as_str())
    }

    pub fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|(n, _)| n == name)
    }

    pub fn text(&self, name: &str) -> Option<String> {
        self.entries.iter().find(|(n, _)| n == name).map(|(_, b)| String::from_utf8_lossy(b).trim_start_matches('\u{feff}').to_string())
    }

    pub fn set(&mut self, name: &str, bytes: Vec<u8>) {
        match self.entries.iter_mut().find(|(n, _)| n == name) {
            Some(e) => e.1 = bytes,
            None => self.entries.push((name.to_string(), bytes)),
        }
    }

    pub fn set_text(&mut self, name: &str, text: &str) {
        self.set(name, text.as_bytes().to_vec());
    }

    /// 先写临时文件再替换，避免写一半损坏原文件
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("sgtmp");
        {
            let file = std::fs::File::create(&tmp).with_context(|| format!("写入失败：{}", tmp.display()))?;
            let mut zip = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for (name, bytes) in &self.entries {
                zip.start_file(name.as_str(), opts)?;
                zip.write_all(bytes)?;
            }
            zip.finish()?;
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            bail!("保存失败（文件可能正在 Word/Excel/WPS 里打开，请先关闭，或用 output 另存）：{} — {e}", path.display());
        }
        Ok(())
    }
}

fn ext_of(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn str_arg<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn shown(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// 已有文件（相对路径按桌面解析）
fn input_path(app: &AppHandle, raw: &str) -> Result<PathBuf> {
    crate::reader::resolve_path(app, raw)
}

/// 新文件（相对路径进应用临时目录）
fn output_path(app: &AppHandle, raw: &str) -> Result<PathBuf> {
    crate::tempfs::resolve(app, raw, crate::tempfs::RelativeBase::Temp)
}

/// 原地修改前备份
fn backup(app: &AppHandle, path: &Path) -> Result<String> {
    let b = crate::writer::backup_file(path, &crate::writer::backup_dir(app))?;
    Ok(shown(&b))
}

/// 编辑类工具的输出位置：给了 output 就另存（不动原文件），否则原地保存并先备份
fn edit_target(app: &AppHandle, input: &Path, args: &Value) -> Result<(PathBuf, Option<String>)> {
    match str_arg(args, "output") {
        Some(o) => {
            let out = output_path(app, o)?;
            if out.exists() && out != input && !args.get("overwrite").and_then(Value::as_bool).unwrap_or(false) {
                bail!("输出文件已存在：{}（要覆盖请设 overwrite=true）", shown(&out));
            }
            Ok((out, None))
        }
        None => Ok((input.to_path_buf(), Some(backup(app, input)?))),
    }
}

fn pptx_read(path: &Path, pages: Option<&str>, max_chars: usize) -> Result<Value> {
    let pkg = Package::open(path)?;
    let mut slides: Vec<(u32, String)> = pkg
        .names()
        .filter_map(|n| n.strip_prefix("ppt/slides/slide").and_then(|r| r.strip_suffix(".xml")).and_then(|d| d.parse::<u32>().ok()).map(|i| (i, n.to_string())))
        .collect();
    slides.sort();
    let total = slides.len() as u32;
    let wanted = match pages {
        Some(p) => pdf::parse_pages(p, total)?,
        None => (1..=total).collect(),
    };
    let para_text = |s: &str| -> String {
        let toks = xml::tokens(s);
        let mut lines = Vec::new();
        let mut cur = String::new();
        let mut in_t = false;
        for t in &toks {
            match t {
                xml::Tok::Start { name, self_closing: false, .. } if name == "a:t" => in_t = true,
                xml::Tok::End { name, .. } if name == "a:t" => in_t = false,
                xml::Tok::Start { name, .. } if name == "a:br" => cur.push('\n'),
                xml::Tok::End { name, .. } if name == "a:p" => {
                    if !cur.trim().is_empty() {
                        lines.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                }
                xml::Tok::Text { start, end } if in_t => cur.push_str(&xml::unescape(&s[*start..*end])),
                _ => {}
            }
        }
        lines.join("\n")
    };
    let mut out = String::new();
    let mut truncated = false;
    for (pos, &p) in wanted.iter().enumerate() {
        let (num, name) = &slides[p as usize - 1];
        let body = pkg.text(name).map(|s| para_text(&s)).unwrap_or_default();
        let notes = pkg.text(&format!("ppt/notesSlides/notesSlide{num}.xml")).map(|s| para_text(&s)).unwrap_or_default();
        let mut chunk = format!("=== 第 {p} 页 ===\n{body}\n");
        let notes: Vec<&str> = notes.lines().filter(|l| l.trim().parse::<u32>().is_err()).collect();
        if !notes.is_empty() {
            chunk.push_str(&format!("[备注] {}\n", notes.join(" ")));
        }
        if out.len() + chunk.len() > max_chars && pos > 0 {
            truncated = true;
            break;
        }
        out.push_str(&chunk);
        out.push('\n');
    }
    Ok(json!({"slides": total, "text": out, "truncated": truncated}))
}

pub async fn execute(app: &AppHandle, name: &str, args: &Value) -> Result<Value> {
    let app = app.clone();
    let name = name.to_string();
    let args = args.clone();
    // 文档处理都是同步 CPU/IO，放到阻塞线程里
    tokio::task::spawn_blocking(move || execute_blocking(&app, &name, &args)).await.map_err(|e| anyhow!("文档处理线程异常：{e}"))?
}

fn execute_blocking(app: &AppHandle, name: &str, args: &Value) -> Result<Value> {
    match name {
        "office_read" => {
            let path = input_path(app, str_arg(args, "path").ok_or_else(|| anyhow!("缺少 path"))?)?;
            let max = args.get("max_chars").and_then(Value::as_u64).unwrap_or(12000).clamp(1000, 100_000) as usize;
            let mut v = match ext_of(&path).as_str() {
                "docx" | "docm" | "dotx" => {
                    let doc = docx::Doc::open(&path)?;
                    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let (text, shown_n, truncated) = doc.outline(offset, max);
                    let mut v = json!({"type": "docx", "stats": doc.stats(), "outline": text});
                    if truncated {
                        v["truncated"] = json!(true);
                        v["next_offset"] = json!(offset + shown_n);
                    }
                    v["note"] = json!("[编号] 供 word_edit 使用；表格行 r1、r2… 与列都从 1 开始");
                    v
                }
                "xlsx" | "xlsm" | "xltx" => {
                    let mut v = xlsx::read(&path, str_arg(args, "sheet"), str_arg(args, "range"), max)?;
                    v["type"] = json!("xlsx");
                    v
                }
                "pdf" => {
                    let mut v = pdf::read(&path, str_arg(args, "pages"), max)?;
                    v["type"] = json!("pdf");
                    v
                }
                "pptx" | "pptm" | "ppsx" => {
                    let mut v = pptx_read(&path, str_arg(args, "pages"), max)?;
                    v["type"] = json!("pptx");
                    v
                }
                "doc" | "xls" | "ppt" | "wps" | "et" | "dps" => bail!("旧版格式 .{} 不能直接解析：先用 office_convert 转成 docx/xlsx/pptx（需要本机有 Office/WPS/LibreOffice）", ext_of(&path)),
                "csv" | "txt" | "md" => bail!("纯文本文件请用 read_file"),
                other => bail!("不支持的格式 .{other}（支持 docx / xlsx / pdf / pptx）"),
            };
            v["path"] = json!(shown(&path));
            Ok(v)
        }
        "word_create" => {
            let path = output_path(app, str_arg(args, "path").ok_or_else(|| anyhow!("缺少 path"))?)?;
            if ext_of(&path) != "docx" {
                bail!("word_create 只生成 .docx");
            }
            if path.exists() && !args.get("overwrite").and_then(Value::as_bool).unwrap_or(false) {
                bail!("文件已存在：{}（要覆盖请设 overwrite=true；修改已有文档用 word_edit）", shown(&path));
            }
            let backup_path = if path.exists() { Some(backup(app, &path)?) } else { None };
            let markdown = args.get("markdown").and_then(Value::as_str).ok_or_else(|| anyhow!("缺少 markdown"))?;
            let opts = docx::CreateOpts {
                title: str_arg(args, "title").map(str::to_string),
                font: str_arg(args, "font").unwrap_or("宋体").to_string(),
                heading_font: str_arg(args, "heading_font").unwrap_or("黑体").to_string(),
                font_size: args.get("font_size").and_then(Value::as_f64).unwrap_or(12.0).clamp(6.0, 72.0),
                line_spacing: args.get("line_spacing").and_then(Value::as_f64).unwrap_or(1.5).clamp(0.8, 4.0),
                first_line_indent: args.get("first_line_indent").and_then(Value::as_bool).unwrap_or(false),
                landscape: args.get("landscape").and_then(Value::as_bool).unwrap_or(false),
            };
            let n = docx::create(&path, markdown, &opts)?;
            let mut v = json!({"ok": true, "path": shown(&path), "blocks": n});
            if let Some(b) = backup_path {
                v["backup"] = json!(b);
            }
            if crate::tempfs::is_under_temp(app, &path) {
                v["hint"] = json!("文件在应用临时目录；交付给用户时请另存到用户指定的位置或告知完整路径");
            }
            Ok(v)
        }
        "word_edit" => {
            let input = input_path(app, str_arg(args, "path").ok_or_else(|| anyhow!("缺少 path"))?)?;
            if !matches!(ext_of(&input).as_str(), "docx" | "docm" | "dotx") {
                bail!("word_edit 只支持 .docx（旧版 .doc 先用 office_convert 转换）");
            }
            let ops = args.get("operations").and_then(Value::as_array).filter(|a| !a.is_empty()).ok_or_else(|| anyhow!("缺少 operations"))?;
            let mut doc = docx::Doc::open(&input)?;
            let results = docx::apply(&mut doc, ops, input.parent())?;
            let (out, backup_path) = edit_target(app, &input, args)?;
            doc.save(&out)?;
            let mut v = json!({"ok": true, "path": shown(&out), "results": results});
            if let Some(b) = backup_path {
                v["backup"] = json!(b);
            }
            Ok(v)
        }
        "excel_edit" => {
            let raw = str_arg(args, "path").ok_or_else(|| anyhow!("缺少 path"))?;
            let ops = args.get("operations").and_then(Value::as_array).filter(|a| !a.is_empty()).ok_or_else(|| anyhow!("缺少 operations"))?;
            let create = args.get("create").and_then(Value::as_bool).unwrap_or(false);
            let (mut book, out, backup_path) = match input_path(app, raw) {
                Ok(input) => {
                    let book = xlsx::open(&input)?;
                    let (out, b) = edit_target(app, &input, args)?;
                    (book, out, b)
                }
                Err(_) if create => {
                    let out = output_path(app, raw)?;
                    if !matches!(ext_of(&out).as_str(), "xlsx" | "xlsm") {
                        bail!("新建工作簿的扩展名应为 .xlsx");
                    }
                    (xlsx::new_book(str_arg(args, "first_sheet")), out, None)
                }
                Err(e) => return Err(e.context("文件不存在；要新建工作簿请设 create=true")),
            };
            let results = xlsx::apply(&mut book, ops)?;
            xlsx::save(&book, &out)?;
            let mut v = json!({"ok": true, "path": shown(&out), "sheets": xlsx::sheet_names(&book), "results": results});
            if let Some(b) = backup_path {
                v["backup"] = json!(b);
            }
            Ok(v)
        }
        "pdf_edit" => {
            let action = str_arg(args, "action").ok_or_else(|| anyhow!("缺少 action"))?;
            let input = str_arg(args, "path").map(|p| input_path(app, p)).transpose()?;
            let inputs: Vec<PathBuf> = args
                .get("inputs")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(|p| input_path(app, p)).collect::<Result<_>>())
                .transpose()?
                .unwrap_or_default();
            let output = str_arg(args, "output").map(|o| output_path(app, o)).transpose()?;
            if action == "to_images" {
                let input = input.ok_or_else(|| anyhow!("to_images 需要 path"))?;
                let dir = output.unwrap_or_else(|| crate::tempfs::temp_dir(app).join("pdf-pages"));
                let width = args.get("width").and_then(Value::as_u64).unwrap_or(1600).clamp(200, 6000) as u32;
                let files = convert::pdf_to_images(&input, &dir, str_arg(args, "pages"), width)?;
                return Ok(json!({"ok": true, "files": files, "hint": "扫描件可对这些图片用 ocr_image 提取文字，或 read_image 看内容"}));
            }
            // 原地旋转先备份
            let backup_path = match (action, &input, &output) {
                ("rotate", Some(i), None) => Some(backup(app, i)?),
                _ => None,
            };
            if let Some(o) = &output {
                if o.exists() && !args.get("overwrite").and_then(Value::as_bool).unwrap_or(false) && action != "split" {
                    bail!("输出文件已存在：{}（要覆盖请设 overwrite=true）", shown(o));
                }
            }
            let mut v = pdf::run(action, input.as_deref(), &inputs, output.as_deref(), args)?;
            if let Some(b) = backup_path {
                v["backup"] = json!(b);
            }
            Ok(v)
        }
        "office_convert" => {
            if str_arg(args, "engine") == Some("list") {
                return Ok(json!({"engines": convert::engines()}));
            }
            let input = input_path(app, str_arg(args, "path").ok_or_else(|| anyhow!("缺少 path"))?)?;
            let output = match (str_arg(args, "output"), str_arg(args, "to")) {
                (Some(o), _) => output_path(app, o)?,
                (None, Some(to)) => input.with_extension(to.trim_start_matches('.').to_lowercase()),
                (None, None) => bail!("需要 to（目标格式，如 pdf / docx / xlsx）或 output（目标路径）"),
            };
            if output.exists() && !args.get("overwrite").and_then(Value::as_bool).unwrap_or(false) {
                bail!("目标文件已存在：{}（要覆盖请设 overwrite=true）", shown(&output));
            }
            convert::convert(&input, &output, str_arg(args, "engine").unwrap_or("auto"))
        }
        other => bail!("未知文档工具 {other}"),
    }
}

/// 用本机真实 Office 校验生成/编辑结果能被正常打开：cargo test --lib office::e2e -- --ignored --nocapture
#[cfg(test)]
mod e2e {
    use super::*;

    #[test]
    #[ignore]
    fn word_and_excel_accept_our_files() {
        let dir = std::env::temp_dir().join(format!("sg-office-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("chart.png");
        image::RgbImage::from_fn(640, 360, |x, y| image::Rgb([(x % 255) as u8, (y % 255) as u8, 160])).save(&img).unwrap();
        let md = format!(
            "# 一、概述\n本报告由 **拾光** 生成，含 *斜体*、`代码` 和 [链接](https://example.com)。\n- 要点一\n  - 子要点\n1. 第一步\n> 引用说明\n| 指标 | 数值 |\n|---|---|\n| 收入 | 100 |\n| 成本 | 60 |\n![示意图]({})\n<!-- pagebreak -->\n## 二、附录\n```\nlet x = 1;\n```\n客户：{{{{客户}}}}",
            img.to_string_lossy().replace('\\', "/")
        );
        let docx_path = dir.join("报告.docx");
        let opts = docx::CreateOpts { title: Some("季度报告".into()), font: "宋体".into(), heading_font: "黑体".into(), font_size: 12.0, line_spacing: 1.5, first_line_indent: true, landscape: false };
        docx::create(&docx_path, &md, &opts).unwrap();
        let r = convert::convert(&docx_path, &dir.join("报告.pdf"), "office").unwrap();
        println!("我们生成的 docx → PDF：{r}");

        // Word 另存一份「原生」docx，再用我们的工具编辑它
        let native = dir.join("native.docx");
        convert::convert(&docx_path, &native, "office").unwrap();
        let mut doc = docx::Doc::open(&native).unwrap();
        let (outline, _, _) = doc.outline(0, 20000);
        println!("Word 原生文件结构：\n{outline}");
        let table_idx = doc.blocks.iter().position(|b| b.xml.starts_with("<w:tbl")).unwrap();
        let ops = serde_json::json!([
            {"op": "fill", "values": {"客户": "某某公司"}},
            {"op": "replace", "find": "拾光", "replace": "ShiGuang"},
            {"op": "insert", "after": 1, "markdown": "插入的段落，带图：\n![]({img})".replace("{img}", &img.to_string_lossy().replace('\\', "/"))},
            {"op": "table_add_row", "index": table_idx, "values": ["利润", "40"]},
            {"op": "set_text", "index": 2, "text": "改写后的第二段 **加粗**"}
        ]);
        let res = docx::apply(&mut doc, ops.as_array().unwrap(), None).unwrap();
        println!("编辑结果：{}", serde_json::to_string(&res).unwrap());
        let edited = dir.join("edited.docx");
        doc.save(&edited).unwrap();
        let r = convert::convert(&edited, &dir.join("edited.pdf"), "office").unwrap();
        println!("编辑后的 Word 原生 docx → PDF：{r}");
        let text = pdf::read(&dir.join("edited.pdf"), None, 20000).unwrap();
        let t = text["text"].as_str().unwrap();
        // 第 2 段先被 replace 再被 set_text 整段改写
        assert!(t.contains("某某公司") && t.contains("改写后的第二段") && t.contains("利润") && t.contains("插入的段落"), "{t}");

        // Excel
        let xlsx_path = dir.join("表.xlsx");
        let mut book = xlsx::new_book(Some("数据"));
        xlsx::apply(&mut book, serde_json::json!([
            {"op": "write", "rows": [["月份", "销量"], ["1月", 10], ["2月", 20]]},
            {"op": "append", "rows": [["合计", "=SUM(B2:B3)"]]},
            {"op": "style", "range": "A1:B1", "bold": true, "fill": "yellow", "border": true},
            {"op": "freeze", "cell": "A2"}, {"op": "width", "columns": "A:B"}, {"op": "merge", "range": "D1:E1"}
        ]).as_array().unwrap()).unwrap();
        xlsx::save(&book, &xlsx_path).unwrap();
        let r = convert::convert(&xlsx_path, &dir.join("表.pdf"), "office").unwrap();
        println!("xlsx → PDF：{r}");
        let t = pdf::read(&dir.join("表.pdf"), None, 5000).unwrap();
        println!("Excel 导出的 PDF 文字：{}", t["text"]);
        assert!(t["text"].as_str().unwrap().contains("30"), "公式应由 Excel 计算出 30");
        println!("产物目录：{}", dir.display());
    }
}
