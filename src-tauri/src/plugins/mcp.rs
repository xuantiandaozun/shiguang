//! 插件 MCP 服务（stdio，JSON-RPC 按行分隔）的客户端与进程管理。
//! 服务在插件启用时后台启动；`import_tools` 的服务初始化完成后会触发一次重载，把
//! tools/list 结果注册为工具。插件文件变化时由 watch 重启对应服务。
use super::manifest::McpServerSpec;
use super::{Plugin, ToolSpec};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const CALL_TIMEOUT: Duration = Duration::from_secs(600);
const INIT_TIMEOUT: Duration = Duration::from_secs(30);

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, String>>>>>;

pub struct Client {
    spec_key: String,
    stdin: tokio::sync::Mutex<tokio::process::ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
    tools: Mutex<Vec<Value>>,
    child: Mutex<Option<tokio::process::Child>>,
}

impl Client {
    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let line = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string();
        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(message))) => Err(anyhow!(message)),
            Ok(Err(_)) => Err(anyhow!("MCP 服务已退出")),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(anyhow!("MCP 请求 {method} 超时"))
            }
        }
    }

    async fn notify(&self, method: &str) -> Result<()> {
        let line = json!({"jsonrpc":"2.0","method":method}).to_string();
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    fn kill(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.start_kill();
        }
    }
}

type Key = (String, String);

fn clients() -> &'static Mutex<HashMap<Key, Arc<Client>>> {
    static CLIENTS: std::sync::OnceLock<Mutex<HashMap<Key, Arc<Client>>>> = std::sync::OnceLock::new();
    CLIENTS.get_or_init(Default::default)
}

fn spec_key(plugin: &Plugin, spec: &McpServerSpec) -> String {
    format!("{}|{}", plugin.dir.display(), serde_json::to_string(spec).unwrap_or_default())
}

async fn spawn(plugin: &Plugin, server: &str, spec: &McpServerSpec) -> Result<Arc<Client>> {
    let render = |s: &str| super::executors::render_template(s, plugin, &Value::Null, false);
    let cwd = match &spec.cwd {
        Some(rel) => super::resolve_inside(&plugin.dir, rel, plugin.builtin)?,
        None => plugin.dir.clone(),
    };
    let mut cmd = tokio::process::Command::new(render(&spec.command));
    cmd.args(spec.args.iter().map(|a| render(a)))
        .current_dir(&cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .env("SHIGUANG_PLUGIN_ID", &plugin.manifest.id)
        .env("SHIGUANG_PLUGIN_DIR", &plugin.dir);
    for (k, v) in &spec.env {
        cmd.env(k, render(v));
    }
    #[cfg(windows)]
    cmd.creation_flags(crate::tasks::CREATE_NO_WINDOW);
    let mut child = cmd.spawn().with_context(|| format!("启动插件 {} 的 MCP 服务 {server} 失败", plugin.manifest.id))?;
    let stdin = child.stdin.take().ok_or_else(|| anyhow!("MCP 服务没有 stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow!("MCP 服务没有 stdout"))?;
    let stderr = child.stderr.take();
    let pending: Pending = Default::default();
    let reader_pending = pending.clone();
    let label = format!("{}/{server}", plugin.manifest.id);
    tauri::async_runtime::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            let Some(id) = msg.get("id").and_then(Value::as_u64) else { continue };
            if msg.get("method").is_some() {
                continue; // 服务端发起的请求（如 sampling）暂不支持
            }
            let Some(tx) = reader_pending.lock().unwrap().remove(&id) else { continue };
            let result = match msg.get("error") {
                Some(err) => Err(err.get("message").and_then(Value::as_str).unwrap_or("MCP 错误").to_string()),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(result);
        }
        reader_pending.lock().unwrap().clear();
    });
    if let Some(stderr) = stderr {
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::info!("[mcp {label}] {line}");
            }
        });
    }
    let client = Arc::new(Client {
        spec_key: spec_key(plugin, spec),
        stdin: tokio::sync::Mutex::new(stdin),
        pending,
        next_id: AtomicU64::new(1),
        tools: Mutex::new(Vec::new()),
        child: Mutex::new(Some(child)),
    });
    client
        .request(
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"shiguang","version":env!("CARGO_PKG_VERSION")}}),
            INIT_TIMEOUT,
        )
        .await?;
    client.notify("notifications/initialized").await?;
    if spec.import_tools {
        let listed = client.request("tools/list", json!({}), INIT_TIMEOUT).await?;
        *client.tools.lock().unwrap() = listed.get("tools").and_then(Value::as_array).cloned().unwrap_or_default();
    }
    Ok(client)
}

async fn client_for(plugin: &Plugin, server: &str) -> Result<Arc<Client>> {
    let spec = plugin
        .manifest
        .mcp_servers
        .get(server)
        .ok_or_else(|| anyhow!("插件 {} 没有 MCP 服务 {server}", plugin.manifest.id))?;
    let key = (plugin.manifest.id.clone(), server.to_string());
    let wanted = spec_key(plugin, spec);
    if let Some(existing) = clients().lock().unwrap().get(&key).cloned() {
        if existing.spec_key == wanted && existing.child.lock().unwrap().as_mut().is_some_and(|c| c.try_wait().ok().flatten().is_none()) {
            return Ok(existing);
        }
    }
    let client = spawn(plugin, server, spec).await?;
    if let Some(old) = clients().lock().unwrap().insert(key, client.clone()) {
        old.kill();
    }
    Ok(client)
}

/// 调用插件 MCP 服务的工具。
pub async fn call(plugin: &Plugin, server: &str, tool: &str, args: &Value, cancel: &CancellationToken) -> Result<Value> {
    let client = client_for(plugin, server).await?;
    let result = tokio::select! {
        _ = cancel.cancelled() => bail!("已取消"),
        result = client.request("tools/call", json!({"name": tool, "arguments": args}), CALL_TIMEOUT) => result?,
    };
    let texts: Vec<&str> = result
        .get("content")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(|i| i.get("text").and_then(Value::as_str)).collect())
        .unwrap_or_default();
    let joined = texts.join("\n");
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        bail!("{}", if joined.is_empty() { "MCP 工具返回错误".to_string() } else { joined });
    }
    if let Some(structured) = result.get("structuredContent") {
        return Ok(structured.clone());
    }
    Ok(serde_json::from_str(joined.trim()).unwrap_or_else(|_| json!({ "output": joined })))
}

/// 已初始化服务导出的工具（快照构建时同步读取）。
pub fn imported_tools(plugin: &Plugin) -> Vec<ToolSpec> {
    let mut out = Vec::new();
    let map = clients().lock().unwrap();
    for (server, spec) in &plugin.manifest.mcp_servers {
        if !spec.import_tools {
            continue;
        }
        let Some(client) = map.get(&(plugin.manifest.id.clone(), server.clone())) else { continue };
        for tool in client.tools.lock().unwrap().iter() {
            let Some(raw) = tool.get("name").and_then(Value::as_str) else { continue };
            let name = format!("{}{}", spec.tool_prefix, raw);
            out.push(ToolSpec {
                definition: json!({"type":"function","function":{
                    "name": name,
                    "description": tool.get("description").and_then(Value::as_str).unwrap_or(""),
                    "parameters": tool.get("inputSchema").cloned().unwrap_or_else(|| json!({"type":"object","properties":{}})),
                }}),
                category: spec.category.clone().unwrap_or_else(|| plugin.manifest.id.clone()),
                executor: json!({"kind":"mcp","server":server,"tool":raw}),
                when: None,
                label: None,
                plugin: plugin.manifest.id.clone(),
                name,
            });
        }
    }
    out
}

/// 根据快照启动/停止服务。需要导入工具的服务初始化后触发一次重载。
pub fn sync(snapshot: &super::Snapshot) {
    let mut wanted = HashMap::new();
    for plugin in snapshot.enabled_plugins() {
        for (server, spec) in &plugin.manifest.mcp_servers {
            wanted.insert((plugin.manifest.id.clone(), server.clone()), (plugin.clone(), spec.clone()));
        }
    }
    let stale: Vec<Key> = {
        let map = clients().lock().unwrap();
        map.iter()
            .filter(|(key, client)| wanted.get(*key).map(|(p, s)| spec_key(p, s) != client.spec_key).unwrap_or(true))
            .map(|(key, _)| key.clone())
            .collect()
    };
    for key in stale {
        if let Some(client) = clients().lock().unwrap().remove(&key) {
            client.kill();
        }
    }
    for (key, (plugin, spec)) in wanted {
        if !spec.import_tools || clients().lock().unwrap().contains_key(&key) {
            continue;
        }
        tauri::async_runtime::spawn(async move {
            match client_for(&plugin, &key.1).await {
                Ok(_) => {
                    // 新工具进入快照；服务配置未变，重载不会再次重启它。
                    tauri::async_runtime::spawn_blocking(|| {
                        super::host().reload();
                    });
                }
                Err(e) => log::warn!("插件 {} 的 MCP 服务 {} 启动失败: {e:#}", key.0, key.1),
            }
        });
    }
}

/// 插件文件变化时重启它的服务。
pub fn restart_plugin(plugin_id: &str) {
    let keys: Vec<Key> = clients().lock().unwrap().keys().filter(|(p, _)| p == plugin_id).cloned().collect();
    for key in keys {
        if let Some(client) = clients().lock().unwrap().remove(&key) {
            client.kill();
        }
    }
}

pub fn stop_all() {
    for (_, client) in clients().lock().unwrap().drain() {
        client.kill();
    }
}
