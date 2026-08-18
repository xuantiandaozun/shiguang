//! 当前这次任务的进度清单（agent 用 todo_write 整表替换）。
//! 与用户提醒待办（todos 表 / add_todo）完全分开：不入库、不提醒、下一轮用户消息清空。

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

const MAX_ITEMS: usize = 12;
const MAX_CONTENT_CHARS: usize = 80;

const STATUS_PENDING: &str = "pending";
const STATUS_IN_PROGRESS: &str = "in_progress";
const STATUS_COMPLETED: &str = "completed";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionTodo {
    pub content: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionTodoSnapshot {
    pub session_id: i64,
    pub todos: Vec<SessionTodo>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionTodoCounts {
    pub pending: usize,
    pub in_progress: usize,
    pub completed: usize,
}

impl SessionTodoCounts {
    fn from_items(items: &[SessionTodo]) -> Self {
        let mut counts = Self {
            pending: 0,
            in_progress: 0,
            completed: 0,
        };
        for item in items {
            match item.status.as_str() {
                STATUS_IN_PROGRESS => counts.in_progress += 1,
                STATUS_COMPLETED => counts.completed += 1,
                _ => counts.pending += 1,
            }
        }
        counts
    }
}

#[derive(Default)]
pub struct SessionTodoHub {
    by_session: Mutex<HashMap<i64, Vec<SessionTodo>>>,
    active_session: Mutex<Option<i64>>,
}

impl SessionTodoHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// 用户新发一条消息：清空该会话进度，并记下后续 todo_write 写到哪一轮。
    pub fn begin_turn(&self, session_id: i64) -> SessionTodoSnapshot {
        {
            let mut map = self.lock_map();
            map.insert(session_id, Vec::new());
        }
        *self.lock_active() = Some(session_id);
        SessionTodoSnapshot {
            session_id,
            todos: Vec::new(),
        }
    }

    pub fn replace_active(&self, todos: Vec<SessionTodo>) -> Result<SessionTodoSnapshot> {
        let session_id = self
            .lock_active()
            .ok_or_else(|| anyhow::anyhow!("当前没有进行中的对话，无法更新进度"))?;
        self.replace(session_id, todos)
    }

    pub fn replace(&self, session_id: i64, todos: Vec<SessionTodo>) -> Result<SessionTodoSnapshot> {
        {
            let mut map = self.lock_map();
            map.insert(session_id, todos.clone());
        }
        if self.lock_active().is_none() {
            *self.lock_active() = Some(session_id);
        }
        Ok(SessionTodoSnapshot { session_id, todos })
    }

    pub fn list(&self, session_id: i64) -> Vec<SessionTodo> {
        self.lock_map()
            .get(&session_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn drop_session(&self, session_id: i64) {
        self.lock_map().remove(&session_id);
        let mut active = self.lock_active();
        if *active == Some(session_id) {
            *active = None;
        }
    }

    fn lock_map(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Vec<SessionTodo>>> {
        self.by_session.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_active(&self) -> std::sync::MutexGuard<'_, Option<i64>> {
        self.active_session.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub fn parse_todos(args: &Value) -> Result<Vec<SessionTodo>> {
    let items = args
        .get("todos")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("todos 必须是非空数组"))?;
    if items.is_empty() {
        bail!("至少写一项进度；下一轮用户消息会自动清空");
    }
    if items.len() > MAX_ITEMS {
        bail!("一次最多 {MAX_ITEMS} 项");
    }
    let mut seen = HashSet::new();
    let mut parsed = Vec::with_capacity(items.len());
    let mut in_progress = 0usize;
    for item in items {
        let content = item
            .get("content")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("每项 content 必须是非空字符串"))?;
        if content.chars().count() > MAX_CONTENT_CHARS {
            bail!("每项 content 最多 {MAX_CONTENT_CHARS} 字");
        }
        if !seen.insert(content.to_string()) {
            bail!("进度条目不能重复：「{content}」");
        }
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("每项必须有 status"))?;
        if status != STATUS_PENDING && status != STATUS_IN_PROGRESS && status != STATUS_COMPLETED {
            bail!("status 只能是 pending、in_progress 或 completed");
        }
        if status == STATUS_IN_PROGRESS {
            in_progress += 1;
        }
        parsed.push(SessionTodo {
            content: content.to_string(),
            status: status.to_string(),
        });
    }
    if in_progress > 1 {
        bail!("同一时刻只能有一项 in_progress（现在有 {in_progress} 项）");
    }
    Ok(parsed)
}

pub fn result_json(snapshot: &SessionTodoSnapshot) -> Value {
    let counts = SessionTodoCounts::from_items(&snapshot.todos);
    json!({
        "ok": true,
        "counts": counts,
        "note": format!(
            "已更新进度：待办 {}，进行中 {}，已完成 {}。",
            counts.pending, counts.in_progress, counts.completed
        ),
    })
}

pub fn execute(app: &AppHandle, args: &Value) -> Result<Value> {
    let todos = parse_todos(args)?;
    let snapshot = {
        let state = app.state::<crate::AppState>();
        state.session_todos.replace_active(todos)?
    };
    let _ = app.emit("session-todos", &snapshot);
    Ok(result_json(&snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(content: &str, status: &str) -> Value {
        json!({ "content": content, "status": status })
    }

    #[test]
    fn parse_rejects_empty_and_duplicates() {
        assert!(parse_todos(&json!({ "todos": [] })).is_err());
        assert!(parse_todos(&json!({
            "todos": [item("扫描桌面", "pending"), item("扫描桌面", "in_progress")]
        }))
        .is_err());
    }

    #[test]
    fn parse_rejects_multiple_in_progress() {
        assert!(parse_todos(&json!({
            "todos": [
                item("扫描", "in_progress"),
                item("分类", "in_progress")
            ]
        }))
        .is_err());
    }

    #[test]
    fn parse_accepts_single_active_item() {
        let todos = parse_todos(&json!({
            "todos": [
                item("扫描桌面", "completed"),
                item("提出方案", "in_progress"),
                item("等你确认", "pending")
            ]
        }))
        .unwrap();
        assert_eq!(todos.len(), 3);
        assert_eq!(todos[1].status, STATUS_IN_PROGRESS);
    }

    #[test]
    fn begin_turn_clears_and_replace_writes_active_session() {
        let hub = SessionTodoHub::new();
        let cleared = hub.begin_turn(7);
        assert!(cleared.todos.is_empty());
        hub.replace_active(vec![SessionTodo {
            content: "整理桌面".into(),
            status: STATUS_IN_PROGRESS.into(),
        }])
        .unwrap();
        assert_eq!(hub.list(7).len(), 1);
        assert!(hub.list(8).is_empty());
        let again = hub.begin_turn(7);
        assert!(again.todos.is_empty());
        assert!(hub.list(7).is_empty());
    }

    #[test]
    fn drop_session_removes_list() {
        let hub = SessionTodoHub::new();
        hub.begin_turn(3);
        hub.replace_active(vec![SessionTodo {
            content: "读文件".into(),
            status: STATUS_PENDING.into(),
        }])
        .unwrap();
        hub.drop_session(3);
        assert!(hub.list(3).is_empty());
        assert!(hub.replace_active(vec![]).is_err());
    }
}
