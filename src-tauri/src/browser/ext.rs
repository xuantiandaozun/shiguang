//! 浏览器扩展桥：本地 WebSocket 服务，等待 browser-extension 扩展连入。
//! 请求/响应协议：{id, action, params} → {id, ok, data|error}。

use anyhow::{anyhow, bail, Result};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, WebSocketStream};

const EXT_PORT: u16 = 17893;

type Ws = WebSocketStream<TcpStream>;

struct Inner {
    writer: Mutex<Option<SplitSink<Ws, Message>>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    next_id: AtomicU64,
    connected: AtomicBool,
    listening: AtomicBool,
    /// 扩展连上后落一个标记文件：这台机器装过扩展。
    /// Hub 据此决定浏览器未启动时值得拉起默认浏览器等扩展，而不是直接开独立实例。
    seen_marker: Option<PathBuf>,
}

#[derive(Clone)]
pub struct ExtServer {
    inner: Arc<Inner>,
}

impl ExtServer {
    fn new(seen_marker: Option<PathBuf>) -> ExtServer {
        ExtServer {
            inner: Arc::new(Inner {
                writer: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                connected: AtomicBool::new(false),
                listening: AtomicBool::new(false),
                seen_marker,
            }),
        }
    }

    pub fn spawn(seen_marker: PathBuf) -> ExtServer {
        let server = Self::new(Some(seen_marker));
        let s = server.clone();
        tauri::async_runtime::spawn(async move {
            match TcpListener::bind(("127.0.0.1", EXT_PORT)).await {
                Ok(l) => {
                    s.inner.listening.store(true, Ordering::SeqCst);
                    s.serve(l).await;
                }
                Err(e) => log::error!("浏览器扩展桥端口 {} 绑定失败: {}", EXT_PORT, e),
            }
        });
        server
    }

    /// WS 服务是否已成功监听（区分「没监听」和「监听了但扩展未连」）
    pub fn listening(&self) -> bool {
        self.inner.listening.load(Ordering::SeqCst)
    }

    async fn serve(self, listener: TcpListener) {
        log::info!("浏览器扩展桥已监听 {:?}", listener.local_addr());
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let s = self.clone();
                    tokio::spawn(async move { s.handle_conn(stream).await });
                }
                Err(e) => {
                    log::warn!("扩展桥 accept 失败: {}", e);
                    break;
                }
            }
        }
    }

    async fn handle_conn(self, stream: TcpStream) {
        let ws = match accept_async(stream).await {
            Ok(w) => w,
            Err(_) => return,
        };
        log::info!("浏览器扩展已连接");
        if let Some(p) = &self.inner.seen_marker {
            if !p.exists() {
                let _ = std::fs::write(p, b"");
            }
        }
        let (write, mut read): (SplitSink<Ws, Message>, SplitStream<Ws>) = ws.split();
        {
            *self.inner.writer.lock().await = Some(write);
        }
        self.inner.connected.store(true, Ordering::SeqCst);
        while let Some(msg) = read.next().await {
            let Ok(msg) = msg else { break };
            if let Message::Text(t) = msg {
                // 扩展侧保活 ping：回复 pong，让 MV3 service worker 的 WebSocket 活动重置空闲计时
                if t.as_str() == "ping" {
                    let mut guard = self.inner.writer.lock().await;
                    if let Some(w) = guard.as_mut() {
                        let _ = w.send(Message::Text("pong".into())).await;
                    }
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                    let id = v.get("id").and_then(|x| x.as_u64()).unwrap_or(0);
                    if let Some(tx) = self.inner.pending.lock().await.remove(&id) {
                        let _ = tx.send(v);
                    }
                }
            }
        }
        self.inner.connected.store(false, Ordering::SeqCst);
        *self.inner.writer.lock().await = None;
        log::info!("浏览器扩展已断开");
    }

    pub fn connected(&self) -> bool {
        self.inner.connected.load(Ordering::SeqCst)
    }

    pub async fn call(&self, action: &str, params: Value) -> Result<Value> {
        if !self.connected() {
            bail!("浏览器扩展未连接");
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel::<Value>();
        self.inner.pending.lock().await.insert(id, tx);
        let req = json!({ "id": id, "action": action, "params": params });
        {
            let mut guard = self.inner.writer.lock().await;
            let w = guard.as_mut().ok_or_else(|| anyhow!("浏览器扩展未连接"))?;
            w.send(Message::Text(req.to_string().into()))
                .await
                .map_err(|e| anyhow!("发送扩展消息失败: {}", e))?;
        }
        let resp = tokio::time::timeout(Duration::from_secs(35), rx)
            .await
            .map_err(|_| anyhow!("扩展响应超时"))?
            .map_err(|_| anyhow!("扩展连接中断"))?;
        if resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            Ok(resp.get("data").cloned().unwrap_or(Value::Null))
        } else {
            let err = resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("扩展调用失败")
                .to_string();
            Err(anyhow!("{}", err))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::connect_async;

    async fn start(port: u16) -> ExtServer {
        let s = ExtServer::new(None);
        let s2 = s.clone();
        let l = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        tokio::spawn(async move { s2.serve(l).await });
        s
    }

    #[tokio::test]
    async fn ping_pong_and_request_response() {
        let server = start(17894).await;
        let (mut ws, _) = connect_async("ws://127.0.0.1:17894").await.unwrap();
        for _ in 0..20 {
            if server.connected() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(server.connected());

        // ping → pong（MV3 service worker 保活）
        ws.send(Message::Text("ping".into())).await.unwrap();
        let msg = ws.next().await.unwrap().unwrap();
        assert_eq!(msg.into_text().unwrap().as_str(), "pong");

        // 服务端发起 call，模拟扩展应答
        let client = tokio::spawn(async move {
            let msg = ws.next().await.unwrap().unwrap();
            let v: Value = serde_json::from_str(msg.into_text().unwrap().as_str()).unwrap();
            assert_eq!(v["action"], "echo");
            ws.send(Message::Text(
                json!({ "id": v["id"], "ok": true, "data": { "hello": "world" } })
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        });
        let resp = server.call("echo", json!({})).await.unwrap();
        client.await.unwrap();
        assert_eq!(resp["hello"], "world");
    }
}
