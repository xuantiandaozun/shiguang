//! 逐帧合成：源画面（打码区域马赛克）→ 镜头变换 → 合成光标与点击波纹 → 原始 RGBA 交给 ffmpeg 编码。
//! 多个 worker 按帧号取模并行渲染，主线程按顺序写给 ffmpeg。

use super::narration::Cue;
use super::timeline::Compiled;
#[cfg(test)]
use super::timeline::FrameState;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LinearGradient, Mask, Paint, PathBuilder, Pixmap,
    PixmapPaint, Point, Rect, Shader, SpreadMode, Stroke, Transform,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    Plain,
    Padded,
}

#[derive(Debug, Clone, Copy)]
pub struct RenderOpts {
    pub fps: u32,
    pub cursor_scale: f64,
    pub style: Style,
}

/// 源画面帧（frames.jsonl 一行）
#[derive(Debug, Clone)]
pub struct SourceFrame {
    pub t: f64,
    pub file: PathBuf,
    /// 和上一帧几乎一样（桌面录屏里输入光标闪烁之类）：紧凑节奏不把它当作「有动静」
    pub minor: bool,
}

/// 手动打码区域（timeline.json masks）：归一化矩形 + 时间范围（源时间）
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MaskRegion {
    pub start: f64,
    pub end: f64,
    pub rect: [f64; 4],
}

pub struct Job {
    pub ffmpeg: PathBuf,
    pub encoder: Vec<String>,
    pub out: PathBuf,
    pub width: u32,
    pub height: u32,
    pub sources: Vec<SourceFrame>,
    pub compiled: Compiled,
    pub masks: Vec<MaskRegion>,
    pub opts: RenderOpts,
    /// 输出帧 → (compiled.frames 下标, 输出时间)。剪切与定格都体现在这里。
    pub frame_map: Vec<(usize, f64)>,
    /// 画进画面的字幕（输出时间）；空表示不画
    pub cues: Vec<Cue>,
    /// 混好的配音 WAV
    pub audio: Option<PathBuf>,
    /// 快速预览：编码更快、质量更低
    pub fast: bool,
}

/// 渲染并编码。progress(已完成帧, 总帧)；cancel 置位时中止。
pub fn run(job: Job, cancel: Arc<AtomicBool>, progress: impl Fn(usize, usize)) -> Result<()> {
    let job = Arc::new(job);
    let total = job.frame_map.len();
    let log_path = job.out.with_extension("ffmpeg.log");
    let mut cmd = Command::new(&job.ffmpeg);
    cmd.args(["-y", "-hide_banner", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgba"])
        .args(["-s", &format!("{}x{}", job.width, job.height)])
        .args(["-r", &job.opts.fps.to_string(), "-i", "-"]);
    if let Some(audio) = &job.audio {
        cmd.arg("-i").arg(audio).args(["-map", "0:v", "-map", "1:a", "-c:a", "aac", "-b:a", "160k"]);
    }
    if job.fast && job.encoder.iter().any(|a| a == "libx264") {
        cmd.args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "30"]);
    } else {
        cmd.args(&job.encoder);
    }
    cmd.args(["-pix_fmt", "yuv420p", "-movflags", "+faststart"])
        .arg(&job.out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    hide_window(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("启动 ffmpeg 失败：{}", job.ffmpeg.display()))?;
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("ffmpeg 没有输入管道"))?;
    let mut stderr = child.stderr.take();
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });

    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8) - 1;
    let mut rxs = Vec::with_capacity(workers);
    let mut handles = Vec::with_capacity(workers);
    for k in 0..workers {
        let (tx, rx) = mpsc::sync_channel::<Result<Vec<u8>>>(2);
        rxs.push(rx);
        let job = job.clone();
        let cancel = cancel.clone();
        handles.push(std::thread::spawn(move || {
            let mut r = Renderer::new(&job);
            let mut i = k;
            while i < total && !cancel.load(Ordering::Relaxed) {
                let frame = r.render(&job, i);
                if tx.send(frame).is_err() {
                    return;
                }
                i += workers;
            }
        }));
    }

    let mut result = Ok(());
    for i in 0..total {
        if cancel.load(Ordering::Relaxed) {
            result = Err(anyhow!("导出已取消"));
            break;
        }
        match rxs[i % workers].recv() {
            Ok(Ok(buf)) => {
                if let Err(e) = stdin.write_all(&buf) {
                    result = Err(anyhow!("写入 ffmpeg 失败：{e}"));
                    break;
                }
            }
            Ok(Err(e)) => {
                result = Err(e);
                break;
            }
            Err(_) => {
                result = Err(anyhow!("渲染线程异常退出"));
                break;
            }
        }
        if i % 15 == 0 || i + 1 == total {
            progress(i + 1, total);
        }
    }
    if result.is_err() {
        cancel.store(true, Ordering::Relaxed);
    }
    drop(rxs);
    drop(stdin);
    for h in handles {
        let _ = h.join();
    }
    let status = child.wait().context("等待 ffmpeg 结束")?;
    let err_text = err_reader.join().unwrap_or_default();
    if !err_text.trim().is_empty() {
        let _ = std::fs::write(&log_path, &err_text);
    }
    result?;
    if !status.success() {
        let tail: String = err_text.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        bail!("ffmpeg 编码失败（{status}）：{tail}");
    }
    let _ = std::fs::remove_file(&log_path);
    Ok(())
}

#[cfg(windows)]
pub fn hide_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000);
}
#[cfg(not(windows))]
pub fn hide_window(_cmd: &mut Command) {}

/// 每个 worker 一个：缓存最近解码的源帧和固定的背景/遮罩。
struct Renderer {
    cache: Option<(usize, Pixmap)>,
    base: Pixmap,
    content: Rect,
    clip: Option<Mask>,
    unit: f32,
    font: Option<&'static ab_glyph::FontVec>,
}

impl Renderer {
    fn new(job: &Job) -> Renderer {
        let (w, h) = (job.width as f32, job.height as f32);
        let unit = h / 1080.0;
        let mut base = Pixmap::new(job.width, job.height).expect("画布尺寸有效");
        let (content, clip) = match job.opts.style {
            Style::Plain => {
                base.fill(Color::BLACK);
                (Rect::from_xywh(0.0, 0.0, w, h).unwrap(), None)
            }
            Style::Padded => {
                let pad = (w.min(h) * 0.06).round();
                let rect = Rect::from_xywh(pad, pad, w - pad * 2.0, h - pad * 2.0).unwrap();
                let radius = 18.0 * unit;
                paint_background(&mut base, rect, radius, unit);
                let mut mask = Mask::new(job.width, job.height).unwrap();
                if let Some(path) = rounded_rect(rect, radius) {
                    mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
                }
                (rect, Some(mask))
            }
        };
        let font = if job.cues.is_empty() { None } else { subtitle_font() };
        Renderer { cache: None, base, content, clip, unit, font }
    }

    fn source(&mut self, job: &Job, t: f64) -> Result<&Pixmap> {
        let idx = job.sources.partition_point(|s| s.t <= t).saturating_sub(1);
        if self.cache.as_ref().map(|c| c.0) != Some(idx) {
            let src = &job.sources[idx];
            let img = image::open(&src.file)
                .with_context(|| format!("读取画面 {}", src.file.display()))?
                .to_rgba8();
            let (w, h) = img.dimensions();
            let mut pm = Pixmap::from_vec(img.into_raw(), tiny_skia::IntSize::from_wh(w, h).unwrap())
                .ok_or_else(|| anyhow!("画面尺寸无效"))?;
            let masks: Vec<_> = job.masks.iter().filter(|m| t >= m.start && t <= m.end).collect();
            for m in masks {
                pixelate(&mut pm, m.rect);
            }
            self.cache = Some((idx, pm));
        } else if job.masks.iter().any(|m| t >= m.start && t <= m.end) {
            // 打码区间可能在同一源帧内开始/结束：重新解码以保证正确
            self.cache = None;
            return self.source(job, t);
        }
        Ok(&self.cache.as_ref().unwrap().1)
    }

    fn render(&mut self, job: &Job, i: usize) -> Result<Vec<u8>> {
        let (si, out_t) = job.frame_map[i];
        let f = job.compiled.frames[si.min(job.compiled.frames.len() - 1)];
        let mut canvas = self.base.clone();
        let content = self.content;
        let clip = self.clip.take();
        let src = self.source(job, f.t)?;
        let (sw, sh) = (src.width() as f32, src.height() as f32);
        let (ox, oy) = f.view_origin();
        let s = f.scale as f32;
        // 源帧像素 → 输出像素：先归一化，再减去可见区域原点，乘放大倍数，映射到内容区
        let sx = s * content.width() / sw;
        let sy = s * content.height() / sh;
        let tx = content.x() - ox as f32 * s * content.width();
        let ty = content.y() - oy as f32 * s * content.height();
        let quality = if s > 1.01 { FilterQuality::Bicubic } else { FilterQuality::Bilinear };
        canvas.draw_pixmap(
            0,
            0,
            src.as_ref(),
            &PixmapPaint { quality, ..Default::default() },
            Transform::from_row(sx, 0.0, 0.0, sy, tx, ty),
            clip.as_ref(),
        );
        self.clip = clip;
        let to_out = |x: f64, y: f64| {
            (
                content.x() + ((x - ox) * f.scale) as f32 * content.width(),
                content.y() + ((y - oy) * f.scale) as f32 * content.height(),
            )
        };
        for &(tc, x, y) in &job.compiled.clicks {
            let age = f.t - tc;
            if (0.0..0.55).contains(&age) {
                let (px, py) = to_out(x, y);
                draw_ripple(&mut canvas, px, py, (age / 0.55) as f32, self.unit * job.opts.cursor_scale as f32);
            }
        }
        if let Some((x, y, press)) = f.cursor {
            let (px, py) = to_out(x, y);
            if px >= content.left() && px <= content.right() && py >= content.top() && py <= content.bottom() {
                let size = 26.0 * self.unit * job.opts.cursor_scale as f32 * (1.0 - 0.15 * press as f32);
                draw_cursor(&mut canvas, px, py, size);
            }
        }
        if let (Some(font), Some(cue)) = (self.font, job.cues.iter().find(|c| out_t >= c.start && out_t < c.end)) {
            draw_subtitle(&mut canvas, font, &cue.text, content, self.unit);
        }
        Ok(canvas.take())
    }
}

/// 字幕字体：Windows 自带的中文字体，找不到就不画字幕（SRT 仍会生成）
fn subtitle_font() -> Option<&'static ab_glyph::FontVec> {
    static FONT: std::sync::OnceLock<Option<ab_glyph::FontVec>> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        let dir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows")).join("Fonts");
        ["msyh.ttc", "msyhbd.ttc", "simhei.ttf", "simsun.ttc", "arial.ttf"].iter().find_map(|name| {
            let bytes = std::fs::read(dir.join(name)).ok()?;
            ab_glyph::FontVec::try_from_vec_and_index(bytes, 0).ok()
        })
    })
    .as_ref()
}

/// 底部居中字幕：半透明圆角底 + 白字，按宽度自动换行（最多 2 行）
fn draw_subtitle(pm: &mut Pixmap, font: &ab_glyph::FontVec, text: &str, content: Rect, unit: f32) {
    use ab_glyph::{Font, PxScale, ScaleFont};
    let size = 40.0 * unit;
    let scaled = font.as_scaled(PxScale::from(size));
    let max_w = content.width() * 0.82;
    let mut lines: Vec<(String, f32)> = vec![(String::new(), 0.0)];
    for c in text.chars() {
        let adv = scaled.h_advance(font.glyph_id(c));
        let (line, w) = lines.last_mut().unwrap();
        if *w + adv > max_w && !line.is_empty() {
            if lines.len() == 2 {
                break;
            }
            lines.push((c.to_string(), adv));
        } else {
            line.push(c);
            *w += adv;
        }
    }
    let line_h = size * 1.35;
    let pad = 14.0 * unit;
    let block_w = lines.iter().map(|l| l.1).fold(0.0, f32::max) + pad * 2.0;
    let block_h = line_h * lines.len() as f32 + pad;
    let x0 = content.x() + (content.width() - block_w) / 2.0;
    let y0 = content.bottom() - block_h - 36.0 * unit;
    if let Some(bg) = Rect::from_xywh(x0, y0, block_w, block_h).and_then(|r| rounded_rect(r, 10.0 * unit)) {
        let mut p = Paint::default();
        p.anti_alias = true;
        p.set_color_rgba8(0, 0, 0, 150);
        pm.fill_path(&bg, &p, FillRule::Winding, Transform::identity(), None);
    }
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let data = pm.data_mut();
    for (li, (line, lw)) in lines.iter().enumerate() {
        let mut x = content.x() + (content.width() - lw) / 2.0;
        let baseline = y0 + pad / 2.0 + line_h * li as f32 + (line_h + scaled.ascent() + scaled.descent()) / 2.0;
        for c in line.chars() {
            let id = font.glyph_id(c);
            let glyph = id.with_scale_and_position(PxScale::from(size), ab_glyph::point(x, baseline));
            x += scaled.h_advance(id);
            let Some(outlined) = font.outline_glyph(glyph) else { continue };
            let b = outlined.px_bounds();
            outlined.draw(|gx, gy, cov| {
                let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                if px < 0 || py < 0 || px >= w || py >= h {
                    return;
                }
                let o = ((py * w + px) * 4) as usize;
                let a = cov.clamp(0.0, 1.0);
                for ch in 0..3 {
                    data[o + ch] = (255.0 * a + data[o + ch] as f32 * (1.0 - a)) as u8;
                }
            });
        }
    }
}

fn rounded_rect(r: Rect, radius: f32) -> Option<tiny_skia::Path> {
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

fn paint_background(pm: &mut Pixmap, content: Rect, radius: f32, unit: f32) {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let mut paint = Paint::default();
    paint.shader = LinearGradient::new(
        Point::from_xy(0.0, 0.0),
        Point::from_xy(w, h),
        vec![
            GradientStop::new(0.0, Color::from_rgba8(0x4f, 0x46, 0xe5, 255)),
            GradientStop::new(0.55, Color::from_rgba8(0x7c, 0x3a, 0xed, 255)),
            GradientStop::new(1.0, Color::from_rgba8(0xdb, 0x27, 0x77, 255)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap_or(Shader::SolidColor(Color::from_rgba8(0x4f, 0x46, 0xe5, 255)));
    pm.fill_rect(Rect::from_xywh(0.0, 0.0, w, h).unwrap(), &paint, Transform::identity(), None);
    // 柔和阴影：几层逐渐变大的半透明圆角矩形
    for i in (1..=8).rev() {
        let grow = i as f32 * 3.0 * unit;
        let r = Rect::from_xywh(content.x() - grow, content.y() - grow + 10.0 * unit, content.width() + grow * 2.0, content.height() + grow * 2.0);
        if let Some(path) = r.and_then(|r| rounded_rect(r, radius + grow)) {
            let mut p = Paint::default();
            p.set_color_rgba8(0, 0, 0, 10);
            p.anti_alias = true;
            pm.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
        }
    }
    if let Some(path) = rounded_rect(content, radius) {
        let mut p = Paint::default();
        p.set_color(Color::BLACK);
        p.anti_alias = true;
        pm.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
    }
}

/// 经典箭头光标，尖端在 (x, y)
fn draw_cursor(pm: &mut Pixmap, x: f32, y: f32, size: f32) {
    let k = size / 24.0;
    let pts = [(0.0, 0.0), (0.0, 17.0), (4.2, 13.2), (7.0, 19.6), (9.8, 18.4), (7.1, 12.2), (12.6, 12.2)];
    let build = |dx: f32, dy: f32| {
        let mut pb = PathBuilder::new();
        for (i, (px, py)) in pts.iter().enumerate() {
            let (px, py) = (x + dx + px * k, y + dy + py * k);
            if i == 0 { pb.move_to(px, py) } else { pb.line_to(px, py) }
        }
        pb.close();
        pb.finish()
    };
    if let Some(shadow) = build(1.2 * k, 1.8 * k) {
        let mut p = Paint::default();
        p.set_color_rgba8(0, 0, 0, 70);
        p.anti_alias = true;
        pm.fill_path(&shadow, &p, FillRule::Winding, Transform::identity(), None);
    }
    if let Some(path) = build(0.0, 0.0) {
        let mut p = Paint::default();
        p.anti_alias = true;
        p.set_color(Color::WHITE);
        pm.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
        p.set_color_rgba8(20, 20, 20, 255);
        let stroke = Stroke { width: 1.4 * k, line_join: tiny_skia::LineJoin::Round, ..Default::default() };
        pm.stroke_path(&path, &p, &stroke, Transform::identity(), None);
    }
}

fn draw_ripple(pm: &mut Pixmap, x: f32, y: f32, k: f32, unit: f32) {
    let ease = 1.0 - (1.0 - k).powi(3);
    let r = (10.0 + 34.0 * ease) * unit;
    let alpha = ((1.0 - k) * 150.0) as u8;
    if let Some(circle) = PathBuilder::from_circle(x, y, r) {
        let mut p = Paint::default();
        p.anti_alias = true;
        p.set_color_rgba8(255, 214, 10, alpha / 2);
        pm.fill_path(&circle, &p, FillRule::Winding, Transform::identity(), None);
        p.set_color_rgba8(255, 180, 0, alpha);
        pm.stroke_path(&circle, &p, &Stroke { width: 3.0 * unit, ..Default::default() }, Transform::identity(), None);
    }
}

/// 马赛克：区域内按块取平均色
fn pixelate(pm: &mut Pixmap, rect: [f64; 4]) {
    let (w, h) = (pm.width() as i64, pm.height() as i64);
    let x0 = ((rect[0] * w as f64) as i64).clamp(0, w);
    let y0 = ((rect[1] * h as f64) as i64).clamp(0, h);
    let x1 = (((rect[0] + rect[2]) * w as f64).ceil() as i64).clamp(0, w);
    let y1 = (((rect[1] + rect[3]) * h as f64).ceil() as i64).clamp(0, h);
    let block = ((w.max(h) / 80).max(8)) as i64;
    let data = pm.data_mut();
    let mut by = y0;
    while by < y1 {
        let mut bx = x0;
        while bx < x1 {
            let (ex, ey) = ((bx + block).min(x1), (by + block).min(y1));
            let mut sum = [0u64; 4];
            let mut n = 0u64;
            for yy in by..ey {
                for xx in bx..ex {
                    let o = ((yy * w + xx) * 4) as usize;
                    for c in 0..4 {
                        sum[c] += data[o + c] as u64;
                    }
                    n += 1;
                }
            }
            if n > 0 {
                let avg = sum.map(|s| (s / n) as u8);
                for yy in by..ey {
                    for xx in bx..ex {
                        let o = ((yy * w + xx) * 4) as usize;
                        data[o..o + 4].copy_from_slice(&avg);
                    }
                }
            }
            bx += block;
        }
        by += block;
    }
}

/// 选编码器：libx264 > h264_mf（Windows 媒体基础）> mpeg4
pub fn pick_encoder(ffmpeg: &Path) -> Vec<String> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-encoders"]).stdout(Stdio::piped()).stderr(Stdio::null());
    hide_window(&mut cmd);
    let list = cmd.output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    let has = |name: &str| list.lines().any(|l| l.split_whitespace().nth(1) == Some(name));
    let v: &[&str] = if has("libx264") {
        &["-c:v", "libx264", "-preset", "veryfast", "-crf", "20"]
    } else if has("h264_mf") {
        &["-c:v", "h264_mf", "-b:v", "8M"]
    } else {
        &["-c:v", "mpeg4", "-q:v", "2"]
    };
    v.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixelate_flattens_blocks() {
        let mut pm = Pixmap::new(64, 64).unwrap();
        for (i, px) in pm.data_mut().chunks_mut(4).enumerate() {
            px.copy_from_slice(&[(i % 256) as u8, 0, 0, 255]);
        }
        pixelate(&mut pm, [0.0, 0.0, 0.25, 0.25]);
        let d = pm.data();
        assert_eq!(d[0], d[4]);
        assert_eq!(d[0], d[(64 * 4) as usize]);
    }

    #[test]
    fn cursor_and_ripple_draw_inside_canvas() {
        let mut pm = Pixmap::new(200, 200).unwrap();
        draw_ripple(&mut pm, 100.0, 100.0, 0.3, 1.0);
        draw_cursor(&mut pm, 100.0, 100.0, 40.0);
        assert!(pm.pixel(102, 110).unwrap().alpha() > 0);
    }

    #[test]
    fn subtitle_draws_white_text_when_font_available() {
        let Some(font) = subtitle_font() else { return };
        let mut pm = Pixmap::new(640, 360).unwrap();
        pm.fill(Color::BLACK);
        draw_subtitle(&mut pm, font, "测试字幕 Subtitle", Rect::from_xywh(0.0, 0.0, 640.0, 360.0).unwrap(), 0.6);
        let bright = pm.pixels().iter().filter(|p| p.red() > 200).count();
        assert!(bright > 50, "{bright}");
    }

    #[test]
    fn frame_state_origin_matches_scale() {
        let f = FrameState { t: 0.0, scale: 2.0, cx: 0.5, cy: 0.5, cursor: None };
        assert_eq!(f.view_origin(), (0.25, 0.25));
    }
}
