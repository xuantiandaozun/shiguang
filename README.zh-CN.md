<div align="center">

<img src="icons/app-icon.png" width="96" alt="拾光" />

# 拾光 ShiGuang

**常驻 Windows 托盘的 AI 桌面助手**

一句话整理桌面、提醒待办、操作浏览器、管理服务器、录操作视频，<br/>
需要新功能时，还能让它自己写一个插件。

[![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11-0078D6?logo=windows&logoColor=white)](#-快速开始)
[![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)](https://tauri.app)
[![Rust](https://img.shields.io/badge/Rust-backend-CE422B?logo=rust&logoColor=white)](src-tauri)
[![React](https://img.shields.io/badge/React-18-61DAFB?logo=react&logoColor=black)](src)
[![Version](https://img.shields.io/badge/version-0.1.6-8B5CF6)](https://github.com/xuantiandaozun/shiguang/releases)
[![License](https://img.shields.io/badge/license-MIT-22C55E)](LICENSE)

[English](README.md) · **简体中文** · [下载](https://github.com/xuantiandaozun/shiguang/releases) · [插件开发](docs/PLUGINS.md) · [LINUX DO](https://linux.do)

</div>

---

## ✨ 亮点

- 🫧 **悬浮球 + 聊天小窗**：常驻托盘，点一下就能对话，关掉窗口也不退出。
- 🗂️ **桌面整理**：先给方案、确认后才动；只移动不删除，每一批都能一键撤销。
- 🌐 **用你自己的浏览器**：通过配套扩展在你登录过的浏览器里读网页、点击、填表。浏览器真实输入引擎会等页面安静后再汇报结果。
- 📸 **截图**：框选区域，或单击某个软件窗口截完整窗口（被遮挡也行）；全局快捷键，自动复制到剪贴板。
- 🎬 **操作录屏**：录网页或桌面软件的操作，导出时自动放大点击处、画平滑光标；网页上的敏感信息在源头就被模糊；支持剪辑、解说、配音、背景音乐和字幕。
- 🧑‍💻 **工作区与外派**：记住你的项目怎么验证、怎么上线；代码修改可以交给 Claude Code / Codex，拾光负责交代任务、核对结果、征得同意后提交。
- 🖥️ **服务器运维**：SSH / SFTP、交互终端、聊天式运维，按风险确认操作。
- 🧩 **插件化**：工具、提示词、Skills、界面都由插件提供，改文件即生效；拾光可以自己编写插件。

## 🆕 最近更新

> 以下内容在 v0.1.6 之后开发，会随下个版本发布。

- **🎬 操作录屏（新）**
  - **录制**：录浏览器标签页，或者桌面上的整块屏幕、某个软件窗口、框选的区域（快捷键 Ctrl+Alt+R 随时开始/停止）。录制时只保存原始画面和操作事件，导出时再处理，所以效果可以反复调整。
  - **不录拾光自己**：桌面录制默认把拾光的窗口排除在画面外，可以放在旁边操作；录拾光自己的宣传视频时勾选「录进去」。
  - **自动放大**：点击和输入的位置自动放大并平滑跟随；光标重新绘制得更大，点击处有波纹。另有「带渐变背景和圆角」的风格，适合宣传视频。
  - **源头打码**：密码、手机号、身份证、邮箱、银行卡、密钥在页面上就被模糊，录到的画面里没有明文；可以自定义选择器和正则。
  - **时间线编辑器**：剪片段、框选补打码、编辑放大段、写解说词，支持快速预览。
  - **配音与字幕**：拾光调用你本机的配音 CLI（比如火山引擎）生成配音。配音比画面长时，画面会自动定格等它说完。字幕会生成 `.srt`，也可以直接画进画面。
  - **拾光代录**：拾光可以一边操作一边录，每一步自动打标记，作为解说草稿。
- **🧩 插件化架构**：薄 Rust 内核 + 插件。工具、提示词、Skills、界面都由插件提供，改文件不用重新编译。新增「插件」页，可以启停、回滚，也能让拾光编写插件。
- **🧑‍💻 工作区与外派**：登记固定项目，说名字就知道改哪里；代码修改外派给 Claude Code / Codex CLI。
- **🖱️ 浏览器真实输入引擎**：点击、输入、上传都通过调试协议发出真实输入，动作后等页面安静，再汇报效果和新开的标签。

<details>
<summary>v0.1.4 – v0.1.6</summary>

- **服务器管理与 AI 运维**：SSH / SFTP 连接管理、独立交互终端、聊天式运维；远程目录浏览、文本预览、多文件上传、传输进度、Tab 补全。
- **精准委托**：Codex / Cursor 可以通过本地 MCP 把任务交给拾光，支持关键节点暂停审核、多轮纠偏和续跑。
- **模型与代理**：供应商独立维护模型和套餐有效期；自动调用资料、文件、浏览器子代理。
- **界面与发布**：焕新主窗口、聊天窗和悬浮球；默认本地构建发版，由 GitHub 负责签名和生成更新信息。

详见 [v0.1.5 发布说明](docs/releases/0.1.5.md)。

</details>

## 🧰 功能一览

### 日常

| 功能 | 说明 |
| --- | --- |
| 🗂️ 桌面整理 | 「帮我整理桌面」→ 生成分类方案 → 在聊天卡片里确认后执行。只移动不删除，每批可撤销。整理细则是可编辑的 Skill |
| ⏰ 待办提醒 | 用自然语言创建，如「明天下午 3 点提醒我交周报」；支持每天/每周重复、优先级、稍后提醒；用系统通知或弹窗提醒，弹窗里可以直接回复 AI |
| 🔎 本地文件索引 | 类 Everything 的持久元数据索引，支持批量搜索和整盘分析；整盘占用可以先用 NTFS/MFT 快速估算 |
| 📄 文件与视觉 | 读文本和 PDF、本地 OCR（PaddleOCR）、图片理解；写文件自动备份 |

### 浏览器与内容

| 功能 | 说明 |
| --- | --- |
| 🌐 浏览器操作 | 通过配套扩展操作你正在用的浏览器（带登录态），扩展不可用时自动切换到独立 CDP 实例；可以读正文、快照、点击、输入、提取数据 |
| 📸 截图 | 框选、单击窗口、指定软件、全屏；默认不截进拾光自己的窗口（可选包含）；保存到「图片/拾光截图」并复制到剪贴板 |
| 🎬 操作录屏 | 录网页或桌面软件操作并导出 MP4：自动放大、合成光标、源头打码、剪辑、解说、配音、背景音乐、字幕。详见[下文](#-操作录屏) |
| 🔍 网页搜索 | 搜索和抓取网页 |

### 开发与运维

| 功能 | 说明 |
| --- | --- |
| 🧑‍💻 工作区与外派 | 记住项目的路径、验证命令、部署方式和规则；代码修改外派给 Claude Code / Codex，拾光核对结果、征得同意后提交。见[说明](docs/WORKSPACES_AGENTS.md) |
| 🖥️ 服务器 | SSH / SFTP 连接、远程命令、目录浏览和文件传输；交互终端支持 Tab 补全，可以切换成聊天式运维。见[说明](docs/SERVERS_MCP.md) |
| ⚙️ 后台命令 | 运行 PowerShell / cmd / AI 写的脚本；长任务在后台跑，结束后再继续对话；自动解析 JSON 输出 |
| 🤝 委托 MCP | Codex / Cursor 可以把任务交给拾光执行，支持审核、追问和中断。见[说明](docs/DELEGATION_MCP.md) |

### 扩展与模型

| 功能 | 说明 |
| --- | --- |
| 🧩 插件 | 工具、提示词、Skills、界面都由插件提供；支持脚本、HTTP、命令、MCP 执行器，以及声明式视图和 iframe 页面。见[插件开发](docs/PLUGINS.md) |
| 📚 Skills | 内置只读 Skills，加上 AI 可以创建和改进的外部 Skills；可以和 Claude / Codex / Cursor 的 Skills 目录同步 |
| 🤖 模型与用量 | 可以保存多个模型配置（DeepSeek、通义千问、火山方舟 Agent Plan、OpenAI 等）并随时切换；显示 Token 用量和缓存命中率 |

## 🎬 操作录屏

```
录制（原始画面 + 事件轨）  →  时间线（放大 / 打码 / 剪切 / 解说）  →  导出 MP4 + SRT
```

1. **录**：在「录屏截图」页选「网页标签页 / 整个屏幕 / 选择窗口或区域」后开始，或按 Ctrl+Alt+R；也可以在聊天里说「录一个在后台新建用户的演示」，由拾光边操作边录。
2. **改**：「编辑」里拖动时间轴，在画面上框选打码，调整放大段，剪掉等待的片段，写解说词。
3. **配音**：在聊天里说「给这个录屏写解说，用火山 CLI 配音」。拾光会先把解说词给你确认，再调你本机的配音 CLI 生成音频，挂到时间线上。
4. **导出**：可以先快速预览；正式导出会生成 MP4 和同名 `.srt`。

**隐私**：
- 网页只录当前标签页，敏感内容在页面上就被模糊；
- 桌面软件不能自动打码，录完在「编辑」里框选打码；按键只记录「有输入」，不记录按了什么；
- 原始素材只保存在本地；
- 配音只把文字发给配音服务，不发画面；
- 导出后请完整看一遍成片再对外发送。

首次导出需要 ffmpeg：会先用本机已有的，没有才自动下载；也可以在设置里指定路径。

## 🧩 插件化架构

```
Rust 内核（src-tauri/src）              插件（src-tauri/plugins + 用户目录）
├─ 窗口、托盘、IPC、更新                ├─ plugin.json     清单、权限、挂载点、设置
├─ Agent 主循环、LLM、上下文管理         ├─ tools.json      工具定义 + 执行器
├─ 文件、命令、数据库、浏览器桥、录屏渲染  ├─ prompts/ skills/ 提示词与内部 Skills
└─ 插件宿主（加载、校验、热重载、安全模式）└─ ui/ *.js         声明式视图、页面、脚本、钩子
```

- 改插件文件即生效，开发时自动重载；内置插件只读，用户插件放在应用数据目录。
- 拾光写插件必须走「校验 → 预览确认 → 版本快照 → 生效」，可以回滚。
- 插件页面运行在沙箱 iframe 里，禁止联网，只能通过 SDK 调用声明过的能力。
- 连续两次启动失败会自动进入安全模式，只加载内置插件。

完整规范：[docs/PLUGINS.md](docs/PLUGINS.md) · [PLUGIN_GUIDE](src-tauri/plugins/core/PLUGIN_GUIDE.md) · [示例插件](docs/plugin-examples/quick-notes/)

## 🛡️ 安全设计

- **桌面整理**：每次执行都要人工确认；只移动不删除（删除进回收站）；快捷方式、`desktop.ini`、隐藏文件默认不动；每次移动都有记录，可以撤销。
- **文件与命令**：写文件自动备份；破坏性命令必须征得同意；整盘扫描先询问，索引只存元数据、不读内容。
- **浏览器与录屏**：网络观察只保存脱敏后的接口结构；录屏在页面源头打码，密码框的按键不进事件记录。
- **外派**：编码智能体不能 commit / push，提交由拾光在你确认后执行。
- **服务器**：首次连接或主机指纹变化时，需要你人工核对后信任。连接密码以明文存在本地数据库，请保护好数据目录。AI 运维时，目标、命令结果和本地路径会发给配置的模型；SFTP 传输的文件内容不会发给模型。

## 🚀 快速开始

**下载使用**：在 [Releases](https://github.com/xuantiandaozun/shiguang/releases) 下载安装包。首次运行后：
1. 右键托盘图标，打开主窗口，在「设置」里添加模型配置。
2. 浏览器扩展：在 Chrome / Edge 的扩展管理页打开开发者模式，点「加载已解压的扩展程序」，选择安装目录下的 `browser-extension` 文件夹。拾光升级后，在扩展管理页点「重新加载」即可。

**从源码运行**（需要 Node.js 18+、Rust（MSVC 工具链）、Microsoft C++ Build Tools，以及 Windows 10/11）：

```bash
npm install
npm run tauri:dev      # 开发
npm run tauri:build    # 构建 EXE（插件同步到 target/release/plugins）
npm run tauri:bundle   # 构建安装包
```

维护者发版流程见 [本地发布说明](docs/LOCAL_RELEASE.md)。

> 提示：Windows 通知只有在**安装后**才会显示应用名和图标，否则会显示为 PowerShell 发出，这是系统限制。

## 📁 数据位置

`%APPDATA%/com.deskhelper.win/`

| 路径 | 内容 |
| --- | --- |
| `deskhelper.db` | 待办、操作记录、设置、聊天历史、工作区 |
| `file_index.db` | 文件元数据索引（不含文件内容） |
| `plugins/` | 用户插件和版本快照 |
| `recordings/` | 录屏原始素材和时间线（导出的视频默认在「视频/拾光录屏」） |
| `skills/` · `tasks/` · `temp/` · `file_backups/` | 外部 Skills、后台任务日志、AI 临时文件、写文件备份 |
| `tools/` | 按需下载的工具（如 ffmpeg） |

API Key 只存在本地数据库，只用于请求你配置的模型服务。

## 🗂️ 目录结构

```
src/                        React 前端
  windows/                  悬浮球、聊天、主窗口等窗口
  components/               主窗口各页（录屏、工作区、服务器、待办……）
  plugins/                  插件挂载：注册表、iframe 宿主 + SDK、声明式视图、「插件」页
  lib/ipc.ts                前后端接口的唯一声明处
src-tauri/src/              Rust 内核
  llm/                      Agent 主循环、OpenAI 兼容客户端、工具执行、子代理
  plugins/                  插件宿主（加载、执行器、QuickJS、MCP、钩子、热重载）
  browser/                  扩展桥 + CDP，真实输入引擎
  recorder/                 录屏：录制写盘、时间线、渲染（tiny-skia）、配音混音、ffmpeg
  organizer/ todo/ servers.rs workspaces.rs agents/ delegation.rs file_index.rs …
src-tauri/plugins/          18 个内置插件（core、browser、recorder、workspaces……）
browser-extension/          配套浏览器扩展（与桌面端 WebSocket 通信）
docs/                       设计与使用文档
```

## 📚 文档

| 文档 | 内容 |
| --- | --- |
| [PLUGINS.md](docs/PLUGINS.md) | 插件架构与约定 |
| [RECORDING.md](docs/RECORDING.md) | 操作录屏设计与实现 |
| [WORKSPACES_AGENTS.md](docs/WORKSPACES_AGENTS.md) | 工作区与外派 |
| [BROWSER_OPERATIONS.md](docs/BROWSER_OPERATIONS.md) | 浏览器操作 |
| [SERVERS_MCP.md](docs/SERVERS_MCP.md) · [DELEGATION_MCP.md](docs/DELEGATION_MCP.md) | 服务器、委托 MCP |
| [LOCAL_RELEASE.md](docs/LOCAL_RELEASE.md) | 本地构建与发布 |

## 🧭 路线图

- [x] 网页操作录屏：自动放大、源头打码、剪辑、配音、字幕
- [ ] 桌面软件录屏：按窗口录制、全局鼠标、OCR 打码、可选排除拾光自身窗口
- [ ] 录屏背景音乐与按键提示
- [ ] 微信桌面版聊天记录监控（正在评估 UIA / OCR 方案和合规风险）

## 📄 许可证

[MIT](LICENSE)

## 💬 社区

本项目认可并友链 [LINUX DO 社区](https://linux.do)，项目最早在这里和开发者们交流。
