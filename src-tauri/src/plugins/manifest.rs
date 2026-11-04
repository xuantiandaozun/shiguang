//! 插件清单（plugin.json）与工具文件（tools/*.json）的数据结构。
//! 规范见 docs/PLUGINS.md。字段都有默认值，缺省时插件仍能加载。
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// 界面与目录的排序值，越小越靠前；同值按 id。
    #[serde(default)]
    pub order: i64,
    #[serde(default = "yes")]
    pub enabled_by_default: bool,
    /// 声明的权限。用户插件只能用这里列出的能力（见 permissions.rs）。
    #[serde(default)]
    pub permissions: Vec<String>,
    /// 工具文件，相对插件目录。
    #[serde(default)]
    pub tools: Vec<String>,
    /// 每轮都提供给模型的核心工具（只在内置插件中生效）。
    #[serde(default)]
    pub core_tools: Vec<String>,
    /// 已下线的工具名：注册表和执行入口都会拒绝。
    #[serde(default)]
    pub retired_tools: Vec<String>,
    /// 下线工具被调用时的说明。
    #[serde(default)]
    pub retired_message: Option<String>,
    /// 能力类别（discover_capabilities 的 categories）。
    #[serde(default)]
    pub categories: BTreeMap<String, CategorySpec>,
    /// 只读的内部 Skills 目录（每个子目录一个 SKILL.md）。
    #[serde(default)]
    pub skills: Option<String>,
    /// 首次运行释放到用户 Skills 目录的可编辑 Skills，之后不覆盖。
    #[serde(default)]
    pub default_skills: Option<String>,
    /// 资源文件：key → 路径或候选路径列表（第一个存在的生效）。
    #[serde(default)]
    pub assets: BTreeMap<String, AssetPath>,
    /// 提示词片段：key → Markdown 文件。
    #[serde(default)]
    pub prompts: BTreeMap<String, String>,
    /// 插件自带的 MCP 服务（stdio）。
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpServerSpec>,
    /// 工具调用前后的钩子。
    #[serde(default)]
    pub hooks: Vec<HookSpec>,
    /// 插件设置的 JSON Schema 文件。
    #[serde(default)]
    pub settings: Option<String>,
    /// 界面挂载点，原样交给前端（见 docs/PLUGINS.md「UI」）。
    #[serde(default)]
    pub ui: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CategorySpec {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    /// 显式成员和顺序；可包含其它插件的工具。省略时为本插件主类别等于它的工具。
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssetPath {
    One(String),
    Candidates(Vec<String>),
}

impl AssetPath {
    pub fn candidates(&self) -> Vec<&str> {
        match self {
            AssetPath::One(path) => vec![path.as_str()],
            AssetPath::Candidates(paths) => paths.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// 工作目录，默认插件目录。
    #[serde(default)]
    pub cwd: Option<String>,
    /// true 时把服务 tools/list 的结果自动注册为工具。
    #[serde(default)]
    pub import_tools: bool,
    /// 自动注册工具的名称前缀，避免与内置工具重名。
    #[serde(default)]
    pub tool_prefix: String,
    /// 自动注册工具的类别，默认插件 id。
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookSpec {
    /// before_tool / after_tool
    pub event: String,
    /// 只对这些工具触发；空表示全部。
    #[serde(default)]
    pub tools: Vec<String>,
    /// 插件内的 JS 文件。
    pub script: String,
    /// 导出的函数名。
    #[serde(default = "default_hook_fn")]
    pub function: String,
}

fn default_hook_fn() -> String {
    "hook".to_string()
}

/// 工具文件里的一项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFileEntry {
    pub name: String,
    /// 聊天里显示的进行式文案，如「打开网页」。
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    /// 主类别；缺省为插件 id。
    #[serde(default)]
    pub category: Option<String>,
    /// 执行器；缺省为 builtin（内核按工具名执行）。
    #[serde(default)]
    pub executor: Option<Value>,
    /// 只有满足条件时才提供给模型，如 "setting:read_image_enabled"。
    #[serde(default)]
    pub when: Option<String>,
}
