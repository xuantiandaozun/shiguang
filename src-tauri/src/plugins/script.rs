//! 插件脚本运行时（QuickJS）。每次调用新建一个隔离的运行时，限制内存和时长。
//!
//! 脚本写法（CommonJS 或顶层函数都可以，支持 async）：
//! ```js
//! module.exports.run = async (args, sg) => {
//!   const r = sg.callTool("web_search", { query: args.q });   // 需要 permissions: tools:web_search
//!   sg.store.set("last", args.q);
//!   return { ok: true, r };
//! };
//! ```
//! `sg` 提供：callTool / http / readText / store.get / store.set / settings / log。
use super::Plugin;
use anyhow::{anyhow, bail, Context as _, Result};
use rquickjs::{CatchResultExt, Context, Function, Runtime};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

const MEMORY_LIMIT: usize = 64 << 20;
const TOOL_TIMEOUT: Duration = Duration::from_secs(300);
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(10);

const PRELUDE: &str = r#"
var module = { exports: {} };
var exports = module.exports;
function __sg_req(op, payload) {
  var raw = __sg_host(op, JSON.stringify(payload === undefined ? null : payload));
  var res = JSON.parse(raw);
  if (res && res.__error) throw new Error(res.__error);
  return res ? res.value : null;
}
var sg = {
  callTool: function (name, args) { return __sg_req("call_tool", { name: name, args: args || {} }); },
  http: function (req) { return __sg_req("http", req); },
  readText: function (path) { return __sg_req("read_text", { path: path }); },
  store: {
    get: function (key) { return __sg_req("store_get", { key: key }); },
    set: function (key, value) { return __sg_req("store_set", { key: key, value: value }); }
  },
  settings: function () { return __sg_req("settings", {}); },
  log: function () { __sg_req("log", { text: Array.prototype.map.call(arguments, String).join(" ") }); }
};
var console = { log: sg.log, warn: sg.log, error: sg.log, info: sg.log };
var __sg_state = null;
function __sg_out(v) { return JSON.stringify(v === undefined ? null : v); }
function __sg_invoke(name, args) {
  var f = (module.exports && module.exports[name]) || globalThis[name];
  if (typeof f !== "function") throw new Error("脚本没有导出函数 " + name);
  var r = f(args, sg);
  if (r && typeof r.then === "function") {
    __sg_state = { done: false };
    r.then(function (v) { __sg_state = { done: true, value: __sg_out(v) }; },
           function (e) { __sg_state = { done: true, error: String((e && e.stack) || e) }; });
    return "__pending__";
  }
  return __sg_out(r);
}
function __sg_poll() { return JSON.stringify(__sg_state); }
"#;

/// 脚本调用宿主的上下文。
#[derive(Clone)]
pub struct HostCtx {
    pub app: Option<AppHandle>,
    pub plugin: Plugin,
    pub session_id: i64,
    /// 没有 AppHandle 时（测试）插件存储放这里。
    pub data_dir: Option<std::path::PathBuf>,
}

impl HostCtx {
    pub fn new(app: AppHandle, plugin: Plugin, session_id: i64) -> Self {
        Self { app: Some(app), plugin, session_id, data_dir: None }
    }

    fn data_dir(&self) -> Result<std::path::PathBuf> {
        match (&self.app, &self.data_dir) {
            (Some(app), _) => plugin_data_dir(app, &self.plugin.manifest.id),
            (None, Some(dir)) => {
                std::fs::create_dir_all(dir)?;
                Ok(dir.clone())
            }
            (None, None) => bail!("当前环境没有插件存储"),
        }
    }
}

pub fn plugin_data_dir(app: &AppHandle, plugin_id: &str) -> Result<std::path::PathBuf> {
    let dir = app.path().app_data_dir()?.join("plugin-data").join(plugin_id);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn has_permission(plugin: &Plugin, permission: &str) -> bool {
    plugin.builtin || plugin.manifest.permissions.iter().any(|p| p == permission)
}

fn host_op(ctx: &HostCtx, op: &str, payload: Value) -> Result<Value> {
    match op {
        "log" => {
            log::info!("[plugin {}] {}", ctx.plugin.manifest.id, payload["text"].as_str().unwrap_or(""));
            Ok(Value::Null)
        }
        "read_text" => {
            let rel = payload["path"].as_str().unwrap_or_default();
            let path = super::resolve_inside(&ctx.plugin.dir, rel, false)?;
            Ok(json!(std::fs::read_to_string(&path).with_context(|| format!("读取 {rel}"))?))
        }
        "store_get" | "store_set" => {
            let path = ctx.data_dir()?.join("store.json");
            let mut store: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({}));
            let key = payload["key"].as_str().ok_or_else(|| anyhow!("store 需要 key"))?.to_string();
            if op == "store_get" {
                return Ok(store.get(&key).cloned().unwrap_or(Value::Null));
            }
            store[key] = payload["value"].clone();
            std::fs::write(&path, serde_json::to_string_pretty(&store)?)?;
            Ok(Value::Null)
        }
        "settings" => Ok(match &ctx.app {
            Some(app) => super::settings::values(app, &ctx.plugin.manifest.id),
            None => super::settings::defaults_for(&ctx.plugin),
        }),
        "call_tool" => {
            let app = ctx.app.clone().ok_or_else(|| anyhow!("当前环境不能调用工具"))?;
            let name = payload["name"].as_str().unwrap_or_default().to_string();
            if !super::executors::may_call_tool(&ctx.plugin, &name) {
                bail!("插件 {} 没有调用 {name} 的权限（permissions 需包含 tools:{name}）", ctx.plugin.manifest.id);
            }
            let args = payload.get("args").cloned().unwrap_or_else(|| json!({}));
            let session_id = ctx.session_id;
            tauri::async_runtime::block_on(async move {
                let cancel = tokio_util::sync::CancellationToken::new();
                crate::llm::tools::execute(&app, &name, &args, &cancel, session_id).await
            })
        }
        "http" => {
            if !has_permission(&ctx.plugin, "net") {
                bail!("插件 {} 没有 net 权限", ctx.plugin.manifest.id);
            }
            let url = payload["url"].as_str().unwrap_or_default().to_string();
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                bail!("只支持 http/https 地址");
            }
            tauri::async_runtime::block_on(async move {
                let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build()?;
                let method = payload["method"].as_str().unwrap_or("GET").to_ascii_uppercase();
                let mut req = client.request(reqwest::Method::from_bytes(method.as_bytes())?, &url);
                if let Some(headers) = payload["headers"].as_object() {
                    for (k, v) in headers {
                        if let Some(v) = v.as_str() {
                            req = req.header(k, v);
                        }
                    }
                }
                if let Some(body) = payload.get("json").filter(|v| !v.is_null()) {
                    req = req.json(body);
                } else if let Some(body) = payload["body"].as_str() {
                    req = req.body(body.to_string());
                }
                let resp = req.send().await?;
                let status = resp.status().as_u16();
                let text = resp.text().await?;
                let parsed = serde_json::from_str::<Value>(&text).ok();
                Ok(json!({ "status": status, "text": text, "json": parsed }))
            })
        }
        other => bail!("未知的宿主操作 {other}"),
    }
}

/// 在当前线程同步运行脚本函数（调用方需在阻塞线程里）。
pub fn run_blocking(ctx: HostCtx, source: &str, file_label: &str, function: &str, input: &Value, timeout: Duration) -> Result<Value> {
    let rt = Runtime::new().map_err(|e| anyhow!("创建脚本运行时失败: {e}"))?;
    rt.set_memory_limit(MEMORY_LIMIT);
    rt.set_max_stack_size(1 << 20);
    let deadline = Instant::now() + timeout;
    rt.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let context = Context::full(&rt).map_err(|e| anyhow!("创建脚本上下文失败: {e}"))?;
    let host = ctx.clone();
    let first: String = context.with(|js| -> Result<String> {
        let host_fn = Function::new(js.clone(), move |op: String, payload: String| -> String {
            let payload = serde_json::from_str(&payload).unwrap_or(Value::Null);
            match host_op(&host, &op, payload) {
                Ok(value) => json!({ "value": value }).to_string(),
                Err(e) => json!({ "__error": format!("{e:#}") }).to_string(),
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
        js.globals().set("__sg_host", host_fn).map_err(|e| anyhow!("{e}"))?;
        js.eval::<(), _>(PRELUDE).catch(&js).map_err(|e| anyhow!("脚本预置失败: {e}"))?;
        js.eval::<(), _>(source).catch(&js).map_err(|e| anyhow!("{file_label} 执行出错: {e}"))?;
        let call = format!("__sg_invoke({}, {})", json!(function), input);
        js.eval::<String, _>(call).catch(&js).map_err(|e| anyhow!("{file_label} 的 {function} 出错: {e}"))
    })?;
    let text = if first == "__pending__" {
        loop {
            match rt.execute_pending_job() {
                Ok(true) => continue,
                Ok(false) => break,
                Err(_) => break,
            }
        }
        let state: String = context.with(|js| js.eval::<String, _>("__sg_poll()").catch(&js).map_err(|e| anyhow!("{e}")))?;
        let state: Value = serde_json::from_str(&state).unwrap_or(Value::Null);
        if let Some(error) = state.get("error").and_then(Value::as_str) {
            bail!("{file_label} 的 {function} 出错: {error}");
        }
        if state.get("done").and_then(Value::as_bool) != Some(true) {
            bail!("{file_label} 的 {function} 没有完成（Promise 未结束；脚本里的宿主调用都是同步的，不要等待外部事件）");
        }
        state.get("value").and_then(Value::as_str).unwrap_or("null").to_string()
    } else {
        first
    };
    if Instant::now() > deadline {
        bail!("{file_label} 运行超时");
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

pub fn read_script(plugin: &Plugin, file: &str) -> Result<String> {
    let path = super::resolve_inside(&plugin.dir, file, plugin.builtin)?;
    std::fs::read_to_string(&path).with_context(|| format!("读取脚本 {}", path.display()))
}

/// 脚本执行器：读取脚本并在阻塞线程运行。
pub async fn run_tool(app: AppHandle, plugin: Plugin, file: String, function: String, args: Value, session_id: i64) -> Result<Value> {
    let source = read_script(&plugin, &file)?;
    let label = format!("{}/{}", plugin.manifest.id, file);
    tauri::async_runtime::spawn_blocking(move || {
        run_blocking(HostCtx::new(app, plugin, session_id), &source, &label, &function, &args, TOOL_TIMEOUT)
    })
    .await
    .map_err(|e| anyhow!("脚本线程异常: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(permissions: &[&str]) -> HostCtx {
        HostCtx {
            app: None,
            plugin: Plugin {
                manifest: serde_json::from_value(json!({"id":"t","name":"T","permissions":permissions})).unwrap(),
                dir: std::env::temp_dir(),
                builtin: false,
                enabled: true,
            },
            session_id: 0,
            data_dir: None,
        }
    }

    #[test]
    fn runs_sync_and_async_functions() {
        let src = "module.exports.run = (a) => ({ sum: a.x + a.y }); async function later(a) { return a.x * 2; }";
        assert_eq!(run_blocking(ctx(&[]), src, "t.js", "run", &json!({"x":1,"y":2}), HOOK_TIMEOUT).unwrap(), json!({"sum":3}));
        assert_eq!(run_blocking(ctx(&[]), src, "t.js", "later", &json!({"x":4}), HOOK_TIMEOUT).unwrap(), json!(8));
    }

    #[test]
    fn errors_and_limits_surface() {
        let err = run_blocking(ctx(&[]), "function run(){ throw new Error('坏了') }", "t.js", "run", &json!({}), HOOK_TIMEOUT).unwrap_err();
        assert!(format!("{err:#}").contains("坏了"));
        let err = run_blocking(ctx(&[]), "function run(){ while(true){} }", "t.js", "run", &json!({}), Duration::from_millis(200)).unwrap_err();
        assert!(!format!("{err:#}").is_empty());
        let err = run_blocking(ctx(&[]), "function run(){ return sg.http({url:'https://x'}) }", "t.js", "run", &json!({}), HOOK_TIMEOUT).unwrap_err();
        assert!(format!("{err:#}").contains("net"));
        let err = run_blocking(ctx(&[]), "function run(){ return sg.readText('../x') }", "t.js", "run", &json!({}), HOOK_TIMEOUT).unwrap_err();
        assert!(format!("{err:#}").contains("越出"));
    }
}
