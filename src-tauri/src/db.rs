use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

pub fn now_str() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn query_like_tokens(query: Option<&str>) -> Vec<String> {
    query
        .map(|value| {
            value
                .split_whitespace()
                .filter(|token| token.chars().count() >= 2)
                .take(8)
                .map(|token| format!("%{token}%"))
                .collect()
        })
        .unwrap_or_default()
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

/// 一次主代理工具调用。通过 request_message_id / response_message_id 与原聊天消息组成完整调用链。
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallRecord {
    pub id: i64,
    pub session_id: i64,
    pub request_message_id: i64,
    pub response_message_id: Option<i64>,
    pub round_index: i64,
    pub call_index: i64,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments_json: String,
    pub result_json: Option<String>,
    pub status: String,
    pub assistant_content: String,
    pub reasoning_content: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub session_title: String,
    pub request_content: String,
    pub response_content: Option<String>,
}

/// 回放到下一轮 LLM messages 的精简工具记录（不含会话标题等展示字段）。
#[derive(Debug, Clone)]
pub struct ToolCallReplay {
    pub request_message_id: i64,
    pub round_index: i64,
    pub call_index: i64,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments_json: String,
    pub result_json: Option<String>,
    pub status: String,
    pub assistant_content: String,
    pub reasoning_content: String,
}

fn tool_call_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ToolCallRecord> {
    Ok(ToolCallRecord {
        id: row.get(0)?,
        session_id: row.get(1)?,
        request_message_id: row.get(2)?,
        response_message_id: row.get(3)?,
        round_index: row.get(4)?,
        call_index: row.get(5)?,
        tool_call_id: row.get(6)?,
        tool_name: row.get(7)?,
        arguments_json: row.get(8)?,
        result_json: row.get(9)?,
        status: row.get(10)?,
        assistant_content: row.get(11)?,
        reasoning_content: row.get(12)?,
        started_at: row.get(13)?,
        completed_at: row.get(14)?,
        session_title: row.get(15)?,
        request_content: row.get(16)?,
        response_content: row.get(17)?,
    })
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct LlmUsageTotals {
    pub requests: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cache_hit_tokens: i64,
    pub cache_miss_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmUsageBySource {
    pub source: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cache_hit_tokens: i64,
    pub cache_miss_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmUsageDay {
    pub day: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cache_hit_tokens: i64,
    pub cache_miss_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct LlmUsagePeriod {
    pub totals: LlmUsageTotals,
    pub by_source: Vec<LlmUsageBySource>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmUsageSnapshot {
    pub today: LlmUsagePeriod,
    pub last_7d: LlmUsagePeriod,
    pub all: LlmUsagePeriod,
    pub daily: Vec<LlmUsageDay>,
    pub recent: Vec<LlmUsageRequest>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmUsageRequest {
    pub id: i64,
    pub source: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cache_hit_tokens: i64,
    pub cache_miss_tokens: i64,
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

/// 可直接执行的自动化流程。它和 Skill 不同：Skill 是 AI 的方法说明，
/// Workflow 是用户保存的、可手动或按计划触发的一次具体工作。
#[derive(Debug, Clone, Serialize)]
pub struct AutomationWorkflow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub schedule_rule: String,
    pub next_run_at: Option<String>,
    pub enabled: bool,
    pub run_count: i64,
    pub last_run_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 经验证的浏览器操作配方：保存站点特征和声明式动作，不保存任意页面脚本或隐私内容。
#[derive(Debug, Clone, Serialize)]
pub struct BrowserRecipe {
    pub id: i64,
    pub name: String,
    pub site_pattern: String,
    pub goal: String,
    pub recipe_json: String,
    pub verification_json: String,
    pub success_count: i64,
    pub failure_count: i64,
    pub last_used_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

fn browser_recipe_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BrowserRecipe> {
    Ok(BrowserRecipe {
        id: row.get(0)?, name: row.get(1)?, site_pattern: row.get(2)?, goal: row.get(3)?,
        recipe_json: row.get(4)?, verification_json: row.get(5)?, success_count: row.get(6)?,
        failure_count: row.get(7)?, last_used_at: row.get(8)?, created_at: row.get(9)?, updated_at: row.get(10)?,
    })
}

fn automation_workflow_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AutomationWorkflow> {
    Ok(AutomationWorkflow {
        id: row.get(0)?, name: row.get(1)?, description: row.get(2)?, prompt: row.get(3)?,
        schedule_rule: row.get(4)?, next_run_at: row.get(5)?, enabled: row.get::<_, i32>(6)? != 0,
        run_count: row.get(7)?, last_run_at: row.get(8)?, created_at: row.get(9)?, updated_at: row.get(10)?,
    })
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
            CREATE TABLE IF NOT EXISTS session_compacts(
              session_id INTEGER PRIMARY KEY,
              cover_until_id INTEGER NOT NULL,
              summary TEXT NOT NULL,
              created_at TEXT NOT NULL,
              FOREIGN KEY(session_id) REFERENCES chat_sessions(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS chat_tool_calls(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              session_id INTEGER NOT NULL,
              request_message_id INTEGER NOT NULL,
              response_message_id INTEGER,
              round_index INTEGER NOT NULL,
              call_index INTEGER NOT NULL,
              tool_call_id TEXT NOT NULL,
              tool_name TEXT NOT NULL,
              arguments_json TEXT NOT NULL,
              result_json TEXT,
              status TEXT NOT NULL CHECK(status IN ('running', 'done', 'error')),
              assistant_content TEXT NOT NULL DEFAULT '',
              reasoning_content TEXT NOT NULL DEFAULT '',
              started_at TEXT NOT NULL,
              completed_at TEXT,
              FOREIGN KEY(session_id) REFERENCES chat_sessions(id) ON DELETE CASCADE,
              FOREIGN KEY(request_message_id) REFERENCES chat_messages(id) ON DELETE CASCADE,
              FOREIGN KEY(response_message_id) REFERENCES chat_messages(id) ON DELETE SET NULL,
              UNIQUE(request_message_id, round_index, call_index)
            );
            CREATE INDEX IF NOT EXISTS idx_chat_tool_calls_session_id
              ON chat_tool_calls(session_id, id);
            CREATE INDEX IF NOT EXISTS idx_chat_tool_calls_request_message_id
              ON chat_tool_calls(request_message_id, round_index, call_index);
            CREATE INDEX IF NOT EXISTS idx_chat_tool_calls_tool_name
              ON chat_tool_calls(tool_name, id);
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
            CREATE TABLE IF NOT EXISTS automation_workflows(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              name TEXT NOT NULL UNIQUE,
              description TEXT NOT NULL DEFAULT '',
              prompt TEXT NOT NULL,
              schedule_rule TEXT NOT NULL DEFAULT 'manual',
              next_run_at TEXT,
              enabled INTEGER NOT NULL DEFAULT 1,
              run_count INTEGER NOT NULL DEFAULT 0,
              last_run_at TEXT,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_automation_workflows_due
              ON automation_workflows(enabled, next_run_at);
            CREATE TABLE IF NOT EXISTS browser_recipes(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              name TEXT NOT NULL UNIQUE,
              site_pattern TEXT NOT NULL,
              goal TEXT NOT NULL,
              recipe_json TEXT NOT NULL,
              verification_json TEXT NOT NULL DEFAULT '{}',
              success_count INTEGER NOT NULL DEFAULT 0,
              failure_count INTEGER NOT NULL DEFAULT 0,
              last_used_at TEXT,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_browser_recipes_site ON browser_recipes(site_pattern);
            CREATE TABLE IF NOT EXISTS profile_entries(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              label TEXT NOT NULL UNIQUE,
              content TEXT NOT NULL DEFAULT '',
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS llm_usage(
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              source TEXT NOT NULL,
              model TEXT NOT NULL DEFAULT '',
              prompt_tokens INTEGER NOT NULL DEFAULT 0,
              completion_tokens INTEGER NOT NULL DEFAULT 0,
              total_tokens INTEGER NOT NULL DEFAULT 0,
              cache_hit_tokens INTEGER NOT NULL DEFAULT 0,
              cache_miss_tokens INTEGER NOT NULL DEFAULT 0,
              created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_llm_usage_created_at
              ON llm_usage(created_at);
            CREATE INDEX IF NOT EXISTS idx_llm_usage_source
              ON llm_usage(source, created_at);
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
        // Preserve reasoning content needed to reconstruct provider-specific tool rounds.
        let tool_call_cols: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(chat_tool_calls)")?;
            let collected = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            collected
        };
        if !tool_call_cols.iter().any(|c| c == "reasoning_content") {
            conn.execute_batch(
                "ALTER TABLE chat_tool_calls
                 ADD COLUMN reasoning_content TEXT NOT NULL DEFAULT '';",
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
            .query_row(
                "SELECT value FROM settings WHERE key=?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
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
        let categories: Vec<PlanCategory> = serde_json::from_str(plan_json).unwrap_or_default();
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

    /// 应用冷启动：当前会话已有消息则新开空白会话。
    /// 上次对话仍留在历史列表，避免界面是空的、模型却带着旧上下文。
    pub fn start_fresh_session_if_needed(&self) -> Result<i64> {
        let current = self.current_session_id()?;
        let n: i64 = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM chat_messages WHERE session_id=?1",
                params![current],
                |r| r.get(0),
            )?
        };
        if n == 0 {
            return Ok(current);
        }
        let id = self.create_session()?;
        self.set_current_session(id)?;
        Ok(id)
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
        conn.execute("DELETE FROM session_compacts WHERE session_id=?1", params![id])?;
        conn.execute("DELETE FROM chat_messages WHERE session_id=?1", params![id])?;
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

    pub fn get_session_compact(&self, session_id: i64) -> Result<Option<(i64, String)>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT cover_until_id, summary FROM session_compacts WHERE session_id=?1",
                params![session_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        Ok(row)
    }

    pub fn put_session_compact(
        &self,
        session_id: i64,
        cover_until_id: i64,
        summary: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO session_compacts(session_id, cover_until_id, summary, created_at)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(session_id) DO UPDATE SET
               cover_until_id=excluded.cover_until_id,
               summary=excluded.summary,
               created_at=excluded.created_at",
            params![session_id, cover_until_id, summary, now_str()],
        )?;
        Ok(())
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

    /// 执行工具前先写入 running 记录。如果触发消息不属于该会话，拒绝执行，
    /// 避免工具动作已发生却无法和对话对应。
    #[allow(clippy::too_many_arguments)]
    pub fn start_tool_call(
        &self,
        session_id: i64,
        request_message_id: i64,
        round_index: usize,
        call_index: usize,
        tool_call_id: &str,
        tool_name: &str,
        arguments_json: &str,
        assistant_content: &str,
        reasoning_content: &str,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let changed = conn.execute(
            "INSERT INTO chat_tool_calls(
               session_id, request_message_id, round_index, call_index,
               tool_call_id, tool_name, arguments_json, status, assistant_content,
               reasoning_content, started_at
             )
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running', ?8, ?9, ?10
             FROM chat_messages
             WHERE id=?2 AND session_id=?1 AND role='user'",
            params![
                session_id,
                request_message_id,
                round_index as i64,
                call_index as i64,
                tool_call_id,
                tool_name,
                arguments_json,
                assistant_content,
                reasoning_content,
                now_str(),
            ],
        )?;
        anyhow::ensure!(changed == 1, "工具调用无法关联到当前用户消息");
        Ok(conn.last_insert_rowid())
    }

    /// 工具返回后落库完整结果；失败结果同样保留，便于之后复盘。
    pub fn finish_tool_call(&self, id: i64, status: &str, result_json: &str) -> Result<()> {
        anyhow::ensure!(matches!(status, "done" | "error"), "无效工具状态");
        let conn = self.conn.lock().unwrap();
        let changed = conn.execute(
            "UPDATE chat_tool_calls
             SET status=?1, result_json=?2, completed_at=?3
             WHERE id=?4 AND status='running'",
            params![status, result_json, now_str(), id],
        )?;
        anyhow::ensure!(changed == 1, "工具调用记录不存在或已完成");
        Ok(())
    }

    /// 最终 AI 回复落库后，把该用户请求下的所有工具调用关联到回复消息。
    pub fn link_tool_calls_response(
        &self,
        session_id: i64,
        request_message_id: i64,
        response_message_id: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let valid_response: bool = conn.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM chat_messages
               WHERE id=?1 AND session_id=?2 AND role='assistant'
             )",
            params![response_message_id, session_id],
            |r| r.get(0),
        )?;
        anyhow::ensure!(valid_response, "工具调用无法关联到最终 AI 回复");
        conn.execute(
            "UPDATE chat_tool_calls SET response_message_id=?1
             WHERE session_id=?2 AND request_message_id=?3",
            params![response_message_id, session_id, request_message_id],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn query_tool_calls(
        &self,
        session_id: Option<i64>,
        tool_name: Option<&str>,
        status: Option<&str>,
        query: Option<&str>,
        before_id: Option<i64>,
        limit: usize,
        include_history_queries: bool,
    ) -> Result<Vec<ToolCallRecord>> {
        let conn = self.conn.lock().unwrap();
        let query_tokens = query_like_tokens(query);
        let include_history_queries = if include_history_queries { 1 } else { 0 };
        let mut sql = String::from(
            "SELECT
               t.id, t.session_id, t.request_message_id, t.response_message_id,
               t.round_index, t.call_index, t.tool_call_id, t.tool_name,
               t.arguments_json, t.result_json, t.status, t.assistant_content,
               t.reasoning_content, t.started_at, t.completed_at, s.title,
               req.content, resp.content
             FROM chat_tool_calls t
             JOIN chat_sessions s ON s.id=t.session_id
             JOIN chat_messages req ON req.id=t.request_message_id
             LEFT JOIN chat_messages resp ON resp.id=t.response_message_id
             WHERE (?1 IS NULL OR t.session_id=?1)
               AND (?2 IS NULL OR t.tool_name=?2)
               AND (?3 IS NULL OR t.status=?3)
               AND (?4 IS NULL OR t.id<?4)",
        );
        if !query_tokens.is_empty() {
            sql.push_str(" AND (");
            for (index, _) in query_tokens.iter().enumerate() {
                if index > 0 {
                    sql.push_str(" OR ");
                }
                let n = 5 + index;
                sql.push_str(&format!(
                    "(t.tool_name LIKE ?{n}
                      OR t.arguments_json LIKE ?{n}
                      OR COALESCE(t.result_json, '') LIKE ?{n}
                      OR req.content LIKE ?{n}
                      OR COALESCE(resp.content, '') LIKE ?{n})"
                ));
            }
            sql.push_str(")");
        }
        let include_idx = 5 + query_tokens.len();
        let limit_idx = include_idx + 1;
        sql.push_str(&format!(
            " AND (?{include_idx}=1 OR t.tool_name<>'get_tool_call_history')
              ORDER BY t.id DESC
              LIMIT ?{limit_idx}"
        ));
        let mut stmt = conn.prepare(&sql)?;
        let limit_i = limit.clamp(1, 100) as i64;
        let mut bind: Vec<&dyn rusqlite::ToSql> =
            vec![&session_id, &tool_name, &status, &before_id];
        for token in &query_tokens {
            bind.push(token);
        }
        bind.push(&include_history_queries);
        bind.push(&limit_i);
        let mapped = stmt.query_map(bind.as_slice(), tool_call_record_from_row)?;
        let mut rows: Vec<ToolCallRecord> = mapped.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// 按用户消息 id 取出工具调用，供下一轮对话按协议回放（assistant.tool_calls + role=tool）。
    pub fn load_tool_calls_for_messages(
        &self,
        session_id: i64,
        request_message_ids: &[i64],
    ) -> Result<Vec<ToolCallReplay>> {
        if request_message_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        let placeholders = vec!["?"; request_message_ids.len()].join(",");
        let sql = format!(
            "SELECT request_message_id, round_index, call_index, tool_call_id, tool_name,
                    arguments_json, result_json, status, assistant_content, reasoning_content
             FROM chat_tool_calls
             WHERE session_id=? AND request_message_id IN ({placeholders})
             ORDER BY request_message_id, round_index, call_index, id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<&dyn rusqlite::ToSql> =
            Vec::with_capacity(request_message_ids.len() + 1);
        params.push(&session_id);
        for id in request_message_ids {
            params.push(id);
        }
        let rows = stmt.query_map(params.as_slice(), |r| {
            Ok(ToolCallReplay {
                request_message_id: r.get(0)?,
                round_index: r.get(1)?,
                call_index: r.get(2)?,
                tool_call_id: r.get(3)?,
                tool_name: r.get(4)?,
                arguments_json: r.get(5)?,
                result_json: r.get(6)?,
                status: r.get(7)?,
                assistant_content: r.get(8)?,
                reasoning_content: r.get(9)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_tool_call(&self, id: i64) -> Result<Option<ToolCallRecord>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT
               t.id, t.session_id, t.request_message_id, t.response_message_id,
               t.round_index, t.call_index, t.tool_call_id, t.tool_name,
               t.arguments_json, t.result_json, t.status, t.assistant_content,
               t.reasoning_content, t.started_at, t.completed_at, s.title,
               req.content, resp.content
             FROM chat_tool_calls t
             JOIN chat_sessions s ON s.id=t.session_id
             JOIN chat_messages req ON req.id=t.request_message_id
             LEFT JOIN chat_messages resp ON resp.id=t.response_message_id
             WHERE t.id=?1",
            params![id],
            tool_call_record_from_row,
        )
        .optional()
        .map_err(Into::into)
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

    pub fn wf_update(
        &self,
        id: i64,
        site: &str,
        title: &str,
        keywords: &str,
        steps: &str,
    ) -> Result<()> {
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
                for kw in w
                    .keywords
                    .split(',')
                    .map(|k| k.trim())
                    .filter(|k| !k.is_empty())
                {
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

    // ---------- automation workflows ----------

    pub fn automation_workflow_list(&self) -> Result<Vec<AutomationWorkflow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, description, prompt, schedule_rule, next_run_at, enabled, run_count, last_run_at, created_at, updated_at
             FROM automation_workflows ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], automation_workflow_from_row)?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    pub fn automation_workflow_get(&self, id: i64) -> Result<Option<AutomationWorkflow>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT id, name, description, prompt, schedule_rule, next_run_at, enabled, run_count, last_run_at, created_at, updated_at
             FROM automation_workflows WHERE id=?1",
            params![id], automation_workflow_from_row,
        ).optional()?)
    }

    pub fn automation_workflow_save(&self, workflow: &AutomationWorkflow) -> Result<AutomationWorkflow> {
        let conn = self.conn.lock().unwrap();
        let now = now_str();
        if workflow.id == 0 {
            conn.execute(
                "INSERT INTO automation_workflows(name, description, prompt, schedule_rule, next_run_at, enabled, created_at, updated_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![workflow.name.trim(), workflow.description.trim(), workflow.prompt.trim(), workflow.schedule_rule, workflow.next_run_at, workflow.enabled as i32, now],
            )?;
            let id = conn.last_insert_rowid();
            drop(conn);
            return Ok(self.automation_workflow_get(id)?.expect("inserted workflow"));
        }
        conn.execute(
            "UPDATE automation_workflows SET name=?1, description=?2, prompt=?3, schedule_rule=?4, next_run_at=?5, enabled=?6, updated_at=?7 WHERE id=?8",
            params![workflow.name.trim(), workflow.description.trim(), workflow.prompt.trim(), workflow.schedule_rule, workflow.next_run_at, workflow.enabled as i32, now, workflow.id],
        )?;
        drop(conn);
        self.automation_workflow_get(workflow.id)?.ok_or_else(|| anyhow!("工作流不存在"))
    }

    pub fn automation_workflow_delete(&self, id: i64) -> Result<()> {
        self.conn.lock().unwrap().execute("DELETE FROM automation_workflows WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn due_automation_workflows(&self, now: &str) -> Result<Vec<AutomationWorkflow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, description, prompt, schedule_rule, next_run_at, enabled, run_count, last_run_at, created_at, updated_at
             FROM automation_workflows WHERE enabled=1 AND next_run_at IS NOT NULL AND next_run_at<=?1",
        )?;
        let rows = stmt.query_map(params![now], automation_workflow_from_row)?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    pub fn mark_automation_workflow_run(&self, id: i64, next_run_at: Option<&str>) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE automation_workflows SET run_count=run_count+1, last_run_at=?1, next_run_at=?2, updated_at=?1 WHERE id=?3",
            params![now_str(), next_run_at, id],
        )?;
        Ok(())
    }

    // ---------- browser recipes ----------

    pub fn browser_recipe_list(&self) -> Result<Vec<BrowserRecipe>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, name, site_pattern, goal, recipe_json, verification_json, success_count, failure_count, last_used_at, created_at, updated_at FROM browser_recipes ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], browser_recipe_from_row)?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    pub fn browser_recipe_get(&self, id: i64) -> Result<Option<BrowserRecipe>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row("SELECT id, name, site_pattern, goal, recipe_json, verification_json, success_count, failure_count, last_used_at, created_at, updated_at FROM browser_recipes WHERE id=?1", params![id], browser_recipe_from_row).optional()?)
    }

    pub fn browser_recipe_save(&self, recipe: &BrowserRecipe) -> Result<BrowserRecipe> {
        let conn = self.conn.lock().unwrap();
        let now = now_str();
        if recipe.id == 0 {
            conn.execute("INSERT INTO browser_recipes(name, site_pattern, goal, recipe_json, verification_json, created_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?6)", params![recipe.name.trim(), recipe.site_pattern.trim(), recipe.goal.trim(), recipe.recipe_json, recipe.verification_json, now])?;
            let id = conn.last_insert_rowid(); drop(conn);
            return Ok(self.browser_recipe_get(id)?.expect("inserted recipe"));
        }
        conn.execute("UPDATE browser_recipes SET name=?1, site_pattern=?2, goal=?3, recipe_json=?4, verification_json=?5, updated_at=?6 WHERE id=?7", params![recipe.name.trim(), recipe.site_pattern.trim(), recipe.goal.trim(), recipe.recipe_json, recipe.verification_json, now, recipe.id])?;
        drop(conn);
        self.browser_recipe_get(recipe.id)?.ok_or_else(|| anyhow!("浏览器配方不存在"))
    }

    pub fn browser_recipe_delete(&self, id: i64) -> Result<()> {
        self.conn.lock().unwrap().execute("DELETE FROM browser_recipes WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn browser_recipe_find(&self, url: &str, goal: &str, limit: usize) -> Result<Vec<BrowserRecipe>> {
        let mut all = self.browser_recipe_list()?;
        let u = url.to_lowercase(); let g = goal.to_lowercase();
        all.retain(|recipe| u.contains(&recipe.site_pattern.to_lowercase()) && (g.is_empty() || recipe.goal.to_lowercase().contains(&g) || recipe.name.to_lowercase().contains(&g)));
        all.sort_by(|a, b| (b.success_count - b.failure_count).cmp(&(a.success_count - a.failure_count)).then(b.updated_at.cmp(&a.updated_at)));
        Ok(all.into_iter().take(limit).collect())
    }

    pub fn browser_recipe_mark(&self, id: i64, success: bool) -> Result<()> {
        let field = if success { "success_count" } else { "failure_count" };
        self.conn.lock().unwrap().execute(&format!("UPDATE browser_recipes SET {field}={field}+1, last_used_at=?1, updated_at=?1 WHERE id=?2"), params![now_str(), id])?;
        Ok(())
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

    // ---------- LLM 用量 ----------

    pub fn insert_llm_usage(
        &self,
        source: &str,
        model: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
        total_tokens: i64,
        cache_hit_tokens: i64,
        cache_miss_tokens: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO llm_usage(
                source, model, prompt_tokens, completion_tokens, total_tokens,
                cache_hit_tokens, cache_miss_tokens, created_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                source,
                model,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cache_hit_tokens,
                cache_miss_tokens,
                now_str()
            ],
        )?;
        Ok(())
    }

    pub fn llm_usage_snapshot(&self) -> Result<LlmUsageSnapshot> {
        let today = chrono::Local::now().date_naive();
        let today_start = format!("{} 00:00:00", today.format("%Y-%m-%d"));
        let week_start = format!(
            "{} 00:00:00",
            (today - chrono::Duration::days(6)).format("%Y-%m-%d")
        );
        let daily_start = format!(
            "{} 00:00:00",
            (today - chrono::Duration::days(13)).format("%Y-%m-%d")
        );
        Ok(LlmUsageSnapshot {
            today: self.llm_usage_period(Some(&today_start))?,
            last_7d: self.llm_usage_period(Some(&week_start))?,
            all: self.llm_usage_period(None)?,
            daily: self.llm_usage_daily(&daily_start, today)?,
            recent: self.llm_usage_recent(20)?,
        })
    }

    fn llm_usage_period(&self, since: Option<&str>) -> Result<LlmUsagePeriod> {
        Ok(LlmUsagePeriod {
            totals: self.llm_usage_totals(since)?,
            by_source: self.llm_usage_by_source(since)?,
        })
    }

    fn llm_usage_totals(&self, since: Option<&str>) -> Result<LlmUsageTotals> {
        let conn = self.conn.lock().unwrap();
        let row = match since {
            Some(ts) => conn.query_row(
                "SELECT COUNT(*),
                        COALESCE(SUM(prompt_tokens), 0),
                        COALESCE(SUM(completion_tokens), 0),
                        COALESCE(SUM(total_tokens), 0),
                        COALESCE(SUM(cache_hit_tokens), 0),
                        COALESCE(SUM(cache_miss_tokens), 0)
                 FROM llm_usage WHERE created_at >= ?1",
                params![ts],
                Self::row_to_usage_totals,
            )?,
            None => conn.query_row(
                "SELECT COUNT(*),
                        COALESCE(SUM(prompt_tokens), 0),
                        COALESCE(SUM(completion_tokens), 0),
                        COALESCE(SUM(total_tokens), 0),
                        COALESCE(SUM(cache_hit_tokens), 0),
                        COALESCE(SUM(cache_miss_tokens), 0)
                 FROM llm_usage",
                [],
                Self::row_to_usage_totals,
            )?,
        };
        Ok(row)
    }

    fn row_to_usage_totals(row: &rusqlite::Row) -> rusqlite::Result<LlmUsageTotals> {
        Ok(LlmUsageTotals {
            requests: row.get(0)?,
            prompt_tokens: row.get(1)?,
            completion_tokens: row.get(2)?,
            total_tokens: row.get(3)?,
            cache_hit_tokens: row.get(4)?,
            cache_miss_tokens: row.get(5)?,
        })
    }

    fn llm_usage_by_source(&self, since: Option<&str>) -> Result<Vec<LlmUsageBySource>> {
        let conn = self.conn.lock().unwrap();
        let sql_all = "SELECT source,
                COUNT(*),
                COALESCE(SUM(prompt_tokens), 0),
                COALESCE(SUM(completion_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(cache_hit_tokens), 0),
                COALESCE(SUM(cache_miss_tokens), 0)
         FROM llm_usage GROUP BY source ORDER BY SUM(total_tokens) DESC";
        let sql_since = "SELECT source,
                COUNT(*),
                COALESCE(SUM(prompt_tokens), 0),
                COALESCE(SUM(completion_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(cache_hit_tokens), 0),
                COALESCE(SUM(cache_miss_tokens), 0)
         FROM llm_usage WHERE created_at >= ?1
         GROUP BY source ORDER BY SUM(total_tokens) DESC";
        let map_row = |r: &rusqlite::Row| {
            Ok(LlmUsageBySource {
                source: r.get(0)?,
                requests: r.get(1)?,
                prompt_tokens: r.get(2)?,
                completion_tokens: r.get(3)?,
                total_tokens: r.get(4)?,
                cache_hit_tokens: r.get(5)?,
                cache_miss_tokens: r.get(6)?,
            })
        };
        let rows = if let Some(ts) = since {
            let mut stmt = conn.prepare(sql_since)?;
            let mapped = stmt.query_map(params![ts], map_row)?;
            mapped.filter_map(|r| r.ok()).collect()
        } else {
            let mut stmt = conn.prepare(sql_all)?;
            let mapped = stmt.query_map([], map_row)?;
            mapped.filter_map(|r| r.ok()).collect()
        };
        Ok(rows)
    }

    fn llm_usage_daily(&self, since: &str, today: chrono::NaiveDate) -> Result<Vec<LlmUsageDay>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT substr(created_at, 1, 10),
                    COUNT(*),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(total_tokens), 0),
                    COALESCE(SUM(cache_hit_tokens), 0),
                    COALESCE(SUM(cache_miss_tokens), 0)
             FROM llm_usage WHERE created_at >= ?1
             GROUP BY substr(created_at, 1, 10)",
        )?;
        let mapped = stmt.query_map(params![since], |r| {
            Ok(LlmUsageDay {
                day: r.get(0)?,
                requests: r.get(1)?,
                prompt_tokens: r.get(2)?,
                completion_tokens: r.get(3)?,
                total_tokens: r.get(4)?,
                cache_hit_tokens: r.get(5)?,
                cache_miss_tokens: r.get(6)?,
            })
        })?;
        let mut by_day: std::collections::HashMap<String, LlmUsageDay> = mapped
            .filter_map(|r| r.ok())
            .map(|d| (d.day.clone(), d))
            .collect();
        let start = today - chrono::Duration::days(13);
        let mut days = Vec::with_capacity(14);
        for offset in 0..14 {
            let day = (start + chrono::Duration::days(offset))
                .format("%Y-%m-%d")
                .to_string();
            days.push(by_day.remove(&day).unwrap_or(LlmUsageDay {
                day: day.clone(),
                requests: 0,
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                cache_hit_tokens: 0,
                cache_miss_tokens: 0,
            }));
        }
        Ok(days)
    }

    fn llm_usage_recent(&self, limit: usize) -> Result<Vec<LlmUsageRequest>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, source, model, prompt_tokens, completion_tokens, total_tokens,
                    cache_hit_tokens, cache_miss_tokens, created_at
             FROM llm_usage ORDER BY id DESC LIMIT ?1",
        )?;
        let mapped = stmt.query_map(params![limit.clamp(1, 50) as i64], |r| {
            Ok(LlmUsageRequest {
                id: r.get(0)?,
                source: r.get(1)?,
                model: r.get(2)?,
                prompt_tokens: r.get(3)?,
                completion_tokens: r.get(4)?,
                total_tokens: r.get(5)?,
                cache_hit_tokens: r.get(6)?,
                cache_miss_tokens: r.get(7)?,
                created_at: r.get(8)?,
            })
        })?;
        Ok(mapped.filter_map(|r| r.ok()).collect())
    }
}

#[cfg(test)]
mod tool_call_tests {
    use super::*;

    fn test_db() -> (Db, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("shiguang-tool-history-{}.db", uuid::Uuid::new_v4()));
        (Db::new(&path).unwrap(), path)
    }

    fn cleanup(db: Db, path: &Path) {
        drop(db);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn tool_calls_keep_order_and_link_both_chat_messages() {
        let (db, path) = test_db();
        let session_id = db.current_session_id().unwrap();
        let request_id = db
            .save_chat(session_id, "user", "打开网页并完成设置")
            .unwrap();

        let failed_id = db
            .start_tool_call(
                session_id,
                request_id,
                0,
                0,
                "call-1",
                "browser_click",
                r#"{"ref":12}"#,
                "",
                "",
            )
            .unwrap();
        db.finish_tool_call(failed_id, "error", r#"{"error":"元素已失效"}"#)
            .unwrap();

        let recovered_id = db
            .start_tool_call(
                session_id,
                request_id,
                1,
                0,
                "call-2",
                "browser_snapshot",
                "{}",
                "我先重新观察页面。",
                "先判断原 ref 已失效。",
            )
            .unwrap();
        db.finish_tool_call(recovered_id, "done", r#"{"ok":true,"ref":27}"#)
            .unwrap();

        let response_id = db
            .save_chat(session_id, "assistant", "已重新定位控件并完成设置。")
            .unwrap();
        db.link_tool_calls_response(session_id, request_id, response_id)
            .unwrap();

        let calls = db
            .query_tool_calls(Some(session_id), None, None, None, None, 20, false)
            .unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, failed_id);
        assert_eq!(calls[0].status, "error");
        assert_eq!(calls[1].id, recovered_id);
        assert_eq!(calls[1].status, "done");
        assert_eq!(calls[1].request_message_id, request_id);
        assert_eq!(calls[1].response_message_id, Some(response_id));
        assert_eq!(calls[1].request_content, "打开网页并完成设置");
        assert_eq!(
            calls[1].response_content.as_deref(),
            Some("已重新定位控件并完成设置。")
        );

        let exact = db.get_tool_call(failed_id).unwrap().unwrap();
        assert_eq!(
            exact.result_json.as_deref(),
            Some(r#"{"error":"元素已失效"}"#)
        );

        let replay = db
            .load_tool_calls_for_messages(session_id, &[request_id])
            .unwrap();
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].tool_name, "browser_click");
        assert_eq!(replay[0].round_index, 0);
        assert_eq!(replay[1].tool_name, "browser_snapshot");
        assert_eq!(replay[1].round_index, 1);
        assert_eq!(replay[1].reasoning_content, "先判断原 ref 已失效。");
        assert!(db
            .load_tool_calls_for_messages(session_id, &[])
            .unwrap()
            .is_empty());

        cleanup(db, &path);
    }

    #[test]
    fn recalling_request_cascades_to_its_tool_history() {
        let (db, path) = test_db();
        let session_id = db.current_session_id().unwrap();
        let request_id = db.save_chat(session_id, "user", "测试撤回").unwrap();
        let call_id = db
            .start_tool_call(
                session_id,
                request_id,
                0,
                0,
                "call-recall",
                "browser_status",
                "{}",
                "",
                "",
            )
            .unwrap();
        db.finish_tool_call(call_id, "done", r#"{"ok":true}"#)
            .unwrap();
        let response_id = db.save_chat(session_id, "assistant", "完成").unwrap();
        db.link_tool_calls_response(session_id, request_id, response_id)
            .unwrap();

        db.recall_message(request_id).unwrap();

        assert!(db.get_tool_call(call_id).unwrap().is_none());
        assert!(db.load_chat(session_id, 10).unwrap().is_empty());

        cleanup(db, &path);
    }

    #[test]
    fn clearing_chat_cascades_to_all_tool_history_in_the_session() {
        let (db, path) = test_db();
        let session_id = db.current_session_id().unwrap();
        let request_id = db.save_chat(session_id, "user", "清空会话").unwrap();
        let call_id = db
            .start_tool_call(
                session_id,
                request_id,
                0,
                0,
                "call-clear",
                "browser_status",
                "{}",
                "",
                "",
            )
            .unwrap();
        db.finish_tool_call(call_id, "done", r#"{"ok":true}"#)
            .unwrap();

        db.clear_chat(session_id).unwrap();

        assert!(db.get_tool_call(call_id).unwrap().is_none());
        cleanup(db, &path);
    }

    #[test]
    fn tool_call_requires_a_user_message_in_the_same_session() {
        let (db, path) = test_db();
        let session_id = db.current_session_id().unwrap();
        let assistant_id = db
            .save_chat(session_id, "assistant", "不是用户消息")
            .unwrap();

        let result = db.start_tool_call(
            session_id,
            assistant_id,
            0,
            0,
            "call-invalid",
            "browser_status",
            "{}",
            "",
            "",
        );

        assert!(result.is_err());
        cleanup(db, &path);
    }

    #[test]
    fn querying_tool_calls_does_not_leak_other_sessions() {
        let (db, path) = test_db();
        let current = db.current_session_id().unwrap();
        let other = db.create_session().unwrap();

        let current_request = db.save_chat(current, "user", "当前会话").unwrap();
        let current_call = db
            .start_tool_call(
                current,
                current_request,
                0,
                0,
                "call-current",
                "browser_status",
                "{}",
                "",
                "",
            )
            .unwrap();
        db.finish_tool_call(current_call, "done", r#"{"ok":true}"#)
            .unwrap();

        let other_request = db.save_chat(other, "user", "其他会话").unwrap();
        let other_call = db
            .start_tool_call(
                other,
                other_request,
                0,
                0,
                "call-other",
                "browser_click",
                "{}",
                "",
                "",
            )
            .unwrap();
        db.finish_tool_call(other_call, "done", r#"{"ok":true}"#)
            .unwrap();

        let current_only = db
            .query_tool_calls(Some(current), None, None, None, None, 20, false)
            .unwrap();
        assert_eq!(current_only.len(), 1);
        assert_eq!(current_only[0].id, current_call);

        let all = db
            .query_tool_calls(None, None, None, None, None, 20, false)
            .unwrap();
        assert_eq!(all.len(), 2);

        cleanup(db, &path);
    }

    #[test]
    fn querying_tool_calls_splits_keywords() {
        let (db, path) = test_db();
        let session_id = db.current_session_id().unwrap();
        let request_id = db.save_chat(session_id, "user", "上传截图").unwrap();
        let call_id = db
            .start_tool_call(
                session_id,
                request_id,
                0,
                0,
                "call-upload",
                "browser_evaluate",
                r#"{"expression":"DataTransfer"}"#,
                "",
                "",
            )
            .unwrap();
        db.finish_tool_call(call_id, "done", r#"{"ok":true}"#)
            .unwrap();

        let hits = db
            .query_tool_calls(
                Some(session_id),
                None,
                None,
                Some("upload_file DataTransfer base64"),
                None,
                20,
                false,
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, call_id);

        cleanup(db, &path);
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    fn test_db() -> (Db, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("shiguang-session-{}.db", uuid::Uuid::new_v4()));
        (Db::new(&path).unwrap(), path)
    }

    fn cleanup(db: Db, path: &Path) {
        drop(db);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn launch_opens_fresh_session_when_last_one_has_messages() {
        let (db, path) = test_db();
        let old = db.current_session_id().unwrap();
        db.save_chat(old, "user", "昨天的对话").unwrap();

        let fresh = db.start_fresh_session_if_needed().unwrap();
        assert_ne!(fresh, old);
        assert!(db.load_chat(fresh, 10).unwrap().is_empty());
        assert_eq!(db.current_session_id().unwrap(), fresh);
        assert_eq!(db.load_chat(old, 10).unwrap().len(), 1);

        let again = db.start_fresh_session_if_needed().unwrap();
        assert_eq!(again, fresh);
        cleanup(db, &path);
    }
}

#[cfg(test)]
mod llm_usage_tests {
    use super::*;

    fn test_db() -> (Db, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("shiguang-llm-usage-{}.db", uuid::Uuid::new_v4()));
        (Db::new(&path).unwrap(), path)
    }

    fn cleanup(db: Db, path: &Path) {
        drop(db);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn usage_snapshot_aggregates_tokens_and_cache() {
        let (db, path) = test_db();
        db.insert_llm_usage("chat", "deepseek-chat", 1000, 80, 1080, 800, 200)
            .unwrap();
        db.insert_llm_usage("subagent", "deepseek-chat", 400, 20, 420, 300, 100)
            .unwrap();
        db.insert_llm_usage("vision", "qwen-vl-max", 200, 50, 250, 0, 200)
            .unwrap();

        let snap = db.llm_usage_snapshot().unwrap();
        assert_eq!(snap.all.totals.requests, 3);
        assert_eq!(snap.all.totals.prompt_tokens, 1600);
        assert_eq!(snap.all.totals.completion_tokens, 150);
        assert_eq!(snap.all.totals.total_tokens, 1750);
        assert_eq!(snap.all.totals.cache_hit_tokens, 1100);
        assert_eq!(snap.all.totals.cache_miss_tokens, 500);
        assert_eq!(snap.today.totals.requests, 3);
        assert_eq!(snap.last_7d.totals.total_tokens, 1750);
        assert_eq!(snap.all.by_source.len(), 3);
        assert_eq!(snap.all.by_source[0].source, "chat");
        assert_eq!(snap.daily.len(), 14);
        assert_eq!(snap.daily.last().unwrap().requests, 3);
        assert_eq!(snap.recent.len(), 3);
        assert_eq!(snap.recent[0].source, "vision");
        assert_eq!(snap.recent[0].cache_hit_tokens, 0);
        assert_eq!(snap.recent[2].source, "chat");

        cleanup(db, &path);
    }
}
