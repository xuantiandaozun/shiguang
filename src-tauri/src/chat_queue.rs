//! 聊天消息队列：AI 正在回复时用户追加的消息（追问）和后台事件通知先排队。
//! 主循环每轮开头取走当前会话的排队消息；一轮回复结束时还有剩余，就接着开新一轮。
//! 排队消息在被取走时才入库，保证数据库里的消息顺序和模型实际看到的一致。

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

/// 后台事件消息的前缀。以 user 角色入库，让模型据此继续；界面按通知样式显示。
pub const EVENT_PREFIX: &str = "【后台通知】";
/// 两条用户消息之间最多处理多少条后台事件，超过后只记系统消息、不再自动开新一轮，防止无人值守时打转。
const MAX_AUTO_TURNS: usize = 12;

#[derive(Clone, Debug)]
pub struct Pending {
    pub session_id: i64,
    pub content: String,
    /// true = 后台事件通知；false = 用户追问
    pub event: bool,
}

#[derive(Default)]
struct Inner {
    running: bool,
    pending: Vec<Pending>,
    auto_streak: HashMap<i64, usize>,
}

#[derive(Default)]
pub struct ChatQueue {
    inner: Mutex<Inner>,
    user_wake: tokio::sync::Notify,
}

pub enum Submit {
    /// 当前空闲，调用方应立即开一轮
    Start,
    /// 已排队，等正在进行的回复取走
    Queued,
}

impl ChatQueue {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 空闲则占用（返回 Start），忙则排队（返回 Queued）。判断与排队在同一把锁里，不会漏消息。
    pub fn submit(&self, app: &AppHandle, item: Pending) -> Submit {
        let mut inner = self.lock();
        if !item.event {
            inner.auto_streak.remove(&item.session_id);
        }
        if inner.running {
            let user = !item.event;
            inner.pending.push(item);
            drop(inner);
            if user {
                self.user_wake.notify_waiters();
            }
            return Submit::Queued;
        }
        inner.running = true;
        app.state::<crate::AppState>().chat_busy.store(true, Ordering::SeqCst);
        Submit::Start
    }

    /// 取走某会话的全部排队消息（主循环每轮开头调用）。
    pub fn take(&self, session_id: i64) -> Vec<Pending> {
        let mut inner = self.lock();
        let (mine, rest): (Vec<_>, Vec<_>) =
            inner.pending.drain(..).partition(|p| p.session_id == session_id);
        inner.pending = rest;
        mine
    }

    fn has_user_message(&self, session_id: i64) -> bool {
        self.lock().pending.iter().any(|p| !p.event && p.session_id == session_id)
    }

    /// 等到该会话有新的用户追问。用于打断等待类工具。
    pub async fn user_message_arrived(&self, session_id: i64) {
        loop {
            let notified = self.user_wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.has_user_message(session_id) {
                return;
            }
            notified.await;
        }
    }

    /// 一轮结束：还有排队消息就返回下一批（保持占用），否则释放。
    fn next_or_idle(&self, app: &AppHandle) -> Option<(i64, Vec<Pending>)> {
        let mut inner = self.lock();
        if let Some(first) = inner.pending.first() {
            let session_id = first.session_id;
            let (mine, rest): (Vec<_>, Vec<_>) =
                inner.pending.drain(..).partition(|p| p.session_id == session_id);
            inner.pending = rest;
            return Some((session_id, mine));
        }
        inner.running = false;
        app.state::<crate::AppState>().chat_busy.store(false, Ordering::SeqCst);
        None
    }

    /// 放弃本次占用（开轮前出错时用）。
    pub fn release(&self, app: &AppHandle) {
        self.lock().running = false;
        app.state::<crate::AppState>().chat_busy.store(false, Ordering::SeqCst);
    }

    /// 用户点停止：排队的消息不再自动处理，交给调用方入库。
    pub fn drain_all(&self) -> Vec<Pending> {
        std::mem::take(&mut self.lock().pending)
    }

    fn bump_auto_streak(&self, session_id: i64) -> bool {
        let mut inner = self.lock();
        let streak = inner.auto_streak.entry(session_id).or_default();
        if *streak >= MAX_AUTO_TURNS {
            return false;
        }
        *streak += 1;
        true
    }
}

/// 排队消息入库，返回消息 id，并通知界面把它放进对话。
/// answering=true 表示 AI 正在这一轮里接着回应它（界面补流式占位）。
pub fn persist(app: &AppHandle, item: &Pending, answering: bool) -> anyhow::Result<i64> {
    let state = app.state::<crate::AppState>();
    let id = state.db.save_chat(item.session_id, "user", &item.content)?;
    let _ = app.emit(
        "chat-queued-absorbed",
        json!({
            "session_id": item.session_id,
            "content": item.content,
            "id": id,
            "event": item.event,
            "answering": answering,
        }),
    );
    Ok(id)
}

/// 启动一轮回复，结束后若还有排队消息就接着下一轮，直到队列清空。
/// 调用前必须已通过 submit 拿到 Start。
pub fn start_runner(app: AppHandle, session_id: i64, request_message_id: i64) {
    tauri::async_runtime::spawn(async move {
        let (mut sid, mut mid) = (session_id, request_message_id);
        loop {
            let cancel = tokio_util::sync::CancellationToken::new();
            {
                let state = app.state::<crate::AppState>();
                let mut guard = state.chat_cancel.lock().unwrap_or_else(|e| e.into_inner());
                *guard = Some(cancel.clone());
            }
            let _ = app.emit("chat-turn-started", json!({ "session_id": sid }));
            let result = crate::llm::agent::run_chat(app.clone(), sid, mid, cancel).await;
            if let Err(e) = result {
                let _ = app.emit("llm-error", json!({ "message": e.to_string() }));
            }
            let state = app.state::<crate::AppState>();
            let Some((next_sid, batch)) = state.chat_queue.next_or_idle(&app) else {
                break;
            };
            let mut last = None;
            for item in &batch {
                match persist(&app, item, false) {
                    Ok(id) => last = Some(id),
                    Err(e) => log::warn!("排队消息入库失败: {e}"),
                }
            }
            let Some(id) = last else {
                // 入库全部失败：释放占用，避免卡死。
                state.chat_queue.release(&app);
                break;
            };
            // 有用户新消息才算新的一轮任务；纯后台通知接着当前进度走。
            if batch.iter().any(|p| !p.event) {
                let snapshot = state.session_todos.begin_turn(next_sid);
                let _ = app.emit("session-todos", &snapshot);
            }
            let _ = app.emit("sessions-changed", ());
            sid = next_sid;
            mid = id;
        }
    });
}

/// 后台事件（任务结束、子代理/外派汇报）通知主对话：
/// AI 正忙则排进当前回复；空闲则自动开一轮让 AI 接着处理。
/// 事件所属会话不是当前会话、或连续自动轮数已满时，只记一条系统消息。
pub fn post_event(app: &AppHandle, session_id: i64, text: &str) {
    let state = app.state::<crate::AppState>();
    let content = format!("{EVENT_PREFIX}{text}");
    let current = state.db.current_session_id().ok();
    if session_id <= 0 || current != Some(session_id) || !state.chat_queue.bump_auto_streak(session_id) {
        let _ = state.db.save_chat(session_id, "system", text);
        let _ = app.emit("subagent-chat", json!({ "session_id": session_id, "content": text }));
        return;
    }
    let item = Pending { session_id, content, event: true };
    if let Submit::Start = state.chat_queue.submit(app, item.clone()) {
        match persist(app, &item, false) {
            Ok(id) => start_runner(app.clone(), session_id, id),
            Err(e) => {
                log::warn!("后台通知入库失败: {e}");
                state.chat_queue.release(app);
            }
        }
    }
}
