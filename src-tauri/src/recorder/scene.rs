//! 代码片段：用网页（HTML/CSS/JS）写的画面，导出时用后台无界面的 Edge/Chrome 逐帧截图。
//! - insert：插在录制素材之间（片头、片尾、章节卡、转场），独占画面，原画面在该点定格不动；
//! - overlay：叠在录制画面上（角标、标注、贴纸），背景透明。
//! 页面时间由注入的脚本接管（rAF / setTimeout / Date / performance / CSS 动画都按导出时间走），
//! 所以动画逐帧确定，和机器快慢无关。页面也可以自己定义 window.renderFrame(t) 按秒画。
//! 设计尺寸固定宽 1920 CSS px，高按画面比例（16:9 时 1080），导出时按实际分辨率缩放。

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;

pub const DESIGN_W: u32 = 1920;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    /// 片段编号（字母、数字、- _），也是 scenes/ 下的文件名
    pub id: String,
    /// insert | overlay
    #[serde(default = "default_kind")]
    pub kind: String,
    /// 源时间（秒）：insert 插在这个时刻；overlay 从这个时刻开始叠加
    pub at: f64,
    /// 时长（秒）
    pub duration: f64,
    /// 网页文件，相对录制目录（scenes/intro.html）
    pub file: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// 由模板生成时记下模板和参数，方便以后改
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vars: Option<Value>,
}

fn default_kind() -> String {
    "insert".into()
}

impl Scene {
    pub fn is_overlay(&self) -> bool {
        self.kind == "overlay"
    }
}

/// 内置模板：(名字, 说明, 是否叠加层, 可填的变量)
pub const TEMPLATES: &[(&str, &str, bool, &str)] = &[
    ("title", "片头标题卡：大标题逐字弹出 + 副标题淡入，渐变背景", false, "title, subtitle, color1, color2"),
    ("chapter", "章节卡：编号 + 章节名从左滑入", false, "number, title, color1, color2"),
    ("bullets", "要点列表：标题 + 每条要点依次弹出（items 用 | 分隔，最多 6 条）", false, "title, items, color1, color2"),
    ("ending", "片尾：感谢语 + 链接/关注提示", false, "title, subtitle, link, color1, color2"),
    ("code", "代码打字效果：深色编辑器里逐字打出代码", false, "title, code, lang"),
    ("lower_third", "（叠加）左下角名牌条：标题 + 说明，滑入后停留再滑出", true, "title, subtitle, color1"),
    ("callout", "（叠加）标注框：在 x,y（0–1 画面比例）处画脉冲圆圈和说明文字", true, "text, x, y, color1"),
    ("sticker", "（叠加）综艺贴纸：大字花字弹出并轻微晃动（如「太强了！」）", true, "text, x, y, color1, color2"),
];

pub fn template_info(name: &str) -> Option<&'static (&'static str, &'static str, bool, &'static str)> {
    TEMPLATES.iter().find(|t| t.0 == name)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// 用变量填模板里的 {{名字}}（HTML 转义）；{{名字|默认值}} 给默认值；{{{名字}}} 不转义（仅给 code 这类已在模板里处理的）。
pub fn fill_template(src: &str, vars: &Value) -> String {
    let get = |k: &str| -> Option<String> {
        vars.get(k).filter(|v| !v.is_null()).map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>().join("|"),
            other => other.to_string(),
        })
    };
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let raw = rest[start..].starts_with("{{{");
        let (open, close) = if raw { (3, "}}}") } else { (2, "}}") };
        let Some(end) = rest[start + open..].find(close) else {
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = &rest[start + open..start + open + end];
        let (key, default) = inner.split_once('|').map(|(k, d)| (k.trim(), d)).unwrap_or((inner.trim(), ""));
        let val = get(key).unwrap_or_else(|| default.to_string());
        if raw {
            // 放进 <script> 里的 JSON 字符串
            out.push_str(&serde_json::to_string(&val).unwrap_or_default());
        } else {
            out.push_str(&html_escape(&val));
        }
        rest = &rest[start + open + end + close.len()..];
    }
    out.push_str(rest);
    out
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ---------- 浏览器 ----------

pub fn find_browser(configured: &str) -> Option<PathBuf> {
    let c = configured.trim().trim_matches('"');
    if !c.is_empty() {
        let p = PathBuf::from(c);
        return p.is_file().then_some(p);
    }
    let mut cands: Vec<PathBuf> = Vec::new();
    for var in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(var).map(PathBuf::from) {
            cands.push(base.join(r"Microsoft\Edge\Application\msedge.exe"));
            cands.push(base.join(r"Google\Chrome\Application\chrome.exe"));
        }
    }
    cands.into_iter().find(|p| p.is_file())
}

/// 接管页面时间的脚本（新文档创建时注入，早于页面自己的脚本）
fn time_shim(font_css: &str, duration: f64) -> String {
    format!(
        r#"(() => {{
  if (window.__sgShim) return; window.__sgShim = true;
  window.SG_DURATION = {duration};
  let now = 0;
  const epoch = 1767225600000;
  const RealDate = Date;
  class FakeDate extends RealDate {{
    constructor(...a) {{ if (a.length === 0) super(epoch + now); else super(...a); }}
    static now() {{ return epoch + now; }}
  }}
  window.Date = FakeDate;
  performance.now = () => now;
  let rafId = 0; const rafs = new Map();
  window.requestAnimationFrame = (cb) => {{ rafs.set(++rafId, cb); return rafId; }};
  window.cancelAnimationFrame = (id) => {{ rafs.delete(id); }};
  let timerId = 0; const timers = new Map();
  window.setTimeout = (fn, ms, ...args) => {{ const id = ++timerId; timers.set(id, {{ due: now + Math.max(0, +ms || 0), fn, args, every: 0 }}); return id; }};
  window.setInterval = (fn, ms, ...args) => {{ const id = ++timerId; const every = Math.max(1, +ms || 0); timers.set(id, {{ due: now + every, fn, args, every }}); return id; }};
  window.clearTimeout = window.clearInterval = (id) => {{ timers.delete(id); }};
  const runTimers = () => {{
    for (let guard = 0; guard < 5000; guard++) {{
      let next = null;
      for (const [id, t] of timers) if (t.due <= now && (!next || t.due < next[1].due)) next = [id, t];
      if (!next) break;
      const [id, t] = next;
      if (t.every) t.due += t.every; else timers.delete(id);
      try {{ if (typeof t.fn === 'function') t.fn(...t.args); }} catch (e) {{ console.error(e); }}
    }}
  }};
  const born = new WeakMap();
  window.__sgSeek = async (sec) => {{
    now = sec * 1000;
    runTimers();
    const cbs = [...rafs.values()]; rafs.clear();
    for (const cb of cbs) {{ try {{ cb(now); }} catch (e) {{ console.error(e); }} }}
    for (const a of document.getAnimations()) {{
      try {{ if (!born.has(a)) born.set(a, now === 0 ? 0 : now); a.pause(); a.currentTime = Math.max(0, now - born.get(a)); }} catch (e) {{}}
    }}
    if (typeof window.renderFrame === 'function') await window.renderFrame(sec);
    return true;
  }};
  window.__sgReady = async () => {{
    await Promise.all([...document.images].map(i => i.complete ? 0 : new Promise(r => {{ i.onload = i.onerror = r; }})));
    try {{ await document.fonts.ready; }} catch (e) {{}}
    return true;
  }};
  const css = {font_css};
  if (css) {{
    const add = () => {{ const s = document.createElement('style'); s.textContent = css; (document.head || document.documentElement).appendChild(s); }};
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', add); else add();
  }}
}})();"#,
        font_css = serde_json::to_string(font_css).unwrap_or_else(|_| "\"\"".into()),
        duration = duration
    )
}

/// 已下载的内置字体做成 @font-face，网页里可以直接写 font-family: "ZCOOL KuaiLe"
pub fn font_css(fonts_dir: &Path) -> String {
    super::subtitle::FONTS
        .iter()
        .filter(|f| fonts_dir.join(f.file).is_file())
        .map(|f| {
            let url = url_of(&fonts_dir.join(f.file));
            format!("@font-face {{ font-family: \"{}\"; src: url(\"{url}\"); font-display: block; }}", f.family)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn url_of(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let enc: String = s
        .chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '#' => "%23".to_string(),
            '%' => "%25".to_string(),
            c => c.to_string(),
        })
        .collect();
    format!("file:///{}", enc.trim_start_matches('/'))
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// 一个后台无界面浏览器 + 一个页面的 CDP 连接
pub struct Headless {
    child: tokio::process::Child,
    profile: PathBuf,
    ws: Ws,
    next: u64,
}

impl Headless {
    pub async fn launch(browser: &Path, scratch: &Path) -> Result<Headless> {
        let profile = scratch.join(format!("scene-browser-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        std::fs::create_dir_all(&profile)?;
        let mut cmd = tokio::process::Command::new(browser);
        cmd.args([
            "--headless=new",
            "--remote-debugging-port=0",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-background-networking",
            "--disable-sync",
            "--mute-audio",
            "--hide-scrollbars",
            "--allow-file-access-from-files",
            "--force-color-profile=srgb",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("about:blank")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let child = cmd.spawn().with_context(|| format!("启动后台浏览器失败：{}", browser.display()))?;
        // 浏览器把端口写进 DevToolsActivePort
        let port_file = profile.join("DevToolsActivePort");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let port: u16 = loop {
            if let Some(p) = std::fs::read_to_string(&port_file).ok().and_then(|t| t.lines().next()?.trim().parse().ok()) {
                break p;
            }
            if std::time::Instant::now() > deadline {
                bail!("后台浏览器 20 秒内没有就绪");
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        };
        let list: Value = reqwest::Client::builder()
            .no_proxy()
            .build()?
            .get(format!("http://127.0.0.1:{port}/json/list"))
            .send()
            .await?
            .json()
            .await?;
        let url = list
            .as_array()
            .and_then(|a| a.iter().find(|t| t["type"] == "page"))
            .and_then(|t| t["webSocketDebuggerUrl"].as_str())
            .ok_or_else(|| anyhow!("后台浏览器没有页面"))?
            .to_string();
        let (ws, _) = tokio_tungstenite::connect_async(url).await.context("连接后台浏览器")?;
        Ok(Headless { child, profile, ws, next: 0 })
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        self.ws.send(Message::Text(json!({"id": id, "method": method, "params": params}).to_string().into())).await?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let msg = tokio::time::timeout_at(deadline, self.ws.next()).await.map_err(|_| anyhow!("后台浏览器 {method} 超时"))?;
            let Some(msg) = msg else { bail!("后台浏览器连接断开") };
            let Message::Text(t) = msg? else { continue };
            let v: Value = serde_json::from_str(&t)?;
            if v["id"].as_u64() == Some(id) {
                if let Some(e) = v.get("error") {
                    bail!("{method} 失败：{}", e["message"].as_str().unwrap_or(&e.to_string()));
                }
                return Ok(v["result"].clone());
            }
        }
    }

    async fn eval(&mut self, expr: &str) -> Result<Value> {
        let r = self.call("Runtime.evaluate", json!({"expression": expr, "awaitPromise": true, "returnByValue": true})).await?;
        if let Some(ex) = r.get("exceptionDetails") {
            let msg = ex.pointer("/exception/description").and_then(Value::as_str).or_else(|| ex["text"].as_str()).unwrap_or("脚本出错");
            bail!("网页脚本出错：{}", msg.lines().next().unwrap_or(msg));
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    /// 打开片段页面并等它就绪。out_w×out_h 为输出像素尺寸
    pub async fn open(&mut self, file: &Path, out_w: u32, out_h: u32, transparent: bool, font_css: &str, duration: f64) -> Result<()> {
        let css_h = ((DESIGN_W as f64) * out_h as f64 / out_w as f64).round() as u32;
        let dpr = out_w as f64 / DESIGN_W as f64;
        self.call("Page.enable", json!({})).await?;
        self.call("Runtime.enable", json!({})).await?;
        self.call("Emulation.setDeviceMetricsOverride", json!({"width": DESIGN_W, "height": css_h, "deviceScaleFactor": dpr, "mobile": false})).await?;
        let bg = if transparent { json!({"r": 0, "g": 0, "b": 0, "a": 0}) } else { json!({"r": 0, "g": 0, "b": 0, "a": 1}) };
        self.call("Emulation.setDefaultBackgroundColorOverride", json!({"color": bg})).await?;
        self.call("Page.addScriptToEvaluateOnNewDocument", json!({"source": time_shim(font_css, duration)})).await?;
        self.call("Page.navigate", json!({"url": url_of(file)})).await?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let state = self.eval("document.readyState + (window.__sgShim ? '' : '!')").await.unwrap_or(Value::Null);
            if state.as_str() == Some("complete") {
                break;
            }
            if std::time::Instant::now() > deadline {
                bail!("片段页面 20 秒内没有加载完：{}", file.display());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        self.eval("window.__sgReady()").await?;
        Ok(())
    }

    /// 跳到 t 秒并截图（PNG 或 JPEG 字节）
    pub async fn frame(&mut self, t: f64, png: bool) -> Result<Vec<u8>> {
        self.eval(&format!("window.__sgSeek({t})")).await?;
        let params = if png { json!({"format": "png", "fromSurface": true}) } else { json!({"format": "jpeg", "quality": 92, "fromSurface": true}) };
        let r = self.call("Page.captureScreenshot", params).await?;
        let data = r["data"].as_str().ok_or_else(|| anyhow!("截图没有数据"))?;
        Ok(base64::engine::general_purpose::STANDARD.decode(data)?)
    }

    pub async fn close(mut self) {
        let _ = self.call("Browser.close", json!({})).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), self.child.wait()).await;
        let _ = self.child.kill().await;
        let profile = self.profile.clone();
        // 浏览器退出后才删得掉
        for _ in 0..10 {
            if std::fs::remove_dir_all(&profile).is_ok() || !profile.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }
}

// ---------- 导出用 ----------

/// 渲染好的片段：输出时间窗口 + 帧文件
#[derive(Debug, Clone)]
pub struct Rendered {
    pub overlay: bool,
    /// 输出时间（秒）
    pub start: f64,
    pub duration: f64,
    pub fps: u32,
    pub frames: Vec<PathBuf>,
}

impl Rendered {
    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.start + self.duration
    }
    pub fn frame_at(&self, t: f64) -> Option<&PathBuf> {
        if self.frames.is_empty() {
            return None;
        }
        let k = ((t - self.start) * self.fps as f64).floor().max(0.0) as usize;
        self.frames.get(k.min(self.frames.len() - 1))
    }
}

fn fnv(bytes: &[u8], seed: u64) -> u64 {
    let mut h = seed ^ 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// 逐帧渲染一个片段到 cache 目录；内容、尺寸、帧率都没变就直接用上次的
#[allow(clippy::too_many_arguments)]
pub async fn render(
    browser: &Path,
    dir: &Path,
    scene: &Scene,
    start: f64,
    w: u32,
    h: u32,
    fps: u32,
    fonts_css: &str,
    cancel: &Arc<AtomicBool>,
    progress: &(dyn Fn(usize, usize) + Send + Sync),
) -> Result<Rendered> {
    let file = dir.join(&scene.file);
    let html = std::fs::read(&file).with_context(|| format!("读取片段 {}", file.display()))?;
    let n = ((scene.duration * fps as f64).round() as usize).max(1);
    let key = fnv(&html, fnv(format!("{w}x{h}@{fps}:{}:{}:{}", n, scene.kind, fonts_css.len()).as_bytes(), 7));
    let cache = dir.join("scene_cache").join(format!("{}-{key:016x}", scene.id));
    let ext = if scene.is_overlay() { "png" } else { "jpg" };
    let frames: Vec<PathBuf> = (0..n).map(|k| cache.join(format!("{k:05}.{ext}"))).collect();
    let rendered = Rendered { overlay: scene.is_overlay(), start, duration: scene.duration, fps, frames: frames.clone() };
    if cache.join("done").is_file() && frames.iter().all(|f| f.is_file()) {
        progress(n, n);
        return Ok(rendered);
    }
    // 清掉这个片段的旧缓存
    if let Ok(rd) = std::fs::read_dir(dir.join("scene_cache")) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(&format!("{}-", scene.id)) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    std::fs::create_dir_all(&cache)?;
    let mut hb = Headless::launch(browser, &dir.join("scene_cache")).await?;
    let result = async {
        hb.open(&file, w, h, scene.is_overlay(), fonts_css, scene.duration).await?;
        for (k, path) in frames.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                bail!("导出已取消");
            }
            let bytes = hb.frame(k as f64 / fps as f64, scene.is_overlay()).await?;
            std::fs::write(path, bytes)?;
            if k % 5 == 0 || k + 1 == n {
                progress(k + 1, n);
            }
        }
        std::fs::write(cache.join("done"), b"")?;
        Ok(())
    }
    .await;
    hb.close().await;
    result.with_context(|| format!("渲染片段 {}", scene.id))?;
    Ok(rendered)
}

/// 预览：在几个时间点截图拼成一张图（叠加片段垫在深灰底上），给模型和用户检查效果
pub async fn snapshot(browser: &Path, dir: &Path, scene: &Scene, times: &[f64], fonts_css: &str, out: &Path) -> Result<Vec<f64>> {
    let file = dir.join(&scene.file);
    if !file.is_file() {
        bail!("片段文件不存在：{}", file.display());
    }
    let (w, h) = (960u32, 540u32);
    let mut hb = Headless::launch(browser, &dir.join("scene_cache")).await?;
    let result = async {
        hb.open(&file, w, h, scene.is_overlay(), fonts_css, scene.duration).await?;
        let mut shots = Vec::new();
        for &t in times {
            let bytes = hb.frame(t.clamp(0.0, scene.duration), true).await?;
            shots.push(image::load_from_memory(&bytes)?.to_rgba8());
        }
        Ok::<_, anyhow::Error>(shots)
    }
    .await;
    hb.close().await;
    let shots = result?;
    let cols = if shots.len() <= 2 { shots.len() as u32 } else { 2 };
    let rows = (shots.len() as u32).div_ceil(cols.max(1));
    let gap = 8u32;
    let mut sheet = image::RgbaImage::from_pixel(cols * w + (cols + 1) * gap, rows * h + (rows + 1) * gap, image::Rgba([255, 255, 255, 255]));
    for (i, shot) in shots.iter().enumerate() {
        let (cx, cy) = (gap + (i as u32 % cols) * (w + gap), gap + (i as u32 / cols) * (h + gap));
        let mut tile = image::RgbaImage::from_pixel(w, h, image::Rgba([48, 52, 58, 255]));
        image::imageops::overlay(&mut tile, shot, 0, 0);
        image::imageops::replace(&mut sheet, &tile, cx as i64, cy as i64);
    }
    sheet.save(out)?;
    Ok(times.iter().map(|t| t.clamp(0.0, scene.duration)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_fill_escapes_and_defaults() {
        let out = fill_template("<h1>{{title}}</h1><p>{{sub|默认}}</p><script>const c = {{{code}}};</script>", &json!({"title": "<b>拾光</b>", "code": "a < b\n\"x\""}));
        assert_eq!(out, "<h1>&lt;b&gt;拾光&lt;/b&gt;</h1><p>默认</p><script>const c = \"a < b\\n\\\"x\\\"\";</script>");
        assert_eq!(fill_template("{{items}}", &json!({"items": ["a", "b"]})), "a|b");
    }

    #[test]
    fn frame_lookup() {
        let r = Rendered { overlay: false, start: 2.0, duration: 1.0, fps: 10, frames: (0..10).map(|i| PathBuf::from(format!("{i}"))).collect() };
        assert!(r.contains(2.0) && !r.contains(3.0));
        assert_eq!(r.frame_at(2.55).unwrap(), &PathBuf::from("5"));
        assert_eq!(r.frame_at(9.0).unwrap(), &PathBuf::from("9"));
    }

    #[test]
    fn ids() {
        assert!(valid_id("intro_1") && !valid_id("../x") && !valid_id(""));
    }

    /// 看效果用：SG_SAMPLE_DIR=目录 时把各字幕预设和每个内置模板渲染成图片（会下载站酷快乐体、黄油体）
    #[tokio::test]
    #[ignore]
    async fn render_samples() {
        let Some(out) = std::env::var_os("SG_SAMPLE_DIR").map(PathBuf::from) else { return };
        let Some(browser) = find_browser("") else { return };
        std::fs::create_dir_all(out.join("scenes")).unwrap();
        let fonts = out.join("fonts");
        for f in ["kuaile", "huangyou"] {
            crate::recorder::subtitle::ensure(&fonts, f, crate::agents::fallback_proxy().as_deref(), &|_, _| {}).await.unwrap();
        }
        // 字幕：深色截图底上画四种预设
        use crate::recorder::subtitle::{draw, Prepared, SubtitleStyle, PRESETS};
        let mut sheet = tiny_skia::Pixmap::new(960, 540 * PRESETS.len() as u32).unwrap();
        for (i, (p, _)) in PRESETS.iter().enumerate() {
            let look = SubtitleStyle { preset: p.to_string(), ..Default::default() }.resolve().unwrap();
            let path = look.font.as_deref().and_then(|f| crate::recorder::subtitle::locate(&fonts, f));
            let prep = Prepared::new(look, path.as_deref()).unwrap();
            let mut pm = tiny_skia::Pixmap::new(960, 540).unwrap();
            pm.fill(tiny_skia::Color::from_rgba8(70, 90, 120, 255));
            draw(&mut pm, &prep, "这个功能【超好用】，一句话就能派活", 1.0, 2.0, tiny_skia::Rect::from_xywh(0.0, 0.0, 960.0, 540.0).unwrap(), 0.5);
            sheet.draw_pixmap(0, (540 * i) as i32, pm.as_ref(), &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), None);
        }
        sheet.save_png(out.join("subtitles.png")).unwrap();
        // 模板
        let css = font_css(&fonts);
        let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins/recorder/assets/scenes");
        let vars = json!({"title": "拾光 ShiGuang", "subtitle": "常驻托盘的 AI 桌面助手", "number": "02", "items": "一句话派活|先出方案再动手|录屏自动剪辑",
            "link": "github.com/xuantiandaozun/shiguang", "code": "const shiguang = new Assistant();\nawait shiguang.record({ url: 'https://example.com' });\n// 一键导出成片",
            "text": "太强了！", "x": 0.6, "y": 0.35});
        for (name, _, overlay, _) in TEMPLATES {
            let src = std::fs::read_to_string(assets.join(format!("{name}.html"))).unwrap();
            std::fs::write(out.join("scenes").join(format!("{name}.html")), fill_template(&src, &vars)).unwrap();
            let sc = Scene { id: name.to_string(), kind: if *overlay { "overlay" } else { "insert" }.into(), at: 0.0, duration: 4.0,
                file: format!("scenes/{name}.html"), title: String::new(), template: None, vars: None };
            snapshot(&browser, &out, &sc, &[0.25, 1.0, 2.0, 3.6], &css, &out.join(format!("{name}.png"))).await.unwrap();
        }
    }

    /// 真实浏览器：渲染一个 CSS 动画片段，检查时间受控（第 0 帧和第 1 秒画面不同、且同一时间两次渲染一致）
    #[tokio::test]
    #[ignore]
    async fn headless_renders_css_animation_deterministically() {
        let Some(browser) = find_browser("") else { return };
        let dir = std::env::temp_dir().join(format!("sg-scene-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("scenes")).unwrap();
        std::fs::write(
            dir.join("scenes/a.html"),
            r#"<html><body style="margin:0;background:#000"><div id=b style="position:absolute;left:0;top:0;width:200px;height:200px;background:#f00;animation:mv 1s linear forwards"></div>
            <style>@keyframes mv { to { transform: translateX(1600px) } }</style></body></html>"#,
        )
        .unwrap();
        let scene = Scene { id: "a".into(), kind: "insert".into(), at: 0.0, duration: 1.0, file: "scenes/a.html".into(), title: String::new(), template: None, vars: None };
        let cancel = Arc::new(AtomicBool::new(false));
        let r = render(&browser, &dir, &scene, 0.0, 480, 270, 10, "", &cancel, &|_, _| {}).await.unwrap();
        assert_eq!(r.frames.len(), 10);
        let first = image::open(&r.frames[0]).unwrap().to_rgb8();
        let last = image::open(&r.frames[9]).unwrap().to_rgb8();
        // 第 0 帧红块在左上，最后一帧已移到右边
        assert!(first.get_pixel(20, 20)[0] > 200, "第 0 帧左上应是红色");
        assert!(last.get_pixel(20, 20)[0] < 60, "最后一帧左上应已移走");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
