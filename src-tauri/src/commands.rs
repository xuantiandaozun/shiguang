use crate::db::{BatchSummary, ChatMsg, Db, Plan, Rule, Todo};
use crate::organizer::{executor, scanner};
use crate::{llm, windows, AppState};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager, State};

/// 一套可切换的大模型接口配置（供应商 / Key / 模型）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProfile {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    /// 统一模型目录 id；`__custom__` 表示自定义 API 模型名
    #[serde(default)]
    pub catalog_model: String,
    /// 实际发给 API 的模型名
    pub model: String,
    #[serde(default)]
    pub thinking_enabled: bool,
    #[serde(default = "default_reasoning_effort")]
    pub reasoning_effort: String,
    /// 自定义模型的识图能力覆盖；None 时继续按内置目录/模型名推断。
    #[serde(default)]
    pub supports_vision: Option<bool>,
    #[serde(default)]
    pub models: Vec<ProfileModel>,
    #[serde(default)]
    pub valid_until: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileModel {
    pub model: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default = "default_context_window")]
    pub context_window: usize,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentRole {
    pub role: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub profile_id: String,
    #[serde(default)]
    pub model: String,
}

pub fn default_subagent_roles() -> Vec<SubagentRole> {
    ["research", "files", "browser"].into_iter().map(|role| SubagentRole {
        role: role.into(), enabled: true, profile_id: String::new(), model: String::new(),
    }).collect()
}

fn default_reasoning_effort() -> String {
    "high".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 已保存的多套大模型配置；当前对话使用 active_llm_profile_id 那一套
    #[serde(default)]
    pub llm_profiles: Vec<LlmProfile>,
    #[serde(default)]
    pub active_llm_profile_id: String,
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
    #[serde(default = "default_subagent_roles")]
    pub subagent_roles: Vec<SubagentRole>,
    /// 摘要压缩模型；留空时跟随主模型。
    #[serde(default)]
    pub compact_model: String,
    /// 当前主模型上下文窗口，用于按预算而不是固定字节数触发压缩。
    #[serde(default = "default_context_window")]
    pub context_window: usize,
    /// 当前主模型最终是否支持图像输入，由激活的档案派生。
    #[serde(default)]
    pub model_supports_vision: bool,
    // ---- System One（高速结构化决策，不是聊天模型）----
    #[serde(default)]
    pub system_one_enabled: bool,
    #[serde(default = "default_system_one_base_url")]
    pub system_one_base_url: String,
    #[serde(default)]
    pub system_one_api_key: String,
    #[serde(default = "default_system_one_model")]
    pub system_one_model: String,
    #[serde(default = "default_system_one_confidence")]
    pub system_one_min_confidence: f64,
    #[serde(default = "default_true")]
    pub system_one_browser_assist: bool,
    #[serde(default = "default_true")]
    pub system_one_context_assist: bool,
    /// 允许 AI 执行命令行（后台任务类工具总开关）
    pub command_tools_enabled: bool,
    /// confirmation: 每次有副作用都确认；balanced: 仅敏感/危险动作确认；autopilot: 非危险动作自动执行
    pub permission_level: String,
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

fn default_context_window() -> usize {
    128_000
}
fn default_system_one_base_url() -> String {
    "https://api.typesafe.ai/v1".into()
}
fn default_system_one_model() -> String {
    "jev-latest".into()
}
fn default_system_one_confidence() -> f64 {
    0.75
}
fn default_true() -> bool {
    true
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
        subagent_roles: db.get_setting("subagent_roles").ok().flatten()
            .and_then(|raw| serde_json::from_str(&raw).ok()).unwrap_or_else(default_subagent_roles),
        compact_model: get("compact_model", ""),
        context_window: get("context_window", "128000")
            .parse()
            .unwrap_or(128_000)
            .clamp(16_000, 20_000_000),
        model_supports_vision: false,
        system_one_enabled: db
            .get_setting("system_one_enabled")
            .ok()
            .flatten()
            .is_some_and(|v| v == "true"),
        system_one_base_url: get("system_one_base_url", "https://api.typesafe.ai/v1"),
        system_one_api_key: get("system_one_api_key", ""),
        system_one_model: get("system_one_model", "jev-latest"),
        system_one_min_confidence: get("system_one_min_confidence", "0.75")
            .parse::<f64>()
            .unwrap_or(0.75)
            .clamp(0.0, 1.0),
        system_one_browser_assist: db
            .get_setting("system_one_browser_assist")
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(true),
        system_one_context_assist: db
            .get_setting("system_one_context_assist")
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(true),
        command_tools_enabled: db
            .get_setting("command_tools_enabled")
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(true),
        permission_level: match get("permission_level", "balanced").as_str() {
            "confirmation" | "balanced" | "autopilot" => get("permission_level", "balanced"),
            _ => "balanced".to_string(),
        },
        profile_name: get("profile_name", ""),
        profile_alias: get("profile_alias", ""),
        profile_gender: get("profile_gender", ""),
        profile_birth: get("profile_birth", ""),
        profile_phone: get("profile_phone", ""),
        profile_email: get("profile_email", ""),
        profile_city: get("profile_city", ""),
        llm_profiles: Vec::new(),
        active_llm_profile_id: String::new(),
        temp_path: default_temp_path(),
    };
    if let Some(new_root) = migrate_legacy_root(db, &desktop, &settings.organize_root) {
        settings.organize_root = new_root;
    }
    hydrate_llm_profiles(db, &mut settings);
    settings
}

fn infer_provider_name(base_url: &str) -> String {
    let u = base_url.to_lowercase();
    if u.contains("deepseek") {
        "DeepSeek".into()
    } else if u.contains("dashscope") || u.contains("aliyuncs") {
        "通义千问".into()
    } else if u.contains("/coding") {
        "火山 Coding Plan".into()
    } else if u.contains("volces.com") || u.contains("volcengine") {
        "火山 Agent Plan".into()
    } else if u.contains("openai.com") {
        "OpenAI".into()
    } else if u.contains("bigmodel") {
        "智谱 GLM".into()
    } else if u.contains("moonshot") {
        "Kimi".into()
    } else if u.contains("siliconflow") {
        "硅基流动".into()
    } else if u.contains("openrouter") {
        "OpenRouter".into()
    } else if u.contains("generativelanguage") || u.contains("googleapis.com") {
        "Google Gemini".into()
    } else if u.contains("agentrouter") {
        "AgentRouter".into()
    } else if u.contains("minimax") {
        "MiniMax".into()
    } else if u.contains("hunyuan") {
        "腾讯混元".into()
    } else {
        "当前配置".into()
    }
}

fn apply_active_llm_profile(settings: &mut Settings) {
    let id = settings.active_llm_profile_id.clone();
    let Some(p) = settings.llm_profiles.iter().find(|p| p.id == id) else {
        return;
    };
    settings.base_url = p.base_url.clone();
    settings.api_key = p.api_key.clone();
    settings.model = p.model.clone();
    settings.thinking_enabled = p.thinking_enabled;
    let effort = p.reasoning_effort.trim();
    settings.reasoning_effort = match effort {
        "low" | "high" | "max" => effort.to_string(),
        _ => "high".to_string(),
    };
    settings.model_supports_vision = if p.catalog_model == "__custom__" {
        p.supports_vision
            .unwrap_or_else(|| crate::llm::models::supports_vision(&p.model))
    } else {
        crate::llm::models::supports_vision(&p.model)
    };
    if let Some(model) = p.models.iter().find(|model| model.model == p.model) {
        settings.model_supports_vision = model.supports_vision;
        settings.context_window = model.context_window;
    }
}

pub fn profile_expired(profile: &LlmProfile) -> anyhow::Result<bool> {
    if profile.valid_until.trim().is_empty() { return Ok(false); }
    let raw = profile.valid_until.trim();
    let deadline = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S"))
        .map_err(|_| anyhow::anyhow!("「{}」的有效期格式无效", profile.name))?;
    Ok(chrono::Local::now().naive_local() >= deadline)
}

pub fn ensure_profile_usable(settings: &Settings, profile_id: &str) -> anyhow::Result<()> {
    if let Some(profile) = settings.llm_profiles.iter().find(|p| p.id == profile_id) {
        if profile_expired(profile)? {
            anyhow::bail!("「{}」已到期，请在设置中续期或切换配置", profile.name);
        }
    }
    Ok(())
}

pub fn validate_profiles(settings: &Settings) -> anyhow::Result<()> {
    let mut ids = std::collections::HashSet::new();
    for profile in &settings.llm_profiles {
        if profile.id.trim().is_empty() || !ids.insert(&profile.id) { anyhow::bail!("配置标识不能为空或重复"); }
        profile_expired(profile)?;
        let mut names = std::collections::HashSet::new();
        for model in &profile.models {
            if model.model.trim().is_empty() || model.model != model.model.trim() || !names.insert(&model.model) { anyhow::bail!("「{}」的模型名称不能为空、带首尾空格或重复", profile.name); }
            if !(16_000..=20_000_000).contains(&model.context_window) { anyhow::bail!("模型上下文窗口应在16k～20m之间"); }
        }
        if !profile.models.is_empty() && !profile.models.iter().any(|m| m.model == profile.model) { anyhow::bail!("「{}」当前模型不在已添加列表中", profile.name); }
    }
    let mut roles = std::collections::HashSet::new();
    for role in &settings.subagent_roles {
        if !matches!(role.role.as_str(), "research" | "files" | "browser") || !roles.insert(&role.role) { anyhow::bail!("子代理类型无效或重复"); }
        let profile_id = if role.profile_id.is_empty() { &settings.active_llm_profile_id } else { &role.profile_id };
        let profile = settings.llm_profiles.iter().find(|p| &p.id == profile_id).ok_or_else(|| anyhow::anyhow!("子代理引用的供应商配置不存在"))?;
        if !role.model.is_empty() && !profile.models.iter().any(|m| m.model == role.model) { anyhow::bail!("子代理模型不在所选供应商的列表中"); }
    }
    Ok(())
}

pub fn settings_for_subagent(settings: &Settings, role: &str) -> anyhow::Result<Settings> {
    let config = settings.subagent_roles.iter().find(|r| r.role == role).ok_or_else(|| anyhow::anyhow!("子代理类型不存在：{role}"))?;
    if !config.enabled { anyhow::bail!("{role} 子代理已停用"); }
    let mut result = settings.clone();
    if !config.profile_id.is_empty() { result.active_llm_profile_id = config.profile_id.clone(); }
    if !result.llm_profiles.iter().any(|p| p.id == result.active_llm_profile_id) { anyhow::bail!("子代理引用的供应商配置不存在"); }
    ensure_profile_usable(&result, &result.active_llm_profile_id)?;
    apply_active_llm_profile(&mut result);
    if !config.model.is_empty() {
        let profile = result.llm_profiles.iter().find(|p| p.id == result.active_llm_profile_id).ok_or_else(|| anyhow::anyhow!("子代理配置不存在"))?;
        let model = profile.models.iter().find(|m| m.model == config.model).ok_or_else(|| anyhow::anyhow!("子代理模型不在供应商列表中"))?;
        result.model = model.model.clone(); result.model_supports_vision = model.supports_vision; result.context_window = model.context_window;
    }
    result.thinking_enabled = settings.subagent_thinking_enabled;
    result.reasoning_effort = settings.subagent_reasoning_effort.clone();
    Ok(result)
}

/// 把旧的单套 base_url/api_key/model 迁成可切换档案；之后以档案为源，回写到生效字段。
fn hydrate_llm_profiles(db: &Db, settings: &mut Settings) {
    let raw = db
        .get_setting("llm_profiles")
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut profiles: Vec<LlmProfile> = if raw.trim().is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(&raw).unwrap_or_default()
    };
    let stored_active = db
        .get_setting("active_llm_profile_id")
        .ok()
        .flatten()
        .unwrap_or_default();

    if profiles.is_empty() {
        let id = "legacy".to_string();
        profiles.push(LlmProfile {
            id: id.clone(),
            name: infer_provider_name(&settings.base_url),
            base_url: settings.base_url.clone(),
            api_key: settings.api_key.clone(),
            catalog_model: String::new(),
            model: settings.model.clone(),
            thinking_enabled: settings.thinking_enabled,
            reasoning_effort: settings.reasoning_effort.clone(),
            supports_vision: None,
            models: Vec::new(), valid_until: String::new(),
        });
        settings.llm_profiles = profiles;
        settings.active_llm_profile_id = id;
        persist_llm_profiles(db, settings);
        return;
    }

    settings.llm_profiles = profiles;
    for p in &mut settings.llm_profiles {
        p.model = crate::llm::provider_headers::sanitize_model(&p.base_url, &p.model);
        if p.models.is_empty() && !p.model.trim().is_empty() {
            p.models.push(ProfileModel { model:p.model.clone(), label:p.model.clone(), supports_vision:p.supports_vision.unwrap_or_else(|| crate::llm::models::supports_vision(&p.model)), context_window:settings.context_window });
        }
    }
    settings.active_llm_profile_id = if settings.llm_profiles.iter().any(|p| p.id == stored_active)
    {
        stored_active
    } else {
        settings.llm_profiles[0].id.clone()
    };
    apply_active_llm_profile(settings);
}

fn persist_llm_profiles(db: &Db, settings: &Settings) {
    if let Ok(json) = serde_json::to_string(&settings.llm_profiles) {
        let _ = db.set_setting("llm_profiles", &json);
    }
    let _ = db.set_setting("active_llm_profile_id", &settings.active_llm_profile_id);
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
    let legacy = desktop
        .join("桌面整理")
        .to_string_lossy()
        .replace('\\', "/");
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

/// 把附件路径拼进用户消息。图片在主模型能看图时会由请求层直接嵌入，不必再调 read_image。
fn compose_user_message(
    text: &str,
    attachments: &[String],
    native_vision: bool,
) -> Result<String, String> {
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

    let mut block = String::from("【用户附带的文件】\n");
    for (i, path) in paths.iter().enumerate() {
        let hint = if crate::llm::models::is_image_file(path) {
            crate::llm::models::image_attachment_hint(native_vision)
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
pub fn import_clipboard_attachments(
    app: AppHandle,
    include_image: bool,
) -> Result<crate::clipboard::ClipboardImport, String> {
    crate::clipboard::import_attachments(&app, include_image)
}

#[tauri::command]
pub fn save_pasted_file(
    app: AppHandle,
    filename: String,
    data_base64: String,
) -> Result<String, String> {
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD
        .decode(data_base64)
        .map_err(|e| format!("粘贴内容解码失败: {e}"))?;
    crate::clipboard::save_pasted_bytes(&app, &filename, &data)
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
    let native_vision = {
        let settings = load_settings(&state.db);
        settings.model_supports_vision
    };
    let composed =
        match compose_user_message(&text, attachments.as_deref().unwrap_or(&[]), native_vision) {
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
    let snapshot = state.session_todos.begin_turn(session_id);
    let _ = app.emit("session-todos", &snapshot);
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
        let result = llm::agent::run_chat(app2.clone(), session_id, msg_id, cancel).await;
        let busy_state = app2.state::<AppState>();
        busy_state.chat_busy.store(false, Ordering::SeqCst);
        if let Err(e) = result {
            let _ = app2.emit("llm-error", serde_json::json!({ "message": e.to_string() }));
        }
    });
    Ok(msg_id)
}

/// 中断当前回复：模型连接/流式输出、正在执行的工具、子代理以及本轮启动的
/// 后台命令都会收到取消信号；命令会强制结束整棵进程树。
#[tauri::command]
pub fn stop_chat_message(state: State<AppState>) -> Result<(), String> {
    let token = state.chat_cancel.lock().map_err(|e| e.to_string())?.take();
    state.ask_user.dismiss();
    state.subagents.cancel_all();
    match token {
        Some(t) => {
            t.cancel();
            Ok(())
        }
        None => Err("当前没有进行中的回复".to_string()),
    }
}

fn messages_for_ui(messages: Vec<ChatMsg>) -> Vec<ChatMsg> {
    messages
        .into_iter()
        .filter_map(|mut msg| {
            if msg.role != "user" {
                msg.content = llm::agent::visible_assistant_content(&msg.content);
                if msg.content.is_empty() {
                    return None;
                }
            }
            Some(msg)
        })
        .collect()
}

#[tauri::command]
pub fn get_current_session(state: State<AppState>) -> Result<SessionView, String> {
    let session_id = state.db.current_session_id().map_err(|e| e.to_string())?;
    let messages = messages_for_ui(
        state
            .db
            .load_chat(session_id, 50)
            .map_err(|e| e.to_string())?,
    );
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
    state
        .db
        .set_current_session(id)
        .map_err(|e| e.to_string())?;
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
    state
        .db
        .set_current_session(id)
        .map_err(|e| e.to_string())?;
    let messages = messages_for_ui(state.db.load_chat(id, 50).map_err(|e| e.to_string())?);
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
    state.session_todos.drop_session(id);
    let messages = messages_for_ui(
        state
            .db
            .load_chat(new_current, 50)
            .map_err(|e| e.to_string())?,
    );
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
    Ok(messages_for_ui(
        state
            .db
            .load_chat(session_id, 50)
            .map_err(|e| e.to_string())?,
    ))
}

#[tauri::command]
pub fn clear_chat_history(state: State<AppState>) -> Result<(), String> {
    let session_id = state.db.current_session_id().map_err(|e| e.to_string())?;
    state.db.clear_chat(session_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_delegations(app: AppHandle) -> Result<Vec<crate::db::DelegationTask>, String> {
    crate::delegation::list(&app).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_delegation(app: AppHandle, id: String) -> Result<crate::delegation::DelegationView, String> {
    crate::delegation::get(&app, &id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn show_delegation_window(app: AppHandle, id: String) -> Result<(), String> {
    crate::delegation::get(&app, &id).map_err(|e| e.to_string())?;
    crate::windows::show_delegation(&app, &id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn create_delegation(app: AppHandle, title: String, goal: String) -> Result<crate::db::DelegationTask, String> {
    crate::delegation::create(&app, &title, &goal, "用户").map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn reply_delegation(app: AppHandle, id: String, content: String) -> Result<crate::db::DelegationTask, String> {
    crate::delegation::reply(&app, &id, "user", &content).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cancel_delegation(app: AppHandle, id: String) -> Result<crate::db::DelegationTask, String> {
    crate::delegation::cancel(&app, &id).await.map_err(|e| e.to_string())
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
        || lower.starts_with("mailto:")
        || lower.starts_with("chrome://")
        || lower.starts_with("edge://");
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

#[tauri::command]
pub fn get_browser_extension_info(app: AppHandle) -> crate::browser::ExtensionInfo {
    crate::browser::extension_info(&app)
}

#[tauri::command]
pub fn open_browser_extension_dir(app: AppHandle) -> Result<(), String> {
    let info = crate::browser::extension_info(&app);
    if !info.ready {
        return Err(format!(
            "扩展目录还不完整：{}。请重新安装拾光，或确认安装目录下有 browser-extension 文件夹",
            info.path
        ));
    }
    let path = info.path.replace('/', "\\");
    std::process::Command::new("explorer")
        .arg(&path)
        .spawn()
        .map_err(|e| format!("打开文件夹失败: {e}"))?;
    Ok(())
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
pub fn set_todo_done(
    app: AppHandle,
    state: State<AppState>,
    id: i64,
    done: bool,
) -> Result<(), String> {
    state
        .db
        .set_todo_done(id, done)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("todos-changed", ());
    Ok(())
}

// ---------- plans ----------

#[tauri::command]
pub fn get_pending_plan(state: State<AppState>) -> Result<Option<Plan>, String> {
    state.db.pending_plan().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_pending_ask(state: State<AppState>) -> Option<crate::ask_user::AskUserPrompt> {
    state.ask_user.prompt()
}

#[tauri::command]
pub fn answer_ask_user(
    state: State<AppState>,
    answers: Vec<crate::ask_user::AskAnswer>,
) -> Result<(), String> {
    state.ask_user.answer(answers).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn dismiss_ask_user(state: State<AppState>) {
    state.ask_user.dismiss();
}

#[tauri::command]
pub fn get_session_todos(
    state: State<AppState>,
    session_id: Option<i64>,
) -> Result<Vec<crate::session_todo::SessionTodo>, String> {
    let id = match session_id {
        Some(id) => id,
        None => state.db.current_session_id().map_err(|e| e.to_string())?,
    };
    Ok(state.session_todos.list(id))
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
pub fn upsert_rule(app: AppHandle, state: State<AppState>, rule: RuleInput) -> Result<i64, String> {
    if !["ext", "keyword", "regex"].contains(&rule.match_type.as_str()) {
        return Err("match_type 必须是 ext / keyword / regex".to_string());
    }
    if rule.match_type == "regex" {
        regex::Regex::new(&rule.pattern).map_err(|e| format!("正则无效: {}", e))?;
    }
    let target = resolve_target_folder(&state.db, &rule.target_folder)?;
    let id = state
        .db
        .upsert_rule(
            rule.id,
            rule.name.trim(),
            &rule.match_type,
            rule.pattern.trim(),
            &target,
        )
        .map_err(|e| e.to_string())?;
    let _ = app.emit("rules-changed", ());
    Ok(id)
}

#[tauri::command]
pub fn toggle_rule(
    app: AppHandle,
    state: State<AppState>,
    id: i64,
    enabled: bool,
) -> Result<(), String> {
    state
        .db
        .toggle_rule(id, enabled)
        .map_err(|e| e.to_string())?;
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
    let mut settings = settings;
    if settings.llm_profiles.is_empty() {
        settings.llm_profiles.push(LlmProfile {
            id: "legacy".into(),
            name: infer_provider_name(&settings.base_url),
            base_url: settings.base_url.clone(),
            api_key: settings.api_key.clone(),
            catalog_model: String::new(),
            model: settings.model.clone(),
            thinking_enabled: settings.thinking_enabled,
            reasoning_effort: settings.reasoning_effort.clone(),
            supports_vision: None,
            models: Vec::new(), valid_until: String::new(),
        });
        settings.active_llm_profile_id = "legacy".into();
    }
    if !settings
        .llm_profiles
        .iter()
        .any(|p| p.id == settings.active_llm_profile_id)
    {
        settings.active_llm_profile_id = settings.llm_profiles[0].id.clone();
    }
    apply_active_llm_profile(&mut settings);
    validate_profiles(&settings).map_err(|e| e.to_string())?;
    db.set_setting("subagent_roles", &serde_json::to_string(&settings.subagent_roles).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    persist_llm_profiles(db, &settings);
    for (k, v) in [
        ("base_url", settings.base_url.trim()),
        ("api_key", settings.api_key.trim()),
        ("model", settings.model.trim()),
        ("organize_root", settings.organize_root.trim()),
    ] {
        db.set_setting(k, v).map_err(|e| e.to_string())?;
    }
    db.set_setting(
        "auto_organize",
        if settings.auto_organize {
            "true"
        } else {
            "false"
        },
    )
    .map_err(|e| e.to_string())?;
    db.set_setting(
        "autostart",
        if settings.autostart { "true" } else { "false" },
    )
    .map_err(|e| e.to_string())?;
    db.set_setting(
        "thinking_enabled",
        if settings.thinking_enabled {
            "true"
        } else {
            "false"
        },
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
        if settings.subagent_thinking_enabled {
            "true"
        } else {
            "false"
        },
    )
    .map_err(|e| e.to_string())?;
    db.set_setting(
        "subagent_reasoning_effort",
        settings.subagent_reasoning_effort.trim(),
    )
    .map_err(|e| e.to_string())?;
    db.set_setting("subagent_model", settings.subagent_model.trim())
        .map_err(|e| e.to_string())?;
    for (k, v) in [
        ("compact_model", settings.compact_model.trim().to_string()),
        (
            "context_window",
            settings
                .context_window
                .clamp(16_000, 20_000_000)
                .to_string(),
        ),
        (
            "system_one_base_url",
            settings.system_one_base_url.trim().to_string(),
        ),
        (
            "system_one_api_key",
            settings.system_one_api_key.trim().to_string(),
        ),
        (
            "system_one_model",
            settings.system_one_model.trim().to_string(),
        ),
        (
            "system_one_min_confidence",
            settings
                .system_one_min_confidence
                .clamp(0.0, 1.0)
                .to_string(),
        ),
    ] {
        db.set_setting(k, &v).map_err(|e| e.to_string())?;
    }
    for (k, enabled) in [
        ("system_one_enabled", settings.system_one_enabled),
        (
            "system_one_browser_assist",
            settings.system_one_browser_assist,
        ),
        (
            "system_one_context_assist",
            settings.system_one_context_assist,
        ),
    ] {
        db.set_setting(k, if enabled { "true" } else { "false" })
            .map_err(|e| e.to_string())?;
    }
    db.set_setting(
        "command_tools_enabled",
        if settings.command_tools_enabled {
            "true"
        } else {
            "false"
        },
    )
    .map_err(|e| e.to_string())?;
    db.set_setting(
        "permission_level",
        match settings.permission_level.as_str() {
            "confirmation" | "balanced" | "autopilot" => settings.permission_level.as_str(),
            _ => "balanced",
        },
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
        if settings.auto_organize {
            "false"
        } else {
            "true"
        },
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

// ---------- automation workflows ----------

#[derive(Debug, Clone, Deserialize)]
pub struct AutomationWorkflowInput {
    pub id: Option<i64>,
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub schedule_rule: String,
    pub next_run_at: Option<String>,
    pub enabled: bool,
}

fn validate_workflow(input: &AutomationWorkflowInput) -> Result<(), String> {
    if input.name.trim().is_empty() || input.name.trim().chars().count() > 60 {
        return Err("工作流名称需为 1–60 个字符".into());
    }
    if input.prompt.trim().is_empty() || input.prompt.trim().chars().count() > 4000 {
        return Err("执行内容需为 1–4000 个字符".into());
    }
    if !["manual", "once", "daily", "weekly"].contains(&input.schedule_rule.as_str()) {
        return Err("不支持的执行计划".into());
    }
    if input.schedule_rule != "manual" && input.next_run_at.as_deref().unwrap_or("").is_empty() {
        return Err("定时工作流需要下一次执行时间".into());
    }
    Ok(())
}

#[tauri::command]
pub fn list_automation_workflows(
    state: State<AppState>,
) -> Result<Vec<crate::db::AutomationWorkflow>, String> {
    state
        .db
        .automation_workflow_list()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_automation_workflow(
    app: AppHandle,
    state: State<AppState>,
    input: AutomationWorkflowInput,
) -> Result<crate::db::AutomationWorkflow, String> {
    validate_workflow(&input)?;
    let saved = state
        .db
        .automation_workflow_save(&crate::db::AutomationWorkflow {
            id: input.id.unwrap_or(0),
            name: input.name,
            description: input.description,
            prompt: input.prompt,
            schedule_rule: input.schedule_rule,
            next_run_at: input.next_run_at,
            enabled: input.enabled,
            run_count: 0,
            last_run_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        })
        .map_err(|e| e.to_string())?;
    let _ = app.emit("automation-workflows-changed", ());
    Ok(saved)
}

#[tauri::command]
pub fn delete_automation_workflow(
    app: AppHandle,
    state: State<AppState>,
    id: i64,
) -> Result<(), String> {
    state
        .db
        .automation_workflow_delete(id)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("automation-workflows-changed", ());
    Ok(())
}

fn next_workflow_run(rule: &str, current: Option<&str>) -> Option<String> {
    let current = current
        .and_then(|value| chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").ok())?;
    let days = match rule {
        "daily" => 1,
        "weekly" => 7,
        _ => return None,
    };
    Some(
        (current + chrono::Duration::days(days))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    )
}

pub async fn run_automation_workflow(
    app: AppHandle,
    id: i64,
    scheduled: bool,
) -> Result<i64, String> {
    let state = app.state::<AppState>();
    let workflow = state
        .db
        .automation_workflow_get(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "工作流不存在".to_string())?;
    if scheduled && !workflow.enabled {
        return Err("工作流已停用".into());
    }
    // 定时工作流不应在当前对话忙碌时被标记为已运行；保留到下一轮调度重试。
    if state.chat_busy.load(Ordering::SeqCst) {
        return Err("聊天正在处理另一项任务，将在下一次调度时重试".into());
    }
    let next = next_workflow_run(&workflow.schedule_rule, workflow.next_run_at.as_deref());
    state
        .db
        .mark_automation_workflow_run(id, next.as_deref())
        .map_err(|e| e.to_string())?;
    let _ = app.emit("automation-workflows-changed", ());
    let origin = if scheduled {
        "定时触发"
    } else {
        "手动执行"
    };
    let result = send_chat_message(app.clone(), format!("【运行工作流：{}｜{}】\n这是一次自主执行任务，不是预设脚本，也不是 Skill。请像处理聊天请求一样，依据目标自行规划并使用当前可用的全部工具完成工作；不要等待用户逐步发送下一条指令。按当前权限策略直接执行允许的操作，仅在危险、不可逆、提权或范围不明时确认。\n\n工作流目标与约束：\n{}", workflow.name, origin, workflow.prompt), None).await;
    if result.is_ok() {
        windows::show_chat(&app);
    }
    result
}

#[tauri::command]
pub async fn run_automation_workflow_cmd(app: AppHandle, id: i64) -> Result<i64, String> {
    run_automation_workflow(app, id, false).await
}

#[tauri::command]
pub fn list_browser_recipes_cmd(
    state: State<AppState>,
) -> Result<Vec<crate::db::BrowserRecipe>, String> {
    state.db.browser_recipe_list().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_browser_recipe_cmd(
    app: AppHandle,
    state: State<AppState>,
    id: i64,
) -> Result<(), String> {
    state
        .db
        .browser_recipe_delete(id)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("browser-recipes-changed", ());
    Ok(())
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
pub fn delete_skill_cmd(
    app: AppHandle,
    state: State<AppState>,
    name: String,
) -> Result<(), String> {
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

#[tauri::command]
pub fn get_llm_usage_stats(state: State<AppState>) -> Result<crate::db::LlmUsageSnapshot, String> {
    state.db.llm_usage_snapshot().map_err(|e| e.to_string())
}

#[cfg(test)]
mod llm_profile_tests {
    use super::*;

    #[test]
    fn provider_models_and_subagent_credentials_stay_independent() {
        let (db,path) = test_db();
        db.set_setting("llm_profiles", r#"[{"id":"a","name":"阿里","base_url":"https://coding.dashscope.aliyuncs.com/v1","api_key":"test-a","model":"qwen-manual","models":[{"model":"qwen-manual","supports_vision":true,"context_window":256000},{"model":"glm-manual","context_window":128000}]},{"id":"b","name":"其他","base_url":"https://api.example.com/v1","api_key":"test-b","model":"other-model","models":[{"model":"other-model","context_window":64000}]}]"#).unwrap();
        db.set_setting("active_llm_profile_id","b").unwrap();
        let mut settings = load_settings(&db);
        settings.subagent_roles.iter_mut().find(|r| r.role == "browser").unwrap().profile_id = "a".into();
        settings.subagent_roles.iter_mut().find(|r| r.role == "browser").unwrap().model = "qwen-manual".into();
        validate_profiles(&settings).unwrap();
        let browser = settings_for_subagent(&settings,"browser").unwrap();
        assert_eq!(settings.api_key,"test-b");
        assert_eq!(browser.api_key,"test-a");
        assert_eq!(browser.base_url,"https://coding.dashscope.aliyuncs.com/v1");
        assert_eq!(browser.model,"qwen-manual");
        assert!(browser.model_supports_vision);
        assert_eq!(browser.context_window,256000);
        settings.subagent_roles.iter_mut().find(|r| r.role == "browser").unwrap().model = "other-model".into();
        assert!(validate_profiles(&settings).is_err());
        assert!(settings_for_subagent(&settings,"browser").is_err());
        cleanup(db,&path);
    }

    #[test]
    fn expired_plan_blocks_until_renewed() {
        let (db,path) = test_db();
        let mut settings = load_settings(&db);
        settings.llm_profiles[0].valid_until = "2000-01-01T00:00".into();
        assert!(ensure_profile_usable(&settings,&settings.active_llm_profile_id).is_err());
        assert!(settings_for_subagent(&settings,"browser").is_err());
        settings.llm_profiles[0].valid_until = "2099-01-01T00:00".into();
        assert!(ensure_profile_usable(&settings,&settings.active_llm_profile_id).is_ok());
        settings.llm_profiles[0].valid_until = "not-a-date".into();
        assert!(validate_profiles(&settings).is_err());
        cleanup(db,&path);
    }

    #[test]
    fn model_lists_and_expiry_survive_storage_roundtrip() {
        let (db,path) = test_db();
        let mut settings = load_settings(&db);
        settings.llm_profiles[0].models = vec![ProfileModel { model:"exact-manual-name".into(),label:"我的模型".into(),supports_vision:false,context_window:128000 }];
        settings.llm_profiles[0].model = "exact-manual-name".into();
        settings.llm_profiles[0].valid_until = "2099-12-01T09:30".into();
        persist_llm_profiles(&db,&settings);
        let restored = load_settings(&db);
        assert_eq!(restored.model,"exact-manual-name");
        assert_eq!(restored.llm_profiles[0].models[0].label,"我的模型");
        assert_eq!(restored.llm_profiles[0].valid_until,"2099-12-01T09:30");
        cleanup(db,&path);
    }
    use crate::db::Db;

    fn test_db() -> (Db, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("shiguang-llm-profile-{}.db", uuid::Uuid::new_v4()));
        (Db::new(&path).unwrap(), path)
    }

    fn cleanup(db: Db, path: &std::path::Path) {
        drop(db);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn migrates_legacy_single_config_into_a_profile() {
        let (db, path) = test_db();
        db.set_setting("base_url", "https://api.deepseek.com/v1")
            .unwrap();
        db.set_setting("api_key", "sk-test").unwrap();
        db.set_setting("model", "deepseek-chat").unwrap();

        let s = load_settings(&db);
        assert_eq!(s.llm_profiles.len(), 1);
        assert_eq!(s.llm_profiles[0].name, "DeepSeek");
        assert_eq!(s.llm_profiles[0].api_key, "sk-test");
        assert_eq!(s.active_llm_profile_id, s.llm_profiles[0].id);
        assert_eq!(s.base_url, "https://api.deepseek.com/v1");
        assert_eq!(s.model, "deepseek-chat");
        cleanup(db, &path);
    }

    #[test]
    fn active_profile_overrides_flattened_fields() {
        let (db, path) = test_db();
        db.set_setting("base_url", "https://api.deepseek.com/v1")
            .unwrap();
        db.set_setting("api_key", "sk-old").unwrap();
        db.set_setting("model", "deepseek-chat").unwrap();
        db.set_setting(
            "llm_profiles",
            r#"[{"id":"a","name":"火山 Agent Plan","base_url":"https://ark.cn-beijing.volces.com/api/v3","api_key":"sk-ark","model":"doubao-seed-2.0-pro","thinking_enabled":false,"reasoning_effort":"high"},{"id":"b","name":"DeepSeek","base_url":"https://api.deepseek.com/v1","api_key":"sk-old","model":"deepseek-chat","thinking_enabled":true,"reasoning_effort":"high"}]"#,
        )
        .unwrap();
        db.set_setting("active_llm_profile_id", "a").unwrap();

        let s = load_settings(&db);
        assert_eq!(s.active_llm_profile_id, "a");
        assert_eq!(s.base_url, "https://ark.cn-beijing.volces.com/api/v3");
        assert_eq!(s.api_key, "sk-ark");
        assert_eq!(s.model, "doubao-seed-2.0-pro");
        assert!(!s.thinking_enabled);
        cleanup(db, &path);
    }

    #[test]
    fn custom_profile_can_override_vision_capability() {
        let (db, path) = test_db();
        db.set_setting(
            "llm_profiles",
            r#"[{"id":"custom","name":"自定义","base_url":"https://api.example.com/v1","api_key":"sk-test","catalog_model":"__custom__","model":"step-3.7-flash","thinking_enabled":false,"reasoning_effort":"high","supports_vision":true}]"#,
        )
        .unwrap();
        db.set_setting("active_llm_profile_id", "custom").unwrap();

        let s = load_settings(&db);
        assert_eq!(s.model, "step-3.7-flash");
        assert!(s.model_supports_vision);
        cleanup(db, &path);
    }

    #[test]
    fn compose_user_message_hints_native_vision() {
        let img =
            std::env::temp_dir().join(format!("shiguang-compose-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&img, b"x").unwrap();
        let path = img.to_string_lossy().replace('\\', "/");
        let native = compose_user_message("看图", &[path.clone()], true).unwrap();
        assert!(native.contains("已直接附上"));
        assert!(!native.contains("理解画面用 read_image"));
        let fallback = compose_user_message("看图", &[path], false).unwrap();
        assert!(fallback.contains("read_image"));
        let _ = std::fs::remove_file(&img);
    }
}
