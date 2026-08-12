//! 浏览器操作统一入口。
//! 通道：已连接的拾光浏览器扩展（操作用户当前浏览器，内部含 scripting /
//! chrome.debugger 双执行面）⇄ 独立 CDP（127.0.0.1:9222 手动调试 / 9223 托管实例）。
//! 路由：默认扩展优先；命中「通道能力型错误」（CSP 拦截、注入被拒等）自动故障转移到
//! CDP 并重试同一动作；params.channel 可强制指定（extension / debugger / cdp）。
//! 冷启动：扩展未连接且装过扩展时，先拉起系统默认浏览器（带登录态）等扩展接入，
//! 接不上才回退独立 CDP 实例。

mod cdp;
mod ext;
mod launch;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Mozilla Readability：正文抽取（与 page-api 一并注入）
pub const READABILITY_JS: &str = include_str!("../../../browser-extension/readability.js");
/// 与浏览器扩展共用的页面操作 JS（单一实现，注入后提供 window.__dh）
pub const PAGE_API_JS: &str = include_str!("../../../browser-extension/page-api.js");

/// CDP / debugger 通道注入源：Readability + page-api
pub fn page_inject_js() -> String {
    format!("{}\n{}", READABILITY_JS, PAGE_API_JS)
}

pub struct Hub {
    ext: ext::ExtServer,
    cdp: tokio::sync::Mutex<Option<cdp::CdpClient>>,
    /// 标记文件：扩展连上过就存在（app_data/browser-ext-seen）
    ext_seen_marker: PathBuf,
    /// 串行化「拉起默认浏览器并等扩展」过程，避免并发重复拉起
    launch_lock: tokio::sync::Mutex<()>,
}

impl Hub {
    pub fn spawn(app_dir: &Path) -> Hub {
        let marker = app_dir.join("browser-ext-seen");
        Hub {
            ext: ext::ExtServer::spawn(marker.clone()),
            cdp: tokio::sync::Mutex::new(None),
            ext_seen_marker: marker,
            launch_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub async fn status(&self) -> Value {
        let cdp_desc = self.cdp.lock().await.as_ref().map(|c| c.describe());
        json!({
            "extension_listening": self.ext.listening(),
            "extension_connected": self.ext.connected(),
            "extension_seen": self.ext_seen(),
            "cdp_connected": cdp_desc.is_some(),
            "cdp_channel": cdp_desc,
            "hint": "默认走扩展（scripting 隔离世界）；浏览器未启动且装过扩展时自动拉起系统默认浏览器（带登录态）等扩展接入，接不上才回退独立 CDP。CSP 受限站点（如 X）自动降级：扩展内 debugger 通道 → 独立 CDP（9222 手动调试 / 9223 托管实例）。响应 channel 字段标识实际通道（extension / extension-debugger / cdp），channel 变化后快照编号失效需重新获取。browser_evaluate 可用 channel 参数强制指定通道。",
        })
    }

    pub async fn call(&self, action: &str, params: Value) -> Result<Value> {
        // LLM 显式指定通道时跳过自动路由
        match params.get("channel").and_then(|v| v.as_str()) {
            Some("cdp") => return self.call_cdp(action, params).await,
            Some("extension") | Some("debugger") => {
                if !self.ext.connected() {
                    bail!("channel={} 需要浏览器扩展已连接", params["channel"]);
                }
                let mut v = self.ext.call(action, params).await?;
                stamp_ext(&mut v);
                return Ok(v);
            }
            _ => {}
        }

        // 扩展未连接（浏览器多半没启动）：优先拉起用户默认浏览器等扩展接入
        self.ensure_default_browser(action, &params).await;

        if self.ext.connected() {
            match self.ext.call(action, params.clone()).await {
                Ok(mut v) => {
                    stamp_ext(&mut v);
                    return Ok(v);
                }
                Err(e) => {
                    if !is_channel_error(&e) {
                        return Err(e);
                    }
                    // 扩展通道做不到（CSP/注入被拒，且扩展内 debugger 兜底也失败）：
                    // 取当前活动页 URL，让 CDP 对齐到同一页面后重试
                    let target_url =
                        self.ext.call("info", json!({})).await.ok().and_then(|i| {
                            i.get("url").and_then(|u| u.as_str()).map(str::to_string)
                        });
                    let mut p = params;
                    if let Some(u) = target_url {
                        p["__target_url"] = json!(u);
                    }
                    return match self.call_cdp(action, p).await {
                        Ok(mut v) => {
                            if let Some(obj) = v.as_object_mut() {
                                obj.insert("failover_from".into(), json!("extension"));
                                obj.insert("failover_reason".into(), json!(e.to_string()));
                                obj.insert(
                                    "note".into(),
                                    json!(
                                        "已切换到 CDP 通道，之前的快照编号已失效，请重新获取快照"
                                    ),
                                );
                            }
                            Ok(v)
                        }
                        Err(ce) => Err(anyhow!("扩展通道失败（{}）；CDP 兜底也失败（{}）", e, ce)),
                    };
                }
            }
        }
        self.call_cdp(action, params).await
    }

    async fn call_cdp(&self, action: &str, params: Value) -> Result<Value> {
        let mut guard = self.cdp.lock().await;
        if guard.is_none() {
            *guard = Some(cdp::CdpClient::connect_or_launch().await?);
        }
        let client = guard.as_mut().expect("cdp client just set");
        match client.call(action, params).await {
            Ok(mut v) => {
                stamp(&mut v, "cdp");
                Ok(v)
            }
            Err(e) => {
                // 可能浏览器被关了，清掉连接下次重建
                *guard = None;
                Err(e)
            }
        }
    }

    /// 扩展在这台机器上连上过吗（装过扩展才会拉起默认浏览器等它接入，
    /// 否则拉了也白拉——白白替用户开一次浏览器后还得回退独立实例）
    fn ext_seen(&self) -> bool {
        self.ext_seen_marker.exists()
    }

    /// 扩展未连接时尝试拉起系统默认浏览器并等待扩展接入；
    /// 接不上则原样返回，由 call_cdp 走 9222/9223/托管实例兜底。
    async fn ensure_default_browser(&self, action: &str, params: &Value) {
        if self.ext.connected() || !self.ext_seen() {
            return;
        }
        // 9222/9223 已有可控实例在跑，交给 call_cdp 复用，不必再开一个浏览器
        if cdp::inspect_or_managed_up().await {
            return;
        }
        let _guard = self.launch_lock.lock().await;
        if self.ext.connected() {
            return;
        }
        // 可能只是 MV3 service worker 瞬断重连间隙，先给它一点时间
        if self.wait_ext(Duration::from_millis(1500)).await {
            return;
        }
        // navigate 的 url 仅作注册表解析失败时的兜底打开方式；
        // 正常路径裸启动浏览器，页面由扩展通道统一 navigate，避免重复开标签
        let fallback_url = if action == "navigate" {
            params.get("url").and_then(|v| v.as_str())
        } else {
            None
        };
        if let Err(e) = launch::launch_default(fallback_url) {
            log::warn!("拉起默认浏览器失败: {}", e);
            return;
        }
        if self.wait_ext(Duration::from_secs(15)).await {
            log::info!("默认浏览器扩展已接入，走用户浏览器通道");
        } else {
            log::warn!("默认浏览器已拉起但扩展 15 秒内未接入，回退独立 CDP 实例");
        }
    }

    async fn wait_ext(&self, d: Duration) -> bool {
        let step = Duration::from_millis(250);
        let mut waited = Duration::ZERO;
        while waited < d {
            if self.ext.connected() {
                return true;
            }
            tokio::time::sleep(step).await;
            waited += step;
        }
        self.ext.connected()
    }
}

/// 通道能力型错误：本通道做不到、换通道才可能做到（CSP 拦截 / 注入被拒 / 调试器无法附着）。
/// 业务错误（元素不存在、没有活动标签页、参数缺失等）不在此列，直接回传给调用方。
fn is_channel_error(e: &anyhow::Error) -> bool {
    let m = e.to_string().to_lowercase();
    const PATTERNS: &[&str] = &[
        "evalerror",
        "unsafe-eval",
        "refused to evaluate",
        "content security policy",
        "cannot access",
        "cannot script",
        "cannot be scripted",
        "cannot attach",
        "not allowed",
    ];
    PATTERNS.iter().any(|p| m.contains(p))
}

/// 扩展返回体里的 via 字段标识扩展内部实际执行面，折叠进 channel 标记
fn stamp_ext(v: &mut Value) {
    let via = v
        .as_object_mut()
        .and_then(|obj| obj.remove("via"))
        .and_then(|x| x.as_str().map(str::to_string));
    stamp(
        v,
        if via.as_deref() == Some("debugger") {
            "extension-debugger"
        } else {
            "extension"
        },
    );
}

fn stamp(v: &mut Value, channel: &str) {
    if let Some(obj) = v.as_object_mut() {
        obj.insert("channel".into(), Value::String(channel.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_api_exposes_adaptive_interaction_primitives() {
        assert!(PAGE_API_JS.contains("const DH_VER = 6"));
        assert!(PAGE_API_JS.contains("aria-controls"));
        assert!(PAGE_API_JS.contains("unique_fuzzy"));
        assert!(PAGE_API_JS.contains("activePopupScrollable"));
        assert!(PAGE_API_JS.contains("ref: (ref) => getRef(ref)"));
    }

    #[test]
    fn channel_error_classification() {
        // CSP 拦截（X 等站点 MAIN-world eval 被拒）→ 应转移
        assert!(is_channel_error(&anyhow!(
            "EvalError: Refused to evaluate a string as JavaScript because 'unsafe-eval' is not an allowed source of script in the following Content Security Policy directive"
        )));
        assert!(is_channel_error(&anyhow!(
            "Cannot access contents of url \"chrome://extensions/\""
        )));
        assert!(is_channel_error(&anyhow!("Cannot attach to this target")));
        // 业务错误 → 不转移，原样返回给 LLM
        assert!(!is_channel_error(&anyhow!(
            "元素 [3] 不存在或已失效，请重新获取快照"
        )));
        assert!(!is_channel_error(&anyhow!("没有活动标签页")));
        assert!(!is_channel_error(&anyhow!("缺少参数 ref")));
        // 页面 JS 自身抛出的普通异常不误判
        assert!(!is_channel_error(&anyhow!(
            "Uncaught TypeError: Cannot read properties of null"
        )));
    }

    #[test]
    fn ext_via_folded_into_channel() {
        let mut v = json!({ "result": 1, "via": "debugger" });
        stamp_ext(&mut v);
        assert_eq!(v["channel"], "extension-debugger");
        assert!(v.get("via").is_none());

        let mut v2 = json!({ "ok": true });
        stamp_ext(&mut v2);
        assert_eq!(v2["channel"], "extension");
    }
}
