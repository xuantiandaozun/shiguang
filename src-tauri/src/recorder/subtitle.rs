//! 字幕样式：预设（经典 / 综艺花字 / 简约 / 卡拉 OK）+ 可覆盖的参数（字体、描边、渐变、投影、重点词颜色、动画）。
//! 字形轮廓转成路径后用 tiny-skia 描边、填充，所以多层粗描边、渐变、出场缩放都能做；逐帧计算，不依赖浏览器。
//! 解说词里用【】标出的词按重点词颜色画（字幕文件里去掉括号）。
//! 综艺字体按需下载免费可商用字体（Google Fonts 收录的站酷系列等，OFL 许可）到 app_data/fonts。

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_skia::{Color, FillRule, GradientStop, LinearGradient, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Point, Rect, Shader, SpreadMode, Stroke, Transform};

// ---------- 样式 ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokeSpec {
    /// 描边宽度（按 1080 高的画面算，像素）
    pub width: f64,
    pub color: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowSpec {
    #[serde(default)]
    pub dx: f64,
    #[serde(default = "default_shadow_dy")]
    pub dy: f64,
    pub color: String,
}

fn default_shadow_dy() -> f64 {
    4.0
}

/// timeline.json 的 subtitle_style。只写 preset 就用预设；其余字段覆盖预设里的同名项。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SubtitleStyle {
    /// classic（默认）| variety | minimal | karaoke
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preset: String,
    /// 字体：内置字体的 key（kuaile、huangyou、xiaowei、mashan）、字体目录里的文件名、系统字体文件名或完整路径
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    /// 字号倍数（1 = 1080p 下 40px）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    /// bottom | top | center
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,
    /// 文字颜色：1 个纯色，2 个以上为从上到下的渐变
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Vec<String>>,
    /// 描边，从外到内
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strokes: Option<Vec<StrokeSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<ShadowSpec>,
    /// 【】重点词的文字颜色（同 fill）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlight: Option<Vec<String>>,
    /// 底框颜色；空字符串表示不要底框
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub box_color: Option<String>,
    /// pop | fade | slide | typewriter | karaoke | none
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation: Option<String>,
}

pub const PRESETS: &[(&str, &str)] = &[
    ("classic", "经典：白字 + 半透明黑底"),
    ("variety", "综艺花字：快乐体、黄橙渐变、白色和深棕双层粗描边、投影、弹出动画，重点词粉红"),
    ("minimal", "简约：白字黑描边，无底框，淡入"),
    ("karaoke", "卡拉 OK：随朗读进度逐字变黄"),
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Anim {
    None,
    Fade,
    Pop,
    Slide,
    Typewriter,
    Karaoke,
}

/// 预设与覆盖合并后的具体参数
#[derive(Debug, Clone)]
pub struct Look {
    pub font: Option<String>,
    pub size: f32,
    pub position: String,
    pub fill: Vec<Color>,
    pub strokes: Vec<(f32, Color)>,
    pub shadow: Option<(f32, f32, Color)>,
    pub highlight: Vec<Color>,
    pub box_color: Option<Color>,
    pub anim: Anim,
}

pub fn parse_color(s: &str) -> Result<Color> {
    let h = s.trim().trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16);
    let (r, g, b, a) = match h.len() {
        6 => (byte(0)?, byte(2)?, byte(4)?, 255),
        8 => (byte(0)?, byte(2)?, byte(4)?, byte(6)?),
        3 => {
            let one = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).map(|v| v * 17);
            (one(0)?, one(1)?, one(2)?, 255)
        }
        _ => bail!("颜色要写成 #RRGGBB 或 #RRGGBBAA（现在是 {s}）"),
    };
    Ok(Color::from_rgba8(r, g, b, a))
}

fn colors(list: &[String]) -> Result<Vec<Color>> {
    if list.is_empty() {
        bail!("颜色列表不能为空");
    }
    list.iter().map(|c| parse_color(c)).collect()
}

impl SubtitleStyle {
    /// 合并预设并校验；出错的说明能直接给模型改正
    pub fn resolve(&self) -> Result<Look> {
        let c = |s: &str| parse_color(s).unwrap();
        let mut look = match self.preset.as_str() {
            "" | "classic" => Look {
                font: None,
                size: 1.0,
                position: "bottom".into(),
                fill: vec![Color::WHITE],
                strokes: vec![],
                shadow: None,
                highlight: vec![c("#FFD43B")],
                box_color: Some(c("#00000096")),
                anim: Anim::Fade,
            },
            "variety" => Look {
                font: Some("kuaile".into()),
                size: 1.6,
                position: "bottom".into(),
                fill: vec![c("#FFF36B"), c("#FFA41B")],
                strokes: vec![(15.0, c("#4A2300")), (7.0, Color::WHITE)],
                shadow: Some((4.0, 6.0, c("#00000080"))),
                highlight: vec![c("#FF8FB8"), c("#FF2D6F")],
                box_color: None,
                anim: Anim::Pop,
            },
            "minimal" => Look {
                font: None,
                size: 1.0,
                position: "bottom".into(),
                fill: vec![Color::WHITE],
                strokes: vec![(5.0, c("#000000CC"))],
                shadow: None,
                highlight: vec![c("#FFD43B")],
                box_color: None,
                anim: Anim::Fade,
            },
            "karaoke" => Look {
                font: Some("kuaile".into()),
                size: 1.25,
                position: "bottom".into(),
                fill: vec![Color::WHITE],
                strokes: vec![(9.0, c("#1B1B1B"))],
                shadow: Some((0.0, 4.0, c("#00000066"))),
                highlight: vec![c("#FFE14D"), c("#FFB800")],
                box_color: None,
                anim: Anim::Karaoke,
            },
            other => bail!("字幕预设只有 classic、variety、minimal、karaoke（现在是 {other}）"),
        };
        if let Some(f) = self.font.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            look.font = Some(f.to_string());
        }
        if let Some(s) = self.size {
            if !(0.5..=3.0).contains(&s) {
                bail!("字幕 size 是字号倍数，要在 0.5–3 之间");
            }
            look.size = s as f32;
        }
        if let Some(p) = &self.position {
            if !matches!(p.as_str(), "bottom" | "top" | "center") {
                bail!("字幕 position 只能是 bottom、top、center");
            }
            look.position = p.clone();
        }
        if let Some(f) = &self.fill {
            look.fill = colors(f)?;
        }
        if let Some(h) = &self.highlight {
            look.highlight = colors(h)?;
        }
        if let Some(st) = &self.strokes {
            look.strokes = st
                .iter()
                .map(|s| {
                    if !(0.0..=40.0).contains(&s.width) {
                        bail!("描边宽度要在 0–40 之间");
                    }
                    Ok((s.width as f32, parse_color(&s.color)?))
                })
                .collect::<Result<_>>()?;
            // 从外到内画：宽的先画
            look.strokes.sort_by(|a, b| b.0.total_cmp(&a.0));
        }
        if let Some(sh) = &self.shadow {
            look.shadow = Some((sh.dx as f32, sh.dy as f32, parse_color(&sh.color)?));
        }
        if let Some(b) = &self.box_color {
            look.box_color = if b.trim().is_empty() { None } else { Some(parse_color(b)?) };
        }
        if let Some(a) = &self.animation {
            look.anim = match a.as_str() {
                "none" => Anim::None,
                "fade" => Anim::Fade,
                "pop" => Anim::Pop,
                "slide" => Anim::Slide,
                "typewriter" => Anim::Typewriter,
                "karaoke" => Anim::Karaoke,
                other => bail!("字幕 animation 只能是 pop、fade、slide、typewriter、karaoke、none（现在是 {other}）"),
            };
        }
        Ok(look)
    }
}

// ---------- 字体 ----------

pub struct FontSpec {
    pub key: &'static str,
    pub label: &'static str,
    /// 网页里用的 font-family
    pub family: &'static str,
    pub file: &'static str,
    pub urls: &'static [&'static str],
}

pub const FONTS: &[FontSpec] = &[
    FontSpec {
        key: "kuaile",
        label: "站酷快乐体（圆润活泼，综艺常用）",
        family: "ZCOOL KuaiLe",
        file: "ZCOOLKuaiLe-Regular.ttf",
        urls: &[
            "https://github.com/google/fonts/raw/main/ofl/zcoolkuaile/ZCOOLKuaiLe-Regular.ttf",
            "https://cdn.jsdelivr.net/gh/google/fonts@main/ofl/zcoolkuaile/ZCOOLKuaiLe-Regular.ttf",
        ],
    },
    FontSpec {
        key: "huangyou",
        label: "站酷庆科黄油体（粗壮醒目，适合标题）",
        family: "ZCOOL QingKe HuangYou",
        file: "ZCOOLQingKeHuangYou-Regular.ttf",
        urls: &[
            "https://github.com/google/fonts/raw/main/ofl/zcoolqingkehuangyou/ZCOOLQingKeHuangYou-Regular.ttf",
            "https://cdn.jsdelivr.net/gh/google/fonts@main/ofl/zcoolqingkehuangyou/ZCOOLQingKeHuangYou-Regular.ttf",
        ],
    },
    FontSpec {
        key: "xiaowei",
        label: "站酷小薇（文艺宋体风）",
        family: "ZCOOL XiaoWei",
        file: "ZCOOLXiaoWei-Regular.ttf",
        urls: &[
            "https://github.com/google/fonts/raw/main/ofl/zcoolxiaowei/ZCOOLXiaoWei-Regular.ttf",
            "https://cdn.jsdelivr.net/gh/google/fonts@main/ofl/zcoolxiaowei/ZCOOLXiaoWei-Regular.ttf",
        ],
    },
    FontSpec {
        key: "mashan",
        label: "马善政毛笔楷书（手写书法）",
        family: "Ma Shan Zheng",
        file: "MaShanZheng-Regular.ttf",
        urls: &[
            "https://github.com/google/fonts/raw/main/ofl/mashanzheng/MaShanZheng-Regular.ttf",
            "https://cdn.jsdelivr.net/gh/google/fonts@main/ofl/mashanzheng/MaShanZheng-Regular.ttf",
        ],
    },
];

pub fn catalog(key: &str) -> Option<&'static FontSpec> {
    let k = key.trim();
    FONTS.iter().find(|f| f.key.eq_ignore_ascii_case(k) || f.family.eq_ignore_ascii_case(k))
}

fn system_fonts_dir() -> PathBuf {
    std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows")).join("Fonts")
}

/// 字体文件路径（不下载）：内置字体 → 字体目录里的文件 → 系统字体 → 完整路径
pub fn locate(fonts_dir: &Path, name: &str) -> Option<PathBuf> {
    if let Some(spec) = catalog(name) {
        let p = fonts_dir.join(spec.file);
        return p.is_file().then_some(p);
    }
    let p = PathBuf::from(name);
    if p.is_absolute() {
        return p.is_file().then_some(p);
    }
    [fonts_dir.join(name), system_fonts_dir().join(name)].into_iter().find(|p| p.is_file())
}

/// 字体目录里已有的字体（内置的已下载的 + 用户放进去的）
pub fn installed(fonts_dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = FONTS.iter().filter(|f| fonts_dir.join(f.file).is_file()).map(|f| f.key.to_string()).collect();
    if let Ok(rd) = std::fs::read_dir(fonts_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let lower = name.to_lowercase();
            if (lower.ends_with(".ttf") || lower.ends_with(".otf") || lower.ends_with(".ttc")) && !FONTS.iter().any(|f| f.file == name) {
                out.push(name);
            }
        }
    }
    out
}

/// 内置字体不在就下载（失败时换下一个地址）；其它字体找不到直接报错
pub async fn ensure(fonts_dir: &Path, name: &str, proxy: Option<&str>, progress: &impl Fn(u64, u64)) -> Result<PathBuf> {
    if let Some(p) = locate(fonts_dir, name) {
        return Ok(p);
    }
    let Some(spec) = catalog(name) else {
        bail!("找不到字体 {name}：可以用内置字体（{}），或把 .ttf/.otf 放进 {}", FONTS.iter().map(|f| f.key).collect::<Vec<_>>().join("、"), fonts_dir.display());
    };
    std::fs::create_dir_all(fonts_dir)?;
    let mut builder = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(15)).timeout(std::time::Duration::from_secs(300));
    if let Some(p) = proxy {
        builder = builder.proxy(reqwest::Proxy::all(p)?);
    }
    let client = builder.build()?;
    let mut errors = Vec::new();
    for url in spec.urls {
        match download(&client, url, &fonts_dir.join(spec.file), progress).await {
            Ok(()) => return Ok(fonts_dir.join(spec.file)),
            Err(e) => errors.push(format!("{url}：{e:#}")),
        }
    }
    bail!("下载字体「{}」失败：\n{}\n也可以手动下载这个字体放进 {}", spec.label, errors.join("\n"), fonts_dir.display())
}

async fn download(client: &reqwest::Client, url: &str, dest: &Path, progress: &impl Fn(u64, u64)) -> Result<()> {
    let resp = client.get(url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let part = dest.with_extension("part");
    let mut file = std::fs::File::create(&part)?;
    let mut got = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk)?;
        got += chunk.len() as u64;
        progress(got, total);
    }
    drop(file);
    // 确认是能用的字体再放到位
    ab_glyph::FontVec::try_from_vec(std::fs::read(&part)?).map_err(|_| anyhow!("下载到的不是有效字体文件"))?;
    std::fs::rename(&part, dest).context("保存字体")?;
    Ok(())
}

fn load_font(path: &Path) -> Result<ab_glyph::FontVec> {
    let bytes = std::fs::read(path).with_context(|| format!("读取字体 {}", path.display()))?;
    ab_glyph::FontVec::try_from_vec_and_index(bytes, 0).map_err(|_| anyhow!("字体文件无效：{}", path.display()))
}

/// 主字体缺字时用的系统字体（微软雅黑粗体优先，字幕需要粗一点）
fn fallback_font() -> Option<ab_glyph::FontVec> {
    ["msyhbd.ttc", "msyh.ttc", "simhei.ttf", "simsun.ttc", "arial.ttf"].iter().find_map(|n| load_font(&system_fonts_dir().join(n)).ok())
}

/// 导出时准备好的字幕：样式 + 字体（主字体 + 缺字回退）
pub struct Prepared {
    pub look: Look,
    fonts: Vec<Arc<ab_glyph::FontVec>>,
}

impl Prepared {
    pub fn new(look: Look, font_path: Option<&Path>) -> Result<Prepared> {
        let mut fonts = Vec::new();
        if let Some(p) = font_path {
            fonts.push(Arc::new(load_font(p)?));
        }
        if let Some(f) = fallback_font() {
            fonts.push(Arc::new(f));
        }
        if fonts.is_empty() {
            bail!("没有可用的字幕字体");
        }
        Ok(Prepared { look, fonts })
    }

    fn font_for(&self, c: char) -> &ab_glyph::FontVec {
        use ab_glyph::Font;
        self.fonts.iter().find(|f| f.glyph_id(c).0 != 0).unwrap_or(&self.fonts[0])
    }
}

// ---------- 文本 ----------

/// 去掉【】，返回 (字, 是否重点词)
pub fn rich(text: &str) -> Vec<(char, bool)> {
    let mut out = Vec::new();
    let mut hi = false;
    for c in text.chars() {
        match c {
            '【' => hi = true,
            '】' => hi = false,
            _ => out.push((c, hi)),
        }
    }
    out
}

/// 字幕文件里用的纯文本
pub fn plain(text: &str) -> String {
    text.chars().filter(|c| *c != '【' && *c != '】').collect()
}

// ---------- 绘制 ----------

fn ease_out_back(x: f32) -> f32 {
    let (c1, c3) = (1.70158f32, 2.70158f32);
    1.0 + c3 * (x - 1.0).powi(3) + c1 * (x - 1.0).powi(2)
}

fn with_alpha(c: Color, a: f32) -> Color {
    let mut c = c;
    c.set_alpha((c.alpha() * a).clamp(0.0, 1.0));
    c
}

fn paint_for(colors: &[Color], top: f32, bottom: f32, alpha: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.anti_alias = true;
    if colors.len() >= 2 {
        let n = colors.len() - 1;
        let stops: Vec<GradientStop> = colors.iter().enumerate().map(|(i, c)| GradientStop::new(i as f32 / n as f32, with_alpha(*c, alpha))).collect();
        if let Some(sh) = LinearGradient::new(Point::from_xy(0.0, top), Point::from_xy(0.0, bottom), stops, SpreadMode::Pad, Transform::identity()) {
            p.shader = sh;
            return p;
        }
    }
    p.shader = Shader::SolidColor(with_alpha(colors.first().copied().unwrap_or(Color::WHITE), alpha));
    p
}

/// 一个字的轮廓路径（输出像素坐标）
fn glyph_path(pb: &mut PathBuilder, font: &ab_glyph::FontVec, c: char, size: f32, x: f32, baseline: f32) {
    use ab_glyph::{Font, OutlineCurve};
    let Some(outline) = font.outline(font.glyph_id(c)) else { return };
    let s = size / font.height_unscaled();
    let p = |pt: ab_glyph::Point| (x + pt.x * s, baseline - pt.y * s);
    let mut last: Option<(f32, f32)> = None;
    for curve in &outline.curves {
        let (start, end) = match curve {
            OutlineCurve::Line(a, b) => (*a, *b),
            OutlineCurve::Quad(a, _, b) => (*a, *b),
            OutlineCurve::Cubic(a, _, _, b) => (*a, *b),
        };
        let st = p(start);
        if last.is_none_or(|l| (l.0 - st.0).abs() > 0.01 || (l.1 - st.1).abs() > 0.01) {
            if last.is_some() {
                pb.close();
            }
            pb.move_to(st.0, st.1);
        }
        match curve {
            OutlineCurve::Line(_, b) => {
                let b = p(*b);
                pb.line_to(b.0, b.1);
            }
            OutlineCurve::Quad(_, c1, b) => {
                let (c1, b) = (p(*c1), p(*b));
                pb.quad_to(c1.0, c1.1, b.0, b.1);
            }
            OutlineCurve::Cubic(_, c1, c2, b) => {
                let (c1, c2, b) = (p(*c1), p(*c2), p(*b));
                pb.cubic_to(c1.0, c1.1, c2.0, c2.1, b.0, b.1);
            }
        }
        last = Some(p(end));
    }
    if last.is_some() {
        pb.close();
    }
}

fn rounded(r: Rect, radius: f32) -> Option<tiny_skia::Path> {
    let (x, y, w, h) = (r.x(), r.y(), r.width(), r.height());
    let k = radius.min(w / 2.0).min(h / 2.0);
    let c = k * 0.552_284_8;
    let mut pb = PathBuilder::new();
    pb.move_to(x + k, y);
    pb.line_to(x + w - k, y);
    pb.cubic_to(x + w - k + c, y, x + w, y + k - c, x + w, y + k);
    pb.line_to(x + w, y + h - k);
    pb.cubic_to(x + w, y + h - k + c, x + w - k + c, y + h, x + w - k, y + h);
    pb.line_to(x + k, y + h);
    pb.cubic_to(x + k - c, y + h, x, y + h - k + c, x, y + h - k);
    pb.line_to(x, y + k);
    pb.cubic_to(x, y + k - c, x + k - c, y, x + k, y);
    pb.close();
    pb.finish()
}

/// 画一条字幕。elapsed / remain：这条字幕已显示和还剩的秒数（算动画）；content：画面内容区；unit：画面高 / 1080
pub fn draw(pm: &mut Pixmap, prep: &Prepared, text: &str, elapsed: f64, remain: f64, content: Rect, unit: f32) {
    use ab_glyph::{Font, PxScale, ScaleFont};
    let look = &prep.look;
    let chars = rich(text);
    if chars.is_empty() {
        return;
    }
    let size = 40.0 * unit * look.size;
    let max_w = content.width() * 0.86;
    // 断行：最多 2 行
    let mut lines: Vec<(Vec<(char, bool, f32)>, f32)> = vec![(Vec::new(), 0.0)];
    for &(c, hi) in &chars {
        let f = prep.font_for(c);
        let adv = f.as_scaled(PxScale::from(size)).h_advance(f.glyph_id(c));
        let full = {
            let (line, w) = lines.last().unwrap();
            *w + adv > max_w && !line.is_empty()
        };
        if full {
            if lines.len() == 2 {
                break;
            }
            lines.push((Vec::new(), 0.0));
        }
        let (line, w) = lines.last_mut().unwrap();
        line.push((c, hi, adv));
        *w += adv;
    }
    let main = &prep.fonts[0];
    let scaled = main.as_scaled(PxScale::from(size));
    let line_h = size * 1.3;
    let outer = look.strokes.first().map(|s| s.0 * unit).unwrap_or(0.0);
    let pad = if look.box_color.is_some() { 16.0 * unit } else { outer / 2.0 + 4.0 * unit };
    let block_w = lines.iter().map(|l| l.1).fold(0.0, f32::max) + pad * 2.0;
    let block_h = line_h * lines.len() as f32 + pad;
    let margin = 44.0 * unit + outer / 2.0;
    let y0 = match look.position.as_str() {
        "top" => content.top() + margin,
        "center" => content.y() + (content.height() - block_h) / 2.0,
        _ => content.bottom() - block_h - margin,
    };
    let x0 = content.x() + (content.width() - block_w) / 2.0;

    // 动画
    let (e, r) = (elapsed as f32, remain as f32);
    let fade_in = |d: f32| (e / d).clamp(0.0, 1.0);
    let fade_out = (r / 0.15).clamp(0.0, 1.0);
    let total = (e + r).max(0.1);
    let mut alpha = fade_out;
    let mut scale = 1.0f32;
    let mut dy = 0.0f32;
    let n_chars: usize = lines.iter().map(|l| l.0.len()).sum();
    let mut visible = n_chars;
    let mut sung = 0usize;
    match look.anim {
        Anim::None => alpha = 1.0,
        Anim::Fade => alpha *= fade_in(0.18),
        Anim::Pop => {
            let k = fade_in(0.28);
            scale = 0.55 + 0.45 * ease_out_back(k);
            alpha *= fade_in(0.1);
        }
        Anim::Slide => {
            let k = fade_in(0.25);
            dy = (1.0 - k).powi(2) * 36.0 * unit;
            alpha *= k;
        }
        Anim::Typewriter => {
            let dur = (total * 0.6).min(n_chars as f32 * 0.07).max(0.2);
            visible = ((e / dur).clamp(0.0, 1.0) * n_chars as f32).ceil() as usize;
        }
        Anim::Karaoke => {
            alpha *= fade_in(0.12);
            sung = ((e / (total * 0.92)).clamp(0.0, 1.0) * n_chars as f32).round() as usize;
        }
    }
    if alpha <= 0.01 || visible == 0 {
        return;
    }
    let (cx, cy) = (x0 + block_w / 2.0, y0 + block_h / 2.0);
    let tf = Transform::from_translate(cx, cy + dy).pre_scale(scale, scale).pre_translate(-cx, -cy);

    if let Some(bc) = look.box_color {
        if let Some(bg) = Rect::from_xywh(x0, y0, block_w, block_h).and_then(|r| rounded(r, 12.0 * unit)) {
            let mut p = Paint::default();
            p.anti_alias = true;
            p.set_color(with_alpha(bc, alpha));
            pm.fill_path(&bg, &p, FillRule::Winding, tf, None);
        }
    }

    // 每个字一条路径（重点词、卡拉 OK、打字机需要分开上色/显示）；整体一条用于描边和投影
    struct G {
        path: tiny_skia::Path,
        hi: bool,
        sung: bool,
        top: f32,
        bottom: f32,
    }
    let mut glyphs: Vec<G> = Vec::new();
    let mut all = PathBuilder::new();
    let mut idx = 0usize;
    for (li, (line, lw)) in lines.iter().enumerate() {
        let mut x = content.x() + (content.width() - lw) / 2.0;
        let top = y0 + pad / 2.0 + line_h * li as f32;
        let baseline = top + (line_h + scaled.ascent() + scaled.descent()) / 2.0;
        for &(c, hi, adv) in line {
            if idx < visible {
                let mut pb = PathBuilder::new();
                glyph_path(&mut pb, prep.font_for(c), c, size, x, baseline);
                glyph_path(&mut all, prep.font_for(c), c, size, x, baseline);
                if let Some(path) = pb.finish() {
                    glyphs.push(G { path, hi, sung: idx < sung, top: top + (line_h - size) / 2.0, bottom: top + (line_h + size) / 2.0 });
                }
            }
            idx += 1;
            x += adv;
        }
    }
    let Some(all) = all.finish() else { return };

    let stroke = |w: f32| Stroke { width: w, line_join: LineJoin::Round, line_cap: LineCap::Round, ..Default::default() };
    if let Some((sx, sy, sc)) = look.shadow {
        let st = tf.pre_translate(sx * unit, sy * unit);
        let mut p = Paint::default();
        p.anti_alias = true;
        p.set_color(with_alpha(sc, alpha));
        if outer > 0.0 {
            pm.stroke_path(&all, &p, &stroke(outer), st, None);
        }
        pm.fill_path(&all, &p, FillRule::Winding, st, None);
    }
    for &(w, color) in &look.strokes {
        let mut p = Paint::default();
        p.anti_alias = true;
        p.set_color(with_alpha(color, alpha));
        pm.stroke_path(&all, &p, &stroke(w * unit), tf, None);
    }
    for g in &glyphs {
        let colors = if g.hi || g.sung { &look.highlight } else { &look.fill };
        let p = paint_for(colors, g.top, g.bottom, alpha);
        pm.fill_path(&g.path, &p, FillRule::Winding, tf, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_resolve_and_overrides_validate() {
        for (p, _) in PRESETS {
            SubtitleStyle { preset: p.to_string(), ..Default::default() }.resolve().unwrap();
        }
        let s = SubtitleStyle {
            preset: "variety".into(),
            fill: Some(vec!["#ffffff".into()]),
            strokes: Some(vec![StrokeSpec { width: 4.0, color: "#000".into() }, StrokeSpec { width: 12.0, color: "#ff0000".into() }]),
            ..Default::default()
        };
        let look = s.resolve().unwrap();
        assert_eq!(look.strokes[0].0, 12.0, "宽的描边先画");
        assert_eq!(look.anim, Anim::Pop);
        assert!(SubtitleStyle { preset: "nope".into(), ..Default::default() }.resolve().is_err());
        assert!(SubtitleStyle { fill: Some(vec!["red".into()]), ..Default::default() }.resolve().is_err());
        assert!(parse_color("#11223380").is_ok());
    }

    #[test]
    fn highlight_markers() {
        let r = rich("这个功能【超好用】！");
        assert_eq!(r.iter().filter(|c| c.1).map(|c| c.0).collect::<String>(), "超好用");
        assert_eq!(plain("这个功能【超好用】！"), "这个功能超好用！");
    }

    #[test]
    fn draws_every_preset_with_system_font() {
        let Ok(prep) = Prepared::new(SubtitleStyle::default().resolve().unwrap(), None) else { return };
        for (p, _) in PRESETS {
            let look = SubtitleStyle { preset: p.to_string(), font: Some("msyhbd.ttc".into()), ..Default::default() }.resolve().unwrap();
            let prep = Prepared { look, fonts: prep.fonts.clone() };
            let mut pm = Pixmap::new(640, 360).unwrap();
            pm.fill(Color::from_rgba8(30, 60, 90, 255));
            draw(&mut pm, &prep, "测试【字幕】Subtitle", 1.0, 1.0, Rect::from_xywh(0.0, 0.0, 640.0, 360.0).unwrap(), 1.0 / 3.0);
            let changed = pm.pixels().iter().filter(|px| px.red() != 30 || px.green() != 60).count();
            assert!(changed > 500, "{p} 应该画出字幕（{changed}）");
        }
    }
}
