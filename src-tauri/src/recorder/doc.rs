//! timeline.json：用户（或拾光）对一次录制的编辑——放大段、打码、剪切、解说、字幕开关。
//! 时间都是源时间（相对第一帧的秒数），坐标都是归一化坐标。

use super::narration::{Narration, AUDIO_EXT};
use super::render::MaskRegion;
use super::timeline::ZoomSegment;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Cut {
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TimelineDoc {
    /// true 时用 zooms 原样渲染；false 时导出时按事件自动生成（zooms 只是上次的结果）
    #[serde(default)]
    pub zooms_edited: bool,
    #[serde(default)]
    pub zooms: Vec<ZoomSegment>,
    #[serde(default)]
    pub masks: Vec<MaskRegion>,
    #[serde(default)]
    pub cuts: Vec<Cut>,
    #[serde(default)]
    pub narration: Vec<Narration>,
    /// 把字幕画进画面；None 用插件设置
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burn_subtitles: Option<bool>,
}

pub fn load(dir: &Path) -> TimelineDoc {
    std::fs::read_to_string(dir.join("timeline.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn exists(dir: &Path) -> bool {
    dir.join("timeline.json").is_file()
}

pub fn save(dir: &Path, doc: &TimelineDoc) -> Result<()> {
    std::fs::write(dir.join("timeline.json"), serde_json::to_vec_pretty(doc)?)?;
    Ok(())
}

fn in_range(v: f64) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}

/// 校验并规整（排序、夹到时长内），出错给出能让人/模型改正的说明
pub fn normalize(doc: &mut TimelineDoc, duration: f64, dir: &Path) -> Result<()> {
    let clamp_t = |t: f64| if t.is_finite() { t.clamp(0.0, duration) } else { 0.0 };
    for z in &mut doc.zooms {
        z.start = clamp_t(z.start);
        z.end = clamp_t(z.end);
        if z.end <= z.start {
            bail!("放大段 {:.1}–{:.1} 秒：结束要晚于开始", z.start, z.end);
        }
        if !(1.0..=3.0).contains(&z.scale) {
            bail!("放大倍数要在 1–3 之间（现在是 {}）", z.scale);
        }
        if let Some([x, y]) = z.focus {
            if !in_range(x) || !in_range(y) {
                bail!("放大焦点要用 0–1 的画面比例坐标");
            }
        }
    }
    doc.zooms.sort_by(|a, b| a.start.total_cmp(&b.start));
    for m in &mut doc.masks {
        m.start = clamp_t(m.start);
        m.end = clamp_t(m.end);
        let [x, y, w, h] = m.rect;
        if m.end <= m.start {
            bail!("打码区域的结束时间要晚于开始时间");
        }
        if !(in_range(x) && in_range(y) && w > 0.0 && h > 0.0 && x + w <= 1.0001 && y + h <= 1.0001) {
            bail!("打码区域 rect 要用 0–1 的画面比例 [x, y, 宽, 高]");
        }
    }
    for c in &mut doc.cuts {
        c.start = clamp_t(c.start);
        c.end = clamp_t(c.end);
        if c.end <= c.start {
            bail!("剪切段 {:.1}–{:.1} 秒：结束要晚于开始", c.start, c.end);
        }
    }
    doc.cuts.sort_by(|a, b| a.start.total_cmp(&b.start));
    let kept: f64 = duration - doc.cuts.iter().map(|c| c.end - c.start).sum::<f64>();
    if kept < 0.5 {
        bail!("剪切后几乎没有剩下画面");
    }
    for n in &mut doc.narration {
        n.at = clamp_t(n.at);
        n.text = n.text.trim().to_string();
        if let Some(a) = n.audio.as_mut() {
            *a = a.trim().to_string();
        }
        if n.audio.as_deref().is_some_and(str::is_empty) {
            n.audio = None;
        }
        if let Some(a) = &n.audio {
            let path = super::narration::resolve_audio(dir, a);
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            if !AUDIO_EXT.contains(&ext.as_str()) {
                bail!("不支持的音频格式：{a}（支持 {}）", AUDIO_EXT.join("/"));
            }
            if !path.is_file() {
                bail!("音频文件不存在：{}", path.display());
            }
        }
        if n.text.is_empty() && n.audio.is_none() {
            bail!("第 {:.1} 秒的解说既没有文字也没有音频", n.at);
        }
    }
    doc.narration.sort_by(|a, b| a.at.total_cmp(&b.at));
    Ok(())
}

/// 按 record_edit 的参数修改：给了哪个字段就整体替换哪个字段
pub fn apply_patch(doc: &mut TimelineDoc, args: &Value) -> Result<Vec<&'static str>> {
    let mut changed = Vec::new();
    let parse = |k: &str| -> Result<Option<Value>> { Ok(args.get(k).filter(|v| !v.is_null()).cloned()) };
    if let Some(v) = parse("zooms")? {
        if v.as_str() == Some("auto") {
            doc.zooms_edited = false;
        } else {
            doc.zooms = serde_json::from_value(v).map_err(|e| anyhow::anyhow!("zooms 格式不对：{e}"))?;
            doc.zooms_edited = true;
        }
        changed.push("zooms");
    }
    if let Some(v) = parse("masks")? {
        doc.masks = serde_json::from_value(v).map_err(|e| anyhow::anyhow!("masks 格式不对：{e}"))?;
        changed.push("masks");
    }
    if let Some(v) = parse("cuts")? {
        doc.cuts = serde_json::from_value(v).map_err(|e| anyhow::anyhow!("cuts 格式不对：{e}"))?;
        changed.push("cuts");
    }
    if let Some(v) = parse("narration")? {
        doc.narration = serde_json::from_value(v).map_err(|e| anyhow::anyhow!("narration 格式不对：{e}"))?;
        changed.push("narration");
    }
    // 显式传 null 表示恢复「按插件设置」
    if let Some(v) = args.get("burn_subtitles") {
        doc.burn_subtitles = v.as_bool();
        changed.push("burn_subtitles");
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patch_replaces_only_given_fields_and_auto_resets_zooms() {
        let mut doc = TimelineDoc::default();
        let changed = apply_patch(&mut doc, &json!({
            "zooms": [{"start": 1, "end": 3, "scale": 2, "focus": [0.3, 0.4]}],
            "narration": [{"at": 2, "text": "点击保存"}],
        }))
        .unwrap();
        assert_eq!(changed, vec!["zooms", "narration"]);
        assert!(doc.zooms_edited);
        assert_eq!(doc.zooms[0].focus, Some([0.3, 0.4]));
        apply_patch(&mut doc, &json!({"zooms": "auto"})).unwrap();
        assert!(!doc.zooms_edited);
        assert_eq!(doc.narration.len(), 1);
    }

    #[test]
    fn normalize_rejects_bad_input_and_clamps_times() {
        let dir = std::env::temp_dir();
        let mut doc = TimelineDoc { masks: vec![MaskRegion { start: 0.0, end: 99.0, rect: [0.1, 0.1, 0.2, 0.2] }], ..Default::default() };
        normalize(&mut doc, 10.0, &dir).unwrap();
        assert_eq!(doc.masks[0].end, 10.0);
        let mut bad = TimelineDoc { masks: vec![MaskRegion { start: 0.0, end: 1.0, rect: [0.9, 0.1, 0.5, 0.2] }], ..Default::default() };
        assert!(normalize(&mut bad, 10.0, &dir).is_err());
        let mut missing = TimelineDoc { narration: vec![Narration { at: 1.0, text: "x".into(), audio: Some("audio/nope.mp3".into()) }], ..Default::default() };
        assert!(normalize(&mut missing, 10.0, &dir).unwrap_err().to_string().contains("不存在"));
        let mut all_cut = TimelineDoc { cuts: vec![Cut { start: 0.0, end: 10.0 }], ..Default::default() };
        assert!(normalize(&mut all_cut, 10.0, &dir).is_err());
    }
}
