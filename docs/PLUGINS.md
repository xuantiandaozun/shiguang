# 插件架构

拾光采用「薄 Rust 内核 + 插件」结构。功能写成插件，改插件文件即生效，不用重新编译打包。

> 插件清单、执行器、脚本 API、界面 SDK、声明式视图的完整规范在
> [`src-tauri/plugins/core/PLUGIN_GUIDE.md`](../src-tauri/plugins/core/PLUGIN_GUIDE.md)（随程序发布，拾光编写插件时也读它）。
> 完整示例：[`docs/plugin-examples/quick-notes/`](plugin-examples/quick-notes/)。

## 分层

```
Rust 内核（src-tauri/src）           插件（src-tauri/plugins + 用户目录）
├─ 外壳：窗口、托盘、IPC、更新         ├─ plugin.json      清单、权限、类别、挂载点
├─ Agent 主循环、LLM、上下文管理       ├─ tools.json       工具 schema + 描述 + 执行器
├─ 基础能力：文件、命令、DB、浏览器桥   ├─ prompts/*.md     提示词
└─ 插件宿主 src/plugins/               ├─ skills/          内部 Skills（只读）
   ├─ mod.rs        加载、快照、校验   ├─ *.js             脚本工具 / 钩子（QuickJS）
   ├─ executors.rs  执行器分派         ├─ ui/              声明式视图 JSON / HTML 页面
   ├─ script.rs     QuickJS 运行时     └─ MCP 服务         任意语言
   ├─ mcp.rs        MCP 客户端
   ├─ hooks.rs      before/after_tool
   ├─ watch.rs      热重载
   ├─ protocol.rs   plugin:// 协议
   ├─ authoring.rs  拾光编写插件（暂存/校验/确认/版本）
   ├─ boot.rs       安全模式
   └─ ipc.rs        前端接口
```

前端（`src/plugins/`）：`registry.ts` 汇总挂载点；`PluginMount` 渲染内置组件 / iframe 页面 / 声明式视图；`PluginFrame` 是 iframe 宿主（SDK 转发与权限检查）；`DeclarativeView`、`SchemaForm`、`PluginsTab`（主窗口「插件」页）。

## 现有功能与插件的对应

| 插件 | 内容 |
|---|---|
| core | 核心工具（发现、历史、ask_user、todo_write）、系统提示词、内置窗口、UI SDK、PLUGIN_GUIDE |
| files / commands / browser / web / system | 文件、命令与后台任务、浏览器、网页、本机信息 |
| organize / todos / profile | 桌面整理（默认 Skill）、待办提醒、个人信息 |
| servers / workspaces / agents / delegation | 服务器、工作区、编码智能体外派、子代理与委托 |
| recorder | 操作录屏：工具（录制/时间线编辑/导出）、设置（打码规则/放大/字幕/ffmpeg）、页面录制脚本、录制与配音 Skill、「录屏」页与编辑器；内核 `src/recorder/` 只做录制写盘与渲染编码，设计见 `docs/RECORDING.md` |
| skills / usage / settings | Skills 管理、用量、设置页 |
| plugins | 插件管理工具、plugin-authoring Skill、「插件」页 |

内置插件的工具多数是 `builtin` 执行器：逻辑仍在内核里，但工具描述、参数、类别、界面挂载、Skills 都在插件文件里，改它们不需要重新编译。

## 关键约定

- **前缀缓存**：插件按 `order`、id 排序；工具注册表按名称排序；系统提示词正文来自 `core/prompts/system.md`。不要在提示词里放动态内容。
- **注册表**：插件工具写入 `tool_registry`（INSERT OR IGNORE）。未手工改过的（revision=1）始终跟随插件文件；插件停用后其工具不再提供。注册表只记录白名单执行器，插件执行器以插件文件为准。
- **discover_capabilities**：内置类别写在 core 的 tools.json；插件新增的类别运行时自动补进枚举和说明。
- **安全**：
  - 内置插件只读；用户插件不能用内置 id、不能用 builtin 执行器、路径不能越出目录，权限必须声明。
  - 插件页面在 `sandbox="allow-scripts allow-forms"` 的 iframe 里（无同源），每个 `plugin://` 响应带严格 CSP（禁止网络、禁止 asset:），只能经 SDK 调用。
  - 应用 CSP 见 `tauri.conf.json`；asset 协议范围保持全盘，是为了聊天附件预览，插件页面访问不到。
  - 拾光写插件必经 `plugin_write`：校验 → 确认（可预览）→ 版本快照 → 生效；`plugin_rollback` 恢复。
- **安全模式**：连续两次启动前端未完成渲染（未调用 `plugins_boot_ok`）时只加载内置插件；也可 `--safe-mode` 或 `SHIGUANG_SAFE_MODE=1`。「插件」页可恢复。

## 开发与发布

- 开发：`npm run tauri:dev`，调试构建直接读 `src-tauri/plugins`，改完自动重载。
- 内置插件目录可用环境变量 `SHIGUANG_PLUGINS_DIR` 覆盖。
- 浏览器页面脚本与扩展共用 `browser-extension/*.js`：开发时直接引用，发布时复制到 `plugins/browser/assets/`。
- 插件**不**放进 Tauri resources（否则每改一个插件文件都会触发 Rust 重新编译）：
  - `npm run tauri:build` 结束后用 `scripts/sync-plugins.mjs` 同步到 `target/release/plugins`；
  - 交付到 `release/current` 时同样执行 `node scripts/sync-plugins.mjs release/current`；
  - 安装包（`tauri:bundle` / `release:local`）通过 `src-tauri/tauri.plugins.conf.json` 带上 `plugins/`。
- 测试：`cargo test --manifest-path src-tauri/Cargo.toml --lib plugins::`（含示例插件的校验与脚本运行）。

## 新增功能怎么选

1. 只是做法 → Skill。
2. 新工具 / 新页面 / 自动检查 → 插件（script、http、command、mcp 执行器；声明式视图优先）。
3. 需要新的系统级能力（新的 Rust 原语）→ 在内核加 builtin 工具，并在对应内置插件的 tools.json 声明；尽量做成通用原语，具体流程留给插件。
