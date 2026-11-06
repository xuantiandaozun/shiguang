//! PDF：按页读取文字、合并、拆分、抽取/删除页、旋转、图片合成 PDF。基于 lopdf，不依赖外部程序。

use anyhow::{anyhow, bail, Context, Result};
use lopdf::{dictionary, Document, Object, ObjectId, Stream};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn load(path: &Path) -> Result<Document> {
    let doc = Document::load(path).with_context(|| format!("打开 PDF 失败：{}", path.display()))?;
    if doc.is_encrypted() {
        bail!("PDF 已加密，需要先用密码解除保护");
    }
    Ok(doc)
}

/// "1-3,5,8-" → 页码列表（从 1 开始，越界的忽略）
pub fn parse_pages(spec: &str, total: u32) -> Result<Vec<u32>> {
    let mut out = Vec::new();
    for part in spec.split([',', '，', ' ']).map(str::trim).filter(|s| !s.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (part, part),
        };
        let parse = |s: &str, default: u32| -> Result<u32> {
            if s.is_empty() {
                return Ok(default);
            }
            if s == "end" || s == "末" {
                return Ok(total);
            }
            s.parse::<u32>().map_err(|_| anyhow!("页码「{part}」格式不对，应如 1-3,5,8-"))
        };
        let (a, b) = (parse(a, 1)?, parse(b, total)?);
        if a == 0 || b == 0 {
            bail!("页码从 1 开始");
        }
        for p in a.min(b)..=a.max(b) {
            if p <= total && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    if out.is_empty() {
        bail!("没有有效页码（共 {total} 页）");
    }
    Ok(out)
}

fn meta(doc: &Document) -> Value {
    let mut v = json!({});
    if let Ok(Object::Reference(id)) = doc.trailer.get(b"Info") {
        if let Ok(Object::Dictionary(d)) = doc.get_object(*id) {
            for key in ["Title", "Author", "Subject", "Creator", "Producer", "CreationDate"] {
                if let Ok(Object::String(bytes, _)) = d.get(key.as_bytes()) {
                    let s = if bytes.starts_with(&[0xFE, 0xFF]) {
                        let units: Vec<u16> = bytes[2..].chunks(2).filter(|c| c.len() == 2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                        String::from_utf16_lossy(&units)
                    } else {
                        String::from_utf8_lossy(bytes).to_string()
                    };
                    if !s.trim().is_empty() {
                        v[key.to_lowercase()] = json!(s.trim());
                    }
                }
            }
        }
    }
    v
}

/// 按页读文字
pub fn read(path: &Path, pages: Option<&str>, max_chars: usize) -> Result<Value> {
    let doc = load(path)?;
    let total = doc.get_pages().len() as u32;
    let wanted = match pages {
        Some(p) => parse_pages(p, total)?,
        None => (1..=total).collect(),
    };
    let texts = pdf_extract::extract_text_by_pages(path).map_err(|e| anyhow!("提取文字失败：{e}"))?;
    let mut out = String::new();
    let mut last = 0;
    let mut truncated = false;
    let mut empty_pages = 0;
    for p in &wanted {
        let t = texts.get(*p as usize - 1).map(|s| s.trim()).unwrap_or("");
        if t.is_empty() {
            empty_pages += 1;
        }
        let chunk = format!("=== 第 {p} 页 ===\n{t}\n\n");
        if out.len() + chunk.len() > max_chars && last != 0 {
            truncated = true;
            break;
        }
        out.push_str(&chunk);
        last = *p;
    }
    let mut v = json!({"pages": total, "metadata": meta(&doc), "text": out});
    if truncated {
        v["truncated"] = json!(true);
        v["next_pages"] = json!(format!("{}-{}", last + 1, wanted.last().copied().unwrap_or(total)));
    }
    if empty_pages > 0 && empty_pages * 2 >= wanted.len() {
        v["hint"] = json!("多数页面没有文字层，可能是扫描件：用 office_convert 把 PDF 转成图片后再 ocr_image，或让用户提供可复制文字的版本");
    }
    Ok(v)
}

fn save(doc: &mut Document, path: &Path) -> Result<()> {
    doc.prune_objects();
    doc.compress();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    doc.save(path).with_context(|| format!("保存 PDF 失败（文件可能正被打开）：{}", path.display()))?;
    Ok(())
}

/// 只保留指定页（按给定顺序以外的页删掉；顺序保持原文档顺序）
fn keep_pages(path: &Path, keep: &[u32], out: &Path) -> Result<u32> {
    let mut doc = load(path)?;
    let total = doc.get_pages().len() as u32;
    let drop: Vec<u32> = (1..=total).filter(|p| !keep.contains(p)).collect();
    doc.delete_pages(&drop);
    save(&mut doc, out)?;
    Ok(total - drop.len() as u32)
}

pub fn merge(inputs: &[PathBuf], out: &Path) -> Result<u32> {
    if inputs.len() < 2 {
        bail!("合并至少需要 2 个 PDF");
    }
    let mut max_id = 1;
    let mut pages: BTreeMap<ObjectId, Object> = BTreeMap::new();
    let mut page_order: Vec<ObjectId> = Vec::new();
    let mut objects: BTreeMap<ObjectId, Object> = BTreeMap::new();
    for p in inputs {
        let mut doc = load(p)?;
        doc.renumber_objects_with(max_id);
        max_id = doc.max_id + 1;
        for (_, id) in doc.get_pages() {
            pages.insert(id, doc.get_object(id)?.to_owned());
            page_order.push(id);
        }
        objects.extend(doc.objects);
    }
    let mut document = Document::with_version("1.5");
    let mut catalog: Option<(ObjectId, Object)> = None;
    let mut pages_root: Option<(ObjectId, Object)> = None;
    for (id, obj) in &objects {
        match obj.type_name().unwrap_or(b"") {
            b"Catalog" => {
                if catalog.is_none() {
                    catalog = Some((*id, obj.clone()));
                }
            }
            b"Pages" => {
                if let Ok(d) = obj.as_dict() {
                    let mut d = d.clone();
                    if let Some((_, old)) = &pages_root {
                        if let Ok(od) = old.as_dict() {
                            d.extend(od);
                        }
                    }
                    let keep_id = pages_root.as_ref().map(|(i, _)| *i).unwrap_or(*id);
                    pages_root = Some((keep_id, Object::Dictionary(d)));
                }
            }
            b"Page" | b"Outlines" | b"Outline" => {}
            _ => {
                document.objects.insert(*id, obj.clone());
            }
        }
    }
    let (pages_id, pages_obj) = pages_root.ok_or_else(|| anyhow!("PDF 缺少页面树"))?;
    let (catalog_id, catalog_obj) = catalog.ok_or_else(|| anyhow!("PDF 缺少目录对象"))?;
    for (id, obj) in &pages {
        if let Ok(d) = obj.as_dict() {
            let mut d = d.clone();
            d.set("Parent", pages_id);
            document.objects.insert(*id, Object::Dictionary(d));
        }
    }
    let mut pd = pages_obj.as_dict()?.clone();
    pd.set("Count", page_order.len() as u32);
    pd.set("Kids", page_order.iter().map(|id| Object::Reference(*id)).collect::<Vec<_>>());
    pd.remove(b"Parent");
    document.objects.insert(pages_id, Object::Dictionary(pd));
    let mut cd = catalog_obj.as_dict()?.clone();
    cd.set("Pages", pages_id);
    cd.remove(b"Outlines");
    document.objects.insert(catalog_id, Object::Dictionary(cd));
    document.trailer.set("Root", catalog_id);
    document.max_id = document.objects.keys().map(|k| k.0).max().unwrap_or(0);
    document.renumber_objects();
    save(&mut document, out)?;
    Ok(page_order.len() as u32)
}

/// 图片（png/jpg/bmp/gif/webp 解码后统一转 JPEG）合成 PDF，每张一页，页面按图片比例、长边 842pt
pub fn images_to_pdf(images: &[PathBuf], out: &Path) -> Result<u32> {
    if images.is_empty() {
        bail!("需要至少一张图片");
    }
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut kids = Vec::new();
    for p in images {
        let img = image::open(p).with_context(|| format!("读取图片失败：{}", p.display()))?.to_rgb8();
        let (w, h) = img.dimensions();
        let mut jpg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 90).encode_image(&img)?;
        let xobj = Stream::new(
            dictionary! {"Type" => "XObject", "Subtype" => "Image", "Width" => w as i64, "Height" => h as i64, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "DCTDecode"},
            jpg,
        );
        let img_id = doc.add_object(xobj);
        let scale = 842.0 / w.max(h) as f64;
        let (pw, ph) = ((w as f64 * scale).round(), (h as f64 * scale).round());
        let content = format!("q {pw} 0 0 {ph} 0 0 cm /Im0 Do Q");
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(pw as f32), Object::Real(ph as f32)],
            "Resources" => dictionary! {"XObject" => dictionary! {"Im0" => img_id}},
            "Contents" => content_id,
        });
        kids.push(Object::Reference(page_id));
    }
    let n = kids.len() as u32;
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! {"Type" => "Pages", "Kids" => kids, "Count" => n}));
    let catalog = doc.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
    doc.trailer.set("Root", catalog);
    save(&mut doc, out)?;
    Ok(n)
}

fn default_out(input: &Path, suffix: &str) -> PathBuf {
    let stem = input.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "output".into());
    input.with_file_name(format!("{stem}_{suffix}.pdf"))
}

/// 执行一个 PDF 操作；路径都已解析为绝对路径
pub fn run(action: &str, input: Option<&Path>, inputs: &[PathBuf], output: Option<&Path>, args: &Value) -> Result<Value> {
    let need_input = || input.ok_or_else(|| anyhow!("{action} 需要 path"));
    match action {
        "merge" => {
            let out = output.ok_or_else(|| anyhow!("merge 需要 output"))?;
            let n = merge(inputs, out)?;
            Ok(json!({"ok": true, "output": out.to_string_lossy().replace('\\', "/"), "pages": n}))
        }
        "images_to_pdf" => {
            let out = output.ok_or_else(|| anyhow!("images_to_pdf 需要 output"))?;
            let n = images_to_pdf(inputs, out)?;
            Ok(json!({"ok": true, "output": out.to_string_lossy().replace('\\', "/"), "pages": n}))
        }
        "extract" | "delete" => {
            let input = need_input()?;
            let total = load(input)?.get_pages().len() as u32;
            let spec = args.get("pages").and_then(Value::as_str).ok_or_else(|| anyhow!("{action} 需要 pages，如 \"1-3,5\""))?;
            let sel = parse_pages(spec, total)?;
            let keep: Vec<u32> = if action == "extract" { sel } else { (1..=total).filter(|p| !sel.contains(p)).collect() };
            if keep.is_empty() {
                bail!("删除后没有剩余页面");
            }
            let out = output.map(Path::to_path_buf).unwrap_or_else(|| default_out(input, if action == "extract" { "节选" } else { "删页" }));
            let n = keep_pages(input, &keep, &out)?;
            Ok(json!({"ok": true, "output": out.to_string_lossy().replace('\\', "/"), "pages": n, "from_total": total}))
        }
        "split" => {
            let input = need_input()?;
            let total = load(input)?.get_pages().len() as u32;
            let dir = output.map(Path::to_path_buf).unwrap_or_else(|| input.with_file_name(format!("{}_拆分", input.file_stem().unwrap_or_default().to_string_lossy())));
            std::fs::create_dir_all(&dir)?;
            let stem = input.file_stem().unwrap_or_default().to_string_lossy().to_string();
            // ranges: ["1-3","4-6"]；或 every: N 每 N 页一份；默认每页一份
            let groups: Vec<Vec<u32>> = if let Some(ranges) = args.get("ranges").and_then(Value::as_array) {
                ranges.iter().filter_map(Value::as_str).map(|r| parse_pages(r, total)).collect::<Result<_>>()?
            } else {
                let every = args.get("every").and_then(Value::as_u64).unwrap_or(1).max(1) as u32;
                (0..total.div_ceil(every)).map(|g| ((g * every + 1)..=((g + 1) * every).min(total)).collect()).collect()
            };
            if groups.len() > 500 {
                bail!("拆分出的文件太多（{}），请用 every 或 ranges 分组", groups.len());
            }
            let mut files = Vec::new();
            for g in &groups {
                let label = if g.len() == 1 { format!("{}", g[0]) } else { format!("{}-{}", g[0], g[g.len() - 1]) };
                let out = dir.join(format!("{stem}_{label}.pdf"));
                keep_pages(input, g, &out)?;
                files.push(out.to_string_lossy().replace('\\', "/"));
            }
            Ok(json!({"ok": true, "dir": dir.to_string_lossy().replace('\\', "/"), "files": files}))
        }
        "rotate" => {
            let input = need_input()?;
            let angle = args.get("degrees").and_then(Value::as_i64).unwrap_or(90);
            if angle % 90 != 0 {
                bail!("degrees 必须是 90 的倍数");
            }
            let mut doc = load(input)?;
            let all = doc.get_pages();
            let total = all.len() as u32;
            let sel = match args.get("pages").and_then(Value::as_str) {
                Some(s) => parse_pages(s, total)?,
                None => (1..=total).collect(),
            };
            for (num, id) in all {
                if !sel.contains(&num) {
                    continue;
                }
                let d = doc.get_object_mut(id).and_then(|o| o.as_dict_mut()).map_err(|e| anyhow!("页面 {num} 损坏：{e}"))?;
                let cur = d.get(b"Rotate").and_then(|o| o.as_i64()).unwrap_or(0);
                d.set("Rotate", ((cur + angle) % 360 + 360) % 360);
            }
            let out = output.map(Path::to_path_buf).unwrap_or_else(|| input.to_path_buf());
            save(&mut doc, &out)?;
            Ok(json!({"ok": true, "output": out.to_string_lossy().replace('\\', "/"), "rotated_pages": sel.len()}))
        }
        other => bail!("未知 PDF 操作 {other}（merge / split / extract / delete / rotate / images_to_pdf）"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_specs() {
        assert_eq!(parse_pages("1-3,5", 10).unwrap(), vec![1, 2, 3, 5]);
        assert_eq!(parse_pages("8-", 10).unwrap(), vec![8, 9, 10]);
        assert_eq!(parse_pages("3-1", 10).unwrap(), vec![1, 2, 3]);
        assert!(parse_pages("0", 10).is_err());
        assert!(parse_pages("20", 10).is_err());
    }

    #[test]
    fn images_merge_split_rotate() {
        let dir = std::env::temp_dir().join(format!("sg-pdf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut imgs = Vec::new();
        for (i, (w, h)) in [(300u32, 200u32), (200, 300), (400, 400)].iter().enumerate() {
            let p = dir.join(format!("{i}.png"));
            image::RgbImage::from_pixel(*w, *h, image::Rgb([200, (i * 60) as u8, 50])).save(&p).unwrap();
            imgs.push(p);
        }
        let a = dir.join("a.pdf");
        assert_eq!(images_to_pdf(&imgs[..2], &a).unwrap(), 2);
        let b = dir.join("b.pdf");
        assert_eq!(images_to_pdf(&imgs[2..], &b).unwrap(), 1);
        let m = dir.join("m.pdf");
        assert_eq!(merge(&[a.clone(), b.clone()], &m).unwrap(), 3);
        assert_eq!(load(&m).unwrap().get_pages().len(), 3);
        let r = run("extract", Some(&m), &[], Some(&dir.join("e.pdf")), &json!({"pages": "2-3"})).unwrap();
        assert_eq!(r["pages"], 2);
        let r = run("split", Some(&m), &[], Some(&dir.join("parts")), &json!({})).unwrap();
        assert_eq!(r["files"].as_array().unwrap().len(), 3);
        run("rotate", Some(&m), &[], None, &json!({"pages": "1", "degrees": 90})).unwrap();
        let doc = load(&m).unwrap();
        let first = doc.get_pages()[&1];
        assert_eq!(doc.get_object(first).unwrap().as_dict().unwrap().get(b"Rotate").unwrap().as_i64().unwrap(), 90);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
