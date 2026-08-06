use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

pub fn now_str() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub id: i64,
    pub title: String,
    pub note: String,
    pub due_at: Option<String>,
    pub repeat_rule: String,
    pub priority: i64,
    pub status: String,
    pub reminded: bool,
    /// 提醒方式：notify=仅系统通知 / popup=弹窗 / popup_input=弹窗+输入框（内容发给 AI）
    pub remind_mode: String,
    pub created_at: String,
    pub done_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub id: i64,
    pub name: String,
    pub match_type: String,
    pub pattern: String,
    pub target_folder: String,
    pub enabled: bool,
    pub approved: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationLog {
    pub id: i64,
    pub batch_id: String,
    pub op_type: String,
    pub src_path: String,
    pub dst_path: String,
    pub undone: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanCategory {
    pub name: String,
    /// "move"=移动到分类文件夹（默认）；"delete"=移入回收站。老数据无此字段，按 move 处理
    #[serde(default = "default_category_action")]
    pub action: String,
    #[serde(default)]
    pub target_folder: String,
    pub files: Vec<String>,
}

fn default_category_action() -> String {
    "move".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: i64,
    pub summary: String,
    pub categories: Vec<PlanCategory>,
    pub status: String,
    pub batch_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchPath {
    pub src: String,
    pub dst: String,
    pub op_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchSummary {
    pub batch_id: String,
    pub created_at: String,
    pub count: i64,
    pub undone: bool,
    pub paths: Vec<BatchPath>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMsg {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: i64,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub msg_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Workflow {
    pub id: i64,
    pub site: String,
    pub title: String,
    pub keywords: String,
    pub steps: String,
    pub use_count: i64,
    pub updated_at: String,
}

/// 个人信息自由条目（AI 在聊天中维护，用户也可在设置页手动管理）
#[derive(Debug, Clone, Serialize)]
pub struct ProfileEntry {
    pub id: i64,
    pub label: String,
    pub content: String,
    pub updated_at: String,
}

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).context("打开数据库失败")?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS todos(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              title TEXT NOT NULL,
              note TEXT NOT NULL DEFAULT '',
              due_at TEXT,
              repeat_rule TEXT NOT NULL DEFAULT 'none',
              priority INTEGER NOT NULL DEFAULT 1,
              status TEXT NOT NULL DEFAULT 'pending',
              reminded INTEGER NOT NULL DEFAULT 0,
              remind_mode TEXT NOT NULL DEFAULT 'notify',
              created_at TEXT NOT NULL,
              done_at TEXT
            );
            CREATE TABLE IF NOT EXISTS rules(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              name TEXT NOT NULL,
              match_type TEXT NOT NULL,
              pattern TEXT NOT NULL,
              target_folder TEXT NOT NULL,
              enabled INTEGER NOT NULL DEFAULT 1,
              approved INTEGER NOT NULL DEFAULT 1,
              created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS operation_logs(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              batch_id TEXT NOT NULL,
              op_type TEXT NOT NULL,
              src_path TEXT NOT NULL,
              dst_path TEXT NOT NULL,
              undone INTEGER NOT NULL DEFAULT 0,
              created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS plans(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              summary TEXT NOT NULL DEFAULT '',
              plan_json TEXT NOT NULL,
              status TEXT NOT NULL DEFAULT 'pending',
              batch_id TEXT,
              created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS settings(
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS chat_messages(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              role TEXT NOT NULL,
              content TEXT NOT NULL,
              created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS chat_sessions(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              title TEXT NOT NULL DEFAULT '新会话',
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS workflows(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              site TEXT NOT NULL DEFAULT '',
              title TEXT NOT NULL,
              keywords TEXT NOT NULL DEFAULT '',
              steps TEXT NOT NULL,
              use_count INTEGER NOT NULL DEFAULT 0,
              source_session_id INTEGER,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS profile_entries(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              label TEXT NOT NULL UNIQUE,
              content TEXT NOT NULL DEFAULT '',
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            "#,
        )?;

        // 老库迁移：chat_messages 增加 session_id 列
        let cols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(chat_messages)")?;
            let collected: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .collect();
            collected
        };
        if !cols.iter().any(|c| c == "session_id") {
            conn.execute_batch(
                "ALTER TABLE chat_messages ADD COLUMN session_id INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        // 老库迁移：todos 增加 remind_mode 列
        let todo_cols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(todos)")?;
            let collected: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .collect();
            collected
        };
        if !todo_cols.iter().any(|c| c == "remind_mode") {
            conn.execute_batch(
                "ALTER TABLE todos ADD COLUMN remind_mode TEXT NOT NULL DEFAULT 'notify';",
            )?;
        }
        // 迁移前的存量消息归入「之前的会话」
        let orphan: i64 = conn.query_row(
            "SELECT COUNT(*) FROM chat_messages WHERE session_id=0",
            [],
            |r| r.get(0),
        )?;
        if orphan > 0 {
            conn.execute(
                "INSERT INTO chat_sessions(title, created_at, updated_at) VALUES('之前的会话', ?1, ?1)",
                params![now_str()],
            )?;
            let sid = conn.last_insert_rowid();
            conn.execute(
                "UPDATE chat_messages SET session_id=?1 WHERE session_id=0",
                params![sid],
            )?;
        }
        let session_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM chat_sessions", [], |r| r.get(0))?;
        if session_count == 0 {
            conn.execute(
                "INSERT INTO chat_sessions(title, created_at, updated_at) VALUES('新会话', ?1, ?1)",
                params![now_str()],
            )?;
        }
        // 确保 current_session_id 指向一个存在的会话
        let has_current: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key='current_session_id'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let current_valid = has_current
            .and_then(|v| v.parse::<i64>().ok())
            .map(|id| {
                conn.query_row(
                    "SELECT COUNT(*) FROM chat_sessions WHERE id=?1",
                    params![id],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0)
                > 0
            })
            .unwrap_or(false);
        if !current_valid {
            let latest: Option<i64> = conn
                .query_row(
                    "SELECT id FROM chat_sessions ORDER BY updated_at DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(id) = latest {
                conn.execute(
                    "INSERT INTO settings(key, value) VALUES('current_session_id', ?1)
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![id.to_string()],
                )?;
            }
        }
        Ok(())
    }

    // ---------- settings ----------

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let v = conn
            .query_row("SELECT value FROM settings WHERE key=?1", params![key], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;
        Ok(v)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ---------- todos ----------

    fn row_to_todo(row: &rusqlite::Row) -> rusqlite::Result<Todo> {
        Ok(Todo {
            id: row.get(0)?,
            title: row.get(1)?,
            note: row.get(2)?,
            due_at: row.get(3)?,
            repeat_rule: row.get(4)?,
            priority: row.get(5)?,
            status: row.get(6)?,
            reminded: row.get(7)?,
            remind_mode: row.get(8)?,
            created_at: row.get(9)?,
            done_at: row.get(10)?,
        })
    }

    const TODO_COLS: &'static str =
        "id, title, note, due_at, repeat_rule, priority, status, reminded, remind_mode, created_at, done_at";

    pub fn insert_todo(
        &self,
        title: &str,
        note: &str,
        due_at: Option<&str>,
        repeat_rule: &str,
        priority: i64,
        remind_mode: &str,
    ) -> Result<Todo> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO todos(title, note, due_at, repeat_rule, priority, status, remind_mode, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7)",
            params![title, note, due_at, repeat_rule, priority, remind_mode, now_str()],
        )?;
        let id = conn.last_insert_rowid();
        let todo = conn.query_row(
            &format!("SELECT {} FROM todos WHERE id=?1", Self::TODO_COLS),
            params![id],
            Self::row_to_todo,
        )?;
        Ok(todo)
    }

    pub fn list_todos(&self, filter: &str) -> Result<Vec<Todo>> {
        let conn = self.conn.lock().unwrap();
        let where_clause = match filter {
            "done" => "WHERE status='done'",
            "all" => "",
            _ => "WHERE status='pending'",
        };
        let sql = format!(
            "SELECT {} FROM todos {} ORDER BY
               CASE WHEN due_at IS NULL THEN 1 ELSE 0 END,
               due_at ASC, priority DESC, id DESC",
            Self::TODO_COLS,
            where_clause
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], Self::row_to_todo)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_todo(
        &self,
        id: i64,
        title: &str,
        note: &str,
        due_at: Option<&str>,
        repeat_rule: &str,
        priority: i64,
        remind_mode: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE todos SET title=?1, note=?2, due_at=?3, repeat_rule=?4, priority=?5, remind_mode=?6, reminded=0 WHERE id=?7",
            params![title, note, due_at, repeat_rule, priority, remind_mode, id],
        )?;
        Ok(())
    }

    pub fn set_todo_done(&self, id: i64, done: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        if done {
            conn.execute(
                "UPDATE todos SET status='done', done_at=?1 WHERE id=?2",
                params![now_str(), id],
            )?;
        } else {
            conn.execute(
                "UPDATE todos SET status='pending', done_at=NULL, reminded=0 WHERE id=?1",
                params![id],
            )?;
        }
        Ok(())
    }

    pub fn delete_todo(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM todos WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn due_todos(&self, now: &str) -> Result<Vec<Todo>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {} FROM todos WHERE status='pending' AND reminded=0 AND due_at IS NOT NULL AND due_at <= ?1",
            Self::TODO_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![now], Self::row_to_todo)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn mark_reminded(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE todos SET reminded=1 WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn snooze(&self, id: i64, new_due: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE todos SET due_at=?1, reminded=0 WHERE id=?2",
            params![new_due, id],
        )?;
        Ok(())
    }

    // ---------- plans ----------

    fn parse_plan(
        id: i64,
        summary: String,
        plan_json: &str,
        status: String,
        batch_id: Option<String>,
        created_at: String,
    ) -> Plan {
        let categories: Vec<PlanCategory> =
            serde_json::from_str(plan_json).unwrap_or_default();
        Plan {
            id,
            summary,
            categories,
            status,
            batch_id,
            created_at,
        }
    }

    pub fn insert_plan(&self, summary: &str, categories: &[PlanCategory]) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let plan_json = serde_json::to_string(categories)?;
        conn.execute(
            "INSERT INTO plans(summary, plan_json, status, created_at) VALUES(?1, ?2, 'pending', ?3)",
            params![summary, plan_json, now_str()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_plan(&self, id: i64) -> Result<Plan> {
        let conn = self.conn.lock().unwrap();
        let row = conn.query_row(
            "SELECT id, summary, plan_json, status, batch_id, created_at FROM plans WHERE id=?1",
            params![id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )?;
        Ok(Self::parse_plan(row.0, row.1, &row.2, row.3, row.4, row.5))
    }

    pub fn pending_plan(&self) -> Result<Option<Plan>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT id, summary, plan_json, status, batch_id, created_at FROM plans
                 WHERE status='pending' ORDER BY id DESC LIMIT 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.map(|r| Self::parse_plan(r.0, r.1, &r.2, r.3, r.4, r.5)))
    }

    pub fn set_plan_status(&self, id: i64, status: &str, batch_id: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE plans SET status=?1, batch_id=?2 WHERE id=?3",
            params![status, batch_id, id],
        )?;
        Ok(())
    }

    pub fn cancel_pending_plans(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE plans SET status='cancelled' WHERE status='pending'",
            [],
        )?;
        Ok(())
    }

    // ---------- operation logs ----------

    pub fn insert_log(&self, batch_id: &str, op_type: &str, src: &str, dst: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO operation_logs(batch_id, op_type, src_path, dst_path, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![batch_id, op_type, src, dst, now_str()],
        )?;
        Ok(())
    }

    pub fn list_batches(&self) -> Result<Vec<BatchSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT batch_id, MIN(created_at), COUNT(*), SUM(undone)
             FROM operation_logs GROUP BY batch_id ORDER BY MAX(id) DESC LIMIT 100",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows.flatten() {
            let (batch_id, created_at, count, undone_sum) = row;
            let mut ps = conn.prepare(
                "SELECT src_path, dst_path, op_type FROM operation_logs WHERE batch_id=?1 ORDER BY id",
            )?;
            let paths: Vec<BatchPath> = ps
                .query_map(params![batch_id], |r| {
                    Ok(BatchPath {
                        src: r.get(0)?,
                        dst: r.get(1)?,
                        op_type: r.get(2)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();
            out.push(BatchSummary {
                batch_id,
                created_at,
                count,
                undone: undone_sum.unwrap_or(0) >= count,
                paths,
            });
        }
        Ok(out)
    }

    pub fn logs_for_batch(&self, batch_id: &str) -> Result<Vec<OperationLog>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, batch_id, op_type, src_path, dst_path, undone, created_at
             FROM operation_logs WHERE batch_id=?1 ORDER BY id DESC",
        )?;
        let rows = stmt.query_map(params![batch_id], |r| {
            Ok(OperationLog {
                id: r.get(0)?,
                batch_id: r.get(1)?,
                op_type: r.get(2)?,
                src_path: r.get(3)?,
                dst_path: r.get(4)?,
                undone: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn mark_batch_undone(&self, batch_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE operation_logs SET undone=1 WHERE batch_id=?1",
            params![batch_id],
        )?;
        Ok(())
    }

    // ---------- rules ----------

    fn row_to_rule(row: &rusqlite::Row) -> rusqlite::Result<Rule> {
        Ok(Rule {
            id: row.get(0)?,
            name: row.get(1)?,
            match_type: row.get(2)?,
            pattern: row.get(3)?,
            target_folder: row.get(4)?,
            enabled: row.get(5)?,
            approved: row.get(6)?,
            created_at: row.get(7)?,
        })
    }

    const RULE_COLS: &'static str =
        "id, name, match_type, pattern, target_folder, enabled, approved, created_at";

    pub fn list_rules(&self) -> Result<Vec<Rule>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM rules ORDER BY id DESC",
            Self::RULE_COLS
        ))?;
        let rows = stmt.query_map([], Self::row_to_rule)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn list_active_rules(&self) -> Result<Vec<Rule>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM rules WHERE enabled=1 AND approved=1 ORDER BY id",
            Self::RULE_COLS
        ))?;
        let rows = stmt.query_map([], Self::row_to_rule)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn upsert_rule(
        &self,
        id: Option<i64>,
        name: &str,
        match_type: &str,
        pattern: &str,
        target_folder: &str,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        if let Some(id) = id {
            conn.execute(
                "UPDATE rules SET name=?1, match_type=?2, pattern=?3, target_folder=?4 WHERE id=?5",
                params![name, match_type, pattern, target_folder, id],
            )?;
            Ok(id)
        } else {
            conn.execute(
                "INSERT INTO rules(name, match_type, pattern, target_folder, enabled, approved, created_at)
                 VALUES(?1, ?2, ?3, ?4, 1, 1, ?5)",
                params![name, match_type, pattern, target_folder, now_str()],
            )?;
            Ok(conn.last_insert_rowid())
        }
    }

    pub fn toggle_rule(&self, id: i64, enabled: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE rules SET enabled=?1 WHERE id=?2",
            params![enabled, id],
        )?;
        Ok(())
    }

    pub fn delete_rule(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM rules WHERE id=?1", params![id])?;
        Ok(())
    }

    // ---------- chat sessions & messages ----------

    pub fn current_session_id(&self) -> Result<i64> {
        if let Some(v) = self.get_setting("current_session_id")? {
            if let Ok(id) = v.parse::<i64>() {
                let conn = self.conn.lock().unwrap();
                let exists: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM chat_sessions WHERE id=?1",
                    params![id],
                    |r| r.get(0),
                )?;
                if exists > 0 {
                    return Ok(id);
                }
            }
        }
        let conn = self.conn.lock().unwrap();
        let latest: Option<i64> = conn
            .query_row(
                "SELECT id FROM chat_sessions ORDER BY updated_at DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let id = match latest {
            Some(x) => x,
            None => {
                conn.execute(
                    "INSERT INTO chat_sessions(title, created_at, updated_at) VALUES('新会话', ?1, ?1)",
                    params![now_str()],
                )?;
                conn.last_insert_rowid()
            }
        };
        drop(conn);
        self.set_setting("current_session_id", &id.to_string())?;
        Ok(id)
    }

    pub fn create_session(&self) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO chat_sessions(title, created_at, updated_at) VALUES('新会话', ?1, ?1)",
            params![now_str()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn set_current_session(&self, id: i64) -> Result<()> {
        self.set_setting("current_session_id", &id.to_string())
    }

    /// 用首条用户消息为未命名会话生成标题
    pub fn auto_title_session(&self, id: i64, first_text: &str) -> Result<()> {
        let title: String = first_text.chars().take(16).collect();
        if title.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE chat_sessions SET title=?1 WHERE id=?2 AND title='新会话'",
            params![title, id],
        )?;
        Ok(())
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.title, s.created_at, s.updated_at,
              (SELECT COUNT(*) FROM chat_messages m WHERE m.session_id=s.id)
             FROM chat_sessions s ORDER BY s.updated_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SessionInfo {
                id: r.get(0)?,
                title: r.get(1)?,
                created_at: r.get(2)?,
                updated_at: r.get(3)?,
                msg_count: r.get(4)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// 删除会话及其消息；若删的是当前会话则切换到最新会话（没有则新建），返回新的当前会话 id
    pub fn delete_session(&self, id: i64) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM chat_messages WHERE session_id=?1",
            params![id],
        )?;
        conn.execute("DELETE FROM chat_sessions WHERE id=?1", params![id])?;
        let cur: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key='current_session_id'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let cur_id = cur.and_then(|v| v.parse::<i64>().ok());
        let need_switch = cur_id.map(|c| c == id).unwrap_or(true);
        let new_current = if need_switch {
            let latest: Option<i64> = conn
                .query_row(
                    "SELECT id FROM chat_sessions ORDER BY updated_at DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            match latest {
                Some(x) => x,
                None => {
                    conn.execute(
                        "INSERT INTO chat_sessions(title, created_at, updated_at) VALUES('新会话', ?1, ?1)",
                        params![now_str()],
                    )?;
                    conn.last_insert_rowid()
                }
            }
        } else {
            cur_id.unwrap()
        };
        conn.execute(
            "INSERT INTO settings(key, value) VALUES('current_session_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![new_current.to_string()],
        )?;
        Ok(new_current)
    }

    pub fn save_chat(&self, session_id: i64, role: &str, content: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let now = now_str();
        conn.execute(
            "INSERT INTO chat_messages(session_id, role, content, created_at) VALUES(?1, ?2, ?3, ?4)",
            params![session_id, role, content, now],
        )?;
        let id = conn.last_insert_rowid();
        conn.execute(
            "UPDATE chat_sessions SET updated_at=?1 WHERE id=?2",
            params![now, session_id],
        )?;
        Ok(id)
    }

    pub fn load_chat(&self, session_id: i64, limit: usize) -> Result<Vec<ChatMsg>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, role, content, created_at FROM chat_messages
             WHERE session_id=?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let mut rows: Vec<ChatMsg> = stmt
            .query_map(params![session_id, limit as i64], |r| {
                Ok(ChatMsg {
                    id: r.get(0)?,
                    role: r.get(1)?,
                    content: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        rows.reverse();
        Ok(rows)
    }

    pub fn clear_chat(&self, session_id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM chat_messages WHERE session_id=?1",
            params![session_id],
        )?;
        Ok(())
    }

    /// 撤回一条用户消息：删除该消息，以及紧随其后、下一条用户消息之前的 AI 回复
    pub fn recall_message(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT session_id, role FROM chat_messages WHERE id=?1",
                params![id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((session_id, role)) = row else {
            return Ok(());
        };
        anyhow::ensure!(role == "user", "只能撤回自己的消息");
        conn.execute("DELETE FROM chat_messages WHERE id=?1", params![id])?;
        conn.execute(
            "DELETE FROM chat_messages
             WHERE session_id=?1 AND role='assistant' AND id>?2
               AND id < COALESCE(
                     (SELECT MIN(id) FROM chat_messages WHERE session_id=?1 AND role='user' AND id>?2),
                     9223372036854775807)",
            params![session_id, id],
        )?;
        Ok(())
    }

    // ---------- workflows ----------

    pub fn wf_insert(
        &self,
        site: &str,
        title: &str,
        keywords: &str,
        steps: &str,
        session_id: i64,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO workflows(site, title, keywords, steps, source_session_id, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![site, title, keywords, steps, session_id, now_str()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn wf_update(&self, id: i64, site: &str, title: &str, keywords: &str, steps: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE workflows SET site=?1, title=?2, keywords=?3, steps=?4, updated_at=?5 WHERE id=?6",
            params![site, title, keywords, steps, now_str(), id],
        )?;
        Ok(())
    }

    pub fn wf_touch(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        for id in ids {
            conn.execute(
                "UPDATE workflows SET use_count=use_count+1 WHERE id=?1",
                params![id],
            )?;
        }
        Ok(())
    }

    pub fn wf_delete(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM workflows WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn wf_list(&self) -> Result<Vec<Workflow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, site, title, keywords, steps, use_count, updated_at
             FROM workflows ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Workflow {
                id: r.get(0)?,
                site: r.get(1)?,
                title: r.get(2)?,
                keywords: r.get(3)?,
                steps: r.get(4)?,
                use_count: r.get(5)?,
                updated_at: r.get(6)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// 按用户消息匹配相关工作流：域名命中权重最高，关键词次之，标题再次。
    pub fn wf_find_relevant(&self, text: &str, limit: usize) -> Result<Vec<Workflow>> {
        let all = self.wf_list()?;
        if all.is_empty() {
            return Ok(Vec::new());
        }
        let lower = text.to_lowercase();
        let mut scored: Vec<(i64, Workflow)> = all
            .into_iter()
            .filter_map(|w| {
                let mut score = 0i64;
                if !w.site.is_empty() && lower.contains(&w.site.to_lowercase()) {
                    score += 10;
                }
                if !w.title.is_empty() && lower.contains(&w.title.to_lowercase()) {
                    score += 4;
                }
                for kw in w.keywords.split(',').map(|k| k.trim()).filter(|k| !k.is_empty()) {
                    if lower.contains(&kw.to_lowercase()) {
                        score += 3;
                    }
                }
                if score > 0 {
                    Some((score + w.use_count.min(5), w))
                } else {
                    None
                }
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.updated_at.cmp(&a.1.updated_at)));
        Ok(scored.into_iter().take(limit).map(|(_, w)| w).collect())
    }

    // ---------- profile entries ----------

    /// 新增或按 label 覆盖更新；返回条目 id
    pub fn pf_upsert(&self, label: &str, content: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO profile_entries(label, content, created_at, updated_at) VALUES(?1, ?2, ?3, ?3)
             ON CONFLICT(label) DO UPDATE SET content=excluded.content, updated_at=excluded.updated_at",
            params![label, content, now_str()],
        )?;
        let id = conn.query_row(
            "SELECT id FROM profile_entries WHERE label=?1",
            params![label],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    pub fn pf_update_by_id(&self, id: i64, label: &str, content: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE profile_entries SET label=?1, content=?2, updated_at=?3 WHERE id=?4",
            params![label, content, now_str(), id],
        )?;
        Ok(())
    }

    pub fn pf_delete(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM profile_entries WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn pf_list(&self) -> Result<Vec<ProfileEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, label, content, updated_at FROM profile_entries ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ProfileEntry {
                id: r.get(0)?,
                label: r.get(1)?,
                content: r.get(2)?,
                updated_at: r.get(3)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }
}
