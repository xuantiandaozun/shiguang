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
  - `tasks.rs` 后台命令；`machine.rs` 本机信息；`lookup_cache.rs` 外部对照数据缓存；`skills.rs` + `builtin_skills.rs` Agent Skills；`reader.rs` / `writer.rs`；`ocr.rs`；`retention.rs` 工具结果头尾保留与落盘；`repeat_guard.rs` 重复工具劝告；`ask_user.rs` 聊天内选择题；`session_todo.rs` 会话内任务进度（与提醒待办分表）；`compact.rs` 对话摘要压缩；`web.rs` 公开网页搜索与抓取
- `src-tauri/builtin-skills/` 内部 Skills 源文件（改完需重新编译打包才生效）
- `src-tauri/sidecars/` 独立辅助程序 Cargo 包：两个 bin 复用 `src/bin/` 源码，索引助手仅编译 `ntfs_helper` 的协议/执行部分和 `ntfs_usn`；不依赖桌面库，也不运行 Tauri build.rs。`scripts/build-index-helper.mjs` 使用 `target/sidecars/` 独立缓存，内容变化才同步到 `binaries/` 和主程序同级。主包的默认 `desktop-client` feature 启用 NTFS 提权客户端。发版修改版本时同步 sidecars/Cargo.toml。

## 关键架构
1. **主代理循环** `agent.rs::run_chat`：上限 50 轮；超长工具结果保留头尾并落入 `temp/tool-spills/`，对话过长时再压缩较早的工具结果（`retention.rs`）。对话仍然过长时调用模型把较早历史收成 `<compacted-summary>` 检查点（`compact.rs`，不改聊天界面记录）。同一工具+相同参数连续 3/5/8 次时追加劝告、不拦截（`repeat_guard.rs`；`list_todos` 等记账工具对计数透明）。需要确认时 `ask_user` 在聊天里弹出选项卡片并阻塞等待（`ask_user.rs`）。多步任务用 `todo_write` 整表替换当前进度，聊天窗显示进度条（`session_todo.rs`，不入库；下一轮用户消息清空；与提醒待办分表）。事件：`llm-token` / `llm-reasoning` / `llm-message-done` / `llm-error` / `llm-cancelled` / `tool-status` / `ask-user` / `ask-user-settled` / `session-todos` / `subagent-finished` / `subagent-chat`。
2. **工具系统** `tools.rs`：新增工具四步 = definitions → execute → prompts（仅非显而易见约束）→ ChatPanel `TOOL_LABELS`。运行时注册表全量和 System One 分类筛选都须经过 `registry_definitions` 的兼容升级，不能在分类分支直接返回持久化旧 schema；核心发现入口保留，可通过 `discover_capabilities` 补齐跨领域类别（如 files + servers），不得把路由裁剪误判为无工具。
3. **子代理** `subagent.rs`：只读白名单 + 15 轮 + 5 分钟；`Box::pin` 打破异步递归。可 `run_in_background`；结束返回 `{status: complete|blocked, summary, findings, evidence, blockers}`。`await_subagent` 挂起主对话直到结束。中断聊天会取消进行中的子代理。
4. **后台任务** `tasks.rs`：`run_command` / `run_command_background` / `await_task` / `check_task` / `list_tasks` / `stop_task`；长任务用 await 挂起对话、结束后自动继续，不要靠 check_task 轮询。开关 `command_tools_enabled`；命令日志按 UTF-8/GBK 自适应解码。stdout JSON 由 `cli_json.rs` 解析。
5. **外部参考缓存** `lookup_cache`：按稳定 key 保存 CLI/API 提炼后的对照表（id↔名称、字段/选项），默认 7 天；目录注入对话末尾。不自动缓存原始命令输出。
6. **待办提醒** `todo/scheduler.rs`：30s 轮询到期，按 `todos.remind_mode` 分发——`notify` 系统通知；`popup`/`popup_input` 发 `reminder-popup` 事件给 reminder 弹窗。`popup_input` 的输入作为聊天消息发给 AI（调 `send_chat_message`，不落本地），发完 `show_chat_window` 展开聊天窗；重复任务提醒即顺延、一次性任务标 reminded。会话内 `todo_write` 进度不是提醒待办，不要写入 `todos` 表。
7. **本机信息** `get_system_info`：便利封装，与 `run_command` 等价可选，不在系统提示词强制引导。
8. **Agent Skills**（两类）
   - **internal**：`builtin_skills.rs` + `include_str!` 编译期嵌入；只读；AI/UI 禁 create/覆盖/删除；可启停。新增：在 `builtin-skills/<name>/SKILL.md` 写文件并在 `builtin_skills::ALL` 登记。现有：`desktop-organize`（整理原则）、`windows-cli`（Windows 下用 argv/stdin 调用外部程序，避免拼命令字符串）。
   - **external**：`app_data/skills/`；AI 可 create/覆盖/删除；可从 Claude/Codex/Cursor 同步；承接旧「工作流经验」一次性迁移（`migrate_workflows`，设置键 `workflows_migrated_to_skills`）。
   - 启用技能目录作为独立 system 消息插在系统提示之后、历史之前（启用集不变则前缀缓存稳定）；命中则 `load_skill`。对话里已有 `<skill_content>` 则不要再 load。完整完成后用 `create_skill` 沉淀经验（取代旧 save_workflow）。
9. **系统提示词**：只写产品约束；参数/细则放 tools 与 Skills。整理细则在自带可编辑外部 skill `desktop-organize`（`default-skills` 首次释放到用户目录，后续不覆盖）；旧自动规则监听、工作流调度和相关 AI 工具已停用。
10. **设置三处同步**：`commands.rs` ↔ `ipc.ts` ↔ `SettingsTab.tsx`。事件命名 `xxx-changed`。
11. **服务器 SSH/SFTP**：`servers.rs::ServerHub` 统一提供连接管理、SSH 命令、目录浏览和文件/目录传输；独立 `servers.db` 保存连接、主机指纹、任务摘要及用户要求的明文密码（`server_secrets`），界面通过独立 IPC 查看密码，普通 AI/MCP 查询不返回。`server_tools.rs` 为内置 AI 与 stdio MCP 共用 schema；`mcp_bridge` 的 `server_tool` 直接调用执行层，不经委托模型。首次/变化主机指纹仅桌面可确认；任务立即返回 task_id，用 wait_remote_task 等待，重启后运行中任务变为 unknown，取消不保证远程进程停止。主窗口 `ServersTab.tsx`；独立终端 `windows/ServerTerminal.tsx`，右侧 `components/ServerFiles.tsx` + `lib/useTerminalFiles.ts` 管理 SFTP 列表、UTF-8 预览和上传。`server_terminal.inc.rs` 提供窗口/服务器隔离的文件 IPC、终端 AI 步进及上传提案；`terminal_command_policy.rs` 按操作影响判风险，已知只读管道/引号查询覆盖模型误报，普通文件操作可按模型低风险判断放行，高风险保底确认。`lib/useTerminalAgent.ts` 管理连续对话/确认/后台执行；`lib/terminalAgentContext.ts` 限制本步输出并生成只含最终摘要、路径、工作目录、检查时间的检查点（最近4任务），不跨轮保存原始日志/命令链，模型每步返回精简 `memory`，测试 `node scripts/test-terminal-agent-context.mjs`；`components/ServerTerminalChat.tsx` 为 AI 模式完整左侧聊天；AI 期间不渲染终端输出，退出自动清屏恢复手动输入；`server_terminal_completion.rs` + `terminal_completion.bash` 为 Bash 4+ 会话临时注册首词文件补全（4 用 Readline 宏，5+ 用 `complete -I`；通过 poll 的 `completion` 状态确认是否真正启用），无系统命令候选时自动加 `./`，不写启动文件、不执行候选，原有参数/TUI 按键保持原生；AI 上传须确认来源/目标并等待原传输完成后继续，文件面板目录独立于 Shell。文档 `docs/SERVERS_MCP.md`。本机联调脚本 `scripts/ssh-test-fixture.py`。

## 发布
- 默认本地 `npm run release:local`，上传草稿 Release 后通过 `release.yml` 的 `sign-local` 校验哈希并签名，不在 GitHub 重复编译。详见 `docs/LOCAL_RELEASE.md`。备用手动 `mode=build`。

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
- 辅助程序测试：`cargo test --manifest-path src-tauri/sidecars/Cargo.toml --target-dir src-tauri/target/sidecars --locked --bins`
- 构建 EXE（不生成安装程序）：`npm run tauri:build`
- 构建安装程序（含更新产物）：`npm run tauri:bundle`
- 打包约定：用户说「打包」「打个包」「重新打包」时，默认只执行 `npm run tauri:build`，生成 EXE，不生成安装程序。只有明确要求发版、更新 GitHub 版本或生成安装包时，才执行 `npm run tauri:bundle`。发布、上传等操作按用户要求另行执行，构建安装包不代表已发布。

## 数据位置
`%APPDATA%/com.deskhelper.win/`：`deskhelper.db`、`tasks/`、`skills/`（仅外部）、`lookup-cache.json`、`screenshots/`、`temp/`（AI 临时文件，含 `tool-spills/` 超长工具结果全文）、`file_backups/`。
