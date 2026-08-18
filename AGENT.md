# AGENT.md — 拾光（ShiGuang）项目速览

> 给 AI 协作者的精简上下文，读这份即可上手，无需重新通读全项目。

## 定位与技术栈
常驻 Windows 托盘的 AI 桌面助手：悬浮球 + 聊天窗，整理桌面文件、待办提醒、操作浏览器。
Tauri 2 (Rust) + React 18 + TS + Vite + Tailwind + Zustand + SQLite (rusqlite bundled)。
LLM 走 OpenAI 兼容接口（默认 DeepSeek），Function Calling + SSE 流式。

## 目录结构
- `src/` React 前端
  - `windows/` FloatBall / ChatPanel / Reminder（待办提醒弹窗）/ MainWindow 四个窗口
  - `components/` 主窗口标签页（Todos / Rules / History / Tasks / Skills / Settings）
  - `lib/ipc.ts` 前后端接口的唯一声明处（invoke + 事件）
  - `stores/chat.ts` 聊天状态（zustand）
- `src-tauri/src/`
  - `lib.rs` 装配：插件、托盘、AppState、IPC 注册
  - `commands.rs` IPC 命令层 + `Settings` 结构（设置存 SQLite settings 表，key-value）
  - `db.rs` SQLite 全部读写
  - `llm/` `agent.rs` 主代理循环 / `client.rs` SSE 客户端 / `tools.rs` 工具定义+执行 / `subagent.rs` 子代理 / `prompts.rs` 系统提示词 / `profile.rs` 个人信息 / `vision.rs` 视觉模型
  - `organizer/` 桌面整理：scanner / executor / rules / watcher
  - `browser/` 浏览器：扩展桥 ext + CDP cdp/launch；page-api + Readability（browser_read 抽正文）
  - `tasks.rs` 后台命令；`machine.rs` 本机信息；`lookup_cache.rs` 外部对照数据缓存；`skills.rs` + `builtin_skills.rs` Agent Skills；`reader.rs` / `writer.rs`；`ocr.rs`
- `src-tauri/builtin-skills/` 内部 Skills 源文件（改完需重新编译打包才生效）

## 关键架构
1. **主代理循环** `agent.rs::run_chat`：上限 50 轮；`trim_context` 压缩早期工具结果。事件：`llm-token` / `llm-reasoning` / `llm-message-done` / `llm-error` / `llm-cancelled` / `tool-status`。
2. **工具系统** `tools.rs`：新增工具四步 = definitions → execute → prompts（仅非显而易见约束）→ ChatPanel `TOOL_LABELS`。
3. **子代理** `subagent.rs`：只读白名单 + 15 轮 + 5 分钟；`Box::pin` 打破异步递归。
4. **后台任务** `tasks.rs`：`run_command` / `run_command_background` / `check_task` / `list_tasks` / `stop_task`；开关 `command_tools_enabled`；命令日志按 UTF-8/GBK 自适应解码。
5. **外部参考缓存** `lookup_cache`：按稳定 key 保存 CLI/API 提炼后的对照表（id↔名称、字段/选项），默认 7 天；目录注入对话末尾。不自动缓存原始命令输出。
6. **待办提醒** `todo/scheduler.rs`：30s 轮询到期，按 `todos.remind_mode` 分发——`notify` 系统通知；`popup`/`popup_input` 发 `reminder-popup` 事件给 reminder 弹窗。`popup_input` 的输入作为聊天消息发给 AI（调 `send_chat_message`，不落本地），发完 `show_chat_window` 展开聊天窗；重复任务提醒即顺延、一次性任务标 reminded。
7. **本机信息** `get_system_info`：便利封装，与 `run_command` 等价可选，不在系统提示词强制引导。
8. **Agent Skills**（两类）
   - **internal**：`builtin_skills.rs` + `include_str!` 编译期嵌入；只读；AI/UI 禁 create/覆盖/删除；可启停。新增：在 `builtin-skills/<name>/SKILL.md` 写文件并在 `builtin_skills::ALL` 登记。现有：`desktop-organize`（整理原则）、`windows-cli`（Windows 下用 argv/stdin 调用外部程序，避免拼命令字符串）。
   - **external**：`app_data/skills/`；AI 可 create/覆盖/删除；可从 Claude/Codex/Cursor 同步；承接旧「工作流经验」一次性迁移（`migrate_workflows`，设置键 `workflows_migrated_to_skills`）。
   - 启用技能目录注入对话末尾；命中 `load_skill`。完整完成后用 `create_skill` 沉淀经验（取代旧 save_workflow）。
9. **系统提示词**：只写产品约束；参数/细则放 tools 与 Skills。整理细则在内部 skill `desktop-organize`。
10. **设置三处同步**：`commands.rs` ↔ `ipc.ts` ↔ `SettingsTab.tsx`。事件命名 `xxx-changed`。

## DeepSeek 兼容要点
- thinking / reasoning_effort 只对含 deepseek 的 base_url 附加。
- 思考模式开启时工具轮次须回传 `reasoning_content`。
- 文本形式工具调用：`ToolTextFilter` + `strip_tool_call_text`，勿删。
- 系统提示词逐字节稳定；动态信息放对话末尾 system 消息。

## 安全红线
- 整理只移动不删除（delete=回收站），执行前用户确认。
- 文件写入自动备份；破坏性 shell 须征得同意。
- 内部 Skill 运行时不可被 AI 篡改。
- 临时/中间产物只写 `app_data/temp/`（create_file 相对路径默认落此）；禁止堆到桌面。任务收尾须询问是否清理；设置页可一键清空。

## 常用命令
- 开发：`npm run tauri:dev`
- 检查：`cargo check --manifest-path src-tauri/Cargo.toml`、`npx tsc --noEmit`
- 测试：`cargo test --manifest-path src-tauri/Cargo.toml --lib`
- 打包：`npm run tauri:build`

## 数据位置
`%APPDATA%/com.deskhelper.win/`：`deskhelper.db`、`tasks/`、`skills/`（仅外部）、`lookup-cache.json`、`screenshots/`、`temp/`（AI 临时文件）、`file_backups/`。
