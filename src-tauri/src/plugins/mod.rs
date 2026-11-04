//! 插件宿主：发现、加载、热重载插件，向内核提供工具定义、类别、Skills、资源、提示词。
//!
//! - 内置插件：随程序发布（`<exe 目录>/plugins`；调试构建直接读 `src-tauri/plugins`），只读。
//! - 用户插件：`app_data/plugins/<id>`，拾光和用户可以新增修改；不能占用内置插件的 id。
//!
//! 快照（Snapshot）在加载时一次性生成，之后只读；文件变化时整体替换，读取方拿到的
//! 始终是一致的版本。工具与插件都按确定顺序排列，保证提示词前缀缓存稳定。
pub mod authoring;
pub mod boot;
pub mod executors;
pub mod hooks;
pub mod ipc;
pub mod manifest;
pub mod mcp;
pub mod protocol;
pub mod script;
pub mod settings;
pub mod watch;

use anyhow::{anyhow, bail, Context, Result};
use manifest::{Manifest, ToolFileEntry};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

pub const PLUGIN_FILE: &str = "plugin.json";

#[derive(Debug, Clone)]
pub struct Plugin {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub builtin: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub plugin: String,
    pub category: String,
    /// OpenAI function 格式：{"type":"function","function":{name,description,parameters}}
    pub definition: Value,
    pub executor: Value,
    pub when: Option<String>,
    pub label: Option<String>,
}

impl ToolSpec {
    pub fn executor_kind(&self) -> &str {
        self.executor.get("kind").and_then(Value::as_str).unwrap_or("builtin")
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PluginError {
    pub plugin: String,
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Category {
    pub id: String,
    pub label: String,
    pub description: String,
    pub plugin: String,
    pub tools: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Snapshot {
    pub plugins: Vec<Plugin>,
    pub tools: Vec<ToolSpec>,
    tool_index: HashMap<String, usize>,
    pub categories: Vec<Category>,
    pub core_tools: Vec<String>,
    retired: HashMap<String, String>,
    pub errors: Vec<PluginError>,
    pub generation: u64,
}

impl Snapshot {
    pub fn tool(&self, name: &str) -> Option<&ToolSpec> {
        self.tool_index.get(name).map(|&i| &self.tools[i])
    }

    pub fn plugin(&self, id: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.manifest.id == id)
    }

    pub fn enabled_plugins(&self) -> impl Iterator<Item = &Plugin> {
        self.plugins.iter().filter(|p| p.enabled)
    }

    /// 全部工具定义（OpenAI function 格式），按加载顺序。
    pub fn definitions(&self) -> Vec<Value> {
        self.tools.iter().map(|t| t.definition.clone()).collect()
    }

    pub fn retired_message(&self, name: &str) -> Option<&str> {
        self.retired.get(name).map(String::as_str)
    }

    pub fn category(&self, id: &str) -> Option<&Category> {
        self.categories.iter().find(|c| c.id == id)
    }

    pub fn tools_for_category(&self, id: &str) -> Vec<String> {
        self.category(id).map(|c| c.tools.clone()).unwrap_or_default()
    }

    /// 工具的主类别；核心工具为 core。
    pub fn category_for_tool(&self, name: &str) -> String {
        if self.core_tools.iter().any(|t| t == name) {
            return "core".to_string();
        }
        self.tool(name).map(|t| t.category.clone()).unwrap_or_else(|| "other".to_string())
    }

    /// 内部只读 Skills：(名称, SKILL.md 全文)，按插件顺序。
    pub fn internal_skills(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for plugin in self.enabled_plugins().filter(|p| p.builtin) {
            let Some(rel) = &plugin.manifest.skills else { continue };
            let Ok(root) = resolve_inside(&plugin.dir, rel, plugin.builtin) else { continue };
            let Ok(entries) = std::fs::read_dir(&root) else { continue };
            let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            dirs.sort();
            for dir in dirs {
                let Some(name) = dir.file_name().and_then(|n| n.to_str()) else { continue };
                if let Ok(content) = std::fs::read_to_string(dir.join("SKILL.md")) {
                    out.push((name.to_string(), content));
                }
            }
        }
        out
    }

    /// 需要首次释放到用户目录的默认 Skills：(名称, 源目录)。
    pub fn default_skills(&self) -> Vec<(String, PathBuf)> {
        let mut out = Vec::new();
        for plugin in self.enabled_plugins() {
            let Some(rel) = &plugin.manifest.default_skills else { continue };
            let Ok(root) = resolve_inside(&plugin.dir, rel, plugin.builtin) else { continue };
            let Ok(entries) = std::fs::read_dir(&root) else { continue };
            let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.join("SKILL.md").is_file()).collect();
            dirs.sort();
            for dir in dirs {
                if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
                    out.push((name.to_string(), dir.clone()));
                }
            }
        }
        out
    }

    /// 读取插件资源文件全文。
    pub fn asset(&self, plugin: &str, key: &str) -> Result<String> {
        let plugin = self.plugin(plugin).ok_or_else(|| anyhow!("插件 {plugin} 不存在"))?;
        let spec = plugin
            .manifest
            .assets
            .get(key)
            .ok_or_else(|| anyhow!("插件 {} 没有声明资源 {key}", plugin.manifest.id))?;
        for candidate in spec.candidates() {
            if let Ok(path) = resolve_inside(&plugin.dir, candidate, plugin.builtin) {
                if path.is_file() {
                    return std::fs::read_to_string(&path).with_context(|| format!("读取 {}", path.display()));
                }
            }
        }
        bail!("插件 {} 的资源 {key} 文件不存在", plugin.manifest.id)
    }

    /// 读取插件提示词片段。
    pub fn prompt(&self, plugin: &str, key: &str) -> Result<String> {
        let plugin = self.plugin(plugin).ok_or_else(|| anyhow!("插件 {plugin} 不存在"))?;
        let rel = plugin
            .manifest
            .prompts
            .get(key)
            .ok_or_else(|| anyhow!("插件 {} 没有声明提示词 {key}", plugin.manifest.id))?;
        let path = resolve_inside(&plugin.dir, rel, plugin.builtin)?;
        std::fs::read_to_string(&path).with_context(|| format!("读取 {}", path.display()))
    }

    /// 读取插件内的 JSON 文件（如设置 schema、声明式视图）。
    pub fn read_json(&self, plugin: &str, rel: &str) -> Result<Value> {
        let plugin = self.plugin(plugin).ok_or_else(|| anyhow!("插件 {plugin} 不存在"))?;
        let path = resolve_inside(&plugin.dir, rel, plugin.builtin)?;
        let text = std::fs::read_to_string(&path).with_context(|| format!("读取 {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("解析 {}", path.display()))
    }

    /// 给前端的插件列表：元信息 + ui 挂载点 + 设置 schema。
    pub fn ui_manifest(&self) -> Value {
        let plugins: Vec<Value> = self
            .plugins
            .iter()
            .map(|p| {
                let settings_schema = p
                    .manifest
                    .settings
                    .as_ref()
                    .and_then(|rel| self.read_json(&p.manifest.id, rel).ok());
                json!({
                    "id": p.manifest.id,
                    "name": p.manifest.name,
                    "version": p.manifest.version,
                    "description": p.manifest.description,
                    "order": p.manifest.order,
                    "builtin": p.builtin,
                    "enabled": p.enabled,
                    "permissions": p.manifest.permissions,
                    "tools": self.tools.iter().filter(|t| t.plugin == p.manifest.id).map(|t| t.name.clone()).collect::<Vec<_>>(),
                    "ui": p.manifest.ui,
                    "settings_schema": settings_schema,
                })
            })
            .collect();
        // 工具显示文案：工具文件的 label，再叠加各插件 ui.tool_labels（含已下线工具的历史文案）。
        let mut tool_labels = serde_json::Map::new();
        for tool in &self.tools {
            if let Some(label) = &tool.label {
                tool_labels.insert(tool.name.clone(), json!(label));
            }
        }
        for plugin in self.enabled_plugins() {
            if let Some(extra) = plugin.manifest.ui.get("tool_labels").and_then(Value::as_object) {
                for (k, v) in extra {
                    tool_labels.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
        json!({
            "generation": self.generation,
            "safe_mode": host().safe_mode(),
            "plugins": plugins,
            "tool_labels": tool_labels,
            "errors": self.errors,
        })
    }
}

/// 插件目录内的相对路径解析。用户插件禁止越出自己的目录；内置插件允许 `..`
/// 以便开发时直接引用仓库里的源文件（如 browser-extension/*.js）。
pub fn resolve_inside(dir: &Path, rel: &str, allow_parent: bool) -> Result<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() || rel.contains(':') {
        bail!("插件路径必须是相对路径: {rel}");
    }
    if !allow_parent && rel_path.components().any(|c| matches!(c, Component::ParentDir)) {
        bail!("插件路径不能越出插件目录: {rel}");
    }
    Ok(dir.join(rel_path))
}

pub fn valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !id.starts_with('.')
}

#[derive(Debug, Clone)]
pub struct Dirs {
    pub builtin: Option<PathBuf>,
    pub user: Option<PathBuf>,
}

/// 内置插件目录：环境变量 > 调试构建的源码目录 > 程序同级 plugins/。
pub fn default_builtin_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("SHIGUANG_PLUGINS_DIR") {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    if cfg!(debug_assertions) || cfg!(test) {
        let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins");
        if dev.is_dir() {
            return Some(dev);
        }
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let beside = exe_dir.join("plugins");
    if beside.is_dir() {
        return Some(beside);
    }
    // 兜底：开发者用 release 构建在 target/release 下运行时，回到源码目录。
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins");
    dev.is_dir().then_some(dev)
}

pub struct PluginHost {
    dirs: RwLock<Dirs>,
    snapshot: RwLock<Arc<Snapshot>>,
    /// 用户在界面里启停插件的覆盖值（plugin id → enabled）。
    overrides: RwLock<BTreeMap<String, bool>>,
    /// 安全模式：只加载内置插件。
    safe_mode: std::sync::atomic::AtomicBool,
    generation: std::sync::atomic::AtomicU64,
    listeners: RwLock<Vec<Box<dyn Fn(&Arc<Snapshot>) + Send + Sync>>>,
}

static HOST: OnceLock<PluginHost> = OnceLock::new();

/// 全局插件宿主。首次调用时用默认内置目录加载；应用启动后由 `configure` 补上用户目录。
pub fn host() -> &'static PluginHost {
    HOST.get_or_init(|| {
        let host = PluginHost {
            dirs: RwLock::new(Dirs { builtin: default_builtin_dir(), user: None }),
            snapshot: RwLock::new(Arc::new(Snapshot::default())),
            overrides: RwLock::new(BTreeMap::new()),
            safe_mode: std::sync::atomic::AtomicBool::new(false),
            generation: std::sync::atomic::AtomicU64::new(0),
            listeners: RwLock::new(Vec::new()),
        };
        host.reload();
        host
    })
}

/// 当前快照的简写。
pub fn snapshot() -> Arc<Snapshot> {
    host().snapshot()
}

impl PluginHost {
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.read().unwrap().clone()
    }

    pub fn dirs(&self) -> Dirs {
        self.dirs.read().unwrap().clone()
    }

    pub fn configure(&self, user_dir: PathBuf, overrides: BTreeMap<String, bool>, safe_mode: bool) {
        let _ = std::fs::create_dir_all(&user_dir);
        self.dirs.write().unwrap().user = Some(user_dir);
        *self.overrides.write().unwrap() = overrides;
        self.safe_mode.store(safe_mode, std::sync::atomic::Ordering::SeqCst);
        self.reload();
    }

    pub fn safe_mode(&self) -> bool {
        self.safe_mode.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn set_override(&self, id: &str, enabled: bool) {
        self.overrides.write().unwrap().insert(id.to_string(), enabled);
        self.reload();
    }

    pub fn overrides(&self) -> BTreeMap<String, bool> {
        self.overrides.read().unwrap().clone()
    }

    /// 快照替换后回调（刷新注册表、通知前端、重启 MCP 服务等）。
    pub fn on_change(&self, f: impl Fn(&Arc<Snapshot>) + Send + Sync + 'static) {
        self.listeners.write().unwrap().push(Box::new(f));
    }

    /// 重新扫描全部插件并替换快照。加载错误记录在快照里，不影响其它插件。
    pub fn reload(&self) -> Arc<Snapshot> {
        let dirs = self.dirs();
        let generation = self.generation.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let snapshot = Arc::new(build_snapshot(&dirs, &self.overrides(), self.safe_mode(), generation));
        for error in &snapshot.errors {
            log::warn!("插件 {} 加载问题（{}）: {}", error.plugin, error.path, error.message);
        }
        *self.snapshot.write().unwrap() = snapshot.clone();
        for listener in self.listeners.read().unwrap().iter() {
            listener(&snapshot);
        }
        snapshot
    }
}

fn read_manifest(dir: &Path) -> Result<Manifest> {
    let path = dir.join(PLUGIN_FILE);
    let text = std::fs::read_to_string(&path).with_context(|| format!("读取 {}", path.display()))?;
    let manifest: Manifest = serde_json::from_str(&text).with_context(|| format!("解析 {}", path.display()))?;
    if !valid_plugin_id(&manifest.id) {
        bail!("插件 id 只能包含小写字母、数字、- 和 _: {}", manifest.id);
    }
    Ok(manifest)
}

fn scan_dir(root: &Path, builtin: bool, errors: &mut Vec<PluginError>) -> Vec<Plugin> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join(PLUGIN_FILE).is_file())
        .filter(|p| !p.file_name().and_then(|n| n.to_str()).unwrap_or("").starts_with('.'))
        .collect();
    dirs.sort();
    let mut out = Vec::new();
    for dir in dirs {
        match read_manifest(&dir) {
            Ok(manifest) => out.push(Plugin { enabled: manifest.enabled_by_default, manifest, dir, builtin }),
            Err(e) => errors.push(PluginError {
                plugin: dir.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string(),
                path: dir.join(PLUGIN_FILE).display().to_string(),
                message: format!("{e:#}"),
            }),
        }
    }
    out
}

fn tool_definition(entry: &ToolFileEntry) -> Value {
    let mut function = serde_json::Map::new();
    function.insert("name".into(), json!(entry.name));
    function.insert("description".into(), json!(entry.description));
    if let Some(parameters) = &entry.parameters {
        function.insert("parameters".into(), parameters.clone());
    }
    json!({ "type": "function", "function": Value::Object(function) })
}

pub fn build_snapshot(dirs: &Dirs, overrides: &BTreeMap<String, bool>, safe_mode: bool, generation: u64) -> Snapshot {
    let mut errors = Vec::new();
    let mut plugins = Vec::new();
    if let Some(dir) = &dirs.builtin {
        plugins.extend(scan_dir(dir, true, &mut errors));
    }
    if let Some(dir) = &dirs.user {
        let builtin_ids: HashSet<String> = plugins.iter().map(|p| p.manifest.id.clone()).collect();
        for plugin in scan_dir(dir, false, &mut errors) {
            if builtin_ids.contains(&plugin.manifest.id) {
                errors.push(PluginError {
                    plugin: plugin.manifest.id.clone(),
                    path: plugin.dir.display().to_string(),
                    message: "与内置插件同名，已忽略。内置插件只读，请换一个 id。".into(),
                });
                continue;
            }
            plugins.push(plugin);
        }
    }
    for plugin in &mut plugins {
        if let Some(&enabled) = overrides.get(&plugin.manifest.id) {
            plugin.enabled = enabled;
        }
        if safe_mode && !plugin.builtin {
            plugin.enabled = false;
        }
    }
    plugins.sort_by(|a, b| (a.manifest.order, &a.manifest.id).cmp(&(b.manifest.order, &b.manifest.id)));

    let mut tools: Vec<ToolSpec> = Vec::new();
    let mut tool_index = HashMap::new();
    let mut core_tools = Vec::new();
    let mut retired = HashMap::new();
    let mut categories: Vec<Category> = Vec::new();

    for plugin in plugins.iter().filter(|p| p.enabled) {
        let id = &plugin.manifest.id;
        if plugin.builtin {
            for name in &plugin.manifest.core_tools {
                if !core_tools.contains(name) {
                    core_tools.push(name.clone());
                }
            }
        }
        let message = plugin.manifest.retired_message.clone().unwrap_or_else(|| "该能力已下线。".into());
        for name in &plugin.manifest.retired_tools {
            retired.insert(name.clone(), message.clone());
        }
        for rel in &plugin.manifest.tools {
            let path = match resolve_inside(&plugin.dir, rel, plugin.builtin) {
                Ok(path) => path,
                Err(e) => {
                    errors.push(PluginError { plugin: id.clone(), path: rel.clone(), message: e.to_string() });
                    continue;
                }
            };
            let parsed = std::fs::read_to_string(&path)
                .with_context(|| format!("读取 {}", path.display()))
                .and_then(|text| serde_json::from_str::<Vec<ToolFileEntry>>(&text).with_context(|| format!("解析 {}", path.display())));
            let entries = match parsed {
                Ok(entries) => entries,
                Err(e) => {
                    errors.push(PluginError { plugin: id.clone(), path: path.display().to_string(), message: format!("{e:#}") });
                    continue;
                }
            };
            for entry in entries {
                if let Err(message) = check_tool_entry(plugin, &entry) {
                    errors.push(PluginError { plugin: id.clone(), path: path.display().to_string(), message });
                    continue;
                }
                if let Some(&existing) = tool_index.get(&entry.name) {
                    let owner: &ToolSpec = &tools[existing];
                    errors.push(PluginError {
                        plugin: id.clone(),
                        path: path.display().to_string(),
                        message: format!("工具 {} 已由插件 {} 提供，已忽略重复定义", entry.name, owner.plugin),
                    });
                    continue;
                }
                let spec = ToolSpec {
                    definition: tool_definition(&entry),
                    category: entry.category.clone().unwrap_or_else(|| id.clone()),
                    executor: entry.executor.clone().unwrap_or_else(|| json!({"kind": "builtin"})),
                    when: entry.when.clone(),
                    label: entry.label.clone(),
                    name: entry.name.clone(),
                    plugin: id.clone(),
                };
                tool_index.insert(spec.name.clone(), tools.len());
                tools.push(spec);
            }
        }
        // MCP 自动导入的工具由 mcp 模块在服务启动后补充（见 mcp::imported_tools）。
        for spec in mcp::imported_tools(plugin) {
            if tool_index.contains_key(&spec.name) {
                continue;
            }
            tool_index.insert(spec.name.clone(), tools.len());
            tools.push(spec);
        }
    }

    // 类别：显式成员优先，否则收集主类别相同的工具。
    for plugin in plugins.iter().filter(|p| p.enabled) {
        for (cid, spec) in &plugin.manifest.categories {
            if categories.iter().any(|c| &c.id == cid) {
                errors.push(PluginError { plugin: plugin.manifest.id.clone(), path: PLUGIN_FILE.into(), message: format!("类别 {cid} 重复声明，已忽略") });
                continue;
            }
            let members = if spec.tools.is_empty() {
                tools.iter().filter(|t| &t.category == cid).map(|t| t.name.clone()).collect()
            } else {
                spec.tools.iter().filter(|n| tool_index.contains_key(*n)).cloned().collect()
            };
            categories.push(Category {
                id: cid.clone(),
                label: spec.label.clone(),
                description: spec.description.clone(),
                plugin: plugin.manifest.id.clone(),
                tools: members,
            });
        }
    }
    // 用户插件的工具若属于未声明的类别，自动建一个以插件 id 命名的类别。
    for tool in &tools {
        if tool.category != "core" && !categories.iter().any(|c| c.id == tool.category) {
            categories.push(Category {
                id: tool.category.clone(),
                label: tool.category.clone(),
                description: String::new(),
                plugin: tool.plugin.clone(),
                tools: tools.iter().filter(|t| t.category == tool.category).map(|t| t.name.clone()).collect(),
            });
        }
    }

    Snapshot { plugins, tools, tool_index, categories, core_tools, retired, errors, generation }
}

/// 工具声明检查：名字合法；用户插件只能用自己有权限的执行器。
fn check_tool_entry(plugin: &Plugin, entry: &ToolFileEntry) -> std::result::Result<(), String> {
    let name_ok = !entry.name.is_empty()
        && entry.name.len() <= 64
        && entry.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !name_ok {
        return Err(format!("工具名不合法: {}", entry.name));
    }
    let kind = entry.executor.as_ref().and_then(|e| e.get("kind")).and_then(Value::as_str).unwrap_or("builtin");
    if !executors::KINDS.contains(&kind) {
        return Err(format!("工具 {} 的执行器 {kind} 不受支持", entry.name));
    }
    if !plugin.builtin {
        if kind == "builtin" {
            return Err(format!("用户插件不能声明 builtin 执行器（工具 {}）；请用 script / command / http / mcp / alias", entry.name));
        }
        if let Some(permission) = executors::required_permission(kind) {
            if !plugin.manifest.permissions.iter().any(|p| p == permission) {
                return Err(format!("工具 {} 使用 {kind} 执行器，需要在 permissions 声明 {permission}", entry.name));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sg-plugins-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn builtin_plugins_load_without_errors() {
        let snapshot = build_snapshot(&Dirs { builtin: default_builtin_dir(), user: None }, &BTreeMap::new(), false, 1);
        assert!(snapshot.errors.is_empty(), "{:?}", snapshot.errors);
        assert!(!snapshot.tools.is_empty());
        assert!(snapshot.core_tools.iter().any(|t| t == "discover_capabilities"));
    }

    #[test]
    fn user_plugin_cannot_shadow_builtin_or_use_builtin_executor() {
        let builtin = temp_root("b");
        let user = temp_root("u");
        write(&builtin.join("core/plugin.json"), r#"{"id":"core","name":"核心","tools":["tools.json"]}"#);
        write(&builtin.join("core/tools.json"), r#"[{"name":"a","description":"A"}]"#);
        write(&user.join("core/plugin.json"), r#"{"id":"core","name":"冒充"}"#);
        write(&user.join("mine/plugin.json"), r#"{"id":"mine","name":"我的","tools":["tools.json"],"permissions":["script"]}"#);
        write(&user.join("mine/tools.json"), r#"[{"name":"b","description":"B"},{"name":"c","description":"C","executor":{"kind":"script","file":"x.js"}},{"name":"a","description":"dup","executor":{"kind":"script","file":"x.js"}}]"#);
        let s = build_snapshot(&Dirs { builtin: Some(builtin.clone()), user: Some(user.clone()) }, &BTreeMap::new(), false, 1);
        assert_eq!(s.plugins.len(), 2);
        assert!(s.tool("a").is_some_and(|t| t.plugin == "core"));
        assert!(s.tool("b").is_none());
        assert!(s.tool("c").is_some_and(|t| t.category == "mine"));
        assert_eq!(s.errors.len(), 3, "{:?}", s.errors);
        let safe = build_snapshot(&Dirs { builtin: Some(builtin.clone()), user: Some(user.clone()) }, &BTreeMap::new(), true, 2);
        assert!(safe.tool("c").is_none());
        let _ = std::fs::remove_dir_all(builtin);
        let _ = std::fs::remove_dir_all(user);
    }

    #[test]
    fn user_paths_cannot_escape() {
        assert!(resolve_inside(Path::new("x"), "../y", false).is_err());
        assert!(resolve_inside(Path::new("x"), "C:/y", false).is_err());
        assert!(resolve_inside(Path::new("x"), "a/b.js", false).is_ok());
        assert!(resolve_inside(Path::new("x"), "../y", true).is_ok());
    }
}
