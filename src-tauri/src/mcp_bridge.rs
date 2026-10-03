//! Loopback bridge used by the stdio MCP sidecar. The random token is stored in the user's app data.
use serde_json::{json, Value};
use std::path::Path;
use tauri::AppHandle;
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}};

pub fn spawn(app: AppHandle, app_dir: &Path) {
    let path = app_dir.join("mcp-bridge.json");
    tauri::async_runtime::spawn(async move {
        if let Err(e) = serve(app, path).await { log::warn!("MCP bridge: {e}"); }
    });
}

async fn serve(app: AppHandle, path: std::path::PathBuf) -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let token = uuid::Uuid::new_v4().to_string();
    std::fs::write(&path, json!({"port":port,"token":token}).to_string())?;
    loop {
        let (socket, _) = listener.accept().await?;
        let app = app.clone();
        let token = token.clone();
        tokio::spawn(async move { let _ = handle(socket, app, &token).await; });
    }
}

async fn handle(mut socket: TcpStream, app: AppHandle, token: &str) -> anyhow::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let n = socket.read(&mut chunk).await?;
        if n == 0 { return Ok(()); }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 65536 { return Ok(()); }
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") { break i + 4; }
    };
    let header = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let len: usize = header.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse().ok())).unwrap_or(0);
    if len > 65536 { return Ok(()); }
    while buf.len() < header_end + len {
        let n = socket.read(&mut chunk).await?;
        if n == 0 { return Ok(()); }
        buf.extend_from_slice(&chunk[..n]);
    }
    let auth = header.lines().any(|l| l.split_once(':').map(|(name, value)| name.eq_ignore_ascii_case("x-shiguang-token") && value.trim() == token).unwrap_or(false));
    let response = if !auth { json!({"error":"unauthorized"}) } else {
        match serde_json::from_slice::<Value>(&buf[header_end..header_end+len]) {
            Ok(request) => match dispatch(&app, &request).await {
                Ok(v) => v,
                Err(e) => json!({"error":e.to_string()}),
            },
            Err(e) => json!({"error":e.to_string()}),
        }
    };
    let body = response.to_string();
    let status = if auth { "200 OK" } else { "403 Forbidden" };
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(body.as_bytes()).await?;
    Ok(())
}

async fn dispatch(app: &AppHandle, r: &Value) -> anyhow::Result<Value> {
    let id = r.get("id").and_then(Value::as_str).unwrap_or("");
    let value = match r.get("action").and_then(Value::as_str).unwrap_or("") {
        "create" => json!(crate::delegation::create(app,
            r.get("title").and_then(Value::as_str).unwrap_or(""),
            r.get("goal").and_then(Value::as_str).unwrap_or(""),
            r.get("origin").and_then(Value::as_str).unwrap_or("Codex/Cursor"))?),
        "list" => json!(crate::delegation::list(app)?),
        "get" => json!(crate::delegation::get(app, id)?),
        "reply" => json!(crate::delegation::reply(app, id, "external", r.get("content").and_then(Value::as_str).unwrap_or("")).await?),
        "resume" => json!(crate::delegation::resume(app, id, r.get("content").and_then(Value::as_str).unwrap_or("")).await?),
        "cancel" => json!(crate::delegation::cancel(app, id).await?),
        _ => json!({"error":"unknown action"}),
    };
    Ok(value)
}
