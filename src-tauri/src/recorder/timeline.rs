//! 时间线：由事件轨算出放大段、镜头运动和平滑光标（纯计算，导出时逐帧使用）。
//! 坐标统一用视口归一化坐标 [0,1]×[0,1]（页面 CSS px ÷ innerWidth/innerHeight），
//! 和画面帧的像素尺寸无关；时间统一用相对第一帧的秒数。

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Event {
    /// 相对第一帧的秒数
    pub t: f64,
    pub kind: String,
    pub pos: Option<(f64, f64)>,
    /// 目标元素框（归一化 x, y, w, h）
    pub bbox: Option<[f64; 4]>,
}

impl Event {
    /// 解析 events.jsonl 的一行（t 为毫秒时间戳，t0 为第一帧的毫秒时间戳）。
    pub fn parse(v: &Value, t0_ms: f64) -> Option<Event> {
        let t = (v.get("t")?.as_f64()? - t0_ms) / 1000.0;
        let kind = v.get("type")?.as_str()?.to_string();
        let vw = v.get("vw").and_then(Value::as_f64).filter(|w| *w > 0.0).unwrap_or(1.0);
        let vh = v.get("vh").and_then(Value::as_f64).filter(|h| *h > 0.0).unwrap_or(1.0);
        let pos = match (v.get("x").and_then(Value::as_f64), v.get("y").and_then(Value::as_f64)) {
            (Some(x), Some(y)) => Some((x / vw, y / vh)),
            _ => None,
        };
        let bbox = v.pointer("/target/bbox").and_then(Value::as_array).and_then(|b| {
            let n: Vec<f64> = b.iter().filter_map(Value::as_f64).collect();
            (n.len() == 4 && n[2] > 0.0 && n[3] > 0.0).then(|| [n[0] / vw, n[1] / vh, n[2] / vw, n[3] / vh])
        });
        Some(Event { t, kind, pos, bbox })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ZoomSegment {
    pub start: f64,
    pub end: f64,
    pub scale: f64,
    /// 固定焦点（归一化）；没有时跟随光标
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<[f64; 2]>,
}

#[derive(Debug, Clone, Copy)]
pub struct Opts {
    pub fps: u32,
    pub zoom: bool,
    /// 放大倍数上限（有元素框时按元素大小算，不超过它）
    pub zoom_scale: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct FrameState {
    pub t: f64,
    /// 镜头：放大倍数与可见区域中心（归一化）
    pub scale: f64,
    pub cx: f64,
    pub cy: f64,
    /// 光标位置（归一化）与按下程度（0 = 松开，1 = 按下）；None 表示不显示
    pub cursor: Option<(f64, f64, f64)>,
}

impl FrameState {
    /// 可见区域左上角（归一化）
    pub fn view_origin(&self) -> (f64, f64) {
        (self.cx - 0.5 / self.scale, self.cy - 0.5 / self.scale)
    }
}

pub struct Compiled {
    pub frames: Vec<FrameState>,
    pub zooms: Vec<ZoomSegment>,
    /// 点击（时间、位置），用于画波纹
    pub clicks: Vec<(f64, f64, f64)>,
}

const LEAD: f64 = 0.5; // 动作前提前开始放大，抵消弹簧滞后
const HOLD: f64 = 1.6; // 动作后保持
const MERGE_GAP: f64 = 0.6;
const TYPE_HOLD: f64 = 1.2;

/// 由点击 / 输入焦点 / 按键生成放大段。
pub fn zoom_segments(events: &[Event], max_scale: f64) -> Vec<ZoomSegment> {
    let max_scale = max_scale.clamp(1.2, 3.0);
    let mut segs: Vec<ZoomSegment> = Vec::new();
    for e in events {
        let scale_for = |bbox: Option<[f64; 4]>| match bbox {
            // 元素约占画面宽度的 40%；太大的元素（整块面板）就不怎么放大
            Some(b) => (0.4 / b[2].max(b[3] * 0.6)).clamp(1.3, max_scale.max(1.3)),
            None => max_scale,
        };
        match e.kind.as_str() {
            "click" | "focus" => {
                let s = scale_for(e.bbox);
                let (start, end) = (e.t - LEAD, e.t + HOLD);
                match segs.last_mut() {
                    Some(last) if start <= last.end + MERGE_GAP => {
                        last.end = last.end.max(end);
                        last.scale = last.scale.min(s);
                    }
                    _ => segs.push(ZoomSegment { start: start.max(0.0), end, scale: s, focus: None }),
                }
            }
            // 连续输入时保持放大
            "key" => {
                if let Some(last) = segs.last_mut() {
                    if e.t <= last.end + 1.0 {
                        last.end = last.end.max(e.t + TYPE_HOLD);
                    }
                }
            }
            _ => {}
        }
    }
    segs
}

/// 临界阻尼弹簧：一步积分。
fn spring(x: &mut f64, v: &mut f64, target: f64, omega: f64, dt: f64) {
    // 半隐式欧拉，dt 很小时稳定
    let a = omega * omega * (target - *x) - 2.0 * omega * *v;
    *v += a * dt;
    *x += *v * dt;
}

/// 原始光标轨迹在 t 时刻的位置（线性插值），以及是否可见。
struct CursorTrack {
    points: Vec<(f64, f64, f64)>,
    hidden: Vec<(f64, f64)>, // 离开页面的区间
    presses: Vec<(f64, f64)>,
}

impl CursorTrack {
    fn new(events: &[Event], duration: f64) -> CursorTrack {
        let mut points = Vec::new();
        let mut hidden = Vec::new();
        let mut presses = Vec::new();
        let mut leave_at: Option<f64> = None;
        let mut down_at: Option<f64> = None;
        for e in events {
            if let Some((x, y)) = e.pos {
                if let Some(l) = leave_at.take() {
                    hidden.push((l, e.t));
                }
                points.push((e.t, x, y));
            }
            match e.kind.as_str() {
                "leave" => leave_at = Some(e.t),
                "down" => down_at = Some(e.t),
                "up" => {
                    if let Some(d) = down_at.take() {
                        presses.push((d, e.t.max(d + 0.08)));
                    }
                }
                // 只有 click 没有 down/up（例如程序触发）：补一个短按
                "click" => {
                    if down_at.is_none() && !presses.iter().any(|p| (p.1 - e.t).abs() < 0.3) {
                        presses.push((e.t - 0.06, e.t + 0.06));
                    }
                }
                _ => {}
            }
        }
        if let Some(l) = leave_at {
            hidden.push((l, duration + 1.0));
        }
        CursorTrack { points, hidden, presses }
    }

    fn at(&self, t: f64) -> Option<(f64, f64)> {
        let first = self.points.first()?;
        if t < first.0 || self.hidden.iter().any(|h| t >= h.0 && t < h.1) {
            return None;
        }
        let i = self.points.partition_point(|p| p.0 <= t);
        let a = self.points[i - 1];
        match self.points.get(i) {
            Some(b) if b.0 > a.0 => {
                let k = ((t - a.0) / (b.0 - a.0)).clamp(0.0, 1.0);
                Some((a.1 + (b.1 - a.1) * k, a.2 + (b.2 - a.2) * k))
            }
            _ => Some((a.1, a.2)),
        }
    }

    fn pressed(&self, t: f64) -> bool {
        self.presses.iter().any(|p| t >= p.0 && t <= p.1)
    }
}

/// 逐帧模拟镜头与光标（按源时间，每 1/fps 一帧）。manual 为用户编辑过的放大段，给了就不再自动生成。
pub fn compile(events: &[Event], duration: f64, opts: Opts, manual: Option<&[ZoomSegment]>) -> Compiled {
    let fps = opts.fps.clamp(10, 60) as f64;
    let dt = 1.0 / fps;
    let n = ((duration * fps).ceil() as usize).max(1);
    let zooms = match manual {
        Some(m) => {
            let mut z: Vec<ZoomSegment> = m
                .iter()
                .filter(|z| z.end > z.start)
                .map(|z| ZoomSegment { scale: z.scale.clamp(1.0, 3.0), ..*z })
                .collect();
            z.sort_by(|a, b| a.start.total_cmp(&b.start));
            z
        }
        None if opts.zoom => zoom_segments(events, opts.zoom_scale),
        None => Vec::new(),
    };
    let track = CursorTrack::new(events, duration);
    let clicks = events
        .iter()
        .filter(|e| e.kind == "click")
        .filter_map(|e| e.pos.map(|(x, y)| (e.t, x, y)))
        .collect();
    // 最近一次动作位置：光标不可见时镜头对准它
    let anchors: Vec<(f64, f64, f64)> = events
        .iter()
        .filter(|e| matches!(e.kind.as_str(), "click" | "focus"))
        .filter_map(|e| {
            e.pos.or_else(|| e.bbox.map(|b| (b[0] + b[2] / 2.0, b[1] + b[3] / 2.0))).map(|(x, y)| (e.t, x, y))
        })
        .collect();

    let (mut s, mut sv) = (1.0, 0.0);
    let (mut cx, mut cxv, mut cy, mut cyv) = (0.5, 0.0, 0.5, 0.0);
    let mut cur: Option<(f64, f64)> = None;
    let (mut curxv, mut curyv) = (0.0, 0.0);
    let mut press = 0.0;
    let mut frames = Vec::with_capacity(n);
    // 每帧内再细分几步积分，保证低帧率时弹簧也稳定
    const SUB: usize = 4;
    let h = dt / SUB as f64;
    for i in 0..n {
        let t = i as f64 * dt;
        for k in 0..SUB {
            let tt = t - dt + (k + 1) as f64 * h;
            let raw = track.at(tt.max(0.0));
            match (raw, cur.as_mut()) {
                (Some((x, y)), Some(c)) => {
                    spring(&mut c.0, &mut curxv, x, 22.0, h);
                    spring(&mut c.1, &mut curyv, y, 22.0, h);
                }
                (Some(p), None) => {
                    cur = Some(p);
                    curxv = 0.0;
                    curyv = 0.0;
                }
                (None, _) => cur = None,
            }
            let seg = zooms.iter().find(|z| tt >= z.start && tt <= z.end);
            let target_s = seg.map_or(1.0, |z| z.scale);
            let (tx, ty) = if let Some([fx, fy]) = seg.and_then(|z| z.focus) {
                (fx, fy)
            } else if seg.is_some() {
                cur.or_else(|| {
                    let j = anchors.partition_point(|a| a.0 <= tt + LEAD);
                    j.checked_sub(1).map(|j| (anchors[j].1, anchors[j].2))
                })
                .unwrap_or((0.5, 0.5))
            } else {
                (0.5, 0.5)
            };
            spring(&mut s, &mut sv, target_s, 6.0, h);
            spring(&mut cx, &mut cxv, tx, 5.0, h);
            spring(&mut cy, &mut cyv, ty, 5.0, h);
            let pt = if track.pressed(tt) { 1.0 } else { 0.0 };
            press += (pt - press) * (h * 30.0).min(1.0);
        }
        let scale = s.max(1.0);
        let half = 0.5 / scale;
        frames.push(FrameState {
            t,
            scale,
            cx: cx.clamp(half, 1.0 - half),
            cy: cy.clamp(half, 1.0 - half),
            cursor: cur.map(|(x, y)| (x, y, press)),
        });
    }
    Compiled { frames, zooms, clicks }
}

/// 输出时间轴：源时间去掉剪切段，并在某些源时间点插入定格（等配音说完）。
#[derive(Debug, Clone, Default)]
pub struct TimeMap {
    pieces: Vec<Piece>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Piece {
    Play(f64, f64),
    /// (源时间, 时长, 标记)：标记是 TimeMap::tagged 传进来的编号（插入的代码片段）
    Hold(f64, f64, Option<usize>),
}

impl Piece {
    fn len(&self) -> f64 {
        match *self {
            Piece::Play(a, b) => b - a,
            Piece::Hold(_, d, _) => d,
        }
    }
}

impl TimeMap {
    /// cuts：要剪掉的源时间段；freezes：(源时间, 定格秒数)
    pub fn new(duration: f64, cuts: &[(f64, f64)], freezes: &[(f64, f64)]) -> TimeMap {
        let tagged: Vec<(f64, f64, Option<usize>)> = freezes.iter().map(|&(t, d)| (t, d, None)).collect();
        TimeMap::tagged(duration, cuts, &tagged)
    }

    /// 同 new，定格可以带编号，之后用 tagged_holds 查它们落在输出时间轴的哪里。同一时刻的多个定格按传入顺序排。
    pub fn tagged(duration: f64, cuts: &[(f64, f64)], freezes: &[(f64, f64, Option<usize>)]) -> TimeMap {
        let mut cuts: Vec<(f64, f64)> = cuts
            .iter()
            .map(|&(a, b)| (a.clamp(0.0, duration), b.clamp(0.0, duration)))
            .filter(|(a, b)| b > a)
            .collect();
        cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut kept = Vec::new();
        let mut at = 0.0;
        for (a, b) in cuts {
            if a > at {
                kept.push((at, a));
            }
            at = f64::max(at, b);
        }
        if duration > at {
            kept.push((at, duration));
        }
        if kept.is_empty() {
            kept.push((0.0, duration.min(0.1)));
        }
        // 定格点落在剪切段里：挪到下一个保留段的开头
        let last_end = kept.last().map(|k| k.1).unwrap_or(0.0);
        let mut holds: Vec<(f64, f64, Option<usize>)> = freezes
            .iter()
            .filter(|f| f.1 > 0.0)
            .map(|&(t, d, tag)| {
                let t = kept.iter().find(|k| t <= k.1).map(|k| t.max(k.0)).unwrap_or(last_end);
                (t, d, tag)
            })
            .collect();
        holds.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut pieces = Vec::new();
        let mut hi = 0;
        for (a, b) in kept {
            let mut start = a;
            while hi < holds.len() && holds[hi].0 <= b {
                let (t, d, tag) = holds[hi];
                if t > start {
                    pieces.push(Piece::Play(start, t));
                    start = t;
                }
                pieces.push(Piece::Hold(t, d, tag));
                hi += 1;
            }
            if b > start {
                pieces.push(Piece::Play(start, b));
            }
        }
        TimeMap { pieces }
    }

    /// 带编号的定格在输出时间轴上的 (编号, 开始, 时长)
    pub fn tagged_holds(&self) -> Vec<(usize, f64, f64)> {
        let mut acc = 0.0;
        let mut out = Vec::new();
        for p in &self.pieces {
            if let Piece::Hold(_, d, Some(tag)) = *p {
                out.push((tag, acc, d));
            }
            acc += p.len();
        }
        out
    }

    pub fn output_duration(&self) -> f64 {
        self.pieces.iter().map(Piece::len).sum()
    }

    /// 输出时间 → 源时间
    pub fn to_source(&self, t: f64) -> f64 {
        let mut acc = 0.0;
        for p in &self.pieces {
            let len = p.len();
            if t < acc + len {
                return match *p {
                    Piece::Play(a, _) => a + (t - acc),
                    Piece::Hold(s, _, _) => s,
                };
            }
            acc += len;
        }
        match self.pieces.last() {
            Some(Piece::Play(_, b)) => *b,
            Some(Piece::Hold(s, _, _)) => *s,
            None => 0.0,
        }
    }

    /// 源时间 → 输出时间。落在剪切段里的时间映射到剪切后的位置；同一时刻的定格算在它之前。
    pub fn to_output(&self, src: f64) -> f64 {
        let mut acc = 0.0;
        for p in &self.pieces {
            match *p {
                Piece::Play(a, b) => {
                    if src < b {
                        return acc + (src - a).max(0.0);
                    }
                }
                Piece::Hold(s, _, _) => {
                    if src < s {
                        return acc;
                    }
                }
            }
            acc += p.len();
        }
        acc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(t: f64, kind: &str, pos: Option<(f64, f64)>) -> Event {
        Event { t, kind: kind.into(), pos, bbox: None }
    }

    #[test]
    fn parse_normalizes_coordinates_and_time() {
        let v = json!({"t": 2500.0, "type": "click", "x": 640, "y": 360, "vw": 1280, "vh": 720,
            "target": {"bbox": [600, 340, 80, 40]}});
        let e = Event::parse(&v, 1000.0).unwrap();
        assert!((e.t - 1.5).abs() < 1e-9);
        assert_eq!(e.pos, Some((0.5, 0.5)));
        assert_eq!(e.bbox.unwrap()[2], 80.0 / 1280.0);
    }

    #[test]
    fn nearby_clicks_merge_into_one_zoom_and_typing_extends_it() {
        let events = vec![
            ev(1.0, "click", Some((0.2, 0.2))),
            ev(2.5, "click", Some((0.3, 0.2))),
            ev(3.5, "key", None),
            ev(10.0, "click", Some((0.8, 0.8))),
        ];
        let z = zoom_segments(&events, 1.8);
        assert_eq!(z.len(), 2);
        assert!((z[0].start - 0.5).abs() < 1e-9);
        assert!((z[0].end - 4.7).abs() < 1e-9);
        assert_eq!(z[1].scale, 1.8);
    }

    #[test]
    fn camera_zooms_in_on_click_and_returns_to_full_view() {
        let events = vec![ev(0.0, "move", Some((0.5, 0.5))), ev(2.0, "move", Some((0.2, 0.25))), ev(2.0, "click", Some((0.2, 0.25)))];
        let c = compile(&events, 8.0, Opts { fps: 30, zoom: true, zoom_scale: 2.0 }, None);
        let at = |t: f64| c.frames[(t * 30.0) as usize];
        assert!(at(0.5).scale < 1.05);
        let mid = at(3.0);
        assert!(mid.scale > 1.8, "scale {}", mid.scale);
        // 中心被限制在画面内：放大 2 倍时中心不小于 0.25
        assert!(mid.cx >= 0.25 - 1e-9 && mid.cx < 0.35);
        assert!(at(7.9).scale < 1.05);
        assert_eq!(c.clicks.len(), 1);
    }

    #[test]
    fn cursor_hidden_before_first_move_and_after_leave() {
        let events = vec![ev(1.0, "move", Some((0.4, 0.4))), ev(2.0, "leave", None)];
        let c = compile(&events, 3.0, Opts { fps: 30, zoom: false, zoom_scale: 1.8 }, None);
        assert!(c.frames[10].cursor.is_none());
        assert!(c.frames[45].cursor.is_some());
        assert!(c.frames[75].cursor.is_none());
        assert!(c.frames.iter().all(|f| f.scale == 1.0));
    }

    #[test]
    fn manual_zoom_with_focus_overrides_auto() {
        let events = vec![ev(1.0, "click", Some((0.9, 0.9)))];
        let manual = [ZoomSegment { start: 0.0, end: 4.0, scale: 2.0, focus: Some([0.3, 0.3]) }];
        let c = compile(&events, 5.0, Opts { fps: 30, zoom: true, zoom_scale: 1.8 }, Some(&manual));
        assert_eq!(c.zooms.len(), 1);
        let f = c.frames[90];
        assert!(f.scale > 1.9 && (f.cx - 0.3).abs() < 0.03, "{f:?}");
    }

    #[test]
    fn time_map_cuts_and_holds() {
        // 10 秒，剪掉 2–4，在 6 秒处定格 1.5 秒
        let m = TimeMap::new(10.0, &[(2.0, 4.0)], &[(6.0, 1.5)]);
        assert!((m.output_duration() - 9.5).abs() < 1e-9);
        assert_eq!(m.to_source(1.0), 1.0);
        assert_eq!(m.to_source(2.5), 4.5);
        assert_eq!(m.to_source(4.5), 6.0); // 定格中
        assert!((m.to_source(5.6) - 6.1).abs() < 1e-9);
        assert_eq!(m.to_output(3.0), 2.0); // 剪切段里
        assert_eq!(m.to_output(6.0), 5.5); // 定格之后才开始
        assert!((m.to_output(10.0) - 9.5).abs() < 1e-9);
    }

    #[test]
    fn hold_inside_cut_moves_to_next_kept_piece() {
        let m = TimeMap::new(10.0, &[(2.0, 4.0)], &[(3.0, 1.0)]);
        assert_eq!(m.to_source(2.5), 4.0);
        assert!((m.output_duration() - 9.0).abs() < 1e-9);
    }
}
