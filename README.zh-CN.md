# 拾光（ShiGuang）— AI 桌面助手

**[English](README.md) | 简体中文**

一个常驻 Windows 托盘的 AI 桌面助手：悬浮球 + 聊天小窗与 AI 对话，帮你**整理桌面文件**、**管理待办并准时提醒**、**操作浏览器**。

## 最近更新（v0.1.4–v0.1.6）

- **服务器管理与 AI 运维**：新增 SSH / SFTP 连接管理、独立交互终端和聊天式运维；支持远程目录浏览、文本预览、多文件上传和传输进度，终端支持 Tab 补全。
- **精准委托**：Codex / Cursor 可通过本地 MCP 交给拾光执行任务，支持关键节点暂停审核、多轮纠偏、续跑和独立历史。
- **模型与代理**：供应商独立维护模型与套餐有效期，支持自动调用资料、文件和浏览器子代理，并修复主 AI 工具发现。
- **界面与交互**：焕新主窗口、聊天窗、设置和悬浮球，优化服务器排序、文件类型图标及终端输入；运维按实际风险确认，减少重复询问。
- **技能与发布**：桌面整理改为可编辑的外部技能；默认本地构建安装包，由 GitHub 校验、签名并生成更新信息，保留手动云端构建备用。

服务器更新详情见 [v0.1.5 发布说明](docs/releases/0.1.5.md)。

## 功能一览

| 模块 | 说明 |
| --- | --- |
| AI 桌面整理 | 对 AI 说「帮我整理一下桌面」→ 生成分类方案 → 你在聊天卡片里确认后执行；全程只移动不删除，支持按批次一键撤销 |
| 自动化规则 | 方案确认后对 AI 说「以后都按这个规则来」，或在主窗口手动建规则；之后桌面新文件命中规则自动归类（仅已审核规则生效，可全局暂停） |
| 待办事项 | 自然语言创建：「明天下午三点提醒我交周报」；支持重复（每天/每周）、优先级、延后提醒；提醒方式可选 Windows 通知或弹窗——带输入框的弹窗能直接把回复发给 AI 继续聊 |
| 浏览器操作 | 读文章正文（Readability）、快照后按编号点击/输入；走配套扩展操作你的真实浏览器（带登录态），失败自动故障转移到独立 CDP 实例 |
| 本机文件索引 | 持久化的 Everything 类元数据索引，用于批量搜索和整盘分析；整盘空间分析可先通过提权的 NTFS/MFT 助手快速估算并按一级目录汇总，需要精确值时再对指定范围逐项扫描 |
| 后台命令 | 可跑 PowerShell、cmd 或 AI 自己写的脚本。短任务同步等待；长任务会挂起对话（期间不再调模型），结束后带着结果自动继续。CLI 返回的 JSON 会自动解析。可在设置中关闭命令工具 |
| 模型与用量 | 可保存多套大模型配置（DeepSeek、通义、火山 Agent Plan、OpenAI 等预设）并手动切换当前使用；「用量」页查看 token 与缓存命中 |
| Agent Skills | 内置只读 Skill + AI 可自建/沉淀的外部 Skill；启用目录会注入对话，按意图加载匹配技能。可与 Claude / Codex / Cursor 的技能目录同步 |
| 委托任务 MCP | Codex / Cursor 可将目标交给拾光 AI 独立执行；双方可多轮交流，用户可查看独立历史并随时插话或中断。配置见 [委托 MCP 指南](docs/DELEGATION_MCP.md) |
| 服务器 SSH / SFTP | 本地保存服务器连接和可查看的密码，执行命令、浏览目录、上传下载文件及目录；内置 AI 与 Codex / Cursor 共用连接，见 [服务器 MCP 指南](docs/SERVERS_MCP.md) |
| 服务器终端与 AI 运维 | 保留目录与 Shell 状态的交互终端，支持 Tab 补全；可切换聊天模式让 AI 执行并总结，按风险确认；侧栏支持远程文件浏览、文本预览和上传进度 |
| 文件与视觉 | 读文件（文本/PDF）、OCR、看图、带自动备份的安全写入、只读子代理 |
| 悬浮球 | 桌面常驻小圆球，可拖动，单击展开/收起聊天窗；聊天窗、主窗口关闭后程序仍在托盘运行 |
| 主窗口 | 待办管理 / 整理规则 / 操作记录（撤销）/ 后台任务 / 委托任务 / 服务器 / Skills / 用量 / 设置 |

## 安全设计

- 任何整理执行前必须人工确认（聊天面板里的方案卡片）
- 只移动文件、绝不删除（delete = 移入回收站）；重名自动加 `(n)` 序号
- 快捷方式 `.lnk/.url`、`desktop.ini`、隐藏文件默认不动
- 所有移动记入 SQLite `operation_logs`，撤销 = 反向移动
- 文件写入自动备份；破坏性 shell 命令必须征得同意
- 整盘与批量文件扫描会先询问是否维护元数据索引；索引只保存元数据，不读取文件内容
- 高速 NTFS/MFT 索引由短生命周期的只读辅助进程隔离执行，弹出 UAC 前必须获得明确同意；其空间结果来自 MFT `$DATA` 逻辑大小，属于带覆盖率的快速估算，精确结果需普通元数据扫描

### 服务器与模型数据

- 首次 SSH 连接或主机指纹变化时，必须人工核对并信任指纹。
- 连接密码和私钥口令以明文保存在本机数据库，请妥善保护数据目录；删除连接不会删除远程文件。
- AI 运维所需的操作目标、命令结果和本机路径会发送给已配置的模型；SFTP 上传的文件本体不会发送给模型。
- 关闭终端或取消本地等待不保证远程进程停止；取消上传后，已完成的文件可能仍保留。

## 技术栈

Tauri 2（Rust）+ React 18 + TypeScript + Vite + TailwindCSS + Zustand + SQLite（rusqlite bundled）
LLM 走 OpenAI 兼容接口（DeepSeek / 通义千问 / OpenAI 等），Function Calling + SSE 流式。

## 开发环境要求

- Node.js 18+
- Rust（rustup，msvc 工具链）+ Microsoft C++ 生成工具（编译 bundled SQLite 需要）
- Windows 10/11（自带 WebView2）

## 本地运行

```bash
npm install
npm run tauri:dev
```

首次运行后：托盘图标右键 → 打开主窗口 →「设置」页添加一套或多套大模型配置（DeepSeek / 通义 / 火山 Agent Plan / OpenAI 等预设），选好当前使用后再聊天。每次启动会开一个新会话，旧对话仍在历史里。

## 打包

```bash
npm run tauri:build
```

产物：`src-tauri/target/release/bundle/nsis/` 下的 NSIS 安装包。安装成功后会在安装目录解压 `browser-extension/`（与 exe 同级），换电脑时在 Chrome/Edge 里「加载已解压的扩展」选这个文件夹即可。

发布维护者可使用 `npm run release:local` 在本地构建，再上传 GitHub 草稿，由工作流校验 SHA256、签名并生成 `latest.json`，供应用内检查更新使用。完整步骤及云端构建备用方式见 [本地构建并发布](docs/LOCAL_RELEASE.md)。

> 注意：Windows 通知仅在**安装后**的应用上显示应用名和图标；直接运行 exe 或开发模式下通知会显示为 PowerShell 发起，这是系统限制。

## 数据位置

`%APPDATA%/com.deskhelper.win/`：

- `deskhelper.db`（SQLite：待办、规则、操作记录、设置、聊天记录）
- `file_index.db`（持久化文件元数据索引：路径、名称、大小、时间；不含文件内容）
- `tasks/`、`skills/`、`screenshots/`、`temp/`（AI 临时文件）、`file_backups/`、`ntfs-helper/`
- `lookup-cache.json`（AI 从外部 CLI 提炼的 id↔名称 / 字段对照，短期缓存）

API Key 保存在本机数据库中，调用模型时用于向所配置的 API 服务认证；不会上传至项目仓库。

## 目录结构

```
src/                    React 前端
  windows/              FloatBall / ChatPanel / Reminder / MainWindow 四个窗口页面
  components/           主窗口标签页（待办 / 规则 / 记录 / 后台任务 / Skills / 用量 / 设置）
  lib/ipc.ts            前端 invoke/事件封装的唯一声明处
  stores/chat.ts        聊天状态（zustand）
src-tauri/src/
  lib.rs                应用装配：插件、托盘、后台任务
  commands.rs           IPC 命令层（设置存 SQLite settings 表）
  db.rs                 SQLite 全部读写
  llm/                  OpenAI 兼容客户端（SSE + Function Calling）、Agent 循环、
                        工具集、子代理、提示词、个人信息、视觉模型
  organizer/            桌面扫描、方案执行/撤销、规则引擎、文件监听
  browser/              扩展桥 + CDP；page-api + Readability（browser_read 抽正文）
  file_index.rs         持久化文件元数据索引、覆盖检查与目录占用汇总
  ntfs_usn.rs           只读 NTFS MFT / USN Journal 支持
  ntfs_helper.rs        短生命周期提权索引辅助进程的协议与客户端
  todo/scheduler.rs     待办提醒调度（通知 / 弹窗 / 带输入弹窗）
  tasks.rs              后台命令（含长时间任务挂起等待）
  lookup_cache.rs / cli_json.rs   外部对照缓存、CLI JSON 解析
  servers.rs / server_tools.rs  SSH/SFTP 连接、命令与文件传输
  server_terminal.inc.rs  交互终端与 AI 运维支持
  skills.rs + builtin_skills.rs  Agent Skills
  reader.rs / writer.rs / ocr.rs / machine.rs / tempfs.rs
src-tauri/builtin-skills/  内置 Skill 源文件（编译进应用）
src-tauri/default-skills/  默认外部技能（含可编辑的桌面整理技能）
browser-extension/      配套浏览器扩展（WebSocket 桥接桌面端）
scripts/gen-icon.cjs    应用图标生成脚本（node scripts/gen-icon.cjs）
scripts/build-index-helper.mjs  打包前构建 NTFS 索引 sidecar
```

## 规划

- 微信桌面端聊天记录监控（需评估 UIA / OCR 路线与合规风险）

## 开源协议

[MIT](LICENSE)
