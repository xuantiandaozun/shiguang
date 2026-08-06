use crate::db::{BatchSummary, ChatMsg, Db, Plan, Rule, Todo};
use crate::organizer::{executor, scanner};
use crate::{llm, windows, AppState};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub organize_root: String,
    pub auto_organize: bool,
    pub autostart: bool,
    pub desktop_path: String,
    /// DeepSeek 思考模式开关（仅对 DeepSeek 接口生效）
    pub thinking_enabled: bool,
    /// 思考强度：low / high / max
    pub reasoning_effort: String,
    /// 视觉模型（图像识别）接口地址，与聊天模型相互独立
    pub vision_base_url: String,
    /// 视觉模型 API Key，与聊天 Key 分开配置
    pub vision_api_key: String,
    /// 视觉模型名称
    pub vision_model: String,
    // ---- 子代理（run_subagent 独立 LLM 循环）----
    /// 子代理思考模式开关，默认关闭（省 token、响应快）
    pub subagent_thinking_enabled: bool,
    /// 子代理思考强度：low / high / max
    pub subagent_reasoning_effort: String,
    /// 子代理模型，留空则跟随主模型
    pub subagent_model: String,
    /// 允许 AI 执行命令行（后台任务类工具总开关）
    pub command_tools_enabled: bool,
    // ---- 个人信息固定字段（用户在设置页维护）----
    pub profile_name: String,
    /// 自媒体号名称（对外默认使用，真实姓名仅招聘等实名场景使用）
    pub profile_alias: String,
    pub profile_gender: String,
    pub profile_birth: String,
    pub profile_phone: String,
    pub profile_email: String,
    pub profile_city: String,
    /// 应用临时目录（只读展示，不入库；路径由 app_data/temp 决定）
    pub temp_path: String,
}

pub fn load_settings(db: &Db) -> Settings {
    let get = |k: &str, d: &str| -> String {
        db.get_setting(k)
            .ok()
            .flatten()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| d.to_string())
    };
    let desktop = dirs::desktop_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    // 分类文件夹直接建在桌面顶层（如「桌面/开发工具」），不再多套一层「桌面整理」
    let default_root = desktop.to_string_lossy().replace('\\', "/");
    let mut settings = Settings {
        base_url: get("base_url", "https://api.deepseek.com/v1"),
        api_key: get("api_key", ""),
        model: get("model", "deepseek-chat"),
        organize_root: get("organize_root", &default_root),
        auto_organize: db
            .get_setting("auto_organize")
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(true),
        autostart: db
            .get_setting("autostart")
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(false),
        desktop_path: desktop.to_string_lossy().replace('\\', "/"),
        thinking_enabled: db
            .get_setting("thinking_enabled")
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(true),
        reasoning_effort: match get("reasoning_effort", "high").as_str() {
            "low" | "high" | "max" => get("reasoning_effort", "high"),
            _ => "high".to_string(),
        },
        vision_base_url: get(
            "vision_base_url",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
        ),
        vision_api_key: get("vision_api_key", ""),
        vision_model: get("vision_model", "qwen-vl-max"),
        subagent_thinking_enabled: db
            .get_setting("subagent_thinking_enabled")
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(false),
        subagent_reasoning_effort: match get("subagent_reasoning_effort", "low").as_str() {
            "low" | "high" | "max" => get("subagent_reasoning_effort", "low"),
            _ => "low".to_string(),
        },
        subagent_model: get("subagent_model", ""),
        command_tools_enabled: db
            .get_setting("command_tools_enabled")
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(true),
        profile_name: get("profile_name", ""),
        profile_alias: get("profile_alias", ""),
        profile_gender: get("profile_gender", ""),
        profile_birth: get("profile_birth", ""),
        profile_phone: get("profile_phone", ""),
        profile_email: get("profile_email", ""),
        profile_city: get("profile_city", ""),
        temp_path: default_temp_path(),
    };
    if let Some(new_root) = migrate_legacy_root(db, &desktop, &settings.organize_root) {
        settings.organize_root = new_root;
    }
    settings
}

fn default_temp_path() -> String {
    dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("com.deskhelper.win")
        .join("temp")
        .to_string_lossy()
        .replace('\\', "/")
}

/// 旧版本默认把分类建在「桌面/桌面整理」下。若用户仍在用该默认值（未自定义过），
/// 自动切换到桌面顶层，并同步改写指向旧根目录的规则目标；返回生效的根目录。
fn migrate_legacy_root(db: &Db, desktop: &std::path::Path, current: &str) -> Option<String> {
    let legacy = desktop.join("桌面整理").to_string_lossy().replace('\\', "/");
    if current.replace('\\', "/") != legacy {
        return None;
    }
    let new_root = desktop.to_string_lossy().replace('\\', "/");
    db.set_setting("organize_root", &new_root).ok()?;
    if let Ok(rules) = db.list_rules() {
        for r in rules {
            let t = r.target_folder.replace('\\', "/");
            let suffix = t.strip_prefix(&legacy).map(|s| s.trim_start_matches('/'));
            if let Some(suffix) = suffix {
                let target = if suffix.is_empty() {
                    new_root.clone()
                } else {
                    format!("{}/{}", new_root, suffix)
                };
                let _ = db.upsert_rule(Some(r.id), &r.name, &r.match_type, &r.pattern, &target);
            }
        }
    }
    Some(new_root)
}

/// 扫描桌面时要跳过的整理根目录文件夹名；根目录就是桌面本身时无需跳过。
pub fn organize_root_skip(db: &Db) -> Option<String> {
    let settings = load_settings(db);
    let root = std::path::Path::new(&settings.organize_root);
    let desktop = crate::organizer::scanner::desktop_dir().ok()?;
    if root == desktop {
        return None;
    }
    root.file_name().map(|s| s.to_string_lossy().to_string())
}

pub fn normalize_due(raw: &str) -> Option<String> {
    let s = raw.trim().replace('T', " ");
    let s = s.trim();
    for f in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(t) = chrono::NaiveDateTime::parse_from_str(s, f) {
            return Some(t.format("%Y-%m-%d %H:%M:%S").to_string());
        }
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return d
            .and_hms_opt(9, 0, 0)
            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string());
    }
    None
}

// ---------- chat ----------

#[derive(Debug, Serialize)]
pub struct SessionView {
    pub session_id: i64,
    pub messages: Vec<ChatMsg>,
}

/// 把附件路径拼进用户消息，让 AI 能用 ocr_image / read_image / read_file 读取。
/// 图片优先标注本地 OCR；其它走 read_file。
fn compose_user_message(text: &str, attachments: &[String]) -> Result<String, String> {
    let text = text.trim();
    let mut paths: Vec<String> = Vec::new();
    for raw in attachments {
        let p = std::path::Path::new(raw.trim());
        if !p.is_absolute() {
            return Err(format!("附件路径必须是绝对路径: {}", raw));
        }
        if !p.exists() {
            return Err(format!("附件不存在: {}", raw));
        }
        if p.is_dir() {
            return Err(format!("暂不支持直接发送文件夹: {}", raw));
        }
        paths.push(p.to_string_lossy().replace('\\', "/"));
    }
    if paths.is_empty() {
        if text.is_empty() {
            return Err("消息不能为空".to_string());
        }
        return Ok(text.to_string());
    }

    let img_exts = [
        "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tif", "tiff", "heic", "avif",
    ];
    let mut block = String::from("【用户附带的文件】\n");
    for (i, path) in paths.iter().enumerate() {
        let ext = std::path::Path::new(path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let hint = if img_exts.contains(&ext.as_str()) {
            "图片：抽文字用 ocr_image；理解画面用 read_image"
        } else {
            "请用 read_file 读取内容"
        };
        block.push_str(&format!("{}. {}（{}）\n", i + 1, path, hint));
    }
    if text.is_empty() {
        block.push_str("请根据上述附件处理（未另附文字说明）。");
        Ok(block)
    } else {
        Ok(format!("{}\n\n{}", text, block))
    }
}

#[tauri::command]
pub async fn send_chat_message(
    app: AppHandle,
    text: String,
    attachments: Option<Vec<String>>,
) -> Result<i64, String> {
    let state = app.state::<AppState>();
    if state.chat_busy.swap(true, Ordering::SeqCst) {
        return Err("上一条消息还在处理中，请稍候".to_string());
    }
    let composed = match compose_user_message(&text, attachments.as_deref().unwrap_or(&[])) {
        Ok(s) => s,
        Err(e) => {
            state.chat_busy.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };
    let session_id = state.db.current_session_id().map_err(|e| e.to_string())?;
    let msg_id = match state.db.save_chat(session_id, "user", &composed) {
        Ok(id) => id,
        Err(e) => {
            state.chat_busy.store(false, Ordering::SeqCst);
            return Err(e.to_string());
        }
    };
    let title_src = if text.trim().is_empty() {
        composed.as_str()
    } else {
        text.trim()
    };
    let _ = state.db.auto_title_session(session_id, title_src);
    let _ = app.emit("sessions-changed", ());
    let cancel = tokio_util::sync::CancellationToken::new();
    {
        let mut guard = state.chat_cancel.lock().map_err(|e| e.to_string())?;
        *guard = Some(cancel.clone());
    }
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = llm::agent::run_chat(app2.clone(), session_id, cancel).await;
        let busy_state = app2.state::<AppState>();
        busy_state.chat_busy.store(false, Ordering::SeqCst);
        if let Err(e) = result {
            let _ = app2.emit(
                "llm-error",
                serde_json::json!({ "message": e.to_string() }),
            );
        }
    });
    Ok(msg_id)
}

/// 中断当前正在生成的回复：流式输出立即停止（已生成的部分内容保留），
/// 正在执行的工具会跑完当前这个再停，保证文件操作的一致性。
#[tauri::command]
pub fn stop_chat_message(state: State<AppState>) -> Result<(), String> {
    let token = state
        .chat_cancel
        .lock()
        .map_err(|e| e.to_string())?
        .take();
    match token {
        Some(t) => {
            t.cancel();
            Ok(())
        }
        None => Err("当前没有进行中的回复".to_string()),
    }
}

#[tauri::command]
pub fn get_current_session(state: State<AppState>) -> Result<SessionView, String> {
    let session_id = state.db.current_session_id().map_err(|e| e.to_string())?;
    let messages = state.db.load_chat(session_id, 50).map_err(|e| e.to_string())?;
    Ok(SessionView {
        session_id,
        messages,
    })
}

#[tauri::command]
pub fn list_sessions(state: State<AppState>) -> Result<Vec<crate::db::SessionInfo>, String> {
    state.db.list_sessions().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn new_session(app: AppHandle, state: State<AppState>) -> Result<SessionView, String> {
    let id = state.db.create_session().map_err(|e| e.to_string())?;
    state.db.set_current_session(id).map_err(|e| e.to_string())?;
    let _ = app.emit("sessions-changed", ());
    Ok(SessionView {
        session_id: id,
        messages: vec![],
    })
}

#[tauri::command]
pub fn switch_session(
    app: AppHandle,
    state: State<AppState>,
    id: i64,
) -> Result<SessionView, String> {
    state.db.set_current_session(id).map_err(|e| e.to_string())?;
    let messages = state.db.load_chat(id, 50).map_err(|e| e.to_string())?;
    let _ = app.emit("sessions-changed", ());
    Ok(SessionView {
        session_id: id,
        messages,
    })
}

#[tauri::command]
pub fn delete_session(
    app: AppHandle,
    state: State<AppState>,
    id: i64,
) -> Result<SessionView, String> {
    let new_current = state.db.delete_session(id).map_err(|e| e.to_string())?;
    let messages = state
        .db
        .load_chat(new_current, 50)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("sessions-changed", ());
    Ok(SessionView {
        session_id: new_current,
        messages,
    })
}

#[tauri::command]
pub fn recall_message(state: State<AppState>, id: i64) -> Result<(), String> {
    state.db.recall_message(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn load_chat_history(state: State<AppState>) -> Result<Vec<ChatMsg>, String> {
    let session_id = state.db.current_session_id().map_err(|e| e.to_string())?;
    state.db.load_chat(session_id, 50).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_chat_history(state: State<AppState>) -> Result<(), String> {
    let session_id = state.db.current_session_id().map_err(|e| e.to_string())?;
    state.db.clear_chat(session_id).map_err(|e| e.to_string())
}

// ---------- windows ----------

/// 在系统默认程序中打开链接或本地文件（聊天消息里的链接/图片点击）。
/// 只放行 http(s)/mailto 链接与真实存在的本地路径，防止 javascript: 等协议注入；
/// 走 rundll32 而非 cmd /c start，避免 URL 中的 & 被 cmd 当成命令分隔符。
#[tauri::command]
pub fn open_external(target: String) -> Result<(), String> {
    let t = target.trim();
    if t.is_empty() || t.len() > 2048 {
        return Err("无效的打开目标".to_string());
    }
    let lower = t.to_lowercase();
    let is_link = lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:");
    if !is_link && !std::path::Path::new(t).exists() {
        return Err(format!("不是支持的链接，也不是存在的文件: {}", t));
    }
    use std::os::windows::process::CommandExt;
    std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", t])
        .creation_flags(crate::tasks::CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("打开失败: {}", e))?;
    Ok(())
}

#[tauri::command]
pub fn toggle_chat(app: AppHandle) {
    windows::toggle_chat(&app);
}

/// 幂等展示聊天窗（区别于 toggle_chat 的开/关切换），提醒弹窗发消息后调用
#[tauri::command]
pub fn show_chat_window(app: AppHandle) {
    windows::show_chat(&app);
}

#[tauri::command]
pub fn open_main_window(app: AppHandle) {
    windows::open_main(&app);
}

#[tauri::command]
pub fn hide_chat(app: AppHandle) {
    windows::hide_chat(&app);
}

// ---------- todos ----------

#[tauri::command]
pub fn list_todos(state: State<AppState>, filter: Option<String>) -> Result<Vec<Todo>, String> {
    state
        .db
        .list_todos(filter.as_deref().unwrap_or("pending"))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_todo(
    app: AppHandle,
    state: State<AppState>,
    title: String,
    note: Option<String>,
    due_at: Option<String>,
    repeat_rule: Option<String>,
    priority: Option<i64>,
    remind_mode: Option<String>,
) -> Result<Todo, String> {
    let due = due_at.and_then(|d| normalize_due(&d));
    let todo = state
        .db
        .insert_todo(
            title.trim(),
            note.as_deref().unwrap_or(""),
            due.as_deref(),
            repeat_rule.as_deref().unwrap_or("none"),
            priority.unwrap_or(1),
            &normalize_remind_mode(remind_mode.as_deref()),
        )
        .map_err(|e| e.to_string())?;
    let _ = app.emit("todos-changed", ());
    Ok(todo)
}

#[tauri::command]
pub fn update_todo(
    app: AppHandle,
    state: State<AppState>,
    id: i64,
    title: String,
    note: Option<String>,
    due_at: Option<String>,
    repeat_rule: Option<String>,
    priority: Option<i64>,
    remind_mode: Option<String>,
) -> Result<(), String> {
    let due = due_at.and_then(|d| normalize_due(&d));
    state
        .db
        .update_todo(
            id,
            title.trim(),
            note.as_deref().unwrap_or(""),
            due.as_deref(),
            repeat_rule.as_deref().unwrap_or("none"),
            priority.unwrap_or(1),
            &normalize_remind_mode(remind_mode.as_deref()),
        )
        .map_err(|e| e.to_string())?;
    let _ = app.emit("todos-changed", ());
    Ok(())
}

/// 提醒方式白名单：notify（仅系统通知）/ popup（弹窗）/ popup_input（弹窗+输入框）
pub fn normalize_remind_mode(v: Option<&str>) -> String {
    match v.unwrap_or("notify") {
        "popup" | "popup_input" => v.unwrap().to_string(),
        _ => "notify".to_string(),
    }
}

/// 提醒弹窗的「稍后再提醒」：把到期时间延后若干分钟
#[tauri::command]
pub fn snooze_todo_cmd(
    app: AppHandle,
    state: State<AppState>,
    id: i64,
    minutes: Option<i64>,
) -> Result<String, String> {
    let mins = minutes.unwrap_or(10).clamp(1, 24 * 60);
    let new_due = (chrono::Local::now() + chrono::Duration::minutes(mins))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    state.db.snooze(id, &new_due).map_err(|e| e.to_string())?;
    let _ = app.emit("todos-changed", ());
    Ok(new_due)
}

#[tauri::command]
pub fn delete_todo(app: AppHandle, state: State<AppState>, id: i64) -> Result<(), String> {
    state.db.delete_todo(id).map_err(|e| e.to_string())?;
    let _ = app.emit("todos-changed", ());
    Ok(())
}

#[tauri::command]
pub fn set_todo_done(app: AppHandle, state: State<AppState>, id: i64, done: bool) -> Result<(), String> {
    state.db.set_todo_done(id, done).map_err(|e| e.to_string())?;
    let _ = app.emit("todos-changed", ());
    Ok(())
}

// ---------- plans ----------

#[tauri::command]
pub fn get_pending_plan(state: State<AppState>) -> Result<Option<Plan>, String> {
    state.db.pending_plan().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn execute_plan_cmd(
    app: AppHandle,
    state: State<AppState>,
    plan_id: i64,
) -> Result<serde_json::Value, String> {
    let res = executor::execute_plan(&state.db, plan_id).map_err(|e| e.to_string())?;
    let _ = app.emit("plan-executed", &res);
    let _ = app.emit("history-changed", ());
    serde_json::to_value(&res).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cancel_plan(app: AppHandle, state: State<AppState>, plan_id: i64) -> Result<(), String> {
    state
        .db
        .set_plan_status(plan_id, "cancelled", None)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("plan-cancelled", serde_json::json!({ "plan_id": plan_id }));
    Ok(())
}

// ---------- history ----------

#[tauri::command]
pub fn list_batches(state: State<AppState>) -> Result<Vec<BatchSummary>, String> {
    state.db.list_batches().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn undo_batch_cmd(
    app: AppHandle,
    state: State<AppState>,
    batch_id: String,
) -> Result<u64, String> {
    let count = executor::undo_batch(&state.db, &batch_id).map_err(|e| e.to_string())?;
    let _ = app.emit("history-changed", ());
    Ok(count)
}

// ---------- rules ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleInput {
    pub id: Option<i64>,
    pub name: String,
    pub match_type: String,
    pub pattern: String,
    pub target_folder: String,
}

pub fn resolve_target_folder(db: &Db, raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("目标文件夹不能为空".to_string());
    }
    let p = std::path::Path::new(raw);
    if p.is_absolute() {
        return Ok(raw.replace('/', "\\"));
    }
    let settings = load_settings(db);
    let root = std::path::Path::new(&settings.organize_root);
    let safe: String = raw
        .chars()
        .map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    Ok(root.join(safe).to_string_lossy().to_string())
}

#[tauri::command]
pub fn list_rules(state: State<AppState>) -> Result<Vec<Rule>, String> {
    state.db.list_rules().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn upsert_rule(
    app: AppHandle,
    state: State<AppState>,
    rule: RuleInput,
) -> Result<i64, String> {
    if !["ext", "keyword", "regex"].contains(&rule.match_type.as_str()) {
        return Err("match_type 必须是 ext / keyword / regex".to_string());
    }
    if rule.match_type == "regex" {
        regex::Regex::new(&rule.pattern).map_err(|e| format!("正则无效: {}", e))?;
    }
    let target = resolve_target_folder(&state.db, &rule.target_folder)?;
    let id = state
        .db
        .upsert_rule(rule.id, rule.name.trim(), &rule.match_type, rule.pattern.trim(), &target)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("rules-changed", ());
    Ok(id)
}

#[tauri::command]
pub fn toggle_rule(app: AppHandle, state: State<AppState>, id: i64, enabled: bool) -> Result<(), String> {
    state.db.toggle_rule(id, enabled).map_err(|e| e.to_string())?;
    let _ = app.emit("rules-changed", ());
    Ok(())
}

#[tauri::command]
pub fn delete_rule(app: AppHandle, state: State<AppState>, id: i64) -> Result<(), String> {
    state.db.delete_rule(id).map_err(|e| e.to_string())?;
    let _ = app.emit("rules-changed", ());
    Ok(())
}

// ---------- settings ----------

#[tauri::command]
pub fn get_settings(app: AppHandle, state: State<AppState>) -> Result<Settings, String> {
    let mut s = load_settings(&state.db);
    s.temp_path = crate::tempfs::temp_path_display(&app);
    Ok(s)
}

#[tauri::command]
pub fn get_temp_info(app: AppHandle) -> Result<crate::tempfs::TempInfo, String> {
    crate::tempfs::info(&app).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_temp_dir(app: AppHandle) -> Result<crate::tempfs::TempInfo, String> {
    crate::tempfs::clear(&app).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    state: State<AppState>,
    settings: Settings,
) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let db = &state.db;
    for (k, v) in [
        ("base_url", settings.base_url.trim()),
        ("api_key", settings.api_key.trim()),
        ("model", settings.model.trim()),
        ("organize_root", settings.organize_root.trim()),
    ] {
        db.set_setting(k, v).map_err(|e| e.to_string())?;
    }
    db.set_setting("auto_organize", if settings.auto_organize { "true" } else { "false" })
        .map_err(|e| e.to_string())?;
    db.set_setting("autostart", if settings.autostart { "true" } else { "false" })
        .map_err(|e| e.to_string())?;
    db.set_setting(
        "thinking_enabled",
        if settings.thinking_enabled { "true" } else { "false" },
    )
    .map_err(|e| e.to_string())?;
    db.set_setting("reasoning_effort", settings.reasoning_effort.trim())
        .map_err(|e| e.to_string())?;
    for (k, v) in [
        ("vision_base_url", settings.vision_base_url.trim()),
        ("vision_api_key", settings.vision_api_key.trim()),
        ("vision_model", settings.vision_model.trim()),
    ] {
        db.set_setting(k, v).map_err(|e| e.to_string())?;
    }
    db.set_setting(
        "subagent_thinking_enabled",
        if settings.subagent_thinking_enabled { "true" } else { "false" },
    )
    .map_err(|e| e.to_string())?;
    db.set_setting(
        "subagent_reasoning_effort",
        settings.subagent_reasoning_effort.trim(),
    )
    .map_err(|e| e.to_string())?;
    db.set_setting("subagent_model", settings.subagent_model.trim())
        .map_err(|e| e.to_string())?;
    db.set_setting(
        "command_tools_enabled",
        if settings.command_tools_enabled { "true" } else { "false" },
    )
    .map_err(|e| e.to_string())?;
    for (k, v) in [
        ("profile_name", settings.profile_name.trim()),
        ("profile_alias", settings.profile_alias.trim()),
        ("profile_gender", settings.profile_gender.trim()),
        ("profile_birth", settings.profile_birth.trim()),
        ("profile_phone", settings.profile_phone.trim()),
        ("profile_email", settings.profile_email.trim()),
        ("profile_city", settings.profile_city.trim()),
    ] {
        db.set_setting(k, v).map_err(|e| e.to_string())?;
    }
    db.set_setting(
        "auto_organize_paused",
        if settings.auto_organize { "false" } else { "true" },
    )
    .map_err(|e| e.to_string())?;
    state
        .auto_paused
        .store(!settings.auto_organize, Ordering::Relaxed);
    let _ = if settings.autostart {
        app.autolaunch().enable()
    } else {
        app.autolaunch().disable()
    };
    Ok(())
}

// ---------- desktop ----------

#[tauri::command]
pub fn scan_desktop_preview(state: State<AppState>) -> Result<Vec<scanner::FileItem>, String> {
    scanner::scan_desktop(500, organize_root_skip(&state.db)).map_err(|e| e.to_string())
}

// ---- 个人信息自由条目 ----

#[tauri::command]
pub fn list_profile_entries(state: State<AppState>) -> Vec<crate::db::ProfileEntry> {
    state.db.pf_list().unwrap_or_default()
}

#[tauri::command]
pub fn save_profile_entry(
    app: AppHandle,
    state: State<AppState>,
    id: Option<i64>,
    label: String,
    content: String,
) -> Result<i64, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("标签不能为空".to_string());
    }
    let result_id = match id {
        Some(i) => {
            state
                .db
                .pf_update_by_id(i, label, content.trim())
                .map_err(|e| e.to_string())?;
            i
        }
        None => state
            .db
            .pf_upsert(label, content.trim())
            .map_err(|e| e.to_string())?,
    };
    let _ = app.emit("profile-changed", ());
    Ok(result_id)
}

#[tauri::command]
pub fn delete_profile_entry(app: AppHandle, state: State<AppState>, id: i64) -> Result<(), String> {
    state.db.pf_delete(id).map_err(|e| e.to_string())?;
    let _ = app.emit("profile-changed", ());
    Ok(())
}

// ---------- 后台任务（主窗口「后台任务」页使用）----------

#[tauri::command]
pub fn list_bg_tasks(state: State<AppState>) -> Vec<crate::tasks::TaskInfo> {
    state.tasks.list()
}

#[tauri::command]
pub fn stop_bg_task(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    state.tasks.stop(&app, &id).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn read_bg_task_tail(
    state: State<AppState>,
    id: String,
    max_chars: Option<usize>,
) -> Result<String, String> {
    state
        .tasks
        .tail(&id, max_chars.unwrap_or(3000))
        .map_err(|e| e.to_string())
}

// ---------- Skills ----------

#[tauri::command]
pub fn list_skills_cmd(state: State<AppState>) -> Vec<crate::skills::SkillInfo> {
    state.skills.list()
}

#[tauri::command]
pub fn create_skill_cmd(
    app: AppHandle,
    state: State<AppState>,
    name: String,
    description: String,
    body: String,
) -> Result<crate::skills::SkillInfo, String> {
    let info = state
        .skills
        .create(&name, &description, &body)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("skills-changed", ());
    Ok(info)
}

#[tauri::command]
pub fn delete_skill_cmd(app: AppHandle, state: State<AppState>, name: String) -> Result<(), String> {
    state.skills.delete(&name).map_err(|e| e.to_string())?;
    let _ = app.emit("skills-changed", ());
    Ok(())
}

#[tauri::command]
pub fn set_skill_enabled(
    app: AppHandle,
    state: State<AppState>,
    name: String,
    enabled: bool,
) -> Result<crate::skills::SkillInfo, String> {
    let info = state
        .skills
        .set_enabled(&name, enabled)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("skills-changed", ());
    Ok(info)
}

#[tauri::command]
pub fn scan_external_skills(state: State<AppState>) -> Vec<crate::skills::ExternalSkill> {
    state.skills.scan_external()
}

#[tauri::command]
pub fn sync_skills_cmd(
    app: AppHandle,
    state: State<AppState>,
    source: Option<String>,
    names: Option<Vec<String>>,
    overwrite: Option<bool>,
) -> Result<serde_json::Value, String> {
    let result = state
        .skills
        .sync_from(
            source.as_deref(),
            names.as_deref(),
            overwrite.unwrap_or(false),
        )
        .map_err(|e| e.to_string())?;
    let _ = app.emit("skills-changed", ());
    Ok(result)
}

#[tauri::command]
pub fn read_skill_content(state: State<AppState>, name: String) -> Result<String, String> {
    state.skills.read_raw(&name).map_err(|e| e.to_string())
}
