//! 小程序（独立工具窗口）与桌面快捷启动：
//! - 任何启用插件的 ui.applets / ui.tabs / 用户 ui.windows 都可作为独立窗口打开，支持同时开多个不同工具或同一工具多开。
//! - 支持命令行启动参数：`shiguang.exe --open <target> [--query <k=v>] [--new]`，冷启动与单实例热启动均直达目标窗口。
//! - Windows 桌面 `.lnk` 快捷方式生成与清理，并自动生成专属 `.ico` 图标。

use crate::plugins::{self, Plugin};
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, LogicalPosition, Manager, WebviewUrl, WebviewWindowBuilder};

const SHORTCUTS_SETTING_KEY: &str = "applet_shortcuts";

/// 内置系统级/辅助窗口，不作为通用小程序列在桌面快捷方式列表里（除非显式声明 applet: true）。
const INTERNAL_WINDOWS: &[&str] = &[
    "floatball",
    "chat",
    "main",
    "reminder",
    "capture",
    "delegation",
    "agent-run",
    "reply-reader",
    "terminal",
    "sftp",
    "recorder-editor",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppletEntry {
    /// 唯一键：<plugin_id>:<slot>:<item_id>
    pub key: String,
    pub plugin_id: String,
    pub plugin_name: String,
    pub builtin: bool,
    /// applet | tab | window
    pub slot: String,
    pub item_id: String,
    pub title: String,
    pub description: String,
    pub icon: Option<String>,
    pub width: f64,
    pub height: f64,
    pub multi: bool,
    pub shortcut_path: Option<String>,
    pub shortcut_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortcutRecord {
    pub key: String,
    pub plugin_id: String,
    pub slot: String,
    pub item_id: String,
    #[serde(default)]
    pub query: Option<String>,
    pub name: String,
    pub lnk_path: String,
    pub icon_path: String,
    #[serde(default)]
    pub new_instance: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub target: String,
    pub query: Option<String>,
    pub new_instance: bool,
}

#[derive(Debug, Clone)]
pub struct AppletSpec {
    pub plugin_id: String,
    pub plugin_name: String,
    pub builtin: bool,
    pub slot: String,
    pub item_id: String,
    pub title: String,
    pub description: String,
    pub icon: Option<String>,
    pub width: f64,
    pub height: f64,
    pub multi: bool,
    pub plugin_dir: PathBuf,
}

impl AppletSpec {
    pub fn key(&self) -> String {
        format!("{}:{}:{}", self.plugin_id, self.slot, self.item_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DispatchTarget {
    Chat,
    Main(Option<String>),
    Applet {
        plugin_id: String,
        slot: Option<String>,
        item_id: String,
    },
}

/// 解析命令行启动参数（跳过 argv[0]）。
/// 支持：`--open <target>` / `--open=<target>`、`--query <q>` / `--query=<q>`、`--new` / `--multi`。
pub fn parse_launch_args<I, S>(args: I) -> Option<LaunchRequest>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let list: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    let mut target: Option<String> = None;
    let mut query: Option<String> = None;
    let mut new_instance = false;

    let mut i = 1; // 跳过可执行文件路径
    while i < list.len() {
        let arg = list[i].as_str();
        if let Some(rest) = arg.strip_prefix("--open=") {
            if !rest.trim().is_empty() {
                target = Some(rest.trim().to_string());
            }
        } else if arg == "--open" {
            if let Some(next) = list.get(i + 1) {
                if !next.trim().is_empty() {
                    target = Some(next.trim().to_string());
                }
                i += 1;
            }
        } else if let Some(rest) = arg.strip_prefix("--query=") {
            if !rest.trim().is_empty() {
                query = Some(rest.trim().to_string());
            }
        } else if arg == "--query" {
            if let Some(next) = list.get(i + 1) {
                if !next.trim().is_empty() {
                    query = Some(next.trim().to_string());
                }
                i += 1;
            }
        } else if arg == "--new" || arg == "--multi" {
            new_instance = true;
        }
        i += 1;
    }

    target.map(|t| LaunchRequest {
        target: t,
        query,
        new_instance,
    })
}

/// 异步派发启动请求（不阻塞主线程，冷启动可设置微小延迟等待事件循环就绪）。
pub fn dispatch_launch(app: AppHandle, req: LaunchRequest, cold_start: bool) {
    tauri::async_runtime::spawn(async move {
        if cold_start {
            tokio::time::sleep(std::time::Duration::from_millis(160)).await;
        }
        if let Err(e) = execute_launch(&app, &req).await {
            log::warn!("启动入口 --open {} 失败: {e:#}", req.target);
            crate::notify_user(&app, "拾光 · 启动失败", &format!("无法打开「{}」：{e}", req.target));
        }
    });
}

async fn execute_launch(app: &AppHandle, req: &LaunchRequest) -> Result<()> {
    match resolve_dispatch_target(&req.target)? {
        DispatchTarget::Chat => {
            crate::windows::show_chat(app);
            Ok(())
        }
        DispatchTarget::Main(tab) => {
            crate::windows::open_main(app);
            if let Some(tab_id) = tab {
                let _ = app.emit("main-switch-tab", tab_id);
            }
            Ok(())
        }
        DispatchTarget::Applet {
            plugin_id,
            slot,
            item_id,
        } => {
            open_applet_impl(
                app,
                &plugin_id,
                &item_id,
                slot.as_deref(),
                req.query.as_deref(),
                req.new_instance,
            )
            .await?;
            Ok(())
        }
    }
}

fn resolve_dispatch_target(raw: &str) -> Result<DispatchTarget> {
    let target = raw.trim();
    if target.is_empty() {
        bail!("启动目标不能为空");
    }
    if target.eq_ignore_ascii_case("chat") {
        return Ok(DispatchTarget::Chat);
    }
    if target.eq_ignore_ascii_case("main") {
        return Ok(DispatchTarget::Main(None));
    }
    if let Some(tab) = target.strip_prefix("main:") {
        return Ok(DispatchTarget::Main(Some(tab.trim().to_string())));
    }

    let parts: Vec<&str> = target.split(':').collect();
    match parts.as_slice() {
        [slot, plugin_id, item_id] if matches!(*slot, "applet" | "tab" | "window") => {
            Ok(DispatchTarget::Applet {
                plugin_id: (*plugin_id).to_string(),
                slot: Some((*slot).to_string()),
                item_id: (*item_id).to_string(),
            })
        }
        [plugin_id, slot, item_id] if matches!(*slot, "applet" | "tab" | "window") => {
            Ok(DispatchTarget::Applet {
                plugin_id: (*plugin_id).to_string(),
                slot: Some((*slot).to_string()),
                item_id: (*item_id).to_string(),
            })
        }
        [plugin_id, item_id] => Ok(DispatchTarget::Applet {
            plugin_id: (*plugin_id).to_string(),
            slot: None,
            item_id: (*item_id).to_string(),
        }),
        [single] => {
            let specs = collect_applets();
            if let Some(found) = specs
                .iter()
                .find(|s| s.plugin_id == *single)
                .or_else(|| specs.iter().find(|s| s.item_id == *single))
            {
                Ok(DispatchTarget::Applet {
                    plugin_id: found.plugin_id.clone(),
                    slot: Some(found.slot.clone()),
                    item_id: found.item_id.clone(),
                })
            } else {
                bail!("未找到对应的小程序或功能：{single}")
            }
        }
        _ => bail!("不支持的启动目标格式：{target}"),
    }
}

fn is_internal_window(route: &str) -> bool {
    INTERNAL_WINDOWS.contains(&route)
}

fn extract_items(plugin: &Plugin, slot: &str, out: &mut Vec<AppletSpec>) {
    let key = match slot {
        "applet" => "applets",
        "tab" => "tabs",
        "window" => "windows",
        _ => return,
    };
    let Some(items) = plugin.manifest.ui.get(key).and_then(Value::as_array) else {
        return;
    };
    for item in items {
        let has_renderable = item.get("component").is_some()
            || item.get("entry").is_some()
            || item.get("view").is_some();
        if !has_renderable {
            continue;
        }
        let item_id = if slot == "window" {
            item.get("route")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
        } else {
            item.get("id")
                .or_else(|| item.get("route"))
                .and_then(Value::as_str)
        };
        let Some(item_id) = item_id.filter(|s| !s.is_empty()) else {
            continue;
        };
        if slot == "window"
            && plugin.builtin
            && is_internal_window(item_id)
            && item.get("applet").and_then(Value::as_bool) != Some(true)
        {
            continue;
        }
        let title = item
            .get("title")
            .or_else(|| item.get("name"))
            .or_else(|| item.get("label"))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&plugin.manifest.name)
            .to_string();
        let description = item
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or(&plugin.manifest.description)
            .to_string();
        let icon = item
            .get("icon")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let default_w = if slot == "window" { 780.0 } else { 940.0 };
        let default_h = if slot == "window" { 580.0 } else { 660.0 };
        let width = item
            .get("width")
            .and_then(Value::as_f64)
            .unwrap_or(default_w)
            .clamp(380.0, 1800.0);
        let height = item
            .get("height")
            .and_then(Value::as_f64)
            .unwrap_or(default_h)
            .clamp(300.0, 1200.0);
        let multi = item.get("multi").and_then(Value::as_bool).unwrap_or(false);

        out.push(AppletSpec {
            plugin_id: plugin.manifest.id.clone(),
            plugin_name: plugin.manifest.name.clone(),
            builtin: plugin.builtin,
            slot: slot.to_string(),
            item_id: item_id.to_string(),
            title,
            description,
            icon,
            width,
            height,
            multi,
            plugin_dir: plugin.dir.clone(),
        });
    }
}

/// 收集所有启用插件里可作为独立小程序打开的入口。
pub fn collect_applets() -> Vec<AppletSpec> {
    let snapshot = plugins::snapshot();
    let mut out = Vec::new();
    for plugin in snapshot.enabled_plugins() {
        extract_items(plugin, "applet", &mut out);
        extract_items(plugin, "tab", &mut out);
        extract_items(plugin, "window", &mut out);
    }
    out
}

pub fn resolve_applet(plugin_id: &str, item_id: &str, slot: Option<&str>) -> Result<AppletSpec> {
    let specs = collect_applets();
    if let Some(slot) = slot.filter(|s| !s.is_empty()) {
        if let Some(found) = specs
            .iter()
            .find(|s| s.plugin_id == plugin_id && s.slot == slot && s.item_id == item_id)
        {
            return Ok(found.clone());
        }
    }
    // 兼容从 tab 迁移到 applet 的已有快捷方式（如 --open servers:tab:servers）
    for prefer in ["applet", "tab", "window"] {
        if let Some(found) = specs
            .iter()
            .find(|s| s.plugin_id == plugin_id && s.slot == prefer && s.item_id == item_id)
        {
            return Ok(found.clone());
        }
    }
    bail!("未找到插件 {plugin_id} 的入口「{item_id}」（插件可能已停用）")
}

fn safe_slug(s: &str, max_len: usize) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if (ch == '-' || ch == '_') && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= max_len {
            break;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "x".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 以独立小程序窗口打开指定功能/插件。支持同时打开多个不同工具，也支持同一工具多开（new_instance = true）。
pub async fn open_applet_impl(
    app: &AppHandle,
    plugin_id: &str,
    item_id: &str,
    slot: Option<&str>,
    query: Option<&str>,
    new_instance: bool,
) -> Result<String> {
    let spec = resolve_applet(plugin_id, item_id, slot)?;
    let clean_query = query
        .map(|q| q.trim().trim_start_matches('?'))
        .filter(|q| !q.is_empty());

    let mut base_label = format!(
        "applet-{}-{}",
        safe_slug(&spec.plugin_id, 24),
        safe_slug(&spec.item_id, 24)
    );
    if spec.multi {
        if let Some(q) = clean_query {
            base_label.push('-');
            base_label.push_str(&safe_slug(q, 24));
        }
    }

    let mut clone_index: usize = 0;
    let label = if new_instance {
        if app.get_webview_window(&base_label).is_none() {
            base_label
        } else {
            let mut chosen = None;
            for n in 2..=99 {
                let cand = format!("{base_label}-{n}");
                if app.get_webview_window(&cand).is_none() {
                    clone_index = n - 1;
                    chosen = Some(cand);
                    break;
                }
            }
            chosen.ok_or_else(|| anyhow!("同一工具打开的窗口过多，请先关闭部分窗口"))?
        }
    } else {
        if let Some(existing) = app.get_webview_window(&base_label) {
            let _ = existing.show();
            let _ = existing.unminimize();
            let _ = existing.set_focus();
            return Ok(base_label);
        }
        base_label
    };

    let mut qs = url::form_urlencoded::Serializer::new(String::new());
    qs.append_pair("plugin", &spec.plugin_id);
    qs.append_pair("slot", &spec.slot);
    qs.append_pair("id", &spec.item_id);
    let mut query_str = qs.finish();
    if let Some(extra) = clean_query {
        query_str.push('&');
        query_str.push_str(extra);
    }

    let url = WebviewUrl::App(format!("/#/applet?{query_str}").into());
    let title = if clone_index > 0 {
        format!("{} ({}) · 拾光", spec.title, clone_index + 1)
    } else {
        format!("{} · 拾光", spec.title)
    };

    let mut builder = WebviewWindowBuilder::new(app, &label, url)
        .title(&title)
        .inner_size(spec.width, spec.height)
        .min_inner_size(420.0, 320.0)
        .resizable(true)
        .center();

    if let Ok((rgba, w, h)) = render_applet_icon_rgba(&spec, 64) {
        builder = builder.icon(tauri::image::Image::new_owned(rgba, w, h))?;
    }

    let window = builder.build().context("创建小程序窗口失败")?;
    if clone_index > 0 {
        if let Ok(pos) = window.outer_position() {
            let scale = window.scale_factor().unwrap_or(1.0);
            let lp: LogicalPosition<f64> = pos.to_logical(scale);
            let offset = (clone_index.min(8) as f64) * 28.0;
            let _ = window.set_position(LogicalPosition::new(lp.x + offset, lp.y + offset));
        }
    }
    let _ = window.set_focus();
    Ok(label)
}

// ---------- 持久化快捷方式记录 ----------

fn load_shortcut_records(app: &AppHandle) -> Vec<ShortcutRecord> {
    let state = app.state::<crate::AppState>();
    state
        .db
        .get_setting(SHORTCUTS_SETTING_KEY)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Vec<ShortcutRecord>>(&raw).ok())
        .unwrap_or_default()
}

fn save_shortcut_records(app: &AppHandle, records: &[ShortcutRecord]) -> Result<()> {
    let state = app.state::<crate::AppState>();
    let json = serde_json::to_string(records)?;
    state.db.set_setting(SHORTCUTS_SETTING_KEY, &json)?;
    Ok(())
}

fn desktop_dir() -> Result<PathBuf> {
    dirs::desktop_dir().ok_or_else(|| anyhow!("无法定位桌面目录"))
}

fn sanitize_shortcut_name(raw: &str, fallback: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') && !c.is_control())
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.');
    let without_ext = trimmed
        .strip_suffix(".lnk")
        .or_else(|| trimmed.strip_suffix(".LNK"))
        .unwrap_or(trimmed)
        .trim();
    if without_ext.is_empty() {
        fallback.to_string()
    } else {
        without_ext.to_string()
    }
}

pub fn build_launch_cli_args(
    spec: &AppletSpec,
    query: Option<&str>,
    new_instance: bool,
) -> String {
    let mut parts = vec![format!("--open {}", spec.key())];
    if let Some(q) = query.map(str::trim).filter(|q| !q.is_empty()) {
        let escaped = q.replace('"', "\\\"");
        parts.push(format!("--query \"{escaped}\""));
    }
    if new_instance {
        parts.push("--new".to_string());
    }
    parts.join(" ")
}

fn encode_powershell_utf16_b64(script: &str) -> String {
    let bytes: Vec<u8> = script
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(windows)]
async fn write_windows_lnk(
    lnk_path: &Path,
    target_exe: &Path,
    arguments: &str,
    working_dir: &Path,
    icon_path: &Path,
    description: &str,
) -> Result<()> {
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ws = New-Object -ComObject WScript.Shell
$sc = $ws.CreateShortcut($env:SG_LNK_PATH)
$sc.TargetPath = $env:SG_TARGET
$sc.Arguments = $env:SG_ARGS
$sc.WorkingDirectory = $env:SG_WORKDIR
if ($env:SG_ICON) { $sc.IconLocation = "$($env:SG_ICON),0" }
if ($env:SG_DESC) { $sc.Description = $env:SG_DESC }
$sc.Save()
"#;

    let encoded = encode_powershell_utf16_b64(SCRIPT);
    let mut cmd = tokio::process::Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-ExecutionPolicy",
        "Bypass",
        "-EncodedCommand",
        &encoded,
    ])
    .env("SG_LNK_PATH", lnk_path)
    .env("SG_TARGET", target_exe)
    .env("SG_ARGS", arguments)
    .env("SG_WORKDIR", working_dir)
    .env("SG_ICON", icon_path)
    .env("SG_DESC", description)
    .creation_flags(crate::tasks::CREATE_NO_WINDOW);

    let out = cmd.output().await.context("调用 PowerShell 创建快捷方式失败")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("创建桌面快捷方式失败：{}", stderr.trim());
    }
    Ok(())
}

#[cfg(not(windows))]
async fn write_windows_lnk(
    _lnk_path: &Path,
    _target_exe: &Path,
    _arguments: &str,
    _working_dir: &Path,
    _icon_path: &Path,
    _description: &str,
) -> Result<()> {
    bail!("桌面快捷方式目前仅支持 Windows")
}

/// 在桌面创建（或更新）某个小程序的快捷方式，并生成专属 `.ico` 图标。
pub async fn create_desktop_shortcut_impl(
    app: &AppHandle,
    plugin_id: &str,
    item_id: &str,
    slot: Option<&str>,
    name: Option<&str>,
    query: Option<&str>,
    new_instance: bool,
) -> Result<ShortcutRecord> {
    let spec = resolve_applet(plugin_id, item_id, slot)?;
    let app_dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."));
    let icon_path = ensure_applet_ico(&app_dir, &spec)?;
    let target_exe = std::env::current_exe().context("无法获取当前程序路径")?;
    let working_dir = target_exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let display_name = sanitize_shortcut_name(name.unwrap_or(&spec.title), &spec.title);
    let desktop = desktop_dir()?;
    std::fs::create_dir_all(&desktop)?;
    let lnk_path = desktop.join(format!("{display_name}.lnk"));
    let args = build_launch_cli_args(&spec, query, new_instance);
    let desc = if spec.description.is_empty() {
        format!("{} · 拾光小程序", spec.title)
    } else {
        format!("{} - {}", spec.title, spec.description)
    };

    write_windows_lnk(&lnk_path, &target_exe, &args, &working_dir, &icon_path, &desc).await?;

    let record = ShortcutRecord {
        key: spec.key(),
        plugin_id: spec.plugin_id.clone(),
        slot: spec.slot.clone(),
        item_id: spec.item_id.clone(),
        query: query.map(str::trim).filter(|q| !q.is_empty()).map(str::to_string),
        name: display_name,
        lnk_path: lnk_path.display().to_string(),
        icon_path: icon_path.display().to_string(),
        new_instance,
    };

    let mut records = load_shortcut_records(app);
    // 若同一 key 之前用了别的名字，清理旧记录（不删用户可能改名保留的其它文件，仅替换记录）
    records.retain(|r| r.key != record.key);
    records.push(record.clone());
    save_shortcut_records(app, &records)?;
    let _ = app.emit("applets-changed", ());
    Ok(record)
}

/// 移除由拾光创建的桌面快捷方式。
pub fn remove_desktop_shortcut_impl(app: &AppHandle, key: &str) -> Result<bool> {
    let mut records = load_shortcut_records(app);
    let Some(pos) = records.iter().position(|r| r.key == key) else {
        return Ok(false);
    };
    let rec = records.remove(pos);
    let lnk = PathBuf::from(&rec.lnk_path);
    if lnk.is_file() {
        let is_lnk = lnk
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("lnk"))
            .unwrap_or(false);
        let in_desktop = desktop_dir()
            .ok()
            .map(|d| lnk.starts_with(&d))
            .unwrap_or(false);
        if is_lnk && in_desktop {
            let _ = std::fs::remove_file(&lnk);
        }
    }
    save_shortcut_records(app, &records)?;
    let _ = app.emit("applets-changed", ());
    Ok(true)
}

pub fn list_applets_impl(app: &AppHandle) -> Vec<AppletEntry> {
    let specs = collect_applets();
    let records = load_shortcut_records(app);
    specs
        .into_iter()
        .map(|s| {
            let key = s.key();
            let rec = records.iter().find(|r| {
                (r.key == key || (r.plugin_id == s.plugin_id && r.item_id == s.item_id))
                    && Path::new(&r.lnk_path).is_file()
            });
            AppletEntry {
                key,
                plugin_id: s.plugin_id,
                plugin_name: s.plugin_name,
                builtin: s.builtin,
                slot: s.slot,
                item_id: s.item_id,
                title: s.title,
                description: s.description,
                icon: s.icon,
                width: s.width,
                height: s.height,
                multi: s.multi,
                shortcut_path: rec.map(|r| r.lnk_path.clone()),
                shortcut_name: rec.map(|r| r.name.clone()),
            }
        })
        .collect()
}

// ---------- 专属图标生成（PNG-in-ICO + 窗口 RGBA） ----------

fn resolve_custom_icon_image(spec: &AppletSpec) -> Option<PathBuf> {
    if let Some(icon) = &spec.icon {
        if icon.contains('.') || icon.contains('/') {
            if let Ok(path) = plugins::resolve_inside(&spec.plugin_dir, icon, spec.builtin) {
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    for candidate in ["icon.png", "assets/icon.png"] {
        if let Ok(path) = plugins::resolve_inside(&spec.plugin_dir, candidate, spec.builtin) {
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn palette_for_icon(icon_key: &str, plugin_id: &str) -> ([u8; 3], [u8; 3]) {
    match icon_key {
        "server" => ([37, 99, 235], [30, 58, 138]),      // 靛蓝
        "todo" => ([16, 185, 129], [4, 120, 87]),        // 翡翠绿
        "video" => ([244, 63, 94], [190, 18, 60]),       // 玫红
        "files" => ([14, 165, 233], [3, 105, 161]),      // 天蓝
        "skill" => ([139, 92, 246], [91, 33, 182]),      // 紫罗兰
        "usage" => ([245, 158, 11], [180, 83, 9]),       // 琥珀橙
        "settings" => ([71, 85, 105], [30, 41, 59]),     // 岩石灰
        "plug" => ([217, 119, 6], [146, 64, 14]),        // 暖金
        "ai" => ([6, 182, 212], [55, 48, 163]),          // 青靛
        "history" => ([99, 102, 241], [49, 46, 129]),    // 蓝紫
        _ => {
            const PRESETS: &[([u8; 3], [u8; 3])] = &[
                ([232, 162, 58], [180, 96, 26]),
                ([20, 184, 166], [15, 118, 110]),
                ([59, 130, 246], [29, 78, 216]),
                ([168, 85, 247], [107, 33, 168]),
                ([236, 72, 153], [157, 23, 77]),
                ([234, 88, 12], [154, 52, 18]),
            ];
            let hash = plugin_id
                .bytes()
                .fold(0usize, |acc, b| acc.wrapping_mul(31).wrapping_add(b as usize));
            PRESETS[hash % PRESETS.len()]
        }
    }
}

fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w * 0.5).min(h * 0.5);
    let k = r * 0.5522848;
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

fn draw_vector_glyph(pm: &mut tiny_skia::Pixmap, icon_key: &str, size: f32) {
    let mut fill = tiny_skia::Paint::default();
    fill.set_color_rgba8(255, 255, 255, 245);
    fill.anti_alias = true;

    let mut accent = tiny_skia::Paint::default();
    accent.set_color_rgba8(255, 255, 255, 155);
    accent.anti_alias = true;

    let mut stroke_paint = tiny_skia::Paint::default();
    stroke_paint.set_color_rgba8(255, 255, 255, 245);
    stroke_paint.anti_alias = true;

    let stroke = tiny_skia::Stroke {
        width: (size * 0.068).max(1.5),
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        ..Default::default()
    };

    let s = |v: f32| v * size;
    let xform = tiny_skia::Transform::identity();

    match icon_key {
        "server" => {
            // 双层机架 + 指示灯
            for y in [0.24, 0.54] {
                if let Some(p) = rounded_rect_path(s(0.21), s(y), s(0.58), s(0.22), s(0.055)) {
                    pm.stroke_path(&p, &stroke_paint, &stroke, xform, None);
                }
                if let Some(dot) = tiny_skia::PathBuilder::from_circle(s(0.32), s(y + 0.11), s(0.036)) {
                    pm.fill_path(&dot, &fill, tiny_skia::FillRule::Winding, xform, None);
                }
                if let Some(bar) = rounded_rect_path(s(0.43), s(y + 0.088), s(0.25), s(0.044), s(0.022)) {
                    pm.fill_path(&bar, &accent, tiny_skia::FillRule::Winding, xform, None);
                }
            }
        }
        "todo" => {
            for y in [0.30, 0.50, 0.70] {
                if let Some(dot) = tiny_skia::PathBuilder::from_circle(s(0.27), s(y), s(0.042)) {
                    pm.fill_path(&dot, &fill, tiny_skia::FillRule::Winding, xform, None);
                }
                let mut pb = tiny_skia::PathBuilder::new();
                pb.move_to(s(0.39), s(y));
                pb.line_to(s(0.75), s(y));
                if let Some(line) = pb.finish() {
                    pm.stroke_path(&line, &stroke_paint, &stroke, xform, None);
                }
            }
        }
        "video" => {
            if let Some(body) = rounded_rect_path(s(0.19), s(0.30), s(0.42), s(0.40), s(0.07)) {
                pm.fill_path(&body, &fill, tiny_skia::FillRule::Winding, xform, None);
            }
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(s(0.64), s(0.43));
            pb.line_to(s(0.81), s(0.33));
            pb.line_to(s(0.81), s(0.67));
            pb.line_to(s(0.64), s(0.57));
            pb.close();
            if let Some(lens) = pb.finish() {
                pm.fill_path(&lens, &accent, tiny_skia::FillRule::Winding, xform, None);
            }
        }
        "files" => {
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(s(0.20), s(0.30));
            pb.line_to(s(0.43), s(0.30));
            pb.line_to(s(0.50), s(0.38));
            pb.line_to(s(0.80), s(0.38));
            pb.line_to(s(0.80), s(0.72));
            pb.line_to(s(0.20), s(0.72));
            pb.close();
            if let Some(folder) = pb.finish() {
                pm.stroke_path(&folder, &stroke_paint, &stroke, xform, None);
            }
        }
        "skill" => {
            // 叠层菱形
            let mut top = tiny_skia::PathBuilder::new();
            top.move_to(s(0.50), s(0.22));
            top.line_to(s(0.78), s(0.38));
            top.line_to(s(0.50), s(0.54));
            top.line_to(s(0.22), s(0.38));
            top.close();
            if let Some(p) = top.finish() {
                pm.fill_path(&p, &fill, tiny_skia::FillRule::Winding, xform, None);
            }
            for dy in [0.12, 0.24] {
                let mut layer = tiny_skia::PathBuilder::new();
                layer.move_to(s(0.24), s(0.38 + dy));
                layer.line_to(s(0.50), s(0.54 + dy));
                layer.line_to(s(0.76), s(0.38 + dy));
                if let Some(p) = layer.finish() {
                    pm.stroke_path(&p, &stroke_paint, &stroke, xform, None);
                }
            }
        }
        "usage" => {
            let cols = [(0.24, 0.48, 0.28), (0.43, 0.26, 0.50), (0.62, 0.38, 0.38)];
            for (x, y, h) in cols {
                if let Some(p) = rounded_rect_path(s(x), s(y), s(0.14), s(h), s(0.04)) {
                    pm.fill_path(&p, &fill, tiny_skia::FillRule::Winding, xform, None);
                }
            }
        }
        "settings" => {
            for (y, knob_x) in [(0.36, 0.38), (0.64, 0.62)] {
                let mut pb = tiny_skia::PathBuilder::new();
                pb.move_to(s(0.22), s(y));
                pb.line_to(s(0.78), s(y));
                if let Some(line) = pb.finish() {
                    pm.stroke_path(&line, &accent, &stroke, xform, None);
                }
                if let Some(knob) = tiny_skia::PathBuilder::from_circle(s(knob_x), s(y), s(0.085)) {
                    pm.fill_path(&knob, &fill, tiny_skia::FillRule::Winding, xform, None);
                }
            }
        }
        "plug" => {
            let mut prongs = tiny_skia::PathBuilder::new();
            prongs.move_to(s(0.40), s(0.22));
            prongs.line_to(s(0.40), s(0.39));
            prongs.move_to(s(0.60), s(0.22));
            prongs.line_to(s(0.60), s(0.39));
            prongs.move_to(s(0.50), s(0.63));
            prongs.line_to(s(0.50), s(0.79));
            if let Some(p) = prongs.finish() {
                pm.stroke_path(&p, &stroke_paint, &stroke, xform, None);
            }
            if let Some(body) = rounded_rect_path(s(0.30), s(0.39), s(0.40), s(0.24), s(0.10)) {
                pm.fill_path(&body, &fill, tiny_skia::FillRule::Winding, xform, None);
            }
        }
        _ => {
            // 通用星芒（适合自定义小程序）
            let mut star = tiny_skia::PathBuilder::new();
            star.move_to(s(0.50), s(0.20));
            star.line_to(s(0.58), s(0.42));
            star.line_to(s(0.80), s(0.50));
            star.line_to(s(0.58), s(0.58));
            star.line_to(s(0.50), s(0.80));
            star.line_to(s(0.42), s(0.58));
            star.line_to(s(0.20), s(0.50));
            star.line_to(s(0.42), s(0.42));
            star.close();
            if let Some(p) = star.finish() {
                pm.fill_path(&p, &fill, tiny_skia::FillRule::Winding, xform, None);
            }
        }
    }
}

fn render_procedural_pixmap(spec: &AppletSpec, size: u32) -> Result<tiny_skia::Pixmap> {
    let mut pm = tiny_skia::Pixmap::new(size, size).ok_or_else(|| anyhow!("创建图标画布失败"))?;
    let icon_key = spec.icon.as_deref().unwrap_or("");
    let (c1, c2) = palette_for_icon(icon_key, &spec.plugin_id);

    let sz = size as f32;
    let margin = (sz * 0.04).max(1.0);
    let inner = sz - margin * 2.0;
    let radius = inner * 0.23;

    if let Some(bg) = rounded_rect_path(margin, margin, inner, inner, radius) {
        if let Some(shader) = tiny_skia::LinearGradient::new(
            tiny_skia::Point::from_xy(margin, margin),
            tiny_skia::Point::from_xy(sz - margin, sz - margin),
            vec![
                tiny_skia::GradientStop::new(0.0, tiny_skia::Color::from_rgba8(c1[0], c1[1], c1[2], 255)),
                tiny_skia::GradientStop::new(1.0, tiny_skia::Color::from_rgba8(c2[0], c2[1], c2[2], 255)),
            ],
            tiny_skia::SpreadMode::Pad,
            tiny_skia::Transform::identity(),
        ) {
            let mut paint = tiny_skia::Paint {
                shader,
                anti_alias: true,
                ..Default::default()
            };
            pm.fill_path(&bg, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
            // 柔和内描边提升精致感
            paint.shader = tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(255, 255, 255, 55));
            let ring = tiny_skia::Stroke {
                width: (sz * 0.02).max(1.0),
                ..Default::default()
            };
            pm.stroke_path(&bg, &paint, &ring, tiny_skia::Transform::identity(), None);
        }
    }

    draw_vector_glyph(&mut pm, icon_key, sz);
    Ok(pm)
}

/// 将 premultiplied RGBA (tiny-skia) 转成直通 RGBA（image / Tauri Window Icon 使用）。
fn unpremultiply_rgba(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for px in data.chunks_exact(4) {
        let a = px[3];
        if a == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
        } else if a == 255 {
            out.extend_from_slice(px);
        } else {
            let af = a as f32 / 255.0;
            out.push(((px[0] as f32 / af).min(255.0)) as u8);
            out.push(((px[1] as f32 / af).min(255.0)) as u8);
            out.push(((px[2] as f32 / af).min(255.0)) as u8);
            out.push(a);
        }
    }
    out
}

pub fn render_applet_icon_rgba(spec: &AppletSpec, size: u32) -> Result<(Vec<u8>, u32, u32)> {
    if let Some(custom) = resolve_custom_icon_image(spec) {
        if let Ok(img) = image::open(&custom) {
            let resized = img
                .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
                .to_rgba8();
            return Ok((resized.into_raw(), size, size));
        }
    }
    let pm = render_procedural_pixmap(spec, size)?;
    Ok((unpremultiply_rgba(pm.data()), size, size))
}

fn encode_png_rgba(rgba: &[u8], size: u32) -> Result<Vec<u8>> {
    use image::ImageEncoder;
    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    encoder.write_image(rgba, size, size, image::ExtendedColorType::Rgba8)?;
    Ok(buf)
}

/// 将多尺寸 PNG 帧打包为标准 Windows ICO（支持 256 / 64 / 48 / 32）。
pub fn pack_png_ico(frames: &[(u32, Vec<u8>)]) -> Result<Vec<u8>> {
    if frames.is_empty() {
        bail!("ICO 至少需要一帧图像");
    }
    let count = frames.len() as u16;
    let header_size = 6usize + 16usize * frames.len();
    let mut out = Vec::with_capacity(header_size + frames.iter().map(|(_, b)| b.len()).sum::<usize>());

    // ICONDIR
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type = ICO
    out.extend_from_slice(&count.to_le_bytes());

    let mut offset = header_size as u32;
    for (size, png) in frames {
        let dim = if *size >= 256 { 0u8 } else { *size as u8 };
        out.push(dim); // bWidth
        out.push(dim); // bHeight
        out.push(0);   // bColorCount
        out.push(0);   // bReserved
        out.extend_from_slice(&1u16.to_le_bytes());  // wPlanes
        out.extend_from_slice(&32u16.to_le_bytes()); // wBitCount
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in frames {
        out.extend_from_slice(png);
    }
    Ok(out)
}

pub fn ensure_applet_ico(app_dir: &Path, spec: &AppletSpec) -> Result<PathBuf> {
    let dir = app_dir.join("applet-icons");
    std::fs::create_dir_all(&dir)?;
    let filename = format!(
        "{}-{}-{}.ico",
        safe_slug(&spec.plugin_id, 24),
        safe_slug(&spec.slot, 12),
        safe_slug(&spec.item_id, 24)
    );
    let path = dir.join(filename);
    let mut frames = Vec::new();
    for size in [256u32, 64, 48, 32] {
        let (rgba, _, _) = render_applet_icon_rgba(spec, size)?;
        let png = encode_png_rgba(&rgba, size)?;
        frames.push((size, png));
    }
    let ico = pack_png_ico(&frames)?;
    std::fs::write(&path, ico)?;
    Ok(path)
}

// ---------- Tauri IPC Commands ----------

#[tauri::command]
pub fn list_applets(app: AppHandle) -> Vec<AppletEntry> {
    list_applets_impl(&app)
}

#[tauri::command]
pub async fn open_applet_window(
    app: AppHandle,
    plugin_id: String,
    item_id: String,
    slot: Option<String>,
    query: Option<String>,
    new_instance: Option<bool>,
) -> std::result::Result<String, String> {
    open_applet_impl(
        &app,
        &plugin_id,
        &item_id,
        slot.as_deref(),
        query.as_deref(),
        new_instance.unwrap_or(false),
    )
    .await
    .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn create_applet_shortcut(
    app: AppHandle,
    plugin_id: String,
    item_id: String,
    slot: Option<String>,
    name: Option<String>,
    query: Option<String>,
    new_instance: Option<bool>,
) -> std::result::Result<ShortcutRecord, String> {
    create_desktop_shortcut_impl(
        &app,
        &plugin_id,
        &item_id,
        slot.as_deref(),
        name.as_deref(),
        query.as_deref(),
        new_instance.unwrap_or(false),
    )
    .await
    .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn remove_applet_shortcut(app: AppHandle, key: String) -> std::result::Result<bool, String> {
    remove_desktop_shortcut_impl(&app, &key).map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_launch_args_variants() {
        assert_eq!(parse_launch_args(["shiguang.exe"]), None);
        assert_eq!(
            parse_launch_args(["shiguang.exe", "--open", "servers:tab:servers"]),
            Some(LaunchRequest {
                target: "servers:tab:servers".into(),
                query: None,
                new_instance: false,
            })
        );
        assert_eq!(
            parse_launch_args([
                "shiguang.exe",
                "--open=todos:todos",
                "--query=filter=today",
                "--new",
            ]),
            Some(LaunchRequest {
                target: "todos:todos".into(),
                query: Some("filter=today".into()),
                new_instance: true,
            })
        );
    }

    #[test]
    fn resolves_dispatch_target_formats() {
        assert_eq!(resolve_dispatch_target("chat").unwrap(), DispatchTarget::Chat);
        assert_eq!(resolve_dispatch_target("main").unwrap(), DispatchTarget::Main(None));
        assert_eq!(
            resolve_dispatch_target("main:servers").unwrap(),
            DispatchTarget::Main(Some("servers".into()))
        );
        assert_eq!(
            resolve_dispatch_target("servers:tab:servers").unwrap(),
            DispatchTarget::Applet {
                plugin_id: "servers".into(),
                slot: Some("tab".into()),
                item_id: "servers".into(),
            }
        );
        assert_eq!(
            resolve_dispatch_target("tab:servers:servers").unwrap(),
            DispatchTarget::Applet {
                plugin_id: "servers".into(),
                slot: Some("tab".into()),
                item_id: "servers".into(),
            }
        );
        assert_eq!(
            resolve_dispatch_target("servers:servers").unwrap(),
            DispatchTarget::Applet {
                plugin_id: "servers".into(),
                slot: None,
                item_id: "servers".into(),
            }
        );
    }

    #[test]
    fn generates_valid_multi_size_ico_and_sanitizes_names() {
        let spec = AppletSpec {
            plugin_id: "servers".into(),
            plugin_name: "服务器".into(),
            builtin: true,
            slot: "tab".into(),
            item_id: "servers".into(),
            title: "服务器".into(),
            description: "SSH/SFTP".into(),
            icon: Some("server".into()),
            width: 940.0,
            height: 660.0,
            multi: false,
            plugin_dir: PathBuf::from("."),
        };
        let tmp = std::env::temp_dir().join(format!("sg-applet-test-{}", uuid::Uuid::new_v4()));
        let ico_path = ensure_applet_ico(&tmp, &spec).unwrap();
        let bytes = std::fs::read(&ico_path).unwrap();
        // ICONDIR: reserved=0, type=1, count=4
        assert_eq!(&bytes[0..6], &[0, 0, 1, 0, 4, 0]);
        assert!(bytes.len() > 200);
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(sanitize_shortcut_name("服务器:管理?.lnk", "默认"), "服务器管理");
        assert_eq!(sanitize_shortcut_name("   ", "默认"), "默认");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn creates_windows_lnk_in_temp_dir() {
        let tmp = std::env::temp_dir().join(format!("sg-lnk-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let spec = AppletSpec {
            plugin_id: "servers".into(),
            plugin_name: "服务器".into(),
            builtin: true,
            slot: "tab".into(),
            item_id: "servers".into(),
            title: "服务器管理".into(),
            description: "SSH/SFTP 连接管理".into(),
            icon: Some("server".into()),
            width: 940.0,
            height: 660.0,
            multi: false,
            plugin_dir: PathBuf::from("."),
        };
        let icon_path = ensure_applet_ico(&tmp, &spec).unwrap();
        let lnk_path = tmp.join("服务器管理.lnk");
        let exe = std::env::current_exe().unwrap();
        let args = build_launch_cli_args(&spec, None, false);
        write_windows_lnk(
            &lnk_path,
            &exe,
            &args,
            exe.parent().unwrap_or(Path::new(".")),
            &icon_path,
            "服务器管理 · 拾光小程序",
        )
        .await
        .unwrap();
        let meta = std::fs::metadata(&lnk_path).unwrap();
        assert!(meta.len() > 100);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
