# 工作区与外派智能体

## 目标

- **工作区**：记住固定项目「在哪、怎么验证、怎么上线、有什么规矩」，说「改博客」就知道改哪里，不用每次报路径。
- **外派**：拾光把工作区里的代码修改交给 Claude Code / Codex CLI，用各自的订阅额度；拾光负责交代任务、回答它的问题、核对结果、征得同意后提交。
- 方向区分：现有「委托」（`delegation.rs`）是**外部智能体把活交给拾光**；本功能是**拾光把活外派出去**。

## 工作区（`workspaces.rs`）

表 `workspaces(id, name UNIQUE, root, data_json, last_used_at, created_at, updated_at)`；`data_json` 存可演进字段：

| 字段 | 说明 |
|---|---|
| aliases | 别名（博客、blog、域名…），用于匹配用户说法 |
| summary | 一句话说明 |
| deploy | 部署方式，如「push main 即 Vercel 上线」 |
| verify_commands | 验证命令，如 `npm run build` |
| tools | 配套 CLI、配置文件路径（只记路径，不抄密钥） |
| links | 相关网址（站点、后台） |
| rules | 必须遵守的规则（push 前问我…） |
| notes | 外派与任务中沉淀的经验 |
| default_agent / agent_mode | 默认外派给谁、默认权限档 |

只存仓库里读不到的信息；`AGENT.md`/`CLAUDE.md`/README、git 状态在打开时现读。

**匹配与绑定**：表 `session_workspaces(session_id, workspace_id)` 记录会话当前工作区。每轮先按别名/名称在用户消息里精确匹配；没命中且配置了 Jev，就在 `plan_turn` 的同一请求里加一道「指哪个工作区」的选择题。命中即绑定，本轮末尾 system 消息附工作区摘要；绑定后 `run_command`/`run_command_background` 未给 workdir 时默认在工作区根目录执行。

**目录**：有工作区时，工作区目录作为稳定 system 消息跟在技能目录后（集合不变则前缀缓存不变）。

**工具**（类别 `workspaces`；`workspace_open` 属核心工具）：
- `workspace_list`、`workspace_open`（绑定并返回详情+现读上下文）
- `workspace_detect`（读 git remote、package.json scripts、vercel.json、说明文件，生成草稿）
- `workspace_save`（新建/更新；**弹确认卡片**，用户同意才写入；`append_notes` 只追加经验也需确认）

主窗口「工作区」标签页可手动增删改、从文件夹识别、查看智能体探测结果。

## 外派（`agents/`）

### 智能体探测
启动或打开标签页时按 PATH/PATHEXT 找 `claude`、`codex`，读 `--version`。设置键 `code_agents` 存启用状态与路径覆盖。

### 适配器
| | Claude Code | Codex |
|---|---|---|
| 启动 | `claude -p --input-format stream-json --output-format stream-json --verbose`，常驻进程，每句话写一行 JSON 到 stdin | `codex exec --json --skip-git-repo-check -c sandbox_mode=…`，每轮一个进程 |
| 续接 | 进程在就直接写 stdin；进程没了用 `--resume <session_id>` | `codex exec resume <thread_id> --json` |
| 权限 plan | `--permission-mode plan` | `sandbox_mode="read-only"` |
| 权限 edit（默认） | `acceptEdits`；允许只读 git 和工作区验证命令；禁止 git commit/push/reset | `sandbox_mode="workspace-write"`（无网络，push 不可能） |
| 回调 | `--mcp-config` 注入 `shiguang-mcp.exe`（`SHIGUANG_AGENT_RUN` 环境变量进入外派模式） | `-c mcp_servers.shiguang.*` 同上，`tool_timeout_sec=1800` |
| 事件 | `system/init`、`assistant`（text / tool_use）、`user`（tool_result）、`result` | `thread.started`、`item.*`（agent_message / command_execution / file_change / mcp_tool_call…）、`turn.completed` |

空闲 30 分钟的 Claude 进程自动关闭，之后有追问再 `--resume`。

### 连续沟通
- **主 AI → 智能体**：`agent_reply`。智能体在等回答就作为回答；一轮已结束就作为新一轮追问（同一会话）。
- **智能体 → 主 AI**：外派模式的 MCP 只提供两个工具：`ask_main(question)`、`report_checkpoint(milestone, done, evidence, next)`。调用后进程阻塞等待；运行状态变成 `asking`，主 AI 的 `await_agent` 返回问题，回答后智能体继续。
- 每个外派最多 12 次来回，超出后 `ask_main` 直接返回「自行判断或结束汇报」。
- 主 AI 不能替用户批准 push、删除、扩大范围；这类问题必须 `ask_user`。

### 运行与结果
表 `agent_runs`（状态、外部会话 id、基线 HEAD、问题、结果）与 `agent_run_events`（运行窗口的事件流）。状态：`running` / `asking` / `idle`（本轮结束可追问）/ `error` / `cancelled` / `closed`。

工具（类别 `agents`）：
- `delegate_to_agent {task, acceptance?, workspace?, agent?, mode?, isolation?, allow_dirty?, await?}`
- `await_agent {run_id}`、`agent_reply {run_id, content}`、`stop_agent {run_id}`、`list_agents`
- `finish_agent_worktree {run_id, action: apply|discard}`（隔离运行时）

外派前：工作区有未提交改动且未给 `allow_dirty` 时拒绝，并列出改动让主 AI 先问用户；记录基线 HEAD。
结束时拾光自己算 `git diff --stat`、改动文件、HEAD 是否被改（智能体私自提交会标红），并附上工作区的验证命令提示；**提交和 push 由拾光在用户确认后执行**。

运行窗口（`#/agent-run?id=`）实时显示文字、工具调用、改动；可中断，可直接给智能体发话（以用户身份）。

### 隔离（worktree）
`isolation: "worktree"`：在 `app_data/worktrees/<run>` 建 `git worktree`（分支 `shiguang/<run>`），智能体在里面改。结束后 `finish_agent_worktree`：
- `apply`：在 worktree 里 `git add -A` 后取 `git diff --cached --binary`，在原仓库 `git apply --3way`；
- `discard`：`git worktree remove --force` 并删分支。

### 自动选择
未指定 agent 时：工作区 `default_agent` → 已启用的 Claude Code → Codex。

## 何时外派
工作区里涉及多文件的代码修改优先外派；查数据、浏览器、跑 CLI、小改一两行仍由拾光自己做。
