# 拾光（ShiGuang）— AI 桌面助手（一期）

一个常驻 Windows 托盘的 AI 桌面助手：悬浮球 + 聊天小窗与 AI 对话，帮你**整理桌面文件**、**管理待办并准时提醒**。

## 功能一览

| 模块 | 说明 |
| --- | --- |
| AI 桌面整理 | 对 AI 说「帮我整理一下桌面」→ 生成分类方案 → 你在聊天卡片里确认后执行；全程只移动不删除，支持按批次一键撤销 |
| 自动化规则 | 方案确认后对 AI 说「以后都按这个规则来」，或在主窗口手动建规则；之后桌面新文件命中规则自动归类（仅已审核规则生效，可全局暂停） |
| 待办事项 | 自然语言创建：「明天下午三点提醒我交周报」；支持重复（每天/每周）、优先级、延后提醒；到点弹 Windows 通知 |
| 悬浮球 | 桌面常驻小圆球，可拖动，单击展开/收起聊天窗；聊天窗、主窗口关闭后程序仍在托盘运行 |
| 主窗口 | 待办管理 / 整理规则 / 操作记录（撤销）/ 设置 |

## 安全设计

- 任何整理执行前必须人工确认（聊天面板里的方案卡片）
- 只移动文件、绝不删除；重名自动加 `(n)` 序号
- 快捷方式 `.lnk/.url`、`desktop.ini`、隐藏文件默认不动
- 所有移动记入 SQLite `operation_logs`，撤销 = 反向移动

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

> 注意：Windows 通知仅在**安装后**的应用上显示应用名和图标；开发模式下通知会显示为 PowerShell 发起，这是系统限制。

## 数据位置

`%APPDATA%/com.deskhelper.win/deskhelper.db`（SQLite：待办、规则、操作记录、设置、聊天记录）。
API Key 仅保存在本机此文件中，不会上传。

## 目录结构

```
src/                    React 前端
  windows/              FloatBall / ChatPanel / MainWindow 三个窗口页面
  components/           主窗口四个标签页
  lib/ipc.ts            前端 invoke/事件封装
  stores/chat.ts        聊天状态（zustand）
src-tauri/src/
  lib.rs                应用装配：插件、托盘、后台任务
  commands.rs           IPC 命令层
  db.rs                 SQLite（rusqlite）
  llm/                  OpenAI 兼容客户端（SSE + Function Calling）、Agent 循环、工具集、提示词
  organizer/            桌面扫描、方案执行/撤销、规则引擎、文件监听
  todo/scheduler.rs     待办提醒调度
scripts/gen-icon.cjs    应用图标生成脚本（node scripts/gen-icon.cjs）
```

## 二期规划（架构已预留，未实现）

- 微信桌面端聊天记录监控（需评估 UIA / OCR 路线与合规风险）
- 浏览器操作（CDP 或浏览器扩展），作为新工具注册进 LLM 工具集即可

## 开源协议

[MIT](LICENSE)
