//! 浏览器操作统一入口。
//! 通道：已连接的拾光浏览器扩展（操作用户当前浏览器，内部含 chrome.debugger
//! 主世界 / scripting 隔离世界双执行面）⇄ 独立 CDP（127.0.0.1:9222 手动调试 / 9223 托管实例）。
//! 路由：默认扩展 debugger 优先（主世界，AI 操控更稳）；无法附着时降级 scripting；
//! 两者都失败才故障转移到独立 CDP。params.channel 可强制指定（extension / debugger / cdp）。
//! 冷启动：扩展未连接且装过扩展时，先拉起系统默认浏览器（带登录态）等扩展接入，
//! 接不上才回退独立 CDP 实例。

mod cdp;
mod ext;
mod launch;
mod unpack;

pub use unpack::{
    ensure_unpacked as ensure_extension_unpacked, info as extension_info, ExtensionInfo,
};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod operations;

/// 页面注入脚本由 browser 插件的 assets 提供（运行时读取，改完即生效）：
/// readability（Mozilla Readability 正文抽取）、page_api（与扩展共用的页面操作，提供 window.__dh）、
/// page_operations、page_runtime（真实输入引擎的页面侧判定）。
pub fn page_asset(key: &str) -> String {
    crate::plugins::snapshot().asset("browser", key).unwrap_or_else(|e| {
        log::error!("读取浏览器页面脚本 {key} 失败: {e:#}");
        String::new()
    })
}

/// All transports share the same low-level primitives and semantic operations.
pub fn page_inject_js() -> String {
    format!(
        "{}
{}
{}
{}",
        page_asset("readability"),
        page_asset("page_api"),
        page_asset("page_operations"),
        page_asset("page_runtime")
    )
}

/// 给动态脚本注入 `$el` / `$args`。
/// `args` 即使没有 `ref` 也要可用：模型常把长文本、文件名放进 args，避免拼进 expression。
pub fn wrap_evaluate_expression(expr: &str, ref_: Option<u64>, args: &Value) -> String {
    if ref_.is_none() && args.is_null() {
        return expr.to_string();
    }
    let args_json = serde_json::to_string(args).unwrap_or_else(|_| "null".to_string());
    if let Some(ref_) = ref_ {
        let missing = format!("元素 [{ref_}] 不存在或已失效，请重新获取快照");
        let missing_json =
            serde_json::to_string(&missing).unwrap_or_else(|_| "\"元素已失效\"".into());
        format!(
            "(async()=>{{const $el=window.__dh.ref({ref_});if(!$el)throw new Error({missing_json});const $args={args_json};return await ({expr});}})()"
        )
    } else {
        format!("(async()=>{{const $args={args_json};return await ({expr});}})()")
    }
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
            "hint": "默认走扩展 debugger 主世界（会显示调试提示条）；无法附着时降级到 scripting 隔离世界。浏览器未启动且装过扩展时自动拉起系统默认浏览器（带登录态）等扩展接入，接不上才回退独立 CDP。debugger 与 scripting 都失败时再降独立 CDP（9222 手动调试 / 9223 托管实例）。响应 channel 字段标识实际通道（extension-debugger / extension / cdp），channel 变化后快照编号失效需重新获取。browser_evaluate 可用 channel 参数强制指定通道。",
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
                    // 扩展 debugger 与 scripting 都做不到：取当前活动页 URL，让 CDP 对齐后重试
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

    /// 开始录屏。扩展已连接走扩展 debugger，否则走独立 CDP。
    /// 返回 (结果, 推送消息接收端, 通道名)。params 需含 session 与 script（页面脚本 + 安装调用）。
    pub async fn record_start(
        &self,
        params: Value,
    ) -> Result<(Value, tokio::sync::mpsc::UnboundedReceiver<Value>, &'static str)> {
        self.ensure_default_browser("record_start", &params).await;
        if self.ext.connected() {
            // 扩展连着时录的就是用户正在用的浏览器，失败也不换到另一个浏览器实例，免得录错地方
            let rx = self.ext.subscribe();
            return match self.ext.call("record_start", params).await {
                Ok(v) => Ok((v, rx, "extension")),
                Err(e) => {
                    self.ext.unsubscribe();
                    if e.to_string().contains("未知动作") {
                        bail!("浏览器扩展版本太旧，不支持录屏。请在浏览器的扩展管理页重新加载「拾光浏览器桥」后再试");
                    }
                    Err(anyhow!("无法录制当前标签页：{e}（浏览器内置页面如设置页、扩展商店不能录）"))
                }
            };
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut guard = self.cdp.lock().await;
        if guard.is_none() {
            *guard = Some(cdp::CdpClient::connect_or_launch().await?);
        }
        let client = guard.as_mut().expect("cdp client just set");
        let v = client.record_start(&params, tx).await?;
        Ok((v, rx, "cdp"))
    }

    pub async fn record_stop(&self, channel: &str, session: &str) -> Result<()> {
        if channel == "extension" {
            let r = self.ext.call("record_stop", json!({"session": session})).await;
            self.ext.unsubscribe();
            r.map(|_| ())
        } else {
            if let Some(c) = self.cdp.lock().await.as_mut() {
                c.record_stop().await;
            }
            Ok(())
        }
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
    use serde_json::{json, Value};

    #[test]
    fn page_api_exposes_adaptive_interaction_primitives() {
        assert!(page_asset("page_api").contains("const DH_VER = 11"));
        assert!(page_asset("page_api").contains("relocate("));
        assert!(page_asset("page_api").contains("aria-controls"));
        assert!(page_asset("page_api").contains("unique_fuzzy"));
        assert!(page_asset("page_api").contains("activePopupScrollable"));
        assert!(page_asset("page_api").contains("ref: (ref) => getRef(ref)"));
    }

    #[test]
    fn evaluate_wraps_args_without_requiring_ref() {
        let wrapped = wrap_evaluate_expression(
            "(async()=>{return $args.names;})()",
            None,
            &json!({ "names": ["a.png"] }),
        );
        assert!(wrapped.contains("const $args="));
        assert!(wrapped.contains("names"));
        assert!(!wrapped.contains("$el"));
        assert_eq!(wrap_evaluate_expression("1+1", None, &Value::Null), "1+1");
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
