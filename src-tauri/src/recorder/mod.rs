//! 操作录屏：录原始画面 + 事件轨，导出时再做放大、光标、打码（设计见 docs/RECORDING.md）。
//! 画面来源：浏览器标签页（CDP 推帧）或桌面（desktop.rs：屏幕、窗口、区域），产出格式相同。
//! 工具描述、设置、页面脚本、Skill、界面都在 plugins/recorder；这里只有录制会话、写盘和渲染。

pub mod desktop;
pub mod doc;
pub mod ffmpeg;
pub mod narration;
pub mod render;
pub mod scene;
pub mod screenshot;
pub mod subtitle;
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
    "record_scene_preview",
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
    /// 桌面录屏的采集线程（网页录制为 None）
    desktop: Option<desktop::Session>,
    /// 录制期间把拾光窗口排除出了画面，停止时要恢复
    self_excluded: bool,
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
    /// 开始录制。args: source = browser（默认）| screen | window | region | select，
    /// 以及 title、url（browser）、window（标题或进程名）、rect、monitor、include_self
    pub async fn start(&self, app: &AppHandle, args: &Value) -> Result<Value> {
        let str_arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
        let source = str_arg("source").unwrap_or("browser");
        if source == "browser" {
            return self.start_browser(app, str_arg("title"), str_arg("url")).await;
        }
        if let Some(a) = self.active.lock().await.as_ref() {
            bail!("已经在录制「{}」，先停止再开始新的", a.title);
        }
        let include_self = args
            .get("include_self")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| settings(app).get("record_include_self").and_then(Value::as_bool).unwrap_or(false));
        let target = match source {
            "screen" => {
                let monitor = args.get("monitor").and_then(Value::as_u64).map(|n| n as usize);
                tokio::task::spawn_blocking(move || desktop::screen_target(monitor)).await??
            }
            "window" => {
                let q = str_arg("window").ok_or_else(|| anyhow!("录窗口需要 window（标题或进程名的一部分），不确定时先 list_windows"))?.to_string();
                tokio::task::spawn_blocking(move || desktop::window_target(&q, include_self)).await??
            }
            "region" => {
                let r: Vec<i32> = args
                    .get("rect")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_f64).map(|v| v.round() as i32).collect())
                    .unwrap_or_default();
                if r.len() != 4 {
                    bail!("录区域需要 rect [x, y, 宽, 高]（屏幕物理像素），或用 source=select 让用户框选");
                }
                desktop::region_target([r[0], r[1], r[2], r[3]])?
            }
            "select" => {
                let sel = screenshot::select_area(app, include_self).await?;
                tokio::task::spawn_blocking(move || desktop::from_selection(&sel)).await??
            }
            other => bail!("未知录制来源 {other}（browser / screen / window / region / select）"),
        };
        self.start_desktop(app, target, str_arg("title"), include_self).await
    }

    async fn start_desktop(&self, app: &AppHandle, target: desktop::Target, title: Option<&str>, include_self: bool) -> Result<Value> {
        let mut active = self.active.lock().await;
        if let Some(a) = active.as_ref() {
            bail!("已经在录制「{}」，先停止再开始新的", a.title);
        }
        let id = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let dir = recordings_dir(app)?.join(&id);
        std::fs::create_dir_all(dir.join("frames"))?;
        if !include_self {
            screenshot::set_self_excluded(app, true);
        }
        let s = settings(app);
        let fps = s.get("desktop_fps").and_then(Value::as_u64).unwrap_or(20) as u32;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let session = match desktop::start(&target, dir.clone(), desktop::Options { fps, include_self }, tx) {
            Ok(s) => s,
            Err(e) => {
                if !include_self {
                    screenshot::set_self_excluded(app, false);
                }
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        let title = title.map(str::to_string).unwrap_or_else(|| match target.kind {
            "window" if !target.title.is_empty() => target.title.clone(),
            _ => format!("桌面录屏 {id}"),
        });
        let stats = Arc::new(Stats::default());
        let max_min = s.get("max_minutes").and_then(Value::as_f64).unwrap_or(30.0).clamp(1.0, 240.0);
        let writer = tokio::spawn(write_loop(app.clone(), id.clone(), dir.clone(), rx, stats.clone(), max_min * 60_000.0));
        let what = target.describe();
        *active = Some(Active {
            id: id.clone(),
            title: title.clone(),
            url: what.clone(),
            dir,
            channel: "desktop",
            started_at: chrono::Local::now().to_rfc3339(),
            started: std::time::Instant::now(),
            stats,
            marks: Vec::new(),
            writer,
            desktop: Some(session),
            self_excluded: !include_self,
        });
        drop(active);
        emit_changed(app);
        let hotkey = s.get("record_hotkey").and_then(Value::as_str).unwrap_or("Ctrl+Alt+R").to_string();
        Ok(json!({
            "id": id, "title": title, "target": what, "channel": "desktop",
            "include_self": include_self,
            "note": format!(
                "已开始录制{what}。{}桌面软件不能在源头打码，录完用 record_edit 的 masks 给敏感区域补马赛克。停止：record_stop，或让用户按 {hotkey}。",
                if include_self { "拾光自己的窗口也会录进去。" } else { "拾光自己的窗口不会出现在画面里。" }
            ),
        }))
    }

    async fn start_browser(&self, app: &AppHandle, title: Option<&str>, url: Option<&str>) -> Result<Value> {
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
            desktop: None,
            self_excluded: false,
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
        if let Some(session) = a.desktop {
            // 先停采集线程：剩下的帧编码完、通道关闭后写盘任务自然结束
            let _ = tokio::task::spawn_blocking(move || session.stop()).await;
        } else {
            let state = app.state::<crate::AppState>();
            if let Err(e) = state.browser.record_stop(a.channel, &a.id).await {
                log::warn!("停止浏览器录屏: {e:#}");
            }
        }
        if a.self_excluded {
            screenshot::set_self_excluded(app, false);
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
                    Some("rec_frame_saved") => {
                        let (Some(i), Some(t)) = (msg.get("i").and_then(Value::as_u64), msg.get("t").and_then(Value::as_f64)) else { continue };
                        let (w, h) = (msg["w"].as_u64().unwrap_or(0) as u32, msg["h"].as_u64().unwrap_or(0) as u32);
                        stats.frames.fetch_add(1, Ordering::SeqCst);
                        let minor = msg.get("minor").and_then(Value::as_bool).unwrap_or(false);
                        let _ = writeln!(frames_log, "{}", json!({"i": i, "t": t, "w": w, "h": h, "minor": minor}));
                        // 多个编码线程可能乱序完成：零点取最早的帧
                        let mut first = stats.first_frame_ms.lock().unwrap();
                        if first.is_none_or(|f| t < f) {
                            *first = Some(t);
                            *stats.size.lock().unwrap() = (w, h);
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
            let minor = v.get("minor").and_then(Value::as_bool).unwrap_or(false);
            Some(render::SourceFrame { t: (t - meta.t0_ms) / 1000.0, file: dir.join("frames").join(format!("{i:06}.jpg")), minor })
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
    tighten: bool,
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
        tighten: overrides
            .get("tighten")
            .and_then(Value::as_bool)
            .or(doc.tighten)
            .or_else(|| s.get("tighten").and_then(Value::as_bool))
            .unwrap_or(true),
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
    let (plan, audio_path, auto_cut_secs) = {
        let items = tl.narration.clone();
        let mut cuts: Vec<(f64, f64)> = tl.cuts.iter().map(|c| (c.start, c.end)).collect();
        let (ff, d) = (ffmpeg.clone(), dir.clone());
        let music = tl.music.clone();
        let tighten = opts.tighten;
        let inserts: Vec<(f64, f64)> = tl.scenes.iter().filter(|s| !s.is_overlay()).map(|s| (s.at, s.duration)).collect();
        // 紧凑节奏用到的「有动静」的时间点：画面帧与操作事件（不含纯鼠标移动）
        let frame_times: Vec<f64> = sources.iter().filter(|f| !f.minor).map(|f| f.t).collect();
        let event_times: Vec<f64> = events.iter().filter(|e| e.kind != "move").map(|e| e.t).collect();
        tokio::task::spawn_blocking(move || -> Result<(narration::Plan, Option<PathBuf>, f64)> {
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
            let mut auto_cut = 0.0;
            if tighten {
                let voice: Vec<(f64, f64)> = items.iter().zip(&durations).map(|(it, d)| (it.at, it.at + d)).collect();
                let extra = narration::idle_cuts(duration, &frame_times, &event_times, &voice, 1.0, 0.4);
                auto_cut = extra.iter().map(|c| c.1 - c.0).sum();
                cuts.extend(extra);
            }
            let plan = narration::plan(duration, &cuts, &items, &durations, &inserts);
            let clips: Vec<(f64, &[i16])> = plan
                .slots
                .iter()
                .zip(&pcms)
                .filter_map(|(slot, p)| Some((slot.as_ref()?.0, p.as_deref()?)))
                .collect();
            let music_pcm = match &music {
                Some(m) => Some(narration::decode(&ff, &narration::resolve_audio(&d, &m.path))?),
                None => None,
            };
            let track = music.as_ref().zip(music_pcm.as_deref()).map(|(m, pcm)| narration::MusicTrack {
                pcm,
                volume: m.volume,
                duck: m.duck,
                fade_in: m.fade_in,
                fade_out: m.fade_out,
            });
            let audio = if clips.is_empty() && track.is_none() {
                None
            } else {
                let path = d.join("narration.wav");
                narration::write_mix(&path, plan.map.output_duration(), &clips, track.as_ref())?;
                Some(path)
            };
            Ok((plan, audio, auto_cut))
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
    let (out_w, out_h) = (w - w % 2, h - h % 2);
    let fonts_dir = fonts_dir(app)?;
    let mut warnings: Vec<String> = Vec::new();
    // 字幕样式：需要内置字体时先下载；下载失败退回系统字体，导出照常
    let subtitle = if opts.burn && !plan.cues.is_empty() {
        let look = tl.subtitle_style.clone().unwrap_or_default().resolve()?;
        let font_path = match look.font.clone() {
            Some(name) => match ensure_font(app, id, &fonts_dir, &name).await {
                Ok(p) => Some(p),
                Err(e) => {
                    warnings.push(format!("字幕字体不可用，改用系统字体：{e:#}"));
                    None
                }
            },
            None => None,
        };
        Some(Arc::new(subtitle::Prepared::new(look, font_path.as_deref())?))
    } else {
        None
    };
    // 代码片段：插入的按排期结果放，叠加的从源时间对应的输出时刻开始
    let mut scenes = Vec::new();
    if !tl.scenes.is_empty() {
        let browser = scene::find_browser(s.get("scene_browser").and_then(Value::as_str).unwrap_or(""))
            .ok_or_else(|| anyhow!("渲染代码片段需要 Edge 或 Chrome：没有找到，可以在「插件 → 录屏」设置里填浏览器路径"))?;
        // 模板用到的字体，尽量备好（失败只是退回系统字体）
        for f in ["kuaile", "huangyou"] {
            let _ = ensure_font(app, id, &fonts_dir, f).await;
        }
        let css = scene::font_css(&fonts_dir);
        let mut insert_idx = 0usize;
        let total = tl.scenes.len();
        for (n, sc) in tl.scenes.iter().enumerate() {
            let start = if sc.is_overlay() {
                plan.map.to_output(sc.at)
            } else {
                let slot = plan.inserts.get(insert_idx).copied().flatten();
                insert_idx += 1;
                match slot {
                    Some((st, _)) => st,
                    None => continue,
                }
            };
            let (app4, id4) = (app.clone(), id.to_string());
            let progress = move |done: usize, all: usize| {
                let _ = app4.emit("recorder-export-progress", json!({"id": id4, "phase": "scene", "done": done, "total": all, "scene": n + 1, "scenes": total}));
            };
            scenes.push(scene::render(&browser, &dir, sc, start, out_w, out_h, opts.fps, &css, &cancel, &progress).await?);
        }
    }
    let zoom_count = compiled.zooms.len();
    let cue_count = plan.cues.len();
    let job = render::Job {
        ffmpeg,
        encoder: encoder.clone(),
        out: out.clone(),
        width: out_w,
        height: out_h,
        sources,
        compiled,
        masks: tl.masks.clone(),
        opts: render::RenderOpts { fps: opts.fps, cursor_scale: opts.cursor_scale, style: opts.style },
        frame_map,
        cues: if opts.burn { plan.cues } else { Vec::new() },
        subtitle,
        scenes,
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
        "music": tl.music.as_ref().map(|m| m.path.clone()),
        "tightened_s": (auto_cut_secs * 10.0).round() / 10.0,
        "srt": if cue_count > 0 { Some(srt_path.to_string_lossy().to_string()) } else { None },
        "encoder": encoder.get(1),
        "scenes": tl.scenes.len(),
        "warnings": warnings,
        "note": if opts.preview { "这是低分辨率预览。" } else { "已导出。请用户看一遍成片，确认没有漏掉的敏感信息再对外发。" },
    }))
}

/// 字幕/片段字体目录：app_data/fonts（用户也可以把 .ttf/.otf 放进来）
pub fn fonts_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(app.path().app_data_dir()?.join("fonts"))
}

async fn ensure_font(app: &AppHandle, id: &str, fonts_dir: &Path, name: &str) -> Result<PathBuf> {
    let (app2, id2, name2) = (app.clone(), id.to_string(), name.to_string());
    let proxy = crate::agents::fallback_proxy();
    subtitle::ensure(fonts_dir, name, proxy.as_deref(), &move |got, total| {
        let _ = app2.emit("recorder-export-progress", json!({"id": id2, "phase": "font", "font": name2, "done": got, "total": total}));
    })
    .await
}

/// 配乐目录（插件设置 music_dir，默认 音乐/拾光配乐）
pub fn music_dir(app: &AppHandle) -> PathBuf {
    settings(app)
        .get("music_dir")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::audio_dir().map(|d| d.join("拾光配乐")))
        .unwrap_or_else(|| std::env::temp_dir().join("拾光配乐"))
}

/// 配乐目录里的音频文件（含子目录一层）
pub fn music_library(app: &AppHandle) -> Vec<String> {
    let dir = music_dir(app);
    // 目录不存在就建好，方便用户把配乐放进来
    let _ = std::fs::create_dir_all(&dir);
    let mut out = Vec::new();
    let mut push = |p: PathBuf| {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if narration::AUDIO_EXT.contains(&ext.as_str()) {
            out.push(p.to_string_lossy().to_string());
        }
    };
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Ok(sub) = std::fs::read_dir(&p) {
                    sub.flatten().for_each(|e| push(e.path()));
                }
            } else {
                push(p);
            }
        }
    }
    out.sort();
    out.truncate(80);
    out
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
            Some("window") => steps.push(json!({"at": at, "action": "切换到窗口", "target": v.get("title").and_then(Value::as_str).unwrap_or("")})),
            _ => {}
        }
        if steps.len() >= 300 {
            break;
        }
    }
    // 桌面录屏的点击由识别线程补发，文件里不一定按时间顺序
    steps.sort_by(|a, b| a["at"].as_f64().unwrap_or(0.0).total_cmp(&b["at"].as_f64().unwrap_or(0.0)));
    let audio_dir = dir.join("audio");
    std::fs::create_dir_all(&audio_dir)?;
    Ok(json!({
        "id": meta.id, "title": meta.title, "url": meta.url,
        "source": if meta.channel == "desktop" { "desktop" } else { "browser" },
        "duration_s": r1(meta.duration_ms / 1000.0),
        "marks": meta.marks.iter().map(|m| json!({"at": r1((m.t_ms - meta.t0_ms) / 1000.0), "label": m.label, "note": m.note})).collect::<Vec<_>>(),
        "steps": steps,
        "timeline": tl,
        "audio_dir": audio_dir.to_string_lossy(),
        "music_dir": music_dir(app).to_string_lossy(),
        "music_library": music_library(app),
        "subtitle_presets": subtitle::PRESETS.iter().map(|p| json!({"id": p.0, "label": p.1})).collect::<Vec<_>>(),
        "fonts": {
            "builtin": subtitle::FONTS.iter().map(|f| json!({"key": f.key, "label": f.label, "css_family": f.family})).collect::<Vec<_>>(),
            "installed": fonts_dir(app).map(|d| subtitle::installed(&d)).unwrap_or_default(),
        },
        "scene_templates": scene::TEMPLATES.iter().map(|t| json!({"id": t.0, "label": t.1, "overlay": t.2, "vars": t.3})).collect::<Vec<_>>(),
        "hint": "时间都是录制开始后的秒数。配音文件放进 audio_dir，再用 record_edit 的 narration[].audio 填相对路径（如 audio/01.mp3）。背景音乐从 music_library 里按视频风格挑一首，用 record_edit 的 music 设置。字幕样式用 subtitle_style，代码片段用 scenes（写完先 record_scene_preview 看效果）。",
    }))
}

/// 修改时间线（AI 工具与界面共用）：先在副本上改并校验，通过才保存
pub fn edit_timeline(app: &AppHandle, id: &str, patch: &Value) -> Result<(doc::TimelineDoc, Vec<&'static str>)> {
    let dir = recordings_dir(app)?.join(valid_id(id)?);
    let meta = load_meta(&dir)?;
    let mut tl = doc::load(&dir);
    let mut patch = patch.clone();
    if let Some(list) = patch.get_mut("scenes").and_then(Value::as_array_mut) {
        for item in list.iter_mut() {
            materialize_scene(&dir, &tl, item)?;
        }
    }
    let changed = doc::apply_patch(&mut tl, &patch)?;
    doc::normalize(&mut tl, meta.duration_ms / 1000.0, &dir)?;
    doc::save(&dir, &tl)?;
    emit_changed(app);
    let _ = app.emit("recorder-timeline-changed", json!({ "id": id }));
    Ok((tl, changed))
}

/// record_edit 的 scenes[] 一项：给了 html 就写成 scenes/<id>.html；给了 template 就用内置模板 + vars 生成；
/// 都没给就沿用同 id 已有片段的文件。之后 item 里只留 file。
fn materialize_scene(dir: &Path, tl: &doc::TimelineDoc, item: &mut Value) -> Result<()> {
    let obj = item.as_object_mut().ok_or_else(|| anyhow!("scenes 里每一项都要是对象"))?;
    let sid = obj.get("id").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if !scene::valid_id(&sid) {
        bail!("片段 id 只能用字母、数字、- 和 _（现在是「{sid}」）");
    }
    let rel = format!("scenes/{sid}.html");
    let write = |content: &str| -> Result<()> {
        std::fs::create_dir_all(dir.join("scenes"))?;
        std::fs::write(dir.join(&rel), content)?;
        Ok(())
    };
    if let Some(html) = obj.remove("html").and_then(|v| v.as_str().map(str::to_string)) {
        write(&html)?;
        obj.insert("file".into(), json!(rel));
        obj.remove("template");
    } else if let Some(t) = obj.get("template").and_then(Value::as_str).map(str::to_string) {
        let info = scene::template_info(&t).ok_or_else(|| {
            anyhow!("没有模板 {t}，可用：{}", scene::TEMPLATES.iter().map(|t| t.0).collect::<Vec<_>>().join("、"))
        })?;
        let src = crate::plugins::snapshot().asset("recorder", &format!("scene_{t}"))?;
        let vars = obj.get("vars").cloned().unwrap_or_else(|| json!({}));
        write(&scene::fill_template(&src, &vars))?;
        obj.insert("file".into(), json!(rel));
        obj.entry("kind").or_insert_with(|| json!(if info.2 { "overlay" } else { "insert" }));
    } else if obj.get("file").and_then(Value::as_str).is_none_or(|f| f.trim().is_empty()) {
        let old = tl.scenes.iter().find(|s| s.id == sid).ok_or_else(|| anyhow!("新片段 {sid} 需要 html 或 template"))?;
        obj.insert("file".into(), json!(old.file));
    }
    Ok(())
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
        "tighten_default": settings(app).get("tighten").and_then(Value::as_bool).unwrap_or(true),
        "music_library": music_library(app),
        "music_dir": music_dir(app).to_string_lossy(),
        "subtitle_presets": subtitle::PRESETS.iter().map(|p| json!({"id": p.0, "label": p.1})).collect::<Vec<_>>(),
        "scene_templates": scene::TEMPLATES.iter().map(|t| json!({"id": t.0, "label": t.1, "overlay": t.2})).collect::<Vec<_>>(),
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
        "record_start" => hub.start(app, args).await,
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
        "record_scene_preview" => {
            let id = match str_arg("id").filter(|s| !s.trim().is_empty()) {
                Some(id) => valid_id(id)?.to_string(),
                None => latest_id(app)?,
            };
            let dir = recordings_dir(app)?.join(&id);
            let tl = doc::load(&dir);
            let sid = str_arg("scene").unwrap_or("");
            let sc = tl.scenes.iter().find(|s| s.id == sid).ok_or_else(|| {
                anyhow!("没有片段 {sid}，现有：{}", tl.scenes.iter().map(|s| s.id.as_str()).collect::<Vec<_>>().join("、"))
            })?;
            let browser = scene::find_browser(settings(app).get("scene_browser").and_then(Value::as_str).unwrap_or(""))
                .ok_or_else(|| anyhow!("需要 Edge 或 Chrome 来渲染片段"))?;
            let fonts = fonts_dir(app)?;
            for f in ["kuaile", "huangyou"] {
                let _ = ensure_font(app, &id, &fonts, f).await;
            }
            let d = sc.duration;
            let times: Vec<f64> = args
                .get("times")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_f64).take(6).collect())
                .filter(|v: &Vec<f64>| !v.is_empty())
                .unwrap_or_else(|| vec![0.3_f64.min(d / 4.0), d * 0.35, d * 0.7, (d - 0.1).max(0.0)]);
            std::fs::create_dir_all(dir.join("scene_cache"))?;
            let out = dir.join("scene_cache").join(format!("preview-{sid}.png"));
            let times = scene::snapshot(&browser, &dir, sc, &times, &scene::font_css(&fonts), &out).await?;
            Ok(json!({
                "image": out.to_string_lossy(), "times": times, "scene": sid, "kind": sc.kind,
                "note": "按时间顺序拼成的截图（左上起，每行 2 张；叠加片段垫在深灰底上）。用 read_image 看这张图检查效果，不满意就改 html 再 record_edit。",
            }))
        }
        "record_edit" => {
            let id = match str_arg("id").filter(|s| !s.trim().is_empty()) {
                Some(id) => id.to_string(),
                None => latest_id(app)?,
            };
            let (tl, changed) = edit_timeline(app, &id, args)?;
            if changed.is_empty() {
                bail!("没有要修改的字段（可改 narration / music / tighten / masks / cuts / zooms / burn_subtitles / subtitle_style / scenes）");
            }
            Ok(json!({
                "id": id, "changed": changed,
                "narration": tl.narration.len(),
                "with_audio": tl.narration.iter().filter(|n| n.audio.is_some()).count(),
                "masks": tl.masks.len(), "cuts": tl.cuts.len(), "zooms": tl.zooms.len(), "zooms_edited": tl.zooms_edited,
                "music": tl.music.as_ref().map(|m| m.path.clone()), "tighten": tl.tighten,
                "subtitle_style": tl.subtitle_style.as_ref().map(|s| if s.preset.is_empty() { "classic".to_string() } else { s.preset.clone() }),
                "scenes": tl.scenes.iter().map(|s| json!({"id": s.id, "kind": s.kind, "at": s.at, "duration": s.duration})).collect::<Vec<_>>(),
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
pub async fn recorder_start(app: AppHandle, title: Option<String>, url: Option<String>, opts: Option<Value>) -> Result<Value, String> {
    let mut args = opts.unwrap_or_else(|| json!({}));
    if !args.is_object() {
        args = json!({});
    }
    args["title"] = json!(title);
    args["url"] = json!(url);
    let state = app.state::<crate::AppState>();
    state.recorder.start(&app, &args).await.map_err(err)
}

/// 录屏快捷键：没在录就录鼠标所在的整块屏幕，在录就停止
pub async fn hotkey_toggle(app: &AppHandle) {
    let state = app.state::<crate::AppState>();
    let recording = state.recorder.recording_id().await.is_some();
    let r = if recording {
        state.recorder.stop(app).await
    } else {
        state.recorder.start(app, &json!({"source": "screen"})).await
    };
    if let Err(e) = r {
        log::info!("快捷键录屏: {e:#}");
        let _ = app.emit("recorder-error", format!("{e:#}"));
    }
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
        obj.entry("music").or_insert(Value::Null);
        obj.entry("tighten").or_insert(Value::Null);
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
    let in_recordings = recordings_dir(&app).map(|r| p.starts_with(r)).unwrap_or(false)
        || p.starts_with(screenshot::output_dir(&app));
    let is_export = list(&app, 500)
        .unwrap_or_default()
        .iter()
        .any(|v| v["exported"].as_str() == Some(path.as_str()));
    if !(in_recordings || is_export) || !p.exists() {
        return Err("只能打开录制目录、导出的视频或截图".into());
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
            sources.push(render::SourceFrame { t: i as f64 * 0.2, file, minor: false });
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
            let plan = narration::plan(6.0, &[(4.0, 5.0)], &items, &[1.0], &[]);
            let tone: Vec<i16> = (0..narration::RATE as usize * 2).map(|i| (((i / 2) as f64 * 0.06).sin() * 8000.0) as i16).collect();
            let wav = root.join("narration.wav");
            let bgm: Vec<i16> = (0..narration::RATE as usize).map(|i| (((i / 2) as f64 * 0.02).sin() * 6000.0) as i16).collect();
            let track = narration::MusicTrack { pcm: &bgm, volume: 0.3, duck: 0.35, fade_in: 0.5, fade_out: 1.0 };
            narration::write_mix(&wav, plan.map.output_duration(), &[(plan.slots[0].unwrap().0, &tone)], Some(&track)).unwrap();
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
                subtitle: subtitle::SubtitleStyle { preset: "variety".into(), font: Some("msyhbd.ttc".into()), ..Default::default() }
                    .resolve()
                    .ok()
                    .and_then(|look| subtitle::Prepared::new(look, None).ok())
                    .map(Arc::new),
                scenes: Vec::new(),
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

    /// 插入片头 + 叠加贴纸 + 综艺字幕，导出成 MP4 并截出几帧（需要本机 ffmpeg、Edge/Chrome，会下载站酷快乐体）：
    /// SG_E2E_DIR=目录 cargo test --release --lib recorder::e2e::export_with_scenes -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn export_with_scenes() {
        let Some(root) = std::env::var_os("SG_E2E_DIR").map(PathBuf::from) else { return };
        let Some(ff) = ffmpeg::find_local("", &root.join("tools")) else { return };
        let Some(browser) = scene::find_browser("") else { return };
        let frames_dir = root.join("frames");
        std::fs::create_dir_all(&frames_dir).unwrap();
        std::fs::create_dir_all(root.join("scenes")).unwrap();
        let (w, h) = (1280u32, 720u32);
        let mut sources = Vec::new();
        for i in 0..30u32 {
            let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([30 + ((x + i * 20) % 200) as u8 / 2, 60 + (y % 120) as u8 / 2, 120]));
            let file = frames_dir.join(format!("{:06}.jpg", i + 1));
            img.save(&file).unwrap();
            sources.push(render::SourceFrame { t: i as f64 * 0.2, file, minor: false });
        }
        let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins/recorder/assets/scenes");
        let fill = |name: &str, vars: Value| {
            let src = std::fs::read_to_string(assets.join(format!("{name}.html"))).unwrap();
            std::fs::write(root.join("scenes").join(format!("{name}.html")), scene::fill_template(&src, &vars)).unwrap();
        };
        fill("title", json!({"title": "拾光演示", "subtitle": "代码生成的片头"}));
        fill("sticker", json!({"text": "太强了！", "x": 0.7, "y": 0.3}));
        let mk = |id: &str, kind: &str, at: f64, d: f64| scene::Scene { id: id.into(), kind: kind.into(), at, duration: d, file: format!("scenes/{id}.html"), title: String::new(), template: None, vars: None };
        let scenes_doc = vec![mk("title", "insert", 0.0, 2.0), mk("sticker", "overlay", 3.0, 2.0)];
        let fonts = root.join("fonts");
        for f in ["kuaile", "huangyou"] {
            subtitle::ensure(&fonts, f, crate::agents::fallback_proxy().as_deref(), &|_, _| {}).await.unwrap();
        }
        let compiled = timeline::compile(&[], 6.0, timeline::Opts { fps: 30, zoom: false, zoom_scale: 2.0 }, None);
        let items = [narration::Narration { at: 1.0, text: "这个功能【超好用】，一句话就能派活。".into(), audio: None }];
        let plan = narration::plan(6.0, &[], &items, &[2.5], &[(0.0, 2.0)]);
        assert_eq!(plan.inserts, vec![Some((0.0, 2.0))]);
        let css = scene::font_css(&fonts);
        let cancel = Arc::new(AtomicBool::new(false));
        let mut rendered = Vec::new();
        for sc in &scenes_doc {
            let start = if sc.is_overlay() { plan.map.to_output(sc.at) } else { plan.inserts[0].unwrap().0 };
            rendered.push(scene::render(&browser, &root, sc, start, w, h, 30, &css, &cancel, &|_, _| {}).await.unwrap());
        }
        let n_out = (plan.map.output_duration() * 30.0).ceil() as usize;
        let frame_map = (0..n_out).map(|k| { let t = k as f64 / 30.0; (((plan.map.to_source(t) * 30.0).round() as usize).min(compiled.frames.len() - 1), t) }).collect();
        let look = subtitle::SubtitleStyle { preset: "variety".into(), ..Default::default() }.resolve().unwrap();
        let font = look.font.as_deref().and_then(|f| subtitle::locate(&fonts, f));
        let out = root.join("scenes-demo.mp4");
        let job = render::Job {
            ffmpeg: ff.clone(), encoder: render::pick_encoder(&ff), out: out.clone(), width: w, height: h,
            sources, compiled, masks: Vec::new(),
            opts: render::RenderOpts { fps: 30, cursor_scale: 1.0, style: render::Style::Plain },
            frame_map, cues: plan.cues.clone(),
            subtitle: Some(Arc::new(subtitle::Prepared::new(look, font.as_deref()).unwrap())),
            scenes: rendered, audio: None, fast: false,
        };
        tokio::task::spawn_blocking(move || render::run(job, Arc::new(AtomicBool::new(false)), |_, _| {})).await.unwrap().unwrap();
        // 截出：片头中间、贴纸 + 字幕
        for (name, t) in [("f_title", "1.2"), ("f_overlay", "5.3")] {
            let st = std::process::Command::new(&ff).args(["-y", "-loglevel", "error", "-ss", t, "-i"]).arg(&out).args(["-frames:v", "1"]).arg(root.join(format!("{name}.png"))).status().unwrap();
            assert!(st.success());
        }
        eprintln!("cues = {:?}", plan.cues);
    }
}
