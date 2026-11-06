//! Excel（.xlsx/.xlsm）：读取工作表为带行列号的表格；写值/公式、追加行、插删行列、增删改工作表、样式、列宽、合并、冻结。
//! 用 umya-spreadsheet 读改写，保留原有格式；公式由 Excel/WPS 打开时重新计算。

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::Path;
use umya_spreadsheet::{self as umya, Spreadsheet, Worksheet};

pub fn col_name(mut n: u32) -> String {
    let mut s = String::new();
    while n > 0 {
        let r = (n - 1) % 26;
        s.insert(0, (b'A' + r as u8) as char);
        n = (n - 1) / 26;
    }
    s
}

pub fn col_index(s: &str) -> Option<u32> {
    let s = s.trim().to_uppercase();
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    Some(s.chars().fold(0, |acc, c| acc * 26 + (c as u32 - 'A' as u32 + 1)))
}

/// "B12" → (列 2, 行 12)
pub fn parse_cell(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().replace('$', "");
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let col = col_index(&s[..split])?;
    let row: u32 = s[split..].parse().ok()?;
    (row >= 1).then_some((col, row))
}

/// "A1:C10" / "A1" / "B:D"（整列） / "2:5"（整行）→ (c1, r1, c2, r2)，整列整行用工作表已用范围补齐
pub fn parse_range(s: &str, max: (u32, u32)) -> Result<(u32, u32, u32, u32)> {
    let s = s.trim();
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    if let (Some(c1), Some(c2)) = (col_index(a), col_index(b)) {
        return Ok((c1.min(c2), 1, c1.max(c2), max.1.max(1)));
    }
    if let (Ok(r1), Ok(r2)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
        return Ok((1, r1.min(r2), max.0.max(1), r1.max(r2)));
    }
    let (c1, r1) = parse_cell(a).ok_or_else(|| anyhow!("区域「{s}」格式不对，应如 A1:C10、B2、B:D、2:5"))?;
    let (c2, r2) = parse_cell(b).ok_or_else(|| anyhow!("区域「{s}」格式不对，应如 A1:C10、B2、B:D、2:5"))?;
    Ok((c1.min(c2), r1.min(r2), c1.max(c2), r1.max(r2)))
}

pub fn open(path: &Path) -> Result<Spreadsheet> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if ext == "xls" || ext == "et" {
        bail!("旧版 .xls 不能直接编辑：先用 office_convert 转成 xlsx");
    }
    umya::reader::xlsx::read(path).map_err(|e| anyhow!("读取 Excel 失败：{e:?}（文件可能已加密、损坏，或正在被独占打开）"))
}

pub fn save(book: &Spreadsheet, path: &Path) -> Result<()> {
    let tmp = path.with_extension(format!("sgtmp.{}", path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "xlsx".into())));
    umya::writer::xlsx::write(book, &tmp).map_err(|e| anyhow!("写入 Excel 失败：{e:?}"))?;
    ensure_normal_style(&tmp)?;
    std::fs::rename(&tmp, path).or_else(|_| {
        std::fs::copy(&tmp, path)?;
        std::fs::remove_file(&tmp)
    })
    .with_context(|| format!("保存失败（文件可能正在 Excel 里打开）：{}", path.display()))
}

/// umya 新建的工作簿没有「常规」单元格样式（cellStyleXfs/cellStyles），屏幕显示正常，
/// 但 Excel/WPS 打印和导出 PDF 时会按错误的默认字体排版（整表缩小、数字变成 #）。缺了就补上。
fn ensure_normal_style(path: &Path) -> Result<()> {
    const STYLES: &str = "xl/styles.xml";
    let mut pkg = super::Package::open(path)?;
    let Some(mut st) = pkg.text(STYLES) else { return Ok(()) };
    if st.contains("<cellStyleXfs") {
        return Ok(());
    }
    let Some(p) = st.find("<cellXfs") else { return Ok(()) };
    st.insert_str(p, "<cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>");
    if !st.contains("<cellStyles") {
        if let Some(e) = st.find("</cellXfs>") {
            st.insert_str(e + "</cellXfs>".len(), "<cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles>");
        }
    }
    pkg.set_text(STYLES, &st);
    pkg.save(path)
}

fn sheet_ref<'a>(book: &'a Spreadsheet, name: Option<&str>) -> Result<&'a Worksheet> {
    match name {
        Some(n) => book.get_sheet_by_name(n).ok_or_else(|| anyhow!("没有名为「{n}」的工作表，现有：{}", sheet_names(book).join("、"))),
        None => book.get_sheet(&0).ok_or_else(|| anyhow!("工作簿没有工作表")),
    }
}

fn sheet_mut<'a>(book: &'a mut Spreadsheet, name: Option<&str>) -> Result<&'a mut Worksheet> {
    let names = sheet_names(book);
    match name {
        Some(n) => book.get_sheet_by_name_mut(n).ok_or_else(|| anyhow!("没有名为「{n}」的工作表，现有：{}", names.join("、"))),
        None => book.get_sheet_mut(&0).ok_or_else(|| anyhow!("工作簿没有工作表")),
    }
}

pub fn sheet_names(book: &Spreadsheet) -> Vec<String> {
    book.get_sheet_collection_no_check().iter().map(|s| s.get_name().to_string()).collect()
}

fn clip(s: &str, max: usize) -> String {
    let flat = s.replace(['\n', '\r'], "↵").replace('|', "¦");
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat
    }
}

/// 读取：工作表概览 + 指定（默认第一个）工作表的区域
pub fn read(path: &Path, sheet: Option<&str>, range: Option<&str>, max_chars: usize) -> Result<Value> {
    let mut book = open(path)?;
    book.read_sheet_collection();
    let sheets: Vec<Value> = book
        .get_sheet_collection()
        .iter()
        .map(|s| {
            let (c, r) = s.get_highest_column_and_row();
            json!({"name": s.get_name(), "used": if c == 0 { "空".to_string() } else { format!("A1:{}{}", col_name(c), r) }})
        })
        .collect();
    let ws = sheet_ref(&book, sheet)?;
    let (mc, mr) = ws.get_highest_column_and_row();
    let (c1, r1, c2, r2) = match range {
        Some(r) => parse_range(r, (mc, mr))?,
        None => (1, 1, mc.max(1), mr.max(1)),
    };
    let mut out = String::new();
    let header: Vec<String> = (c1..=c2).map(col_name).collect();
    out.push_str(&format!("    | {}\n", header.join(" | ")));
    let mut last_row = r1;
    let mut truncated = false;
    let mut formulas = 0;
    for r in r1..=r2 {
        let mut cells = Vec::new();
        let mut any = false;
        for c in c1..=c2 {
            let text = match ws.get_cell((c, r)) {
                Some(cell) => {
                    // 只有数字按单元格格式显示（日期、百分比等）；文本原样（"001" 不能变成 1）
                    let v = if cell.get_data_type() == "n" { cell.get_formatted_value() } else { cell.get_value().to_string() };
                    if cell.is_formula() {
                        formulas += 1;
                        any = true;
                        if v.is_empty() {
                            format!("={}", cell.get_formula())
                        } else {
                            format!("={} → {}", cell.get_formula(), clip(&v, 40))
                        }
                    } else {
                        if !v.is_empty() {
                            any = true;
                        }
                        clip(&v, 60)
                    }
                }
                None => String::new(),
            };
            cells.push(text);
        }
        let line = format!("{r:>4}| {}\n", cells.join(" | "));
        if out.len() + line.len() > max_chars {
            truncated = true;
            break;
        }
        // 连续空行只留一行占位
        if any || !out.ends_with("…\n") {
            out.push_str(if any { &line } else { "    …\n" });
        }
        last_row = r;
    }
    let merged: Vec<String> = ws.get_merge_cells().iter().map(|m| m.get_range()).take(30).collect();
    let mut v = json!({
        "sheets": sheets,
        "sheet": ws.get_name(),
        "range": format!("{}{}:{}{}", col_name(c1), r1, col_name(c2), last_row),
        "table": out,
    });
    if !merged.is_empty() {
        v["merged"] = json!(merged);
    }
    if truncated {
        v["truncated"] = json!(true);
        v["next"] = json!(format!("{}{}:{}{}", col_name(c1), last_row + 1, col_name(c2), r2));
    }
    if formulas > 0 {
        v["formulas"] = json!(formulas);
        v["note"] = json!("「=公式 → 值」中的值是上次保存时的计算结果；拾光刚写入、还没在 Excel 里打开过的公式没有结果");
    }
    Ok(v)
}

fn set_value(ws: &mut Worksheet, col: u32, row: u32, v: &Value) {
    let cell = ws.get_cell_mut((col, row));
    match v {
        Value::Null => {
            cell.set_blank();
        }
        Value::Bool(b) => {
            cell.set_value_bool(*b);
        }
        Value::Number(n) => {
            cell.set_value_number(n.as_f64().unwrap_or(0.0));
        }
        Value::String(s) if s.starts_with('=') && s.len() > 1 => {
            cell.set_formula(&s[1..]);
        }
        Value::String(s) => {
            // 文本形式的数字（如 "001"、身份证号）保持文本，不自动转数字
            cell.set_value_string(s.as_str());
        }
        other => {
            cell.set_value_string(other.to_string());
        }
    }
}

fn str_of<'a>(op: &'a Value, key: &str) -> Option<&'a str> {
    op.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn rows_of(op: &Value) -> Result<Vec<Vec<Value>>> {
    let rows = op.get("rows").and_then(Value::as_array).ok_or_else(|| anyhow!("需要 rows：二维数组，如 [[\"姓名\",\"分数\"],[\"张三\",90]]"))?;
    Ok(rows.iter().map(|r| r.as_array().cloned().unwrap_or_else(|| vec![r.clone()])).collect())
}

fn color(s: &str) -> String {
    let h = s.trim().trim_start_matches('#').to_uppercase();
    let named = match h.as_str() {
        "RED" | "红" | "红色" => "FF0000",
        "GREEN" | "绿" | "绿色" => "00B050",
        "BLUE" | "蓝" | "蓝色" => "0070C0",
        "YELLOW" | "黄" | "黄色" => "FFFF00",
        "GRAY" | "GREY" | "灰" | "灰色" => "D9D9D9",
        "BLACK" | "黑" | "黑色" => "000000",
        "WHITE" | "白" | "白色" => "FFFFFF",
        "ORANGE" | "橙" | "橙色" => "FFC000",
        _ => h.as_str(),
    };
    if named.len() == 8 {
        named.to_string()
    } else {
        format!("FF{named}")
    }
}

pub fn apply(book: &mut Spreadsheet, ops: &[Value]) -> Result<Vec<Value>> {
    let mut results = Vec::new();
    for (n, op) in ops.iter().enumerate() {
        let kind = op.get("op").and_then(Value::as_str).ok_or_else(|| anyhow!("第 {} 个操作缺少 op", n + 1))?;
        let r = apply_one(book, kind, op).map_err(|e| anyhow!("第 {} 个操作（{kind}）失败：{e:#}", n + 1))?;
        results.push(r);
    }
    Ok(results)
}

fn apply_one(book: &mut Spreadsheet, kind: &str, op: &Value) -> Result<Value> {
    let sheet = str_of(op, "sheet");
    match kind {
        "set" => {
            let ws = sheet_mut(book, sheet)?;
            let cell = str_of(op, "cell").ok_or_else(|| anyhow!("set 需要 cell，如 B2"))?;
            let (c, r) = parse_cell(cell).ok_or_else(|| anyhow!("单元格「{cell}」格式不对"))?;
            set_value(ws, c, r, op.get("value").unwrap_or(&Value::Null));
            Ok(json!({"op": "set", "cell": cell}))
        }
        "write" => {
            let ws = sheet_mut(book, sheet)?;
            let start = str_of(op, "start").unwrap_or("A1");
            let (c0, r0) = parse_cell(start).ok_or_else(|| anyhow!("start「{start}」格式不对"))?;
            let rows = rows_of(op)?;
            let mut width = 0;
            for (i, row) in rows.iter().enumerate() {
                width = width.max(row.len());
                for (j, v) in row.iter().enumerate() {
                    set_value(ws, c0 + j as u32, r0 + i as u32, v);
                }
            }
            Ok(json!({"op": "write", "range": format!("{start}:{}{}", col_name(c0 + width.max(1) as u32 - 1), r0 + rows.len().max(1) as u32 - 1)}))
        }
        "append" => {
            let ws = sheet_mut(book, sheet)?;
            let (_, max_row) = ws.get_highest_column_and_row();
            let c0 = str_of(op, "start_col").and_then(col_index).unwrap_or(1);
            let rows = rows_of(op)?;
            let first = max_row + 1;
            for (i, row) in rows.iter().enumerate() {
                for (j, v) in row.iter().enumerate() {
                    set_value(ws, c0 + j as u32, first + i as u32, v);
                }
            }
            Ok(json!({"op": "append", "rows": format!("{first}-{}", first + rows.len() as u32 - 1)}))
        }
        "insert_rows" | "delete_rows" => {
            let name = sheet_ref(book, sheet)?.get_name().to_string();
            let row = op.get("row").and_then(Value::as_u64).ok_or_else(|| anyhow!("需要 row（从 1 开始）"))? as u32;
            let count = op.get("count").and_then(Value::as_u64).unwrap_or(1).max(1) as u32;
            if kind == "insert_rows" {
                book.insert_new_row(&name, &row, &count);
            } else {
                book.remove_row(&name, &row, &count);
            }
            Ok(json!({"op": kind, "row": row, "count": count}))
        }
        "insert_cols" | "delete_cols" => {
            let name = sheet_ref(book, sheet)?.get_name().to_string();
            let col = str_of(op, "col").filter(|c| col_index(c).is_some()).ok_or_else(|| anyhow!("需要 col（列字母，如 C）"))?.to_uppercase();
            let count = op.get("count").and_then(Value::as_u64).unwrap_or(1).max(1) as u32;
            if kind == "insert_cols" {
                book.insert_new_column(&name, &col, &count);
            } else {
                book.remove_column(&name, &col, &count);
            }
            Ok(json!({"op": kind, "col": col, "count": count}))
        }
        "add_sheet" => {
            let name = str_of(op, "name").ok_or_else(|| anyhow!("add_sheet 需要 name"))?;
            default_margins(book.new_sheet(name).map_err(|e| anyhow!("{e}"))?);
            Ok(json!({"op": "add_sheet", "name": name}))
        }
        "rename_sheet" => {
            let name = str_of(op, "name").ok_or_else(|| anyhow!("rename_sheet 需要 name（新名称）"))?;
            let old = sheet_ref(book, sheet)?.get_name().to_string();
            let idx = sheet_names(book).iter().position(|n| *n == old).unwrap_or(0);
            book.set_sheet_name(idx, name).map_err(|e| anyhow!("{e}"))?;
            Ok(json!({"op": "rename_sheet", "from": old, "to": name}))
        }
        "delete_sheet" => {
            let name = sheet.ok_or_else(|| anyhow!("delete_sheet 需要 sheet"))?;
            book.remove_sheet_by_name(name).map_err(|e| anyhow!("{e}"))?;
            Ok(json!({"op": "delete_sheet", "name": name}))
        }
        "clear" => {
            let ws = sheet_mut(book, sheet)?;
            let max = ws.get_highest_column_and_row();
            let (c1, r1, c2, r2) = parse_range(str_of(op, "range").ok_or_else(|| anyhow!("clear 需要 range"))?, max)?;
            for r in r1..=r2 {
                for c in c1..=c2 {
                    ws.remove_cell((c, r));
                }
            }
            Ok(json!({"op": "clear"}))
        }
        "style" => {
            let ws = sheet_mut(book, sheet)?;
            let max = ws.get_highest_column_and_row();
            let range = str_of(op, "range").ok_or_else(|| anyhow!("style 需要 range"))?;
            let (c1, r1, c2, r2) = parse_range(range, max)?;
            if (c2 - c1 + 1) as u64 * (r2 - r1 + 1) as u64 > 200_000 {
                bail!("区域太大，请缩小范围");
            }
            for r in r1..=r2 {
                for c in c1..=c2 {
                    let st = ws.get_style_mut((c, r));
                    if let Some(b) = op.get("bold").and_then(Value::as_bool) {
                        st.get_font_mut().set_bold(b);
                    }
                    if let Some(b) = op.get("italic").and_then(Value::as_bool) {
                        st.get_font_mut().set_italic(b);
                    }
                    if let Some(s) = op.get("font_size").and_then(Value::as_f64) {
                        st.get_font_mut().set_size(s);
                    }
                    if let Some(f) = str_of(op, "font") {
                        st.get_font_mut().set_name(f);
                    }
                    if let Some(col) = str_of(op, "font_color") {
                        st.get_font_mut().get_color_mut().set_argb(color(col));
                    }
                    if let Some(fill) = str_of(op, "fill") {
                        st.set_background_color(color(fill));
                    }
                    if let Some(fmt) = str_of(op, "number_format") {
                        st.get_number_format_mut().set_format_code(fmt);
                    }
                    if let Some(a) = str_of(op, "align") {
                        use umya::HorizontalAlignmentValues as H;
                        let v = match a {
                            "left" => H::Left,
                            "center" => H::Center,
                            "right" => H::Right,
                            _ => bail!("align 只能是 left/center/right"),
                        };
                        st.get_alignment_mut().set_horizontal(v);
                    }
                    if let Some(a) = str_of(op, "valign") {
                        use umya::VerticalAlignmentValues as V;
                        let v = match a {
                            "top" => V::Top,
                            "center" | "middle" => V::Center,
                            "bottom" => V::Bottom,
                            _ => bail!("valign 只能是 top/center/bottom"),
                        };
                        st.get_alignment_mut().set_vertical(v);
                    }
                    if let Some(w) = op.get("wrap").and_then(Value::as_bool) {
                        st.get_alignment_mut().set_wrap_text(w);
                    }
                    if op.get("border").and_then(Value::as_bool) == Some(true) {
                        let b = st.get_borders_mut();
                        b.get_left_mut().set_border_style(umya::Border::BORDER_THIN);
                        b.get_right_mut().set_border_style(umya::Border::BORDER_THIN);
                        b.get_top_mut().set_border_style(umya::Border::BORDER_THIN);
                        b.get_bottom_mut().set_border_style(umya::Border::BORDER_THIN);
                    }
                }
            }
            Ok(json!({"op": "style", "range": range}))
        }
        "width" => {
            let ws = sheet_mut(book, sheet)?;
            let cols = str_of(op, "columns").ok_or_else(|| anyhow!("width 需要 columns，如 A 或 A:D"))?;
            let (a, b) = cols.split_once(':').unwrap_or((cols, cols));
            let (c1, c2) = (col_index(a).ok_or_else(|| anyhow!("列「{a}」不对"))?, col_index(b).ok_or_else(|| anyhow!("列「{b}」不对"))?);
            let width = op.get("width");
            for c in c1.min(c2)..=c1.max(c2) {
                let w = match width.and_then(Value::as_f64) {
                    Some(w) => w,
                    None => {
                        // auto：按该列最长内容估算（中文按 2 个字符宽）
                        let longest = ws
                            .get_collection_by_column(&c)
                            .iter()
                            .map(|cell| cell.get_formatted_value().chars().map(|ch| if ch.is_ascii() { 1.0 } else { 2.0 }).sum::<f64>())
                            .fold(0.0, f64::max);
                        (longest * 1.2 + 2.0).clamp(8.43, 80.0)
                    }
                };
                ws.get_column_dimension_mut(&col_name(c)).set_width(w);
            }
            Ok(json!({"op": "width", "columns": cols}))
        }
        "merge" => {
            let ws = sheet_mut(book, sheet)?;
            let range = str_of(op, "range").ok_or_else(|| anyhow!("merge 需要 range"))?;
            parse_range(range, (1, 1))?;
            ws.add_merge_cells(range.to_uppercase());
            Ok(json!({"op": "merge", "range": range}))
        }
        "freeze" => {
            let ws = sheet_mut(book, sheet)?;
            let cell = str_of(op, "cell").unwrap_or("A2");
            let (c, r) = parse_cell(cell).ok_or_else(|| anyhow!("freeze 的 cell 格式不对"))?;
            let mut pane = umya::Pane::default();
            if c > 1 {
                pane.set_horizontal_split((c - 1) as f64);
            }
            if r > 1 {
                pane.set_vertical_split((r - 1) as f64);
            }
            pane.get_top_left_cell_mut().set_coordinate(cell.to_uppercase());
            pane.set_state(umya::PaneStateValues::Frozen);
            pane.set_active_pane(if c > 1 && r > 1 {
                umya::PaneValues::BottomRight
            } else if r > 1 {
                umya::PaneValues::BottomLeft
            } else {
                umya::PaneValues::TopRight
            });
            let views = ws.get_sheet_views_mut().get_sheet_view_list_mut();
            if views.is_empty() {
                ws.get_sheet_views_mut().add_sheet_view_list_mut(umya::SheetView::default());
            }
            ws.get_sheet_views_mut().get_sheet_view_list_mut()[0].set_pane(pane);
            Ok(json!({"op": "freeze", "cell": cell}))
        }
        other => bail!("未知操作 {other}（set / write / append / insert_rows / delete_rows / insert_cols / delete_cols / add_sheet / rename_sheet / delete_sheet / clear / style / width / merge / freeze）"),
    }
}

/// 新建空工作簿（第一个工作表名可指定）
/// 新工作表默认页边距是 0，打印时贴边；改成 Excel 的默认值
fn default_margins(ws: &mut Worksheet) {
    ws.get_page_margins_mut().set_left(0.7).set_right(0.7).set_top(0.75).set_bottom(0.75).set_header(0.3).set_footer(0.3);
}

pub fn new_book(first_sheet: Option<&str>) -> Spreadsheet {
    let mut book = umya::new_file();
    if let Some(ws) = book.get_sheet_mut(&0) {
        default_margins(ws);
    }
    if let Some(name) = first_sheet.filter(|n| !n.trim().is_empty()) {
        let _ = book.set_sheet_name(0, name.trim());
    }
    book
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_and_range_parsing() {
        assert_eq!(col_name(1), "A");
        assert_eq!(col_name(28), "AB");
        assert_eq!(col_index("ab"), Some(28));
        assert_eq!(parse_cell("$C$12"), Some((3, 12)));
        assert_eq!(parse_range("B2:A1", (5, 5)).unwrap(), (1, 1, 2, 2));
        assert_eq!(parse_range("B:D", (9, 40)).unwrap(), (2, 1, 4, 40));
        assert_eq!(parse_range("3:4", (9, 40)).unwrap(), (1, 3, 9, 4));
        assert!(parse_range("1x", (1, 1)).is_err());
    }

    #[test]
    fn edit_and_read_roundtrip() {
        let dir = std::env::temp_dir().join(format!("sg-xlsx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.xlsx");
        let mut book = new_book(Some("成绩"));
        let ops = serde_json::json!([
            {"op": "write", "start": "A1", "rows": [["姓名", "分数", "编号"], ["张三", 90, "001"], ["李四", 85.5, "002"]]},
            {"op": "append", "rows": [["合计", "=SUM(B2:B3)"]]},
            {"op": "style", "range": "A1:C1", "bold": true, "fill": "gray", "border": true},
            {"op": "width", "columns": "A:C"},
            {"op": "freeze", "cell": "A2"},
            {"op": "add_sheet", "name": "备注"},
            {"op": "set", "sheet": "备注", "cell": "A1", "value": "测试"}
        ]);
        apply(&mut book, ops.as_array().unwrap()).unwrap();
        save(&book, &path).unwrap();
        let v = read(&path, None, None, 10000).unwrap();
        let table = v["table"].as_str().unwrap();
        assert!(table.contains("张三 | 90 | 001"), "{table}");
        assert!(table.contains("=SUM(B2:B3)"), "{table}");
        assert_eq!(v["sheets"][1]["name"], "备注");
        let mut book = open(&path).unwrap();
        apply(&mut book, serde_json::json!([{"op": "insert_rows", "row": 2}, {"op": "set", "cell": "A2", "value": "王五"}]).as_array().unwrap()).unwrap();
        save(&book, &path).unwrap();
        let v = read(&path, Some("成绩"), Some("A1:B4"), 10000).unwrap();
        let table = v["table"].as_str().unwrap();
        assert!(table.contains("   2| 王五"), "{table}");
        assert!(table.contains("   3| 张三"), "{table}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
