//! Word（.docx）：结构化读取、原位编辑（替换/填模板/插入/删除/改段落/改表格）、从 Markdown 新建。
//! 只改动涉及的 XML 片段，其余内容（样式、页眉页脚、批注、图片）原样保留。

use super::md::{self, Media};
use super::xml::{self, Tok};
use super::Package;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

const DOC: &str = "word/document.xml";
const STYLES: &str = "word/styles.xml";
const RELS: &str = "word/_rels/document.xml.rels";
const TYPES: &str = "[Content_Types].xml";

// ---------- 文本提取 ----------

/// 一段 XML 的可见文字：w:t 内容、w:tab → \t、w:br/w:cr → \n；跳过删除修订
pub fn text_of(s: &str) -> String {
    let toks = xml::tokens(s);
    let mut out = String::new();
    let mut in_t = false;
    for t in &toks {
        match t {
            Tok::Start { name, self_closing, start, end } => match name.as_str() {
                "w:t" if !self_closing => in_t = true,
                "w:tab" => out.push('\t'),
                "w:br" | "w:cr" => {
                    let tag = &s[*start..*end];
                    if xml::attr(tag, "w:type") != Some("page") {
                        out.push('\n');
                    }
                }
                _ => {}
            },
            Tok::End { name, .. } if name == "w:t" => in_t = false,
            Tok::End { name, .. } if name == "w:p" => out.push('\n'),
            Tok::Text { start, end } if in_t => out.push_str(&xml::unescape(&s[*start..*end])),
            _ => {}
        }
    }
    out.trim_end_matches('\n').to_string()
}

/// 某个开始标签（下标 i）到其结束标签之间，深度 1 的指定子元素区间（字节）
fn children(s: &str, toks: &[Tok], i: usize, tag: &str) -> Vec<(usize, usize)> {
    let end = xml::matching_end(toks, i);
    let mut out = Vec::new();
    let mut k = i + 1;
    while k < end {
        match &toks[k] {
            Tok::Start { name, .. } => {
                let e = xml::matching_end(toks, k);
                if name == tag {
                    out.push((xml::tok_start(&toks[k]), xml::tok_end(&toks[e])));
                }
                k = e + 1;
            }
            _ => k += 1,
        }
    }
    let _ = s;
    out
}

fn table_rows(s: &str) -> Vec<Vec<String>> {
    let toks = xml::tokens(s);
    let Some(ti) = toks.iter().position(|t| matches!(t, Tok::Start { name, .. } if name == "w:tbl")) else { return Vec::new() };
    children(s, &toks, ti, "w:tr")
        .into_iter()
        .map(|(a, b)| {
            let row = &s[a..b];
            let rt = xml::tokens(row);
            children(row, &rt, 0, "w:tc").into_iter().map(|(x, y)| text_of(&row[x..y])).collect()
        })
        .collect()
}

// ---------- 样式 ----------

#[derive(Default)]
pub struct Styles {
    /// styleId → 名称
    names: HashMap<String, String>,
    /// 标题级别 → styleId
    headings: HashMap<usize, String>,
}

impl Styles {
    fn parse(xml_s: &str) -> Styles {
        let mut st = Styles::default();
        let toks = xml::tokens(xml_s);
        let mut cur: Option<String> = None;
        for t in &toks {
            if let Tok::Start { name, start, end, .. } = t {
                let tag = &xml_s[*start..*end];
                if name == "w:style" {
                    cur = (xml::attr(tag, "w:type") == Some("paragraph")).then(|| xml::attr(tag, "w:styleId").unwrap_or("").to_string());
                } else if name == "w:name" {
                    if let (Some(id), Some(n)) = (&cur, xml::attr(tag, "w:val")) {
                        st.names.insert(id.clone(), n.to_string());
                        let lower = n.to_lowercase();
                        if let Some(l) = lower.strip_prefix("heading ").and_then(|d| d.trim().parse::<usize>().ok()) {
                            st.headings.entry(l).or_insert_with(|| id.clone());
                        }
                    }
                }
            }
        }
        st
    }

    fn label(&self, id: &str) -> String {
        let name = self.names.get(id).map(String::as_str).unwrap_or(id);
        let lower = name.to_lowercase();
        if let Some(l) = lower.strip_prefix("heading ") {
            return format!("标题{}", l.trim());
        }
        match lower.as_str() {
            "title" => "文档标题".into(),
            "subtitle" => "副标题".into(),
            "normal" | "" => "正文".into(),
            "list paragraph" => "列表段落".into(),
            "caption" => "题注".into(),
            "toc heading" => "目录标题".into(),
            _ => name.to_string(),
        }
    }
}

// ---------- 块 ----------

#[derive(Debug, Clone)]
pub struct Block {
    pub xml: String,
    /// 读取时的编号；新插入的为 None
    pub orig: Option<usize>,
}

pub struct Doc {
    pub pkg: Package,
    head: String,
    tail: String,
    pub blocks: Vec<Block>,
    styles: Styles,
}

impl Doc {
    pub fn open(path: &Path) -> Result<Doc> {
        let pkg = Package::open(path)?;
        let xml_s = pkg.text(DOC).ok_or_else(|| anyhow!("不是有效的 Word 文档（缺少 word/document.xml）"))?;
        let styles = pkg.text(STYLES).map(|s| Styles::parse(&s)).unwrap_or_default();
        let toks = xml::tokens(&xml_s);
        let bi = toks.iter().position(|t| matches!(t, Tok::Start { name, .. } if name == "w:body")).ok_or_else(|| anyhow!("文档缺少正文"))?;
        let be = xml::matching_end(&toks, bi);
        let mut blocks = Vec::new();
        let body_start = xml::tok_end(&toks[bi]);
        let mut tail_start = xml::tok_start(&toks[be]);
        let mut k = bi + 1;
        while k < be {
            match &toks[k] {
                Tok::Start { name, .. } => {
                    let e = xml::matching_end(&toks, k);
                    let (a, b) = (xml::tok_start(&toks[k]), xml::tok_end(&toks[e]));
                    if name == "w:sectPr" {
                        tail_start = a;
                        break;
                    }
                    let n = blocks.len();
                    blocks.push(Block { xml: xml_s[a..b].to_string(), orig: Some(n) });
                    k = e + 1;
                }
                _ => k += 1,
            }
        }
        Ok(Doc { head: xml_s[..body_start].to_string(), tail: xml_s[tail_start..].to_string(), blocks, styles, pkg })
    }

    fn kind(&self, b: &Block) -> (String, String) {
        let x = &b.xml;
        if x.starts_with("<w:tbl") {
            let rows = table_rows(x);
            let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
            return (format!("表格 {}行×{}列", rows.len(), cols), String::new());
        }
        if x.starts_with("<w:p") && (x.as_bytes().get(4) == Some(&b'>') || x.as_bytes().get(4) == Some(&b' ') || x.as_bytes().get(4) == Some(&b'/')) {
            let style = xml::tokens(x)
                .iter()
                .find_map(|t| match t {
                    Tok::Start { name, start, end, .. } if name == "w:pStyle" => xml::attr(&x[*start..*end], "w:val").map(str::to_string),
                    _ => None,
                })
                .unwrap_or_default();
            let mut label = self.styles.label(&style);
            if x.contains("<w:numPr>") && label == "正文" {
                label = "列表".into();
            }
            if x.contains("<w:drawing") || x.contains("<w:pict") {
                label.push_str("+图片");
            }
            return (label, text_of(x));
        }
        if x.starts_with("<w:sdt") {
            return ("内容控件".into(), text_of(x));
        }
        let tag: String = x.trim_start_matches('<').chars().take_while(|c| !c.is_whitespace() && *c != '>' && *c != '/').collect();
        (tag, text_of(x))
    }

    /// 结构化文本：[编号] 类型 │ 内容；表格逐行列出
    pub fn outline(&self, offset: usize, max_chars: usize) -> (String, usize, bool) {
        let mut out = String::new();
        let mut shown = 0;
        for b in self.blocks.iter().skip(offset) {
            let i = b.orig.unwrap_or(0);
            let (kind, text) = self.kind(b);
            let mut entry = if kind.starts_with("表格") {
                let mut s = format!("[{i}] {kind}\n");
                for (r, row) in table_rows(&b.xml).iter().enumerate() {
                    let cells: Vec<String> = row.iter().map(|c| c.replace('\n', " / ")).collect();
                    s.push_str(&format!("    r{}: {}\n", r + 1, cells.join(" | ")));
                }
                s
            } else if text.trim().is_empty() && !kind.contains("图片") {
                format!("[{i}] (空段落)\n")
            } else {
                format!("[{i}] {kind} │ {}\n", text.replace('\n', "↵"))
            };
            if out.len() + entry.len() > max_chars {
                if shown == 0 {
                    entry.truncate(entry.char_indices().take_while(|(p, _)| *p < max_chars).last().map(|(p, c)| p + c.len_utf8()).unwrap_or(0));
                    out.push_str(&entry);
                    shown = 1;
                }
                return (out, shown, true);
            }
            out.push_str(&entry);
            shown += 1;
        }
        (out, shown, false)
    }

    pub fn stats(&self) -> Value {
        let tables = self.blocks.iter().filter(|b| b.xml.starts_with("<w:tbl")).count();
        let chars: usize = self.blocks.iter().map(|b| text_of(&b.xml).chars().filter(|c| !c.is_whitespace()).count()).sum();
        let headers = self.pkg.names().filter(|n| n.starts_with("word/header") || n.starts_with("word/footer")).count();
        json!({"blocks": self.blocks.len(), "tables": tables, "characters": chars, "header_footer_parts": headers, "has_comments": self.pkg.has("word/comments.xml")})
    }

    fn pos_of(&self, orig: usize) -> Result<usize> {
        self.blocks.iter().position(|b| b.orig == Some(orig)).ok_or_else(|| anyhow!("编号 {orig} 不存在（或已在本次操作中删除）。编号以 office_read 读到的为准"))
    }

    fn ensure_heading_styles(&mut self) -> HashMap<usize, String> {
        let mut map = self.styles.headings.clone();
        let Some(mut styles) = self.pkg.text(STYLES) else { return map };
        let mut added = false;
        for l in 1..=6usize {
            if map.contains_key(&l) {
                continue;
            }
            let id = format!("SgHeading{l}");
            let size = [32, 28, 26, 24, 24, 24][l - 1];
            let def = format!("<w:style w:type=\"paragraph\" w:styleId=\"{id}\"><w:name w:val=\"heading {l}\"/><w:next w:val=\"{}\"/><w:qFormat/><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"240\" w:after=\"120\"/><w:ind w:firstLine=\"0\" w:firstLineChars=\"0\"/><w:outlineLvl w:val=\"{}\"/></w:pPr><w:rPr><w:b/><w:bCs/><w:sz w:val=\"{size}\"/><w:szCs w:val=\"{size}\"/></w:rPr></w:style>", self.normal_style_id(), l - 1);
            if let Some(p) = styles.rfind("</w:styles>") {
                styles.insert_str(p, &def);
                map.insert(l, id);
                added = true;
            }
        }
        if added {
            self.pkg.set_text(STYLES, &styles);
        }
        map
    }

    fn normal_style_id(&self) -> String {
        self.styles.names.iter().find(|(_, n)| n.eq_ignore_ascii_case("normal")).map(|(id, _)| id.clone()).unwrap_or_else(|| "Normal".into())
    }

    fn next_rel_id(&self) -> u32 {
        let rels = self.pkg.text(RELS).unwrap_or_default();
        let mut max = 0;
        for part in rels.split("Id=\"rIdSg").skip(1) {
            if let Some(n) = part.split('"').next().and_then(|d| d.parse::<u32>().ok()) {
                max = max.max(n);
            }
        }
        max
    }

    /// Markdown → 块，并把图片/链接登记进包
    fn markdown_blocks(&mut self, markdown: &str, base_dir: Option<&Path>) -> Result<Vec<String>> {
        let headings = self.ensure_heading_styles();
        let mut media = Media { next_id: self.next_rel_id(), ..Default::default() };
        let hs = move |l: usize| headings.get(&l).cloned();
        let blocks = {
            let mut ctx = md::Ctx { heading_style: &hs, base_dir: base_dir.map(Path::to_path_buf), media: &mut media, body_ppr: String::new() };
            md::to_blocks(markdown, &mut ctx)?
        };
        register_media(&mut self.pkg, &media)?;
        // 补命名空间（极少数文档没有声明 wp / r）
        for (prefix, uri) in [
            ("xmlns:wp", "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"),
            ("xmlns:r", "http://schemas.openxmlformats.org/officeDocument/2006/relationships"),
        ] {
            if !self.head.contains(&format!("{prefix}=")) {
                if let Some(p) = self.head.find("<w:document") {
                    let insert_at = p + "<w:document".len();
                    self.head.insert_str(insert_at, &format!(" {prefix}=\"{uri}\""));
                }
            }
        }
        Ok(blocks)
    }

    pub fn save(&mut self, path: &Path) -> Result<()> {
        let mut body = String::with_capacity(self.head.len() + self.tail.len() + self.blocks.iter().map(|b| b.xml.len()).sum::<usize>());
        body.push_str(&self.head);
        for b in &self.blocks {
            body.push_str(&b.xml);
        }
        body.push_str(&self.tail);
        self.pkg.set_text(DOC, &body);
        self.pkg.save(path)
    }
}

fn register_media(pkg: &mut Package, media: &Media) -> Result<()> {
    if media.rels.is_empty() {
        return Ok(());
    }
    for (name, bytes) in &media.files {
        pkg.set(name, bytes.clone());
    }
    let mut rels = pkg.text(RELS).unwrap_or_else(|| "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".into());
    let mut add = String::new();
    for (rid, ty, target, external) in &media.rels {
        add.push_str(&format!(
            "<Relationship Id=\"{rid}\" Type=\"{ty}\" Target=\"{}\"{}/>",
            xml::escape(target),
            if *external { " TargetMode=\"External\"" } else { "" }
        ));
    }
    let p = rels.rfind("</Relationships>").ok_or_else(|| anyhow!("关系文件损坏"))?;
    rels.insert_str(p, &add);
    pkg.set_text(RELS, &rels);
    let mut types = pkg.text(TYPES).ok_or_else(|| anyhow!("缺少 [Content_Types].xml"))?;
    for ext in &media.extensions {
        let lower = types.to_lowercase();
        if !lower.contains(&format!("extension=\"{ext}\"")) {
            let p = types.rfind("</Types>").ok_or_else(|| anyhow!("[Content_Types].xml 损坏"))?;
            types.insert_str(p, &format!("<Default Extension=\"{ext}\" ContentType=\"image/{ext}\"/>"));
        }
    }
    pkg.set_text(TYPES, &types);
    Ok(())
}

// ---------- 跨 run 的文字替换 ----------

struct Seg {
    /// 文本内容的字节区间
    start: usize,
    end: usize,
    /// <w:t ...> 开始标签区间
    tag: (usize, usize),
    text: Vec<char>,
}

/// 每个段落里的 w:t 片段（按最内层段落分组，文本框里的段落单独成组）
fn paragraph_segments(s: &str) -> Vec<Vec<Seg>> {
    let toks = xml::tokens(s);
    let mut groups: Vec<Vec<Seg>> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut open_t: Option<(usize, usize)> = None;
    for (k, t) in toks.iter().enumerate() {
        match t {
            Tok::Start { name, self_closing: false, start, end } if name == "w:p" => {
                let _ = (start, end);
                groups.push(Vec::new());
                stack.push(groups.len() - 1);
            }
            Tok::End { name, .. } if name == "w:p" => {
                stack.pop();
            }
            Tok::Start { name, self_closing: false, start, end } if name == "w:t" => open_t = Some((*start, *end)),
            Tok::End { name, start, .. } if name == "w:t" => {
                if let (Some(tag), Some(&g)) = (open_t.take(), stack.last()) {
                    // w:t 为空时没有 Text 记号，这里统一按区间取
                    let content_start = tag.1;
                    let content_end = *start;
                    let _ = k;
                    groups[g].push(Seg { start: content_start, end: content_end, tag, text: xml::unescape(&s[content_start..content_end]).chars().collect() });
                }
            }
            _ => {}
        }
    }
    groups
}

fn eq_char(a: char, b: char, case: bool) -> bool {
    a == b || (!case && a.to_lowercase().eq(b.to_lowercase()))
}

/// 在一段 XML 的每个段落里替换文字（可跨越格式不同的 run），保留第一个 run 的格式。返回 (新 XML, 次数)
pub fn replace_text(s: &str, find: &str, repl: &str, all: bool, match_case: bool, limit_left: &mut usize) -> (String, usize) {
    let pat: Vec<char> = find.chars().collect();
    if pat.is_empty() {
        return (s.to_string(), 0);
    }
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut count = 0;
    for mut segs in paragraph_segments(s) {
        if *limit_left == 0 {
            break;
        }
        // 平铺：(片段, 片段内字符下标)
        let flat: Vec<(usize, usize, char)> = segs.iter().enumerate().flat_map(|(si, sg)| sg.text.iter().enumerate().map(move |(ci, &c)| (si, ci, c))).collect();
        let mut matches = Vec::new();
        let mut i = 0;
        while i + pat.len() <= flat.len() {
            if (0..pat.len()).all(|k| eq_char(flat[i + k].2, pat[k], match_case)) {
                matches.push(i);
                i += pat.len();
                if !all || matches.len() >= *limit_left {
                    break;
                }
            } else {
                i += 1;
            }
        }
        if matches.is_empty() {
            continue;
        }
        *limit_left -= matches.len().min(*limit_left);
        count += matches.len();
        let mut deleted: Vec<Vec<bool>> = segs.iter().map(|sg| vec![false; sg.text.len()]).collect();
        let mut inserts: Vec<(usize, usize)> = Vec::new();
        for &m in &matches {
            for k in 0..pat.len() {
                let (si, ci, _) = flat[m + k];
                deleted[si][ci] = true;
            }
            inserts.push((flat[m].0, flat[m].1));
        }
        let mut touched = vec![false; segs.len()];
        for (si, sg) in segs.iter_mut().enumerate() {
            let mut new_text = String::new();
            for (ci, &c) in sg.text.iter().enumerate() {
                if inserts.contains(&(si, ci)) {
                    new_text.push_str(repl);
                    touched[si] = true;
                }
                if deleted[si][ci] {
                    touched[si] = true;
                } else {
                    new_text.push(c);
                }
            }
            if touched[si] {
                edits.push((sg.start, sg.end, xml::escape(&new_text)));
                let tag = &s[sg.tag.0..sg.tag.1];
                if !tag.contains("xml:space") {
                    edits.push((sg.tag.0, sg.tag.1, "<w:t xml:space=\"preserve\">".to_string()));
                }
            }
        }
    }
    if edits.is_empty() {
        return (s.to_string(), 0);
    }
    (xml::splice(s, edits), count)
}

/// 段落开头的 <w:p ...><w:pPr>…</w:pPr> 与第一个 run 的 <w:rPr>…</w:rPr>
fn para_props(p: &str) -> (String, String, String) {
    let toks = xml::tokens(p);
    let open = toks.first().map(|t| p[..xml::tok_end(t)].to_string()).unwrap_or_else(|| "<w:p>".into());
    let open = if open.ends_with("/>") { format!("{}>", open.trim_end_matches("/>")) } else { open };
    let mut ppr = String::new();
    let mut rpr = String::new();
    for (k, t) in toks.iter().enumerate() {
        if let Tok::Start { name, .. } = t {
            if name == "w:pPr" && ppr.is_empty() {
                let e = xml::matching_end(&toks, k);
                ppr = p[xml::tok_start(t)..xml::tok_end(&toks[e])].to_string();
            }
            if name == "w:r" {
                if let Some(rk) = toks[k..].iter().position(|x| matches!(x, Tok::Start { name, .. } if name == "w:rPr")) {
                    let rk = k + rk;
                    let rend = xml::matching_end(&toks, k);
                    if rk < rend {
                        let e = xml::matching_end(&toks, rk);
                        rpr = p[xml::tok_start(&toks[rk])..xml::tok_end(&toks[e])].to_string();
                    }
                }
                break;
            }
        }
    }
    (open, ppr, rpr)
}

fn rebuild_paragraph(p: &str, text: &str) -> String {
    let (open, ppr, rpr) = para_props(p);
    let inner = rpr.strip_prefix("<w:rPr>").and_then(|r| r.strip_suffix("</w:rPr>")).unwrap_or("");
    let mut media = Media::default();
    let runs = if text.contains("**") || text.contains('`') {
        md::inline(text, inner, &mut media)
    } else {
        let mut out = String::new();
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                out.push_str(&format!("<w:r>{rpr}<w:br/></w:r>"));
            }
            if !line.is_empty() {
                out.push_str(&format!("<w:r>{rpr}<w:t xml:space=\"preserve\">{}</w:t></w:r>", xml::escape(line)));
            }
        }
        out
    };
    format!("{open}{ppr}{runs}</w:p>")
}

/// 表格第 row 行第 col 列（从 1 开始）的单元格区间（相对表格 XML）
fn cell_range(tbl: &str, row: usize, col: usize) -> Result<(usize, usize)> {
    let toks = xml::tokens(tbl);
    let rows = children(tbl, &toks, 0, "w:tr");
    let (ra, rb) = *rows.get(row.wrapping_sub(1)).ok_or_else(|| anyhow!("表格只有 {} 行", rows.len()))?;
    let row_xml = &tbl[ra..rb];
    let rt = xml::tokens(row_xml);
    let cells = children(row_xml, &rt, 0, "w:tc");
    let (ca, cb) = *cells.get(col.wrapping_sub(1)).ok_or_else(|| anyhow!("第 {row} 行只有 {} 列", cells.len()))?;
    Ok((ra + ca, ra + cb))
}

fn set_cell_text(cell: &str, text: &str) -> String {
    let toks = xml::tokens(cell);
    let paras = children(cell, &toks, 0, "w:p");
    let Some(&(pa, _)) = paras.first() else { return cell.to_string() };
    let pb = paras.last().unwrap().1;
    let first = &cell[pa..paras[0].1];
    let new_paras: String = text.split('\n').map(|line| rebuild_paragraph(first, line)).collect();
    format!("{}{}{}", &cell[..pa], new_paras, &cell[pb..])
}

fn op_index(op: &Value, key: &str) -> Result<usize> {
    op.get(key).and_then(Value::as_u64).map(|v| v as usize).ok_or_else(|| anyhow!("缺少 {key}（office_read 输出里的 [编号]）"))
}

fn header_footer_parts(pkg: &Package) -> Vec<String> {
    pkg.names().filter(|n| (n.starts_with("word/header") || n.starts_with("word/footer") || n == &"word/footnotes.xml") && n.ends_with(".xml")).map(str::to_string).collect()
}

/// 依次执行编辑操作。编号一律指 office_read 读到的原始编号（同一批操作里不会因插入/删除而移位）。
pub fn apply(doc: &mut Doc, ops: &[Value], base_dir: Option<&Path>) -> Result<Vec<Value>> {
    let mut results = Vec::new();
    for (n, op) in ops.iter().enumerate() {
        let kind = op.get("op").and_then(Value::as_str).ok_or_else(|| anyhow!("第 {} 个操作缺少 op", n + 1))?;
        let r = apply_one(doc, kind, op, base_dir).map_err(|e| anyhow!("第 {} 个操作（{kind}）失败：{e:#}", n + 1))?;
        results.push(r);
    }
    Ok(results)
}

fn apply_one(doc: &mut Doc, kind: &str, op: &Value, base_dir: Option<&Path>) -> Result<Value> {
    match kind {
        "replace" | "fill" => {
            let pairs: Vec<(String, String)> = if kind == "replace" {
                let find = op.get("find").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("replace 需要 find"))?;
                let repl = op.get("replace").and_then(Value::as_str).unwrap_or("");
                vec![(find.to_string(), repl.to_string())]
            } else {
                let values = op.get("values").and_then(Value::as_object).ok_or_else(|| anyhow!("fill 需要 values：{{\"占位名\": \"内容\"}}，替换文档里的 {{{{占位名}}}}"))?;
                values
                    .iter()
                    .flat_map(|(k, v)| {
                        let text = match v {
                            Value::String(s) => s.clone(),
                            Value::Null => String::new(),
                            other => other.to_string(),
                        };
                        [(format!("{{{{{k}}}}}"), text.clone()), (format!("{{{{ {k} }}}}"), text)]
                    })
                    .collect()
            };
            let all = op.get("all").and_then(Value::as_bool).unwrap_or(true);
            let case = op.get("match_case").and_then(Value::as_bool).unwrap_or(true);
            let mut total = 0;
            for (find, repl) in &pairs {
                let mut left = if all { usize::MAX } else { 1 };
                for b in doc.blocks.iter_mut() {
                    let (x, c) = replace_text(&b.xml, find, repl, all, case, &mut left);
                    if c > 0 {
                        b.xml = x;
                        total += c;
                    }
                }
                for part in header_footer_parts(&doc.pkg) {
                    if left == 0 {
                        break;
                    }
                    if let Some(s) = doc.pkg.text(&part) {
                        let (x, c) = replace_text(&s, find, repl, all, case, &mut left);
                        if c > 0 {
                            doc.pkg.set_text(&part, &x);
                            total += c;
                        }
                    }
                }
            }
            let mut v = json!({"op": kind, "replaced": total});
            if kind == "fill" {
                let text: String = doc.blocks.iter().map(|b| text_of(&b.xml)).collect::<Vec<_>>().join("\n");
                let re = regex::Regex::new(r"\{\{\s*([^{}]{1,40}?)\s*\}\}").unwrap();
                let left: Vec<String> = re.captures_iter(&text).map(|c| c[1].to_string()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                if !left.is_empty() {
                    v["unfilled"] = json!(left);
                }
            } else if total == 0 {
                bail!("没有找到「{}」。注意文字要与文档完全一致（可先 office_read 确认）", pairs[0].0);
            }
            Ok(v)
        }
        "insert" => {
            let markdown = op.get("markdown").and_then(Value::as_str).ok_or_else(|| anyhow!("insert 需要 markdown"))?;
            let new_blocks = doc.markdown_blocks(markdown, base_dir)?;
            let pos = if let Some(a) = op.get("after").and_then(Value::as_u64) {
                doc.pos_of(a as usize)? + 1
            } else if let Some(b) = op.get("before").and_then(Value::as_u64) {
                doc.pos_of(b as usize)?
            } else if op.get("at").and_then(Value::as_str) == Some("start") {
                0
            } else {
                doc.blocks.len()
            };
            let n = new_blocks.len();
            for (k, x) in new_blocks.into_iter().enumerate() {
                doc.blocks.insert(pos + k, Block { xml: x, orig: None });
            }
            Ok(json!({"op": "insert", "blocks": n}))
        }
        "delete" => {
            let i = op_index(op, "index")?;
            let count = op.get("count").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
            let mut removed = 0;
            for k in i..i + count {
                let p = doc.pos_of(k)?;
                doc.blocks.remove(p);
                removed += 1;
            }
            Ok(json!({"op": "delete", "removed": removed}))
        }
        "set_text" => {
            let i = op_index(op, "index")?;
            let text = op.get("text").and_then(Value::as_str).ok_or_else(|| anyhow!("set_text 需要 text"))?;
            let p = doc.pos_of(i)?;
            let b = &mut doc.blocks[p];
            if !b.xml.starts_with("<w:p") || b.xml.starts_with("<w:pict") {
                bail!("编号 {i} 不是段落；改表格用 table_cell");
            }
            b.xml = rebuild_paragraph(&b.xml, text);
            Ok(json!({"op": "set_text", "index": i}))
        }
        "table_cell" => {
            let i = op_index(op, "index")?;
            let row = op.get("row").and_then(Value::as_u64).ok_or_else(|| anyhow!("table_cell 需要 row（从 1 开始）"))? as usize;
            let col = op.get("col").and_then(Value::as_u64).ok_or_else(|| anyhow!("table_cell 需要 col（从 1 开始）"))? as usize;
            let text = op.get("text").and_then(Value::as_str).unwrap_or("");
            let p = doc.pos_of(i)?;
            let tbl = doc.blocks[p].xml.clone();
            if !tbl.starts_with("<w:tbl") {
                bail!("编号 {i} 不是表格");
            }
            let (a, b) = cell_range(&tbl, row, col)?;
            let new_cell = set_cell_text(&tbl[a..b], text);
            doc.blocks[p].xml = format!("{}{}{}", &tbl[..a], new_cell, &tbl[b..]);
            Ok(json!({"op": "table_cell", "index": i, "row": row, "col": col}))
        }
        "table_add_row" => {
            let i = op_index(op, "index")?;
            let values: Vec<String> = op
                .get("values")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("table_add_row 需要 values 数组"))?
                .iter()
                .map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| if v.is_null() { String::new() } else { v.to_string() }))
                .collect();
            let p = doc.pos_of(i)?;
            let tbl = doc.blocks[p].xml.clone();
            if !tbl.starts_with("<w:tbl") {
                bail!("编号 {i} 不是表格");
            }
            let toks = xml::tokens(&tbl);
            let rows = children(&tbl, &toks, 0, "w:tr");
            if rows.is_empty() {
                bail!("表格没有行可作为模板");
            }
            // 以指定行（默认最后一行）为模板复制格式
            let after = op.get("after_row").and_then(Value::as_u64).map(|r| r as usize).unwrap_or(rows.len()).clamp(1, rows.len());
            let template = &tbl[rows[after - 1].0..rows[after - 1].1];
            let mut new_row = template.replace("<w:tblHeader/>", "");
            let rt = xml::tokens(&new_row);
            let cells = children(&new_row, &rt, 0, "w:tc");
            let mut edits = Vec::new();
            for (k, (ca, cb)) in cells.iter().enumerate() {
                edits.push((*ca, *cb, set_cell_text(&new_row[*ca..*cb], values.get(k).map(String::as_str).unwrap_or(""))));
            }
            new_row = xml::splice(&new_row, edits);
            let at = rows[after - 1].1;
            doc.blocks[p].xml = format!("{}{}{}", &tbl[..at], new_row, &tbl[at..]);
            Ok(json!({"op": "table_add_row", "index": i, "after_row": after}))
        }
        other => bail!("未知操作 {other}（replace / fill / insert / delete / set_text / table_cell / table_add_row）"),
    }
}

// ---------- 新建 ----------

pub struct CreateOpts {
    pub title: Option<String>,
    pub font: String,
    pub heading_font: String,
    pub font_size: f64,
    pub line_spacing: f64,
    pub first_line_indent: bool,
    pub landscape: bool,
}

pub fn create(path: &Path, markdown: &str, o: &CreateOpts) -> Result<usize> {
    let mut media = Media::default();
    let hs = |l: usize| Some(format!("Heading{l}"));
    let body_ppr = if o.first_line_indent { "<w:ind w:firstLineChars=\"200\"/>".to_string() } else { String::new() };
    let mut blocks = {
        let mut ctx = md::Ctx { heading_style: &hs, base_dir: path.parent().map(Path::to_path_buf), media: &mut media, body_ppr };
        md::to_blocks(markdown, &mut ctx)?
    };
    if let Some(t) = o.title.as_deref().filter(|t| !t.trim().is_empty()) {
        blocks.insert(0, format!("<w:p><w:pPr><w:pStyle w:val=\"Title\"/></w:pPr>{}</w:p>", md::inline(t.trim(), "", &mut Media::default())));
    }
    let n = blocks.len();
    let (w, h) = if o.landscape { (16838, 11906) } else { (11906, 16838) };
    let orient = if o.landscape { " w:orient=\"landscape\"" } else { "" };
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><w:body>{}<w:sectPr><w:pgSz w:w=\"{w}\" w:h=\"{h}\"{orient}/><w:pgMar w:top=\"1440\" w:right=\"1800\" w:bottom=\"1440\" w:left=\"1800\" w:header=\"851\" w:footer=\"992\" w:gutter=\"0\"/><w:cols w:space=\"425\"/><w:docGrid w:type=\"lines\" w:linePitch=\"312\"/></w:sectPr></w:body></w:document>",
        blocks.concat()
    );
    let sz = (o.font_size * 2.0).round() as u32;
    let line = (o.line_spacing * 240.0).round() as u32;
    let f = xml::escape(&o.font);
    let hf = xml::escape(&o.heading_font);
    let mut heading_styles = String::new();
    for l in 1..=6usize {
        let hsz = [32, 28, 26, 24, 24, 24][l - 1].max(sz);
        heading_styles.push_str(&format!(
            "<w:style w:type=\"paragraph\" w:styleId=\"Heading{l}\"><w:name w:val=\"heading {l}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:uiPriority w:val=\"9\"/><w:qFormat/><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"{}\" w:after=\"120\"/><w:ind w:firstLine=\"0\" w:firstLineChars=\"0\"/><w:outlineLvl w:val=\"{}\"/></w:pPr><w:rPr><w:rFonts w:eastAsia=\"{hf}\"/><w:b/><w:bCs/><w:sz w:val=\"{hsz}\"/><w:szCs w:val=\"{hsz}\"/></w:rPr></w:style>",
            if l == 1 { 360 } else { 240 },
            l - 1
        ));
    }
    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Times New Roman\" w:hAnsi=\"Times New Roman\" w:eastAsia=\"{f}\" w:cs=\"Times New Roman\"/><w:sz w:val=\"{sz}\"/><w:szCs w:val=\"{sz}\"/><w:lang w:val=\"en-US\" w:eastAsia=\"zh-CN\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after=\"120\" w:line=\"{line}\" w:lineRule=\"auto\"/><w:jc w:val=\"both\"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style><w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:spacing w:before=\"240\" w:after=\"360\"/><w:ind w:firstLine=\"0\" w:firstLineChars=\"0\"/><w:jc w:val=\"center\"/><w:outlineLvl w:val=\"0\"/></w:pPr><w:rPr><w:rFonts w:eastAsia=\"{hf}\"/><w:b/><w:bCs/><w:sz w:val=\"44\"/><w:szCs w:val=\"44\"/></w:rPr></w:style>{heading_styles}<w:style w:type=\"character\" w:default=\"1\" w:styleId=\"DefaultParagraphFont\"><w:name w:val=\"Default Paragraph Font\"/><w:uiPriority w:val=\"1\"/><w:semiHidden/></w:style><w:style w:type=\"table\" w:default=\"1\" w:styleId=\"TableNormal\"><w:name w:val=\"Normal Table\"/><w:semiHidden/><w:tblPr><w:tblInd w:w=\"0\" w:type=\"dxa\"/><w:tblCellMar><w:top w:w=\"0\" w:type=\"dxa\"/><w:left w:w=\"108\" w:type=\"dxa\"/><w:bottom w:w=\"0\" w:type=\"dxa\"/><w:right w:w=\"108\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr></w:style></w:styles>"
    );
    let title = xml::escape(o.title.as_deref().unwrap_or(""));
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let core = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><dc:title>{title}</dc:title><dc:creator>拾光</dc:creator><dcterms:created xsi:type=\"dcterms:W3CDTF\">{now}</dcterms:created><dcterms:modified xsi:type=\"dcterms:W3CDTF\">{now}</dcterms:modified></cp:coreProperties>"
    );
    let mut pkg = Package::default();
    pkg.set_text(TYPES, "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/><Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/><Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/></Types>");
    pkg.set_text("_rels/.rels", "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/></Relationships>");
    pkg.set_text("docProps/core.xml", &core);
    pkg.set_text(DOC, &document);
    pkg.set_text(STYLES, &styles);
    pkg.set_text(RELS, "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>");
    register_media(&mut pkg, &media)?;
    pkg.save(path)?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_across_runs_keeps_first_run_format() {
        let p = r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>{{na</w:t></w:r><w:r><w:t>me}} 你好 {{name}}</w:t></w:r></w:p>"#;
        let mut left = usize::MAX;
        let (out, n) = replace_text(p, "{{name}}", "张&三", true, true, &mut left);
        assert_eq!(n, 2);
        assert_eq!(text_of(&out), "张&三 你好 张&三");
        assert!(out.contains(r#"<w:rPr><w:b/></w:rPr><w:t xml:space="preserve">张&amp;三</w:t>"#));
    }

    #[test]
    fn replace_case_insensitive_and_first_only() {
        let p = r#"<w:p><w:r><w:t>Foo foo FOO</w:t></w:r></w:p>"#;
        let mut left = 1;
        let (out, n) = replace_text(p, "foo", "bar", false, false, &mut left);
        assert_eq!(n, 1);
        assert_eq!(text_of(&out), "bar foo FOO");
    }

    #[test]
    fn table_rows_and_cell_edit() {
        let tbl = r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:rPr><w:i/></w:rPr><w:t>1</w:t></w:r></w:p></w:tc><w:tc><w:p/></w:tc></w:tr></w:tbl>"#;
        assert_eq!(table_rows(tbl), vec![vec!["A", "B"], vec!["1", ""]]);
        let (a, b) = cell_range(tbl, 2, 1).unwrap();
        let cell = set_cell_text(&tbl[a..b], "九");
        assert!(cell.contains("<w:i/>") && text_of(&cell) == "九");
        let (a, b) = cell_range(tbl, 2, 2).unwrap();
        assert_eq!(text_of(&set_cell_text(&tbl[a..b], "x")), "x");
    }

    #[test]
    fn create_then_open_edit_roundtrip() {
        let dir = std::env::temp_dir().join(format!("sg-docx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.docx");
        let opts = CreateOpts { title: Some("报告".into()), font: "宋体".into(), heading_font: "黑体".into(), font_size: 12.0, line_spacing: 1.5, first_line_indent: true, landscape: false };
        create(&path, "# 一、背景\n尊敬的 {{客户}}：\n| 项 | 值 |\n|---|---|\n| a | 1 |", &opts).unwrap();
        let mut doc = Doc::open(&path).unwrap();
        let (outline, _, _) = doc.outline(0, 10000);
        assert!(outline.contains("[0] 文档标题 │ 报告"), "{outline}");
        assert!(outline.contains("[1] 标题1 │ 一、背景"), "{outline}");
        assert!(outline.contains("r2: a | 1"), "{outline}");
        let ops = serde_json::json!([
            {"op": "fill", "values": {"客户": "王总"}},
            {"op": "insert", "after": 1, "markdown": "## 1.1 新小节\n内容"},
            {"op": "table_add_row", "index": 3, "values": ["b", "2"]},
            {"op": "table_cell", "index": 3, "row": 2, "col": 2, "text": "100"},
            {"op": "delete", "index": 0}
        ]);
        let res = apply(&mut doc, ops.as_array().unwrap(), None).unwrap();
        assert_eq!(res[0]["replaced"], 1);
        doc.save(&path).unwrap();
        let doc = Doc::open(&path).unwrap();
        let (outline, _, _) = doc.outline(0, 10000);
        assert!(!outline.contains("报告"), "{outline}");
        assert!(outline.contains("尊敬的 王总："), "{outline}");
        assert!(outline.contains("标题2 │ 1.1 新小节"), "{outline}");
        assert!(outline.contains("r2: a | 100") && outline.contains("r3: b | 2"), "{outline}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
