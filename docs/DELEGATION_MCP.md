# 拾光委托 MCP

启动拾光桌面应用后，Codex 或 Cursor 可以通过 `shiguang-mcp.exe` 把任务委托给拾光。每项委托使用独立任务记录，不进入拾光主聊天会话。新委托会自动打开独立对话窗口，用户可在其中查看 Codex/Cursor 与拾光的交流、补充意见或中断任务；关闭窗口不会中断任务，主窗口的「委托任务」页保留历史记录并可重新打开对话。拾光遇到需要委托方提供项目上下文的问题时会暂停；外部智能体读取问题并回复，拾光继续执行。

不同委托可并行，当前最多同时运行 4 项，其余等待空位。同一委托的消息按顺序处理。

委托窗口标题栏支持最小化，任务继续执行，可从 Windows 任务栏或「委托任务」页恢复。同一任务的补充意见和中断操作串行处理；每个工具结果及时保存，中断后继续时会补齐未返回的工具记录，并要求先核实实际状态，避免重复写入或发布。委托可读取已启用的技能目录，重复工具调用会收到换方法提醒；浏览器超时和失效目标会附带恢复指引。步骤预算临近用完时会提示收尾。

## 本地开发

运行 `npm run sidecar:dev` 会构建 MCP 程序到 `src-tauri/binaries/shiguang-mcp-x86_64-pc-windows-msvc.exe`。也可以直接运行 `cargo build --manifest-path src-tauri/Cargo.toml --bin shiguang-mcp`，使用 `src-tauri/target/debug/shiguang-mcp.exe`。

拾光必须正在运行。MCP 程序只连接当前 Windows 用户的本机拾光实例；桥接地址和随机令牌由拾光写入其应用数据目录。不要把这个文件分享给其他人。

## Codex 配置示例

在 `~/.codex/config.toml` 中加入（替换成实际的绝对路径）：

```toml
[mcp_servers.shiguang]
command = "D:\\openspace\\deskHelper\\src-tauri\\target\\debug\\shiguang-mcp.exe"
```

## Cursor 配置示例

在用户或项目的 `mcp.json` 中加入（替换成实际的绝对路径）：

```json
{
  "mcpServers": {
    "shiguang": {
      "command": "D:\\openspace\\deskHelper\\src-tauri\\target\\debug\\shiguang-mcp.exe"
    }
  }
}
```

安装版随应用打包 `shiguang-mcp.exe`。配置客户端时指向安装目录中的该程序。

## 委托循环

1. `delegate_to_shiguang` 创建任务，得到 `id`。
2. `wait_shiguang_delegation` 等待状态变化。超时仍为 `running` 时可以再次等待；不用新建任务。
3. 若状态为 `waiting_peer`，读取 `question`，利用当前项目上下文回答，然后调用 `reply_shiguang_delegation`。
4. 若状态为 `waiting_user`，拾光正在独立窗口等待用户确认；委托方不能代答，待用户回答后继续等待。
5. `blocked` / `error` 时先检查 `result`，用 `resume_shiguang_delegation` 携带原 `id` 和可选的 `content` 继续，保留原目标和历史。步骤耗尽可不填 `content`；真实阻碍先补充信息或修复，避免无限重试。任务仍在运行时 resume 只返回当前状态，不会重启。
6. 继续等待直到 `complete` 或 `cancelled`，或真实阻碍需要用户处理。不要把暂停或出错当成完成，也不要为同一目标另起委托。丢失任务 ID 时先 list/get 核实原任务。MCP 返回的 `continuation` 提供建议工具与原任务 ID；`waiting_user` 的恢复必须由用户回答。

`get_shiguang_delegation` 返回完整交流记录。委托使用拾光当前启用的完整工具集，包括文件写入、浏览器操作、命令、待办、技能和工作流；工具仍受拾光现有设置和用户确认规则约束。委托方可以补充项目上下文，但需要用户授权的动作应由用户确认。

## 精准委托与关键节点

Codex 委托技能维护版本位于 [docs/codex-skills/shiguang/SKILL.md](codex-skills/shiguang/SKILL.md)。委托方先提供准确目标、对象和环境、已知项目事实、允许/排除范围、产出绝对路径和验收标准。源码分析与目录定位由委托方负责；除明确委托的源码任务外，拾光不主动搜源码或全盘找文件。缺信息用 `ask_peer` 提问。

复杂任务约定少量会影响方向或外部结果的关键节点。拾光单独调用 `report_checkpoint`，提供 `milestone`、`summary`、非空 `evidence` 数组及 `next_step` 后进入 `waiting_peer`，等待委托方审核；节点与其他工具一起调用时，该批次均不执行。`get` / `wait` 的 `checkpoint` 包含结构化回传，`question` 提供可读内容。委托方用原 ID 的 `reply_shiguang_delegation` 批准下一阶段范围或纠偏。普通节点审核不等于新增用户授权。

阶段内部独立执行，不逐次查询工具历史或盯屏。`wait_shiguang_delegation` 仅返回状态、问题、节点与结果；完整记录按需通过 `get_shiguang_delegation` 读取。简单只读任务可以直接返回最终结果。

`todo_write` 的条数、长度或状态参数错误会返回错误与修正指引，不再使整项委托进入 `error`。模型可缩短/合并清单继续执行，委托方也无需另起任务。
