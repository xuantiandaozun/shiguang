//! 简化 Markdown → Word 段落 XML。每个非空行是一个段落（不按 Markdown 合并相邻行）。
//! 支持：# 标题（1–6 级）、- / * / 1. 列表（两个空格一级缩进）、> 引用、| 表格 |、``` 代码块、
//! ![说明](图片路径) 独占一行插图、--- 分隔线、<!-- pagebreak --> 分页；行内 **粗体** *斜体* `代码` [文字](链接)。

use super::xml::escape;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;

/// 转换时需要往包里登记的东西（图片、超链接）
#[derive(Default)]
pub struct Media {
    /// (包内路径, 内容)
    pub files: Vec<(String, Vec<u8>)>,
    /// (rId, 类型, 目标, 是否外部)
    pub rels: Vec<(String, &'static str, String, bool)>,
    pub next_id: u32,
    pub extensions: Vec<String>,
}

pub const REL_IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
pub const REL_LINK: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";

impl Media {
    fn rid(&mut self) -> String {
        self.next_id += 1;
        format!("rIdSg{}", self.next_id)
    }
}

pub struct Ctx<'a> {
    /// 标题级别 → 样式 id；None 时用直接格式
    pub heading_style: &'a dyn Fn(usize) -> Option<String>,
    pub base_dir: Option<PathBuf>,
    pub media: &'a mut Media,
    /// 正文段落额外的段落属性（如首行缩进），放在 <w:pPr> 里
    pub body_ppr: String,
}

const MAX_IMAGE_EMU: f64 = 5_212_080.0; // 约 14.5 cm，A4 默认版心内

fn run(text: &str, rpr: &str) -> String {
    let mut out = String::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push_str(&format!("<w:r>{}<w:br/></w:r>", wrap_rpr(rpr)));
        }
        if line.is_empty() {
            continue;
        }
        // 制表符单独成元素
        for (j, part) in line.split('\t').enumerate() {
            if j > 0 {
                out.push_str(&format!("<w:r>{}<w:tab/></w:r>", wrap_rpr(rpr)));
            }
            if !part.is_empty() {
                out.push_str(&format!("<w:r>{}<w:t xml:space=\"preserve\">{}</w:t></w:r>", wrap_rpr(rpr), escape(part)));
            }
        }
    }
    out
}

fn wrap_rpr(rpr: &str) -> String {
    if rpr.is_empty() {
        String::new()
    } else {
        format!("<w:rPr>{rpr}</w:rPr>")
    }
}

/// 行内格式 → 若干 <w:r>
pub fn inline(text: &str, base_rpr: &str, media: &mut Media) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut buf = String::new();
    let (mut bold, mut italic) = (false, false);
    let rpr = |b: bool, i: bool| format!("{}{}{}", if b { "<w:b/><w:bCs/>" } else { "" }, if i { "<w:i/><w:iCs/>" } else { "" }, base_rpr);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '\\' && next.is_some_and(|n| "*`[]\\_".contains(n)) {
            buf.push(next.unwrap());
            i += 2;
            continue;
        }
        let rest_has = |from: usize, pat: &str| chars.get(from..).is_some_and(|r| r.iter().collect::<String>().contains(pat));
        if c == '*' && next == Some('*') && (bold || rest_has(i + 2, "**")) {
            out.push_str(&run(&std::mem::take(&mut buf), &rpr(bold, italic)));
            bold = !bold;
            i += 2;
            continue;
        }
        if c == '*' && next != Some('*') && (italic || (next.is_some_and(|n| !n.is_whitespace()) && rest_has(i + 1, "*"))) {
            out.push_str(&run(&std::mem::take(&mut buf), &rpr(bold, italic)));
            italic = !italic;
            i += 1;
            continue;
        }
        if c == '`' {
            if let Some(len) = chars[i + 1..].iter().position(|&x| x == '`') {
                out.push_str(&run(&std::mem::take(&mut buf), &rpr(bold, italic)));
                let code: String = chars[i + 1..i + 1 + len].iter().collect();
                let mono = format!("<w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\"/><w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F2F2F2\"/>{}", rpr(bold, italic));
                out.push_str(&run(&code, &mono));
                i += len + 2;
                continue;
            }
        }
        if c == '[' {
            // [文字](链接)
            let rest: String = chars[i..].iter().collect();
            if let Some(close) = rest.find("](") {
                if let Some(end) = rest[close + 2..].find(')') {
                    let label = &rest[1..close];
                    let url = &rest[close + 2..close + 2 + end];
                    if !label.is_empty() && !url.is_empty() && !url.contains(' ') {
                        out.push_str(&run(&std::mem::take(&mut buf), &rpr(bold, italic)));
                        let rid = media.rid();
                        media.rels.push((rid.clone(), REL_LINK, url.to_string(), true));
                        let link_rpr = format!("<w:color w:val=\"0563C1\"/><w:u w:val=\"single\"/>{}", rpr(bold, italic));
                        out.push_str(&format!("<w:hyperlink r:id=\"{rid}\" w:history=\"1\">{}</w:hyperlink>", run(label, &link_rpr)));
                        i += rest[..close + 2 + end + 1].chars().count();
                        continue;
                    }
                }
            }
        }
        buf.push(c);
        i += 1;
    }
    out.push_str(&run(&buf, &rpr(bold, italic)));
    out
}

fn para(ppr: &str, runs: &str) -> String {
    if ppr.is_empty() {
        format!("<w:p>{runs}</w:p>")
    } else {
        format!("<w:p><w:pPr>{ppr}</w:pPr>{runs}</w:p>")
    }
}

fn heading(level: usize, text: &str, ctx: &mut Ctx) -> String {
    let lvl = level.clamp(1, 6);
    match (ctx.heading_style)(lvl) {
        Some(id) => para(&format!("<w:pStyle w:val=\"{}\"/><w:outlineLvl w:val=\"{}\"/>", escape(&id), lvl - 1), &inline(text, "", ctx.media)),
        None => {
            let size = [32, 28, 26, 24, 24, 24][lvl - 1];
            para(
                &format!("<w:keepNext/><w:spacing w:before=\"240\" w:after=\"120\"/><w:outlineLvl w:val=\"{}\"/>", lvl - 1),
                &inline(text, &format!("<w:b/><w:bCs/><w:sz w:val=\"{size}\"/><w:szCs w:val=\"{size}\"/>"), ctx.media),
            )
        }
    }
}

fn image(alt: &str, raw_path: &str, ctx: &mut Ctx) -> Result<String> {
    let mut path = PathBuf::from(raw_path.trim().trim_matches('<').trim_matches('>'));
    if path.is_relative() {
        if let Some(base) = &ctx.base_dir {
            path = base.join(path);
        }
    }
    let bytes = std::fs::read(&path).with_context(|| format!("读取图片失败：{}", path.display()))?;
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let ext = match ext.as_str() {
        "png" => "png",
        "jpg" | "jpeg" => "jpeg",
        "gif" => "gif",
        "bmp" => "bmp",
        _ => bail!("图片只支持 png/jpg/gif/bmp：{}", path.display()),
    };
    let (w, h) = image::image_dimensions(&path).unwrap_or((800, 600));
    let mut cx = w as f64 * 9525.0;
    let mut cy = h as f64 * 9525.0;
    if cx > MAX_IMAGE_EMU {
        cy *= MAX_IMAGE_EMU / cx;
        cx = MAX_IMAGE_EMU;
    }
    let rid = ctx.media.rid();
    let n = ctx.media.next_id;
    let name = format!("media/sg_image{n}.{ext}");
    ctx.media.files.push((format!("word/{name}"), bytes));
    ctx.media.rels.push((rid.clone(), REL_IMAGE, name, false));
    if !ctx.media.extensions.iter().any(|e| e == ext) {
        ctx.media.extensions.push(ext.to_string());
    }
    let (cx, cy) = (cx.round() as i64, cy.round() as i64);
    let alt = escape(alt);
    let id = 4000 + n;
    Ok(format!(
        "<w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr><w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\"><wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:docPr id=\"{id}\" name=\"图片 {id}\" descr=\"{alt}\"/><wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" noChangeAspect=\"1\"/></wp:cNvGraphicFramePr><a:graphic xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:nvPicPr><pic:cNvPr id=\"{id}\" name=\"sg_image{n}.{ext}\" descr=\"{alt}\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed=\"{rid}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"
    ))
}

fn table_cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().replace("<br>", "\n").replace("<br/>", "\n")).collect()
}

fn is_separator_row(line: &str) -> bool {
    let cells = table_cells(line);
    !cells.is_empty() && cells.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')))
}

/// 表格：rows[0] 为表头（加粗、浅灰底）
pub fn table(rows: &[Vec<String>], header: bool, media: &mut Media) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let width = 5000 / cols as u32;
    let mut out = String::from(
        "<w:tbl><w:tblPr><w:tblW w:w=\"5000\" w:type=\"pct\"/><w:tblBorders><w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"808080\"/><w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"808080\"/><w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"808080\"/><w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"808080\"/><w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"808080\"/><w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"808080\"/></w:tblBorders><w:tblCellMar><w:left w:w=\"108\" w:type=\"dxa\"/><w:right w:w=\"108\" w:type=\"dxa\"/></w:tblCellMar><w:tblLook w:val=\"04A0\" w:firstRow=\"1\" w:lastRow=\"0\" w:firstColumn=\"1\" w:lastColumn=\"0\" w:noHBand=\"0\" w:noVBand=\"1\"/></w:tblPr><w:tblGrid>",
    );
    for _ in 0..cols {
        out.push_str("<w:gridCol/>");
    }
    out.push_str("</w:tblGrid>");
    for (r, row) in rows.iter().enumerate() {
        let head = header && r == 0;
        out.push_str(if head { "<w:tr><w:trPr><w:tblHeader/></w:trPr>" } else { "<w:tr>" });
        for c in 0..cols {
            let text = row.get(c).map(String::as_str).unwrap_or("");
            let shade = if head { "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F2F2F2\"/>" } else { "" };
            let rpr = if head { "<w:b/><w:bCs/>" } else { "" };
            out.push_str(&format!(
                "<w:tc><w:tcPr><w:tcW w:w=\"{width}\" w:type=\"pct\"/>{shade}</w:tcPr><w:p><w:pPr><w:spacing w:before=\"0\" w:after=\"0\"/><w:ind w:firstLine=\"0\" w:firstLineChars=\"0\"/></w:pPr>{}</w:p></w:tc>",
                inline(text, rpr, media)
            ));
        }
        out.push_str("</w:tr>");
    }
    out.push_str("</w:tbl>");
    out
}

/// Markdown → 段落/表格 XML 列表
pub fn to_blocks(md: &str, ctx: &mut Ctx) -> Result<Vec<String>> {
    let lines: Vec<&str> = md.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let no_indent = "<w:ind w:firstLine=\"0\" w:firstLineChars=\"0\"/>";
    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim_end();
        let t = line.trim_start();
        if t.is_empty() {
            i += 1;
            continue;
        }
        // 代码块
        if t.starts_with("```") {
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                let code = lines[i].trim_end();
                out.push(para(
                    &format!("<w:spacing w:before=\"0\" w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/>{no_indent}<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F2F2F2\"/>"),
                    &run(&code.replace('\t', "    "), "<w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\" w:eastAsia=\"等线\"/><w:sz w:val=\"20\"/>"),
                ));
                i += 1;
            }
            i += 1;
            continue;
        }
        // 表格：连续的 | 行
        if t.starts_with('|') {
            let mut rows: Vec<Vec<String>> = Vec::new();
            let mut header = false;
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                if is_separator_row(lines[i]) {
                    header = rows.len() == 1;
                } else {
                    rows.push(table_cells(lines[i]));
                }
                i += 1;
            }
            out.push(table(&rows, header, ctx.media));
            // 表格后面紧跟表格时 Word 会把两个表格并在一起，中间留一个空段落
            out.push("<w:p/>".to_string());
            continue;
        }
        i += 1;
        if t == "<!-- pagebreak -->" || t == "\\pagebreak" {
            out.push("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>".to_string());
            continue;
        }
        if t.chars().all(|c| c == '-') && t.len() >= 3 || t == "***" {
            out.push(para("<w:pBdr><w:bottom w:val=\"single\" w:sz=\"6\" w:space=\"1\" w:color=\"A6A6A6\"/></w:pBdr>", ""));
            continue;
        }
        let hashes = t.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            out.push(heading(hashes, t[hashes..].trim(), ctx));
            continue;
        }
        if let Some(rest) = t.strip_prefix("![") {
            if let Some(close) = rest.find("](") {
                if rest.ends_with(')') {
                    let alt = &rest[..close];
                    let path = &rest[close + 2..rest.len() - 1];
                    out.push(image(alt, path, ctx)?);
                    continue;
                }
            }
        }
        let level = (line.len() - t.len()) / 2;
        let bullet = ["- ", "* ", "+ "].iter().find_map(|p| t.strip_prefix(p));
        let ordered = t.split_once(". ").filter(|(n, _)| !n.is_empty() && n.len() <= 3 && n.chars().all(|c| c.is_ascii_digit()));
        if bullet.is_some() || ordered.is_some() {
            let (marker, body) = match (bullet, ordered) {
                (Some(b), _) => (if level == 0 { "•".to_string() } else { "◦".to_string() }, b),
                (None, Some((n, b))) => (format!("{n}."), b),
                _ => unreachable!(),
            };
            let left = 420 * (level + 1);
            let runs = format!("{}{}", run(&format!("{marker}\t"), ""), inline(body.trim(), "", ctx.media));
            out.push(para(&format!("<w:tabs><w:tab w:val=\"left\" w:pos=\"{left}\"/></w:tabs><w:spacing w:before=\"0\" w:after=\"60\"/><w:ind w:left=\"{left}\" w:hanging=\"300\" w:firstLineChars=\"0\"/>"), &runs));
            continue;
        }
        if let Some(q) = t.strip_prefix('>') {
            out.push(para(
                &format!("<w:pBdr><w:left w:val=\"single\" w:sz=\"18\" w:space=\"8\" w:color=\"BFBFBF\"/></w:pBdr><w:ind w:left=\"360\" w:firstLine=\"0\" w:firstLineChars=\"0\"/>"),
                &inline(q.trim(), "<w:color w:val=\"595959\"/>", ctx.media),
            ));
            continue;
        }
        out.push(para(&ctx.body_ppr, &inline(t, "", ctx.media)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(md: &str) -> (Vec<String>, Media) {
        let mut media = Media::default();
        let hs = |l: usize| Some(format!("Heading{l}"));
        let blocks = {
            let mut ctx = Ctx { heading_style: &hs, base_dir: None, media: &mut media, body_ppr: String::new() };
            to_blocks(md, &mut ctx).unwrap()
        };
        (blocks, media)
    }

    #[test]
    fn headings_lists_tables_and_inline() {
        let (b, media) = convert("# 标题\n正文 **粗** 和 *斜* 以及 `x<y`\n- 第一\n  - 子项\n1. 步骤\n| A | B |\n|---|---|\n| 1 | 2 |\n[官网](https://a.com)\n<!-- pagebreak -->");
        assert!(b[0].contains("<w:pStyle w:val=\"Heading1\"/>"));
        assert!(b[1].contains("<w:b/>") && b[1].contains("<w:i/>") && b[1].contains("x&lt;y"));
        assert!(b[2].contains("•") && b[3].contains("◦") && b[4].contains("1."));
        assert!(b[5].starts_with("<w:tbl>") && b[5].contains("<w:tblHeader/>"));
        assert_eq!(b[6], "<w:p/>");
        assert!(b[7].contains("w:hyperlink") && media.rels.iter().any(|r| r.2 == "https://a.com" && r.3));
        assert!(b[8].contains("w:type=\"page\""));
    }

    #[test]
    fn code_block_keeps_lines() {
        let (b, _) = convert("```\nfn main() {\n\tprint!(\"*a*\");\n}\n```");
        assert_eq!(b.len(), 3);
        assert!(b[1].contains("*a*") && b[1].contains("Consolas"));
    }
}
