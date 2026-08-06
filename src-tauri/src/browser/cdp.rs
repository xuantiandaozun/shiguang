//! CDP 后端：通过 Chrome DevTools Protocol 操作浏览器。
//! 连接顺序：127.0.0.1:9222（chrome://inspect 手动调试）→ 9223（本程序托管的调试实例）→ 自动拉起实例。

use super::page_inject_js;
use anyhow::{anyhow, bail, Result};
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::page::{BringToFrontParams, CaptureScreenshotFormat};
use chromiumoxide::page::{Page, ScreenshotParams};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use tokio::task::JoinHandle;

const INSPECT_PORT: u16 = 9222;
const MANAGED_PORT: u16 = 9223;

pub struct CdpClient {
    browser: Browser,
    _driver: JoinHandle<()>,
    page: Option<Page>,
    origin: String,
}

impl CdpClient {
    pub async fn connect_or_launch() -> Result<CdpClient> {
        if let Ok(c) = connect_port(INSPECT_PORT, "inspect:9222(用户手动调试)").await {
            return Ok(c);
        }
        if let Ok(c) = connect_port(MANAGED_PORT, "managed:9223(独立调试实例)").await {
            return Ok(c);
        }
        launch_managed().await
    }

    pub fn describe(&self) -> String {
        self.origin.clone()
    }

    pub async fn call(&mut self, action: &str, params: Value) -> Result<Value> {
        // 故障转移时由 Hub 注入的目标页面 URL：先对齐到同一页面再执行动作
        if let Some(u) = params.get("__target_url").and_then(|v| v.as_str()) {
            self.align_page(u).await?;
        }
        match action {
            "navigate" => {
                let url = params
                    .get("url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("缺少 url"))?;
                let new_tab = params
                    .get("new_tab")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let page = if new_tab {
                    self.browser
                        .new_page(url)
                        .await
                        .map_err(|e| anyhow!("打开新标签失败: {}", e))?
                } else {
                    let p = self.current_page().await?;
                    p.goto(url)
                        .await
                        .map_err(|e| anyhow!("打开网页失败: {}", e))?;
                    p
                };
                self.page = Some(page);
                self.info().await
            }
            "snapshot" => {
                let max = params
                    .get("max_chars")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(8000);
                let scope = params.get("scope").cloned().unwrap_or(Value::Null);
                let snap = self.dh("snapshot", json!([max, scope])).await?;
                let info = self.info().await.unwrap_or(Value::Null);
                Ok(json!({
                    "title": info.get("title"),
                    "url": info.get("url"),
                    "snapshot": snap,
                }))
            }
            "read" => {
                let max = params
                    .get("max_chars")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(12000);
                let mut article = self.dh("read", json!([max])).await?;
                let info = self.info().await.unwrap_or(Value::Null);
                if let Some(obj) = article.as_object_mut() {
                    if !obj.contains_key("url") {
                        obj.insert("url".into(), info.get("url").cloned().unwrap_or(Value::Null));
                    }
                    if obj
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .is_empty()
                    {
                        obj.insert(
                            "title".into(),
                            info.get("title").cloned().unwrap_or(Value::Null),
                        );
                    }
                }
                Ok(article)
            }
            "click" => {
                let r = req_u64(&params, "ref")?;
                self.dh("click", json!([r])).await
            }
            "type" => {
                let r = req_u64(&params, "ref")?;
                let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let clear = params
                    .get("clear")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                self.dh("type", json!([r, text, clear])).await
            }
            "scroll" => {
                let dir = params
                    .get("direction")
                    .and_then(|v| v.as_str())
                    .unwrap_or("down");
                let amount = params
                    .get("amount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(600);
                let ref_ = params.get("ref").cloned().unwrap_or(Value::Null);
                self.dh("scroll", json!([dir, amount, ref_])).await
            }
            "tabs" => {
                let pages = self
                    .browser
                    .pages()
                    .await
                    .map_err(|e| anyhow!("获取标签页失败: {}", e))?;
                let current = self.page.as_ref().map(|p| p.target_id().clone());
                let mut tabs = Vec::new();
                for (i, p) in pages.iter().enumerate() {
                    let url = p.url().await.unwrap_or_default();
                    let title = p.get_title().await.ok().flatten().unwrap_or_default();
                    let active = current
                        .as_ref()
                        .map(|c| c == p.target_id())
                        .unwrap_or(false);
                    tabs.push(json!({ "id": i, "title": title, "url": url, "active": active }));
                }
                Ok(json!({ "tabs": tabs }))
            }
            "activate_tab" => {
                let idx = req_u64(&params, "id")? as usize;
                let pages = self
                    .browser
                    .pages()
                    .await
                    .map_err(|e| anyhow!("获取标签页失败: {}", e))?;
                let Some(p) = pages.get(idx) else {
                    bail!("标签页序号无效: {}", idx);
                };
                p.execute(BringToFrontParams::default())
                    .await
                    .map_err(|e| anyhow!("切换标签失败: {}", e))?;
                self.page = Some(p.clone());
                Ok(json!({ "ok": true }))
            }
            "screenshot" => {
                let page = self.current_page().await?;
                let bytes = page
                    .screenshot(
                        ScreenshotParams::builder()
                            .format(CaptureScreenshotFormat::Png)
                            .build(),
                    )
                    .await
                    .map_err(|e| anyhow!("截图失败: {}", e))?;
                use base64::Engine as _;
                Ok(json!({
                    "png_base64": base64::engine::general_purpose::STANDARD.encode(bytes)
                }))
            }
            "evaluate" => {
                let expr = params
                    .get("expression")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("缺少 expression"))?;
                let page = self.current_page().await?;
                let v = page
                    .evaluate(expr)
                    .await
                    .map_err(|e| anyhow!("JS 执行失败: {}", e))?
                    .into_value::<Value>()
                    .map_err(|e| anyhow!("JS 返回值解析失败: {}", e))?;
                Ok(json!({ "result": v }))
            }
            "info" => self.info().await,
            other => bail!("未知浏览器动作: {}", other),
        }
    }

    /// 将当前操作页对齐到指定 URL 的标签页（扩展故障转移时保持操作同一页面）；
    /// 没有匹配页则新开一个该 URL 的标签（如 9223 托管实例里没有此页面）。
    async fn align_page(&mut self, url: &str) -> Result<()> {
        let strip_hash = |u: &str| u.split('#').next().unwrap_or(u).to_string();
        let target = strip_hash(url);
        let pages = self
            .browser
            .pages()
            .await
            .map_err(|e| anyhow!("获取标签页失败: {}", e))?;
        for p in pages {
            if strip_hash(&p.url().await.ok().flatten().unwrap_or_default()) == target {
                self.page = Some(p);
                return Ok(());
            }
        }
        let p = self
            .browser
            .new_page(url)
            .await
            .map_err(|e| anyhow!("打开目标页面失败: {}", e))?;
        self.page = Some(p);
        Ok(())
    }

    async fn current_page(&mut self) -> Result<Page> {
        if let Some(p) = &self.page {
            if p.evaluate("1").await.is_ok() {
                return Ok(p.clone());
            }
        }
        let pages = self
            .browser
            .pages()
            .await
            .map_err(|e| anyhow!("获取标签页失败: {}", e))?;
        let p = match pages.into_iter().next() {
            Some(p) => p,
            None => self
                .browser
                .new_page("about:blank")
                .await
                .map_err(|e| anyhow!("新建标签页失败: {}", e))?,
        };
        self.page = Some(p.clone());
        Ok(p)
    }

    async fn info(&mut self) -> Result<Value> {
        let page = self.current_page().await?;
        let url = page.url().await.unwrap_or_default();
        let title = page.get_title().await.ok().flatten().unwrap_or_default();
        Ok(json!({ "url": url, "title": title }))
    }

    /// 注入 Readability + page-api.js 后调用 window.__dh 方法
    async fn dh(&mut self, method: &str, args: Value) -> Result<Value> {
        let page = self.current_page().await?;
        let expr = format!(
            "{}\n;window.__dh.{}(...{});",
            page_inject_js(),
            method,
            serde_json::to_string(&args)?
        );
        let v = page
            .evaluate(expr)
            .await
            .map_err(|e| anyhow!("页面执行失败: {}", e))?
            .into_value::<Value>()
            .map_err(|e| anyhow!("页面返回值解析失败: {}", e))?;
        if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
            bail!("{}", err);
        }
        Ok(v)
    }
}

fn req_u64(params: &Value, key: &str) -> Result<u64> {
    params
        .get(key)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("缺少参数 {}", key))
}

/// 快速探测 9222/9223 上是否已有可控的 CDP 服务（不建立完整连接）。
/// 用于拉起默认浏览器前判断「是否已有实例可复用」，避免重复开浏览器。
pub async fn inspect_or_managed_up() -> bool {
    port_up(INSPECT_PORT).await || port_up(MANAGED_PORT).await
}

async fn port_up(port: u16) -> bool {
    let http = format!("http://127.0.0.1:{}/json/version", port);
    let http_up = tokio::time::timeout(Duration::from_millis(600), reqwest::get(&http))
        .await
        .map(|r| matches!(r, Ok(resp) if resp.status().is_success()))
        .unwrap_or(false);
    if http_up {
        return true;
    }
    // chrome://inspect 模式没有 HTTP 发现端点，再试固定 ws 地址
    let direct = format!("ws://127.0.0.1:{}/devtools/browser", port);
    tokio::time::timeout(Duration::from_millis(600), ws_reachable(&direct))
        .await
        .unwrap_or(false)
}

async fn connect_port(port: u16, origin: &str) -> Result<CdpClient> {
    let http = format!("http://127.0.0.1:{}/json/version", port);
    let ws = match reqwest::get(&http).await {
        Ok(resp) if resp.status().is_success() => resp
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| {
                v.get("webSocketDebuggerUrl")
                    .and_then(|u| u.as_str())
                    .map(str::to_string)
            }),
        _ => None,
    };
    let ws = match ws {
        Some(w) => w,
        None => {
            // chrome://inspect 模式：无 HTTP 发现端点，直连固定浏览器 ws 地址
            let direct = format!("ws://127.0.0.1:{}/devtools/browser", port);
            if ws_reachable(&direct).await {
                direct
            } else {
                bail!("127.0.0.1:{} 上没有可用的 CDP 服务", port);
            }
        }
    };
    connect_ws(&ws, origin).await
}

async fn ws_reachable(url: &str) -> bool {
    match tokio_tungstenite::connect_async(url).await {
        Ok((mut s, _)) => {
            let _ = s.close(None).await;
            true
        }
        Err(_) => false,
    }
}

async fn connect_ws(ws: &str, origin: &str) -> Result<CdpClient> {
    let (browser, mut handler) = Browser::connect(ws)
        .await
        .map_err(|e| anyhow!("连接 CDP 失败: {}", e))?;
    let driver = tokio::spawn(async move {
        while let Some(_ev) = handler.next().await {}
    });
    Ok(CdpClient {
        browser,
        _driver: driver,
        page: None,
        origin: origin.to_string(),
    })
}

async fn launch_managed() -> Result<CdpClient> {
    let exe = find_browser_exe().ok_or_else(|| {
        anyhow!("未找到 Chrome/Edge。请安装浏览器，或在浏览器中加载拾光扩展（browser-extension 目录）")
    })?;
    let dir = profile_dir()?;
    std::fs::create_dir_all(&dir)?;
    std::process::Command::new(&exe)
        .arg(format!("--remote-debugging-port={}", MANAGED_PORT))
        .arg(format!("--user-data-dir={}", dir.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-session-crashed-bubble")
        .arg("--hide-crash-restore-bubble")
        .arg("about:blank")
        .spawn()
        .map_err(|e| anyhow!("启动浏览器失败: {}", e))?;
    log::info!("已启动托管调试浏览器: {}", exe.display());
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok(c) = connect_port(MANAGED_PORT, "managed:9223(独立调试实例)").await {
            return Ok(c);
        }
    }
    bail!("浏览器已启动，但 CDP 端口 {} 一直未就绪", MANAGED_PORT)
}

fn find_browser_exe() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for var in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
        if let Ok(base) = std::env::var(var) {
            candidates.push(
                PathBuf::from(&base).join(r"Google\Chrome\Application\chrome.exe"),
            );
            candidates.push(
                PathBuf::from(&base).join(r"Microsoft\Edge\Application\msedge.exe"),
            );
        }
    }
    candidates.into_iter().find(|p| p.exists())
}

fn profile_dir() -> Result<PathBuf> {
    let base = dirs::data_local_dir().ok_or_else(|| anyhow!("无法定位本机数据目录"))?;
    Ok(base.join("deskHelper").join("browser-profile"))
}
