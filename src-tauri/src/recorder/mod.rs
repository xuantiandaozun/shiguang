//! 操作录屏：录原始画面 + 事件轨，导出时再做放大、光标、打码（设计见 docs/RECORDING.md）。
//! 工具描述、设置、页面脚本、Skill、界面都在 plugins/recorder；这里只有录制会话、写盘和渲染。

pub mod doc;
pub mod ffmpeg;
pub mod narration;
pub mod render;
pub mod timeline;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

pub const TOOL_NAMES: &[&str] = &[
    "record_start",
    "record_stop",
    "record_status",
    "record_mark",
    "record_list",
    "record_export",
    "record_delete",
    "record_timeline",
    "record_edit",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mark {
    pub t_ms: f64,
    pub label: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Meta {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub channel: String,
    pub started_at: String,
    /// 第一帧的毫秒时间戳（事件时间的零点）
    pub t0_ms: f64,
    pub duration_ms: f64,
    pub frame_count: u64,
    pub event_count: u64,
    pub click_count: u64,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub end_reason: Option<String>,
    #[serde(default)]
    pub marks: Vec<Mark>,
    #[serde(default)]
    pub exported: Option<String>,
    #[serde(default)]
    pub exported_at: Option<String>,
}

#[derive(Default)]
struct Stats {
    frames: AtomicU64,
    events: AtomicU64,
    clicks: AtomicU64,
    first_frame_ms: Mutex<Option<f64>>,
    size: Mutex<(u32, u32)>,
    end_reason: Mutex<Option<String>>,
}

struct Active {
    id: String,
    title: String,
    url: String,
    dir: PathBuf,
    channel: &'static str,
    started_at: String,
    started: std::time::Instant,
    stats: Arc<Stats>,
    marks: Vec<Mark>,
    writer: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
pub struct RecorderHub {
    active: tokio::sync::Mutex<Option<Active>>,
    export_cancel: Mutex<Option<Arc<AtomicBool>>>,
}

fn now_ms() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64
}

fn recordings_dir(app: &AppHandle) -> Result<PathBuf> {
    let dir = app.path().app_data_dir()?.join("recordings");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn settings(app: &AppHandle) -> Value {
    crate::plugins::settings::values(app, "recorder")
}

fn lines(v: &Value) -> Vec<String> {
    v.as_str()
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// 页面脚本 + 安装调用（打码规则来自插件设置）
fn page_script(app: &AppHandle) -> Result<String> {
    let src = crate::plugins::snapshot().asset("recorder", "page_recorder")?;
    let s = settings(app);
    let flag = |k: &str| s.get(k).and_then(Value::as_bool).unwrap_or(true);
    let cfg = json!({
        "presets": {
            "phone": flag("mask_phone"), "idcard": flag("mask_idcard"), "email": flag("mask_email"),
            "bankcard": flag("mask_bankcard"), "secret": flag("mask_secret"),
        },
        "selectors": lines(&s["mask_selectors"]),
        "patterns": lines(&s["mask_patterns"]),
        "blur": s.get("mask_blur").and_then(Value::as_f64).unwrap_or(10.0),
        "events": true,
    });
    Ok(format!("{src}\n;window.__sgRecInstall({cfg});"))
}

fn emit_changed(app: &AppHandle) {
    let _ = app.emit("recorder-changed", ());
}

impl RecorderHub {
    pub async fn start(&self, app: &AppHandle, title: Option<&str>, url: Option<&str>) -> Result<Value> {
        let mut active = self.active.lock().await;
        if let Some(a) = active.as_ref() {
            bail!("已经在录制「{}」，先停止再开始新的", a.title);
        }
        let state = app.state::<crate::AppState>();
        if let Some(url) = url.map(str::trim).filter(|u| !u.is_empty()) {
            state.browser.call("navigate", json!({"url": url})).await?;
        }
        let id = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let dir = recordings_dir(app)?.join(&id);
        std::fs::create_dir_all(dir.join("frames"))?;
        let script = page_script(app)?;
        let params = json!({"session": id, "script": script, "quality": 85, "max_width": 1920, "max_height": 1080});
        let (info, rx, channel) = match state.browser.record_start(params).await {
            Ok(v) => v,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        let page_title = info.get("title").and_then(Value::as_str).unwrap_or("").to_string();
        let title = title
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| if page_title.is_empty() { format!("录屏 {id}") } else { page_title });
        let page_url = info.get("url").and_then(Value::as_str).unwrap_or("").to_string();
        let stats = Arc::new(Stats::default());
        let max_min = settings(app).get("max_minutes").and_then(Value::as_f64).unwrap_or(30.0).clamp(1.0, 240.0);
        let writer = tokio::spawn(write_loop(app.clone(), id.clone(), dir.clone(), rx, stats.clone(), max_min * 60_000.0));
        *active = Some(Active {
            id: id.clone(),
            title: title.clone(),
            url: page_url.clone(),
            dir,
            channel,
            started_at: chrono::Local::now().to_rfc3339(),
            started: std::time::Instant::now(),
            stats,
            marks: Vec::new(),
            writer,
        });
        drop(active);
        emit_changed(app);
        Ok(json!({
            "id": id, "title": title, "url": page_url, "channel": channel,
            "note": "已开始录制当前标签页。敏感信息在页面上已模糊。录制中不要切换标签页。",
        }))
    }

    pub async fn stop(&self, app: &AppHandle) -> Result<Value> {
        let Some(a) = self.active.lock().await.take() else {
            bail!("当前没有在录制");
        };
        let state = app.state::<crate::AppState>();
        if let Err(e) = state.browser.record_stop(a.channel, &a.id).await {
            log::warn!("停止浏览器录屏: {e:#}");
        }
        let mut writer = a.writer;
        if tokio::time::timeout(std::time::Duration::from_secs(5), &mut writer).await.is_err() {
            writer.abort();
        }
        let stats = &a.stats;
        let frames = stats.frames.load(Ordering::SeqCst);
        if frames == 0 {
            let _ = std::fs::remove_dir_all(&a.dir);
            emit_changed(app);
            bail!("没有录到画面（页面一直没有变化，或浏览器没有推送画面）。录制已丢弃");
        }
        let t0 = stats.first_frame_ms.lock().unwrap().unwrap_or_else(now_ms);
        let (width, height) = *stats.size.lock().unwrap();
        let meta = Meta {
            id: a.id.clone(),
            title: a.title,
            url: a.url,
            channel: a.channel.to_string(),
            started_at: a.started_at,
            t0_ms: t0,
            duration_ms: (now_ms() - t0).max(500.0),
            frame_count: frames,
            event_count: stats.events.load(Ordering::SeqCst),
            click_count: stats.clicks.load(Ordering::SeqCst),
            width,
            height,
            end_reason: stats.end_reason.lock().unwrap().clone(),
            marks: a.marks,
            exported: None,
            exported_at: None,
        };
        save_meta(&a.dir, &meta)?;
        // 打过标记且写了说明的，作为解说词草稿
        let drafts: Vec<narration::Narration> = meta
            .marks
            .iter()
            .filter(|m| !m.note.trim().is_empty())
            .map(|m| narration::Narration { at: ((m.t_ms - t0) / 1000.0).max(0.0), text: m.note.trim().to_string(), audio: None })
            .collect();
        if !drafts.is_empty() && !doc::exists(&a.dir) {
            let _ = doc::save(&a.dir, &doc::TimelineDoc { narration: drafts, ..Default::default() });
        }
        emit_changed(app);
        Ok(json!({
            "id": meta.id, "title": meta.title,
            "duration_s": (meta.duration_ms / 100.0).round() / 10.0,
            "frames": meta.frame_count, "clicks": meta.click_count, "marks": meta.marks.len(),
            "end_reason": meta.end_reason,
            "dir": a.dir.to_string_lossy(),
            "note": "已保存。用 record_export 导出 MP4。",
        }))
    }

    pub async fn status(&self) -> Value {
        match self.active.lock().await.as_ref() {
            Some(a) => json!({
                "recording": true, "id": a.id, "title": a.title, "url": a.url, "channel": a.channel,
                "elapsed_s": a.started.elapsed().as_secs(),
                "frames": a.stats.frames.load(Ordering::SeqCst),
                "events": a.stats.events.load(Ordering::SeqCst),
                "clicks": a.stats.clicks.load(Ordering::SeqCst),
                "marks": a.marks.len(),
                "ended_reason": a.stats.end_reason.lock().unwrap().clone(),
            }),
            None => json!({"recording": false}),
        }
    }

    pub async fn mark(&self, label: &str, note: &str) -> Result<Value> {
        let mut guard = self.active.lock().await;
        let a = guard.as_mut().ok_or_else(|| anyhow!("当前没有在录制"))?;
        let label = label.trim();
        if label.is_empty() {
            bail!("标记需要 label");
        }
        a.marks.push(Mark { t_ms: now_ms(), label: label.to_string(), note: note.trim().to_string() });
        Ok(json!({"marked": label, "index": a.marks.len(), "elapsed_s": a.started.elapsed().as_secs()}))
    }

    async fn recording_id(&self) -> Option<String> {
        self.active.lock().await.as_ref().map(|a| a.id.clone())
    }
}

/// 接收浏览器推来的画面帧与事件并写盘。
async fn write_loop(
    app: AppHandle,
    id: String,
    dir: PathBuf,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Value>,
    stats: Arc<Stats>,
    max_ms: f64,
) {
    let open = |name: &str| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(name))
            .map(std::io::BufWriter::new)
    };
    let (mut frames_log, mut events_log) = match (open("frames.jsonl"), open("events.jsonl")) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            *stats.end_reason.lock().unwrap() = Some("无法写入录制目录".into());
            return;
        }
    };
    let started = now_ms();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut end: Option<String> = None;
    loop {
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                if msg.get("session").and_then(Value::as_str).is_some_and(|s| s != id) {
                    continue;
                }
                match msg.get("event").and_then(Value::as_str) {
                    Some("rec_frame") => {
                        if let Err(e) = write_frame(&dir, &msg, &stats, &mut frames_log) {
                            log::warn!("写入录屏画面失败: {e:#}");
                        }
                    }
                    Some("rec_event") => {
                        let Some(payload) = msg.get("payload").and_then(Value::as_str) else { continue };
                        let Ok(v) = serde_json::from_str::<Value>(payload) else { continue };
                        if v.get("type").and_then(Value::as_str) == Some("click") {
                            stats.clicks.fetch_add(1, Ordering::SeqCst);
                        }
                        stats.events.fetch_add(1, Ordering::SeqCst);
                        let _ = writeln!(events_log, "{v}");
                    }
                    Some("rec_end") => {
                        end = Some(msg.get("reason").and_then(Value::as_str).unwrap_or("浏览器结束了录制").to_string());
                        break;
                    }
                    _ => {}
                }
            }
            _ = tick.tick() => {
                let _ = frames_log.flush();
                let _ = events_log.flush();
                if now_ms() - started > max_ms {
                    end = Some("达到最长录制时长，已自动停止".into());
                    break;
                }
            }
        }
    }
    let _ = frames_log.flush();
    let _ = events_log.flush();
    if let Some(reason) = end {
        *stats.end_reason.lock().unwrap() = Some(reason);
        // 意外结束：自动收尾保存（stop 会等本任务结束，所以另起任务）
        tauri::async_runtime::spawn(async move {
            let state = app.state::<crate::AppState>();
            if let Err(e) = state.recorder.stop(&app).await {
                log::warn!("录制自动结束: {e:#}");
            }
        });
    }
}

fn write_frame(dir: &Path, msg: &Value, stats: &Stats, log: &mut impl Write) -> Result<()> {
    let data = msg.get("data").and_then(Value::as_str).ok_or_else(|| anyhow!("画面帧没有数据"))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()?
        .into_dimensions()?;
    let t = msg
        .pointer("/metadata/timestamp")
        .and_then(Value::as_f64)
        .map(|s| s * 1000.0)
        .unwrap_or_else(now_ms);
    let i = stats.frames.fetch_add(1, Ordering::SeqCst) + 1;
    std::fs::write(dir.join("frames").join(format!("{i:06}.jpg")), &bytes)?;
    writeln!(log, "{}", json!({"i": i, "t": t, "w": w, "h": h}))?;
    let mut first = stats.first_frame_ms.lock().unwrap();
    if first.is_none() {
        *first = Some(t);
        *stats.size.lock().unwrap() = (w, h);
    }
    Ok(())
}

fn save_meta(dir: &Path, meta: &Meta) -> Result<()> {
    std::fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(meta)?)?;
    Ok(())
}

fn load_meta(dir: &Path) -> Result<Meta> {
    let text = std::fs::read_to_string(dir.join("meta.json")).with_context(|| format!("录制 {} 不完整", dir.display()))?;
    Ok(serde_json::from_str(&text)?)
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    let Ok(f) = std::fs::File::open(path) else { return Vec::new() };
    std::io::BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

fn valid_id(id: &str) -> Result<&str> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        bail!("录制 id 无效：{id}");
    }
    Ok(id)
}

pub fn list(app: &AppHandle, limit: usize) -> Result<Vec<Value>> {
    let root = recordings_dir(app)?;
    let mut items: Vec<(String, Value)> = std::fs::read_dir(&root)?
        .flatten()
        .filter_map(|e| {
            let dir = e.path();
            let meta = load_meta(&dir).ok()?;
            let exported_exists = meta.exported.as_ref().is_some_and(|p| Path::new(p).is_file());
            Some((
                meta.id.clone(),
                json!({
                    "id": meta.id, "title": meta.title, "url": meta.url, "started_at": meta.started_at,
                    "duration_s": (meta.duration_ms / 100.0).round() / 10.0,
                    "clicks": meta.click_count, "marks": meta.marks.len(),
                    "size": [meta.width, meta.height],
                    "end_reason": meta.end_reason,
                    "exported": if exported_exists { meta.exported.clone() } else { None },
                    "exported_at": if exported_exists { meta.exported_at.clone() } else { None },
                    "thumb": dir.join("frames").join("000001.jpg").to_string_lossy(),
                    "dir": dir.to_string_lossy(),
                }),
            ))
        })
        .collect();
    items.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(items.into_iter().take(limit).map(|(_, v)| v).collect())
}

pub async fn delete(app: &AppHandle, id: &str) -> Result<()> {
    let id = valid_id(id)?;
    let state = app.state::<crate::AppState>();
    if state.recorder.recording_id().await.as_deref() == Some(id) {
        bail!("这个录制还在进行中，先停止");
    }
    let dir = recordings_dir(app)?.join(id);
    if !dir.is_dir() {
        bail!("没有找到录制 {id}");
    }
    std::fs::remove_dir_all(&dir)?;
    emit_changed(app);
    Ok(())
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    let s = s.trim().trim_end_matches('.').to_string();
    s.chars().take(60).collect()
}

fn latest_id(app: &AppHandle) -> Result<String> {
    list(app, 1)?
        .first()
        .and_then(|v| v["id"].as_str().map(str::to_string))
        .ok_or_else(|| anyhow!("还没有任何录制"))
}

/// 导出 MP4。overrides 可覆盖设置里的 zoom/zoom_scale/cursor_scale/style/fps/out_path。
pub async fn export(app: &AppHandle, id: Option<&str>, overrides: &Value, cancel: Arc<AtomicBool>) -> Result<Value> {
    let state = app.state::<crate::AppState>();
    let id = match id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => valid_id(id)?.to_string(),
        None => latest_id(app)?,
    };
    if state.recorder.recording_id().await.as_deref() == Some(id.as_str()) {
        bail!("这个录制还在进行中，先停止再导出");
    }
    {
        let mut slot = state.recorder.export_cancel.lock().unwrap();
        if slot.is_some() {
            bail!("已有导出在进行，等它完成或先取消");
        }
        *slot = Some(cancel.clone());
    }
    let result = export_inner(app, &id, overrides, cancel).await;
    *state.recorder.export_cancel.lock().unwrap() = None;
    let _ = app.emit("recorder-export-progress", json!({"id": id, "phase": "done", "ok": result.is_ok()}));
    emit_changed(app);
    result
}

/// 读取录制的源帧与事件
fn load_sources(dir: &Path, meta: &Meta) -> (Vec<render::SourceFrame>, Vec<timeline::Event>) {
    let mut sources: Vec<render::SourceFrame> = read_jsonl(&dir.join("frames.jsonl"))
        .iter()
        .filter_map(|v| {
            let i = v.get("i")?.as_u64()?;
            let t = v.get("t")?.as_f64()?;
            Some(render::SourceFrame { t: (t - meta.t0_ms) / 1000.0, file: dir.join("frames").join(format!("{i:06}.jpg")) })
        })
        .filter(|f| f.file.is_file())
        .collect();
    sources.sort_by(|a, b| a.t.total_cmp(&b.t));
    let mut events: Vec<timeline::Event> = read_jsonl(&dir.join("events.jsonl"))
        .iter()
        .filter_map(|v| timeline::Event::parse(v, meta.t0_ms))
        .collect();
    events.sort_by(|a, b| a.t.total_cmp(&b.t));
    (sources, events)
}

struct ExportOpts {
    fps: u32,
    zoom: bool,
    zoom_scale: f64,
    cursor_scale: f64,
    style: render::Style,
    burn: bool,
    preview: bool,
}

fn export_opts(app: &AppHandle, overrides: &Value, doc: &doc::TimelineDoc) -> ExportOpts {
    let s = settings(app);
    let pick = |k: &str| overrides.get(k).filter(|v| !v.is_null()).or_else(|| s.get(k)).cloned().unwrap_or(Value::Null);
    let preview = overrides.get("preview").and_then(Value::as_bool).unwrap_or(false);
    ExportOpts {
        fps: if preview { 20 } else { pick("fps").as_u64().unwrap_or(30).clamp(10, 60) as u32 },
        zoom: pick("zoom").as_bool().or_else(|| s.get("zoom_auto").and_then(Value::as_bool)).unwrap_or(true),
        zoom_scale: pick("zoom_scale").as_f64().unwrap_or(1.8).clamp(1.2, 3.0),
        cursor_scale: pick("cursor_scale").as_f64().unwrap_or(1.6).clamp(0.5, 4.0),
        style: serde_json::from_value(pick("style")).unwrap_or(render::Style::Plain),
        burn: overrides
            .get("burn_subtitles")
            .and_then(Value::as_bool)
            .or(doc.burn_subtitles)
            .or_else(|| s.get("burn_subtitles").and_then(Value::as_bool))
            .unwrap_or(true),
        preview,
    }
}

async fn export_inner(app: &AppHandle, id: &str, overrides: &Value, cancel: Arc<AtomicBool>) -> Result<Value> {
    let dir = recordings_dir(app)?.join(id);
    let mut meta = load_meta(&dir)?;
    let s = settings(app);
    let mut tl = doc::load(&dir);
    let opts = export_opts(app, overrides, &tl);
    let (sources, events) = load_sources(&dir, &meta);
    if sources.is_empty() {
        bail!("录制 {id} 没有画面");
    }
    let duration = meta.duration_ms / 1000.0;
    let compiled = timeline::compile(
        &events,
        duration,
        timeline::Opts { fps: opts.fps, zoom: opts.zoom, zoom_scale: opts.zoom_scale },
        tl.zooms_edited.then_some(tl.zooms.as_slice()),
    );
    if !tl.zooms_edited {
        // 记下自动生成的放大段，编辑器在此基础上改
        tl.zooms = compiled.zooms.clone();
        doc::save(&dir, &tl)?;
    }

    let tools_dir = app.path().app_data_dir()?.join("tools");
    let urls = lines(&s["ffmpeg_urls"]);
    let configured = s.get("ffmpeg_path").and_then(Value::as_str).unwrap_or("").to_string();
    let app2 = app.clone();
    let id2 = id.to_string();
    let ffmpeg = ffmpeg::ensure(&configured, &tools_dir, &urls, move |got, total| {
        let _ = app2.emit("recorder-export-progress", json!({"id": id2, "phase": "ffmpeg", "done": got, "total": total}));
    })
    .await?;
    if cancel.load(Ordering::Relaxed) {
        bail!("导出已取消");
    }

    // 配音：解码得到真实时长，排期（必要时定格画面），混成一条音轨
    let _ = app.emit("recorder-export-progress", json!({"id": id, "phase": "audio"}));
    let (plan, audio_path) = {
        let items = tl.narration.clone();
        let cuts: Vec<(f64, f64)> = tl.cuts.iter().map(|c| (c.start, c.end)).collect();
        let (ff, d) = (ffmpeg.clone(), dir.clone());
        tokio::task::spawn_blocking(move || -> Result<(narration::Plan, Option<PathBuf>)> {
            let mut pcms: Vec<Option<Vec<i16>>> = Vec::with_capacity(items.len());
            for it in &items {
                pcms.push(match &it.audio {
                    Some(a) => Some(narration::decode(&ff, &narration::resolve_audio(&d, a))?),
                    None => None,
                });
            }
            let durations: Vec<f64> = items
                .iter()
                .zip(&pcms)
                .map(|(it, p)| p.as_ref().map(|p| narration::pcm_seconds(p)).unwrap_or_else(|| narration::estimate(&it.text)))
                .collect();
            let plan = narration::plan(duration, &cuts, &items, &durations);
            let clips: Vec<(f64, &[i16])> = plan
                .slots
                .iter()
                .zip(&pcms)
                .filter_map(|(slot, p)| Some((slot.as_ref()?.0, p.as_deref()?)))
                .collect();
            let audio = if clips.is_empty() {
                None
            } else {
                let path = d.join("narration.wav");
                narration::write_mix(&path, plan.map.output_duration(), &clips)?;
                Some(path)
            };
            Ok((plan, audio))
        })
        .await
        .map_err(|e| anyhow!("处理配音失败：{e}"))??
    };

    let out = if opts.preview {
        dir.join("preview.mp4")
    } else {
        match overrides.get("out_path").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => PathBuf::from(p),
            None => {
                let base = s
                    .get("output_dir")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
                    .map(PathBuf::from)
                    .or_else(|| dirs::video_dir().map(|d| d.join("拾光录屏")))
                    .unwrap_or_else(|| dir.clone());
                base.join(format!("{}-{id}.mp4", sanitize(&meta.title)))
            }
        }
    };
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let srt_path = out.with_extension("srt");
    if !plan.cues.is_empty() {
        std::fs::write(&srt_path, narration::srt(&plan.cues))?;
    }

    let (mut w, mut h) = if meta.width > 0 { (meta.width, meta.height) } else { (1280, 720) };
    if opts.preview && w > 960 {
        h = (h as f64 * 960.0 / w as f64).round() as u32;
        w = 960;
    }
    let encoder = {
        let f = ffmpeg.clone();
        tokio::task::spawn_blocking(move || render::pick_encoder(&f)).await?
    };
    let fps = opts.fps as f64;
    let out_dur = plan.map.output_duration();
    let n_out = ((out_dur * fps).ceil() as usize).max(1);
    let last = compiled.frames.len().saturating_sub(1);
    let frame_map: Vec<(usize, f64)> = (0..n_out)
        .map(|k| {
            let t = k as f64 / fps;
            (((plan.map.to_source(t) * fps).round() as usize).min(last), t)
        })
        .collect();
    let zoom_count = compiled.zooms.len();
    let cue_count = plan.cues.len();
    let job = render::Job {
        ffmpeg,
        encoder: encoder.clone(),
        out: out.clone(),
        width: w - w % 2,
        height: h - h % 2,
        sources,
        compiled,
        masks: tl.masks.clone(),
        opts: render::RenderOpts { fps: opts.fps, cursor_scale: opts.cursor_scale, style: opts.style },
        frame_map,
        cues: if opts.burn { plan.cues } else { Vec::new() },
        audio: audio_path.clone(),
        fast: opts.preview,
    };
    let app3 = app.clone();
    let id3 = id.to_string();
    tokio::task::spawn_blocking(move || {
        render::run(job, cancel, |done, total| {
            let _ = app3.emit("recorder-export-progress", json!({"id": id3, "phase": "render", "done": done, "total": total}));
        })
    })
    .await
    .map_err(|e| anyhow!("渲染线程异常：{e}"))??;
    if let Some(a) = audio_path {
        let _ = std::fs::remove_file(a);
    }

    if !opts.preview {
        meta.exported = Some(out.to_string_lossy().to_string());
        meta.exported_at = Some(chrono::Local::now().to_rfc3339());
        save_meta(&dir, &meta)?;
    }
    Ok(json!({
        "id": id, "path": out.to_string_lossy(), "preview": opts.preview,
        "duration_s": (out_dur * 10.0).round() / 10.0,
        "zoom_segments": zoom_count, "subtitles": cue_count,
        "srt": if cue_count > 0 { Some(srt_path.to_string_lossy().to_string()) } else { None },
        "encoder": encoder.get(1),
        "note": if opts.preview { "这是低分辨率预览。" } else { "已导出。请用户看一遍成片，确认没有漏掉的敏感信息再对外发。" },
    }))
}

/// 给模型看的时间线：步骤标记、点击/输入/页面切换、当前编辑内容和配音目录
pub fn timeline_summary(app: &AppHandle, id: &str) -> Result<Value> {
    let dir = recordings_dir(app)?.join(valid_id(id)?);
    let meta = load_meta(&dir)?;
    let tl = doc::load(&dir);
    let r1 = |t: f64| (t * 10.0).round() / 10.0;
    let mut steps = Vec::new();
    for v in read_jsonl(&dir.join("events.jsonl")) {
        let Some(t) = v.get("t").and_then(Value::as_f64) else { continue };
        let at = r1((t - meta.t0_ms) / 1000.0);
        let label = v.pointer("/target/label").and_then(Value::as_str).unwrap_or("").to_string();
        match v.get("type").and_then(Value::as_str) {
            Some("click") => steps.push(json!({"at": at, "action": "点击", "target": label})),
            Some("focus") => steps.push(json!({"at": at, "action": "进入输入框", "target": label})),
            Some("page") => steps.push(json!({"at": at, "action": "打开页面", "target": v.get("title").and_then(Value::as_str).unwrap_or("")})),
            _ => {}
        }
        if steps.len() >= 300 {
            break;
        }
    }
    let audio_dir = dir.join("audio");
    std::fs::create_dir_all(&audio_dir)?;
    Ok(json!({
        "id": meta.id, "title": meta.title, "url": meta.url,
        "duration_s": r1(meta.duration_ms / 1000.0),
        "marks": meta.marks.iter().map(|m| json!({"at": r1((m.t_ms - meta.t0_ms) / 1000.0), "label": m.label, "note": m.note})).collect::<Vec<_>>(),
        "steps": steps,
        "timeline": tl,
        "audio_dir": audio_dir.to_string_lossy(),
        "hint": "时间都是录制开始后的秒数。配音文件放进 audio_dir，再用 record_edit 的 narration[].audio 填相对路径（如 audio/01.mp3）。",
    }))
}

/// 修改时间线（AI 工具与界面共用）：先在副本上改并校验，通过才保存
pub fn edit_timeline(app: &AppHandle, id: &str, patch: &Value) -> Result<(doc::TimelineDoc, Vec<&'static str>)> {
    let dir = recordings_dir(app)?.join(valid_id(id)?);
    let meta = load_meta(&dir)?;
    let mut tl = doc::load(&dir);
    let changed = doc::apply_patch(&mut tl, patch)?;
    doc::normalize(&mut tl, meta.duration_ms / 1000.0, &dir)?;
    doc::save(&dir, &tl)?;
    emit_changed(app);
    Ok((tl, changed))
}

/// 编辑器用：源帧时间表、点击与标记、时间线（未手动编辑时给出自动放大段）
pub fn project(app: &AppHandle, id: &str) -> Result<Value> {
    let dir = recordings_dir(app)?.join(valid_id(id)?);
    let meta = load_meta(&dir)?;
    let mut tl = doc::load(&dir);
    let (sources, events) = load_sources(&dir, &meta);
    let duration = meta.duration_ms / 1000.0;
    if !tl.zooms_edited {
        let s = settings(app);
        let zoom_scale = s.get("zoom_scale").and_then(Value::as_f64).unwrap_or(1.8);
        let auto = s.get("zoom_auto").and_then(Value::as_bool).unwrap_or(true);
        tl.zooms = if auto { timeline::zoom_segments(&events, zoom_scale) } else { Vec::new() };
    }
    let r2 = |t: f64| (t * 100.0).round() / 100.0;
    let frames: Vec<Value> = sources
        .iter()
        .map(|f| json!([r2(f.t), f.file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()]))
        .collect();
    let clicks: Vec<Value> = read_jsonl(&dir.join("events.jsonl"))
        .iter()
        .filter(|v| v.get("type").and_then(Value::as_str) == Some("click"))
        .filter_map(|v| {
            let e = timeline::Event::parse(v, meta.t0_ms)?;
            Some(json!({"t": r2(e.t), "label": v.pointer("/target/label").and_then(Value::as_str).unwrap_or("")}))
        })
        .collect();
    let preview = dir.join("preview.mp4");
    Ok(json!({
        "meta": meta, "duration": duration, "dir": dir.to_string_lossy(),
        "frames": frames, "clicks": clicks,
        "marks": meta.marks.iter().map(|m| json!({"t": r2((m.t_ms - meta.t0_ms) / 1000.0), "label": m.label, "note": m.note})).collect::<Vec<_>>(),
        "timeline": tl,
        "preview": if preview.is_file() { Some(preview.to_string_lossy().to_string()) } else { None },
        "burn_default": settings(app).get("burn_subtitles").and_then(Value::as_bool).unwrap_or(true),
    }))
}

// ---------- AI 工具 ----------

pub async fn execute(
    app: &AppHandle,
    name: &str,
    args: &Value,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Value> {
    let state = app.state::<crate::AppState>();
    let hub = &state.recorder;
    let str_arg = |k: &str| args.get(k).and_then(Value::as_str);
    match name {
        "record_start" => hub.start(app, str_arg("title"), str_arg("url")).await,
        "record_stop" => hub.stop(app).await,
        "record_status" => Ok(hub.status().await),
        "record_mark" => hub.mark(str_arg("label").unwrap_or(""), str_arg("note").unwrap_or("")).await,
        "record_list" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 100) as usize;
            Ok(json!({"recordings": list(app, limit)?}))
        }
        "record_export" => {
            let flag = Arc::new(AtomicBool::new(false));
            let watcher = {
                let (flag, cancel) = (flag.clone(), cancel.clone());
                tokio::spawn(async move {
                    cancel.cancelled().await;
                    flag.store(true, Ordering::Relaxed);
                })
            };
            let r = export(app, str_arg("id"), args, flag).await;
            watcher.abort();
            r
        }
        "record_delete" => {
            let id = valid_id(str_arg("id").unwrap_or(""))?.to_string();
            let meta = load_meta(&recordings_dir(app)?.join(&id))?;
            let prompt = json!({"questions":[{
                "id": "delete", "header": "录屏",
                "question": format!("删除录制「{}」的原始素材？", meta.title),
                "details": format!("时长 {:.1} 秒。已导出的 MP4 不会删除。", meta.duration_ms / 1000.0),
                "options": [{"label": "删除"}, {"label": "保留"}],
            }]});
            let outcome = crate::ask_user::execute(app, &prompt, cancel).await?;
            let ok = outcome
                .pointer("/answers/0/selected")
                .and_then(Value::as_array)
                .is_some_and(|s| s.iter().any(|v| v.as_str() == Some("删除")));
            if !ok {
                return Ok(json!({"deleted": false, "note": "用户选择保留"}));
            }
            delete(app, &id).await?;
            Ok(json!({"deleted": true, "id": id}))
        }
        "record_timeline" => {
            let id = match str_arg("id").filter(|s| !s.trim().is_empty()) {
                Some(id) => id.to_string(),
                None => latest_id(app)?,
            };
            timeline_summary(app, &id)
        }
        "record_edit" => {
            let id = match str_arg("id").filter(|s| !s.trim().is_empty()) {
                Some(id) => id.to_string(),
                None => latest_id(app)?,
            };
            let (tl, changed) = edit_timeline(app, &id, args)?;
            if changed.is_empty() {
                bail!("没有要修改的字段（可改 narration / masks / cuts / zooms / burn_subtitles）");
            }
            Ok(json!({
                "id": id, "changed": changed,
                "narration": tl.narration.len(),
                "with_audio": tl.narration.iter().filter(|n| n.audio.is_some()).count(),
                "masks": tl.masks.len(), "cuts": tl.cuts.len(), "zooms": tl.zooms.len(), "zooms_edited": tl.zooms_edited,
                "note": "已保存。可以用 record_export 的 preview: true 先出低分辨率预览。",
            }))
        }
        _ => bail!("未知录屏工具 {name}"),
    }
}

// ---------- 界面命令 ----------

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[tauri::command]
pub async fn recorder_status(app: AppHandle) -> Result<Value, String> {
    Ok(app.state::<crate::AppState>().recorder.status().await)
}

#[tauri::command]
pub async fn recorder_list(app: AppHandle) -> Result<Vec<Value>, String> {
    list(&app, 200).map_err(err)
}

#[tauri::command]
pub async fn recorder_start(app: AppHandle, title: Option<String>, url: Option<String>) -> Result<Value, String> {
    let state = app.state::<crate::AppState>();
    state.recorder.start(&app, title.as_deref(), url.as_deref()).await.map_err(err)
}

#[tauri::command]
pub async fn recorder_stop(app: AppHandle) -> Result<Value, String> {
    app.state::<crate::AppState>().recorder.stop(&app).await.map_err(err)
}

#[tauri::command]
pub async fn recorder_export(app: AppHandle, id: String, opts: Option<Value>) -> Result<Value, String> {
    let opts = opts.unwrap_or_else(|| json!({}));
    export(&app, Some(&id), &opts, Arc::new(AtomicBool::new(false))).await.map_err(err)
}

#[tauri::command]
pub async fn recorder_project(app: AppHandle, id: String) -> Result<Value, String> {
    project(&app, &id).map_err(err)
}

/// 界面保存整份时间线（各字段整体替换）
#[tauri::command]
pub async fn recorder_save_timeline(app: AppHandle, id: String, timeline: Value) -> Result<Value, String> {
    let mut patch = timeline;
    if patch.get("zooms_edited").and_then(Value::as_bool) == Some(false) {
        patch["zooms"] = json!("auto");
    }
    if let Some(obj) = patch.as_object_mut() {
        obj.remove("zooms_edited");
        // 界面传 null 表示按设置
        obj.entry("burn_subtitles").or_insert(Value::Null);
    }
    let (tl, _) = edit_timeline(&app, &id, &patch).map_err(err)?;
    serde_json::to_value(tl).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn recorder_cancel_export(app: AppHandle) -> Result<(), String> {
    if let Some(flag) = app.state::<crate::AppState>().recorder.export_cancel.lock().unwrap().as_ref() {
        flag.store(true, Ordering::Relaxed);
    }
    Ok(())
}

#[tauri::command]
pub async fn recorder_delete(app: AppHandle, id: String) -> Result<(), String> {
    delete(&app, &id).await.map_err(err)
}

/// 在资源管理器里显示文件（只允许录制目录和已导出的视频）
#[tauri::command]
pub fn recorder_reveal(app: AppHandle, path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    let in_recordings = recordings_dir(&app).map(|r| p.starts_with(r)).unwrap_or(false);
    let is_export = list(&app, 500)
        .unwrap_or_default()
        .iter()
        .any(|v| v["exported"].as_str() == Some(path.as_str()));
    if !(in_recordings || is_export) || !p.exists() {
        return Err("只能打开录制目录或导出的视频".into());
    }
    let mut cmd = std::process::Command::new("explorer");
    if p.is_file() {
        cmd.arg(format!("/select,{}", path.replace('/', "\\")));
    } else {
        cmd.arg(path.replace('/', "\\"));
    }
    cmd.spawn().map_err(|e| format!("打开失败：{e}"))?;
    Ok(())
}

#[cfg(test)]
mod e2e {
    use super::*;

    /// 用本机已安装的 ffmpeg 导出一段合成录制（不下载）：
    /// cargo test --lib recorder::e2e -- --ignored --nocapture
    #[test]
    #[ignore]
    fn export_synthetic_recording() {
        let root = std::env::temp_dir().join("sg-rec-e2e");
        let Some(ff) = ffmpeg::find_local("", &root.join("tools")) else {
            eprintln!("本机没有 ffmpeg，跳过");
            return;
        };
        let frames_dir = root.join("frames");
        std::fs::create_dir_all(&frames_dir).unwrap();
        let (w, h) = (1280u32, 720u32);
        let mut sources = Vec::new();
        for i in 0..30u32 {
            let img = image::RgbImage::from_fn(w, h, |x, y| {
                let band = ((x / 80 + y / 80 + i) % 2) as u8;
                image::Rgb([40 + band * 160, 90, 200 - band * 100])
            });
            let file = frames_dir.join(format!("{:06}.jpg", i + 1));
            img.save(&file).unwrap();
            sources.push(render::SourceFrame { t: i as f64 * 0.2, file });
        }
        let ev = |t: f64, kind: &str, pos: (f64, f64), bbox: Option<[f64; 4]>| timeline::Event { t, kind: kind.into(), pos: Some(pos), bbox };
        let events = vec![
            ev(0.1, "move", (0.5, 0.5), None),
            ev(1.5, "move", (0.25, 0.3), None),
            ev(1.6, "click", (0.25, 0.3), Some([0.2, 0.27, 0.1, 0.06])),
            ev(4.0, "move", (0.8, 0.7), None),
        ];
        let encoder = render::pick_encoder(&ff);
        eprintln!("ffmpeg = {} encoder = {:?}", ff.display(), encoder);
        for style in [render::Style::Plain, render::Style::Padded] {
            let compiled = timeline::compile(&events, 6.0, timeline::Opts { fps: 30, zoom: true, zoom_scale: 2.0 }, None);
            // 2 秒处定格 1 秒、剪掉 4–5 秒，带字幕和一段 1 秒的配音
            let items = [narration::Narration { at: 2.0, text: "点击这里，打开设置。".into(), audio: Some("tone.wav".into()) }];
            let plan = narration::plan(6.0, &[(4.0, 5.0)], &items, &[1.0]);
            let tone: Vec<i16> = (0..narration::RATE as usize * 2).map(|i| (((i / 2) as f64 * 0.06).sin() * 8000.0) as i16).collect();
            let wav = root.join("narration.wav");
            narration::write_mix(&wav, plan.map.output_duration(), &[(plan.slots[0].unwrap().0, &tone)]).unwrap();
            let n_out = (plan.map.output_duration() * 30.0).ceil() as usize;
            let frame_map = (0..n_out).map(|k| { let t = k as f64 / 30.0; (((plan.map.to_source(t) * 30.0).round() as usize).min(compiled.frames.len() - 1), t) }).collect();
            let out = root.join(format!("out-{style:?}.mp4"));
            let job = render::Job {
                ffmpeg: ff.clone(), encoder: encoder.clone(), out: out.clone(), width: w, height: h,
                sources: sources.clone(), compiled,
                masks: vec![render::MaskRegion { start: 0.0, end: 6.0, rect: [0.6, 0.1, 0.3, 0.1] }],
                opts: render::RenderOpts { fps: 30, cursor_scale: 1.6, style },
                frame_map,
                cues: plan.cues.clone(),
                audio: Some(wav.clone()),
                fast: false,
            };
            let t = std::time::Instant::now();
            render::run(job, Arc::new(AtomicBool::new(false)), |_, _| {}).unwrap();
            let size = std::fs::metadata(&out).unwrap().len();
            eprintln!("{style:?}: {size} bytes in {:?} -> {}", t.elapsed(), out.display());
            assert!(size > 10_000);
        }
    }
}
