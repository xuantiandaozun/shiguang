//! 前端用的插件 IPC：插件列表（含 UI 挂载点）、启停、设置、版本、回滚，
//! 以及插件页面（iframe）经宿主 SDK 发起的工具调用。
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub fn list_plugins() -> Value {
    super::snapshot().ui_manifest()
}

#[tauri::command]
pub fn reload_plugins() -> Value {
    super::host().reload().ui_manifest()
}

#[tauri::command]
pub fn set_plugin_enabled(app: AppHandle, id: String, enabled: bool) -> Result<Value, String> {
    super::authoring::set_enabled(&app, &id, enabled).map_err(err)
}

#[tauri::command]
pub fn get_plugin_settings(app: AppHandle, id: String) -> Value {
    super::settings::values(&app, &id)
}

#[tauri::command]
pub fn save_plugin_settings(app: AppHandle, id: String, values: Value) -> Result<Value, String> {
    let saved = super::settings::save(&app, &id, &values).map_err(err)?;
    // 截图 / 录屏快捷键改了要重新注册
    if id == "recorder" {
        if let Err(e) = crate::recorder::screenshot::apply_hotkey(&app) {
            return Err(format!("设置已保存，但快捷键没有生效：{e:#}"));
        }
    }
    Ok(saved)
}

#[tauri::command]
pub fn plugin_versions(id: String) -> Result<Vec<Value>, String> {
    super::authoring::versions(&id).map_err(err)
}

/// 用户在界面里点回滚：本人操作，不再弹确认卡片。
#[tauri::command]
pub fn rollback_plugin(id: String, version: String) -> Result<Value, String> {
    super::authoring::rollback_direct(&id, &version).map_err(err)
}

#[tauri::command]
pub fn read_plugin_view(id: String, path: String) -> Result<Value, String> {
    let snapshot = super::snapshot();
    let plugin = snapshot.plugin(&id).ok_or("插件不存在")?;
    let file = super::resolve_inside(&plugin.dir, &path, false).map_err(err)?;
    let text = std::fs::read_to_string(&file).map_err(err)?;
    serde_json::from_str(&text).map_err(err)
}

#[tauri::command]
pub fn open_plugins_dir() -> Result<(), String> {
    let dir = super::host().dirs().user.ok_or("用户插件目录尚未初始化")?;
    std::fs::create_dir_all(&dir).map_err(err)?;
    std::process::Command::new("explorer")
        .arg(dir.display().to_string().replace('/', "\\"))
        .spawn()
        .map_err(|e| format!("打开文件夹失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn set_plugin_safe_mode(app: AppHandle, enabled: bool) -> Value {
    let state = app.state::<crate::AppState>();
    let overrides = super::settings::load_overrides(&state.db);
    if let Some(user) = super::host().dirs().user {
        super::host().configure(user, overrides, enabled);
    }
    super::snapshot().ui_manifest()
}

/// 前端主界面渲染成功后调用，清除启动标记（连续两次没走到这里则下次进入安全模式）。
#[tauri::command]
pub fn plugins_boot_ok(app: AppHandle) {
    if let Ok(dir) = app.path().app_data_dir() {
        super::boot::finish(&dir);
    }
}

/// 插件页面调用工具：只能调用本插件的工具，或 permissions 里声明的 tools:<name>。
#[tauri::command]
pub async fn plugin_call_tool(app: AppHandle, plugin_id: String, tool: String, args: Value) -> Result<Value, String> {
    let snapshot = super::snapshot();
    let plugin = snapshot.plugin(&plugin_id).filter(|p| p.enabled).ok_or("插件不存在或未启用")?.clone();
    let owns = snapshot.tool(&tool).is_some_and(|t| t.plugin == plugin_id);
    if !owns && !super::executors::may_call_tool(&plugin, &tool) {
        return Err(format!("插件 {plugin_id} 没有调用 {tool} 的权限（permissions 需包含 tools:{tool}）"));
    }
    let session_id = {
        let state = app.state::<crate::AppState>();
        state.db.current_session_id().unwrap_or(0)
    };
    let cancel = tokio_util::sync::CancellationToken::new();
    let args = if args.is_null() { json!({}) } else { args };
    crate::llm::tools::execute(&app, &tool, &args, &cancel, session_id).await.map_err(|e| format!("{e:#}"))
}

/// 打开插件声明的独立窗口（ui.windows 里的 route）。普通系统标题栏，便于用户关闭。
#[tauri::command]
pub fn open_plugin_window(app: AppHandle, plugin_id: String, route: String, query: Option<String>) -> Result<(), String> {
    let snapshot = super::snapshot();
    let plugin = snapshot.plugin(&plugin_id).filter(|p| p.enabled).ok_or("插件不存在或未启用")?;
    let item = plugin
        .manifest
        .ui
        .get("windows")
        .and_then(Value::as_array)
        .and_then(|items| items.iter().find(|w| w.get("route").and_then(Value::as_str) == Some(route.as_str())))
        .ok_or_else(|| format!("插件 {plugin_id} 没有窗口 {route}"))?;
    let safe: String = route.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let label = format!("plugin-{}-{safe}", plugin_id.replace('_', "-"));
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    let query = query.filter(|q| !q.is_empty()).map(|q| format!("?{q}")).unwrap_or_default();
    let url = tauri::WebviewUrl::App(format!("/#/{route}{query}").into());
    let title = item.get("title").and_then(Value::as_str).unwrap_or(&plugin.manifest.name).to_string();
    let width = item.get("width").and_then(Value::as_f64).unwrap_or(720.0);
    let height = item.get("height").and_then(Value::as_f64).unwrap_or(560.0);
    tauri::WebviewWindowBuilder::new(&app, label, url)
        .title(format!("拾光 · {title}"))
        .inner_size(width, height)
        .min_inner_size(320.0, 240.0)
        .resizable(true)
        .center()
        .build()
        .map_err(err)?;
    Ok(())
}
