//! 配音与字幕：解说挂在源时间点上；上一段还没说完就让画面在该点定格等待。
//! 音频统一用 ffmpeg 解码成 48k 立体声 PCM，在这里混成一条音轨（不依赖 ffmpeg 版本的混音滤镜）。

use super::timeline::TimeMap;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const RATE: u32 = 48_000;
const GAP: f64 = 0.3; // 两段解说之间至少留的空隙

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Narration {
    /// 源时间（秒）
    pub at: f64,
    #[serde(default)]
    pub text: String,
    /// 音频文件：相对录制目录（如 audio/01.mp3）或绝对路径
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

pub struct Plan {
    pub map: TimeMap,
    /// 每段解说在输出时间轴上的开始时间与时长（与输入顺序一致，跳过的为 None）
    pub slots: Vec<Option<(f64, f64)>>,
    pub cues: Vec<Cue>,
}

pub const AUDIO_EXT: &[&str] = &["mp3", "wav", "m4a", "aac", "ogg", "opus", "flac"];

pub fn resolve_audio(dir: &Path, audio: &str) -> PathBuf {
    let p = PathBuf::from(audio.trim());
    if p.is_absolute() {
        p
    } else {
        dir.join(p)
    }
}

/// 只有文字没有音频时的朗读时长估计（约每秒 4 个字）
pub fn estimate(text: &str) -> f64 {
    let n = text.chars().filter(|c| !c.is_whitespace()).count() as f64;
    (n / 4.0).clamp(1.5, 30.0)
}

/// 排期：返回时间映射（剪切 + 定格）、每段的位置和字幕。durations[i] 为第 i 段的时长（无音频时用估计值）。
pub fn plan(duration: f64, cuts: &[(f64, f64)], items: &[Narration], durations: &[f64]) -> Plan {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| items[a].at.total_cmp(&items[b].at));
    let mut freezes: Vec<(f64, f64)> = Vec::new();
    let mut slots = vec![None; items.len()];
    let mut prev_end = 0.0;
    for &i in &order {
        let it = &items[i];
        let d = durations.get(i).copied().unwrap_or(0.0);
        if d <= 0.0 || (it.text.trim().is_empty() && it.audio.is_none()) {
            continue;
        }
        let at = it.at.clamp(0.0, duration);
        let anchor = TimeMap::new(duration, cuts, &freezes).to_output(at);
        let start = if anchor < prev_end {
            freezes.push((at, prev_end - anchor));
            prev_end
        } else {
            anchor
        };
        slots[i] = Some((start, d));
        prev_end = start + d + GAP;
    }
    // 最后一段说完之前不结束
    let out_dur = TimeMap::new(duration, cuts, &freezes).output_duration();
    if prev_end - GAP + 0.5 > out_dur {
        freezes.push((duration, prev_end - GAP + 0.5 - out_dur));
    }
    let map = TimeMap::new(duration, cuts, &freezes);
    let mut cues = Vec::new();
    for (i, slot) in slots.iter().enumerate() {
        if let Some((start, d)) = slot {
            cues.extend(split_cues(&items[i].text, *start, *d));
        }
    }
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    Plan { map, slots, cues }
}

/// 把一段解说按句子切成多条字幕，时长按字数分配；每条不超过约 24 个字。
pub fn split_cues(text: &str, start: f64, dur: f64) -> Vec<Cue> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in text.trim().chars() {
        if c == '\n' {
            if !cur.trim().is_empty() {
                parts.push(cur.trim().to_string());
            }
            cur.clear();
            continue;
        }
        cur.push(c);
        let len = cur.chars().count();
        let hard = "。！？；!?;".contains(c);
        let soft = "，、,：:".contains(c) && len >= 14;
        if hard || soft || len >= 24 {
            parts.push(cur.trim().to_string());
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_string());
    }
    parts.retain(|p| !p.is_empty());
    let total: f64 = parts.iter().map(|p| p.chars().count().max(1) as f64).sum();
    let mut t = start;
    parts
        .into_iter()
        .map(|p| {
            let d = dur * p.chars().count().max(1) as f64 / total;
            let text = p.trim_end_matches(['。', '，', '、', ',', '；', ';']).to_string();
            let cue = Cue { start: t, end: t + d, text };
            t += d;
            cue
        })
        .collect()
}

pub fn srt(cues: &[Cue]) -> String {
    let ts = |t: f64| {
        let ms = (t.max(0.0) * 1000.0).round() as u64;
        format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
    };
    cues.iter()
        .enumerate()
        .map(|(i, c)| format!("{}\n{} --> {}\n{}\n", i + 1, ts(c.start), ts(c.end), c.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 用 ffmpeg 把音频解码成 48k 立体声 s16 PCM
pub fn decode(ffmpeg: &Path, file: &Path) -> Result<Vec<i16>> {
    if !file.is_file() {
        bail!("音频文件不存在：{}", file.display());
    }
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-v", "error", "-i"])
        .arg(file)
        .args(["-f", "s16le", "-ac", "2", "-ar", &RATE.to_string(), "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    super::render::hide_window(&mut cmd);
    let out = cmd.output().with_context(|| format!("解码 {}", file.display()))?;
    if !out.status.success() {
        bail!("无法解码音频 {}：{}", file.display(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect())
}

pub fn pcm_seconds(pcm: &[i16]) -> f64 {
    pcm.len() as f64 / 2.0 / RATE as f64
}

/// 把各段 PCM 放到排好的位置，写成 WAV
pub fn write_mix(path: &Path, total_secs: f64, clips: &[(f64, &[i16])]) -> Result<()> {
    let total = ((total_secs * RATE as f64).ceil() as usize) * 2;
    let mut mix = vec![0i32; total];
    for &(start, pcm) in clips {
        let off = ((start * RATE as f64).round() as usize) * 2;
        for (i, &s) in pcm.iter().enumerate() {
            if let Some(m) = mix.get_mut(off + i) {
                *m += s as i32;
            }
        }
    }
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let data_len = (mix.len() * 2) as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&2u16.to_le_bytes())?; // 声道
    f.write_all(&RATE.to_le_bytes())?;
    f.write_all(&(RATE * 4).to_le_bytes())?;
    f.write_all(&4u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in mix {
        f.write_all(&(s.clamp(i16::MIN as i32, i16::MAX as i32) as i16).to_le_bytes())?;
    }
    f.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(at: f64, text: &str) -> Narration {
        Narration { at, text: text.into(), audio: None }
    }

    #[test]
    fn long_narration_freezes_video_until_done() {
        // 第一段 1 秒处说 5 秒；第二段锚在 3 秒处，要等第一段说完
        let items = [n(1.0, "第一段"), n(3.0, "第二段")];
        let p = plan(10.0, &[], &items, &[5.0, 2.0]);
        assert_eq!(p.slots[0], Some((1.0, 5.0)));
        let (s2, _) = p.slots[1].unwrap();
        assert!((s2 - 6.3).abs() < 1e-9);
        // 画面在 3 秒处定格了 3.3 秒
        assert!((p.map.output_duration() - 13.3).abs() < 1e-9);
        assert_eq!(p.map.to_source(4.0), 3.0);
        assert_eq!(p.map.to_output(3.0), 6.3);
    }

    #[test]
    fn narration_past_end_extends_video() {
        let p = plan(4.0, &[], &[n(3.0, "收尾")], &[3.0]);
        assert!((p.map.output_duration() - 6.5).abs() < 1e-9);
    }

    #[test]
    fn cut_shifts_narration_earlier() {
        let p = plan(10.0, &[(1.0, 3.0)], &[n(5.0, "剪切后")], &[1.0]);
        assert_eq!(p.slots[0], Some((3.0, 1.0)));
    }

    #[test]
    fn cues_split_by_sentence_and_share_time() {
        let cues = split_cues("先打开设置页。然后点击保存按钮！", 2.0, 4.0);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "先打开设置页");
        assert!((cues[1].end - 6.0).abs() < 1e-9);
        let s = srt(&cues);
        assert!(s.starts_with("1\n00:00:02,000 --> "));
    }

    #[test]
    fn wav_has_expected_length() {
        let path = std::env::temp_dir().join(format!("sg-mix-{}.wav", uuid::Uuid::new_v4()));
        let clip = vec![1000i16; RATE as usize * 2];
        write_mix(&path, 2.0, &[(0.5, &clip)]).unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(len, 44 + 2 * RATE as u64 * 2 * 2);
        let _ = std::fs::remove_file(path);
    }
}
