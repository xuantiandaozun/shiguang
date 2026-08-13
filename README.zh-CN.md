# 拾光（ShiGuang）— AI 桌面助手

**[English](README.md) | 简体中文**

一个常驻 Windows 托盘的 AI 桌面助手：悬浮球 + 聊天小窗与 AI 对话，帮你**整理桌面文件**、**管理待办并准时提醒**、**操作浏览器**。

## 功能一览

| 模块 | 说明 |
| --- | --- |
| AI 桌面整理 | 对 AI 说「帮我整理一下桌面」→ 生成分类方案 → 你在聊天卡片里确认后执行；全程只移动不删除，支持按批次一键撤销 |
| 自动化规则 | 方案确认后对 AI 说「以后都按这个规则来」，或在主窗口手动建规则；之后桌面新文件命中规则自动归类（仅已审核规则生效，可全局暂停） |
| 待办事项 | 自然语言创建：「明天下午三点提醒我交周报」；支持重复（每天/每周）、优先级、延后提醒；提醒方式可选 Windows 通知或弹窗——带输入框的弹窗能直接把回复发给 AI 继续聊 |
| 浏览器操作 | 读文章正文（Readability）、快照后按编号点击/输入；走配套扩展操作你的真实浏览器（带登录态），失败自动故障转移到独立 CDP 实例 |
| 本机文件索引 | 持久化的 Everything 类元数据索引，用于批量搜索和整盘分析；整盘空间分析可先通过提权的 NTFS/MFT 助手快速估算并按一级目录汇总，需要精确值时再对指定范围逐项扫描 |
| 后台命令 | AI 可同步或后台执行 PowerShell / cmd；执行前检查 PowerShell 语法，保持中文输出编码，传播原生程序退出码，并可稍后查看长任务结果；可在设置中关闭命令工具 |
| Agent Skills | 内置只读 Skill + AI 可自建/沉淀的外部 Skill；可与 Claude / Codex / Cursor 的技能目录同步 |
| 文件与视觉 | 读文件（文本/PDF）、OCR、看图、带自动备份的安全写入、只读子代理 |
| 悬浮球 | 桌面常驻小圆球，可拖动，单击展开/收起聊天窗；聊天窗、主窗口关闭后程序仍在托盘运行 |
| 主窗口 | 待办管理 / 整理规则 / 操作记录（撤销）/ 后台任务 / Skills / 设置 |

## 安全设计

- 任何整理执行前必须人工确认（聊天面板里的方案卡片）
- 只移动文件、绝不删除（delete = 移入回收站）；重名自动加 `(n)` 序号
- 快捷方式 `.lnk/.url`、`desktop.ini`、隐藏文件默认不动
- 所有移动记入 SQLite `operation_logs`，撤销 = 反向移动
- 文件写入自动备份；破坏性 shell 命令必须征得同意
- 整盘与批量文件扫描会先询问是否维护元数据索引；索引只保存元数据，不读取文件内容
- 高速 NTFS/MFT 索引由短生命周期的只读辅助进程隔离执行，弹出 UAC 前必须获得明确同意；其空间结果来自 MFT `$DATA` 逻辑大小，属于带覆盖率的快速估算，精确结果需普通元数据扫描

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

首次运行后：托盘图标右键 → 打开主窗口 →「设置」页填写大模型 API（有 DeepSeek / 通义 / OpenAI 预设），保存后即可在聊天窗使用。

## 打包

```bash
npm run tauri:build
```

产物：`src-tauri/target/release/bundle/nsis/` 下的 NSIS 安装包。

> 注意：Windows 通知仅在**安装后**的应用上显示应用名和图标；直接运行 exe 或开发模式下通知会显示为 PowerShell 发起，这是系统限制。

## 数据位置

`%APPDATA%/com.deskhelper.win/`：

- `deskhelper.db`（SQLite：待办、规则、操作记录、设置、聊天记录）
- `file_index.db`（持久化文件元数据索引：路径、名称、大小、时间；不含文件内容）
- `tasks/`、`skills/`、`screenshots/`、`temp/`（AI 临时文件）、`file_backups/`、`ntfs-helper/`

API Key 仅保存在本机此数据库中，不会上传。

## 目录结构

```
src/                    React 前端
  windows/              FloatBall / ChatPanel / Reminder / MainWindow 四个窗口页面
  components/           主窗口标签页（待办 / 规则 / 记录 / 后台任务 / Skills / 设置）
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
  tasks.rs              后台命令；skills.rs + builtin_skills.rs  Agent Skills
  reader.rs / writer.rs / ocr.rs / machine.rs / tempfs.rs
src-tauri/builtin-skills/  内置 Skill 源文件（编译进应用）
browser-extension/      配套浏览器扩展（WebSocket 桥接桌面端）
scripts/gen-icon.cjs    应用图标生成脚本（node scripts/gen-icon.cjs）
scripts/build-index-helper.mjs  打包前构建 NTFS 索引 sidecar
```

## 规划

- 微信桌面端聊天记录监控（需评估 UIA / OCR 路线与合规风险）

## 开源协议

[MIT](LICENSE)
