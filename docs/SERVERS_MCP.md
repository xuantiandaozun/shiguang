# 服务器与本地 MCP

主窗口「服务器」展示连接列表和交互终端状态，可打开独立 SSH 终端、浏览远程目录、上传下载文件或目录。拾光内置 AI、Codex 和 Cursor 共用同一连接库及执行层。多标签 Shell、跳板机、断点续传和目录同步尚未实现。

## 首次使用

1. 新增连接，填写名称、主机、端口、用户名和认证方式。可使用密码、私钥文件或 SSH Agent。私钥保存本机绝对路径，不复制私钥内容。
2. 点击「测试连接」，通过可信渠道核对 SHA256 主机指纹后信任。首次信任和指纹变化后的重新信任只能在拾光界面完成，AI/MCP 无法直接修改信任。
3. 点击列表「连接」打开独立交互终端，支持连续命令、切换目录、Ctrl+C 和终端程序。每台服务器一个窗口，窗口内可断开、重连和清屏；关闭窗口将断开会话，但不保证远程进程停止。列表每两秒更新终端状态（不是服务器在线监测）。也可打开远程目录。上传目录的目标是目录根路径，例如本地 `D:\build` → `/srv/app` 会生成 `/srv/app/文件`。目录合并不会清除目标中的其他文件。
4. 列表提供独立「查看密码」（私钥认证为「查看口令」）按钮，在弹窗中查看和复制，关闭后不再显示。编辑连接时也可点击「查看」读取已保存密码；「清除」并保存会删除密码内容。编辑时不填写密码会保留原值；更换认证方式且未提供新密码时清空旧凭据。

按用户选择，密码和私钥口令**明文**保存在 `%APPDATA%/com.deskhelper.win/servers.db` 的 `server_secrets` 表中。连接资料、主机指纹和任务摘要也在此数据库，命令输出保存于 `remote-tasks/`。复制数据库即包含密码，请按个人密码文件管理。列表和普通查询不自动返回密码；界面的查看功能通过单独的本地 IPC 读取。

Windows 构建使用 libssh2 的系统加密后端。私钥格式和算法支持取决于该后端；遇到不支持的密钥格式时可先使用密码或兼容的 RSA PEM 私钥。键盘交互 MFA 不在此版本范围内。

## 终端文件面板与 AI 模式

独立 SSH 窗口采用左终端、右远程文件布局，支持原生标题栏和工具栏的最大化/还原。工具栏可隐藏/显示文件列表，并按服务器记住显示偏好；隐藏时终端或聊天区域自动占满，目录、预览及上传状态保留。分隔条支持拖动和左右方向键调整宽度。

- 命令模式明确转发键盘 Tab/Shift+Tab，并提供「Tab 补全」按钮；输入通过有序队列发送，避免补全键早于已输入前缀到达服务器。候选由远程 Shell 按真实当前目录生成，不使用右侧文件面板目录代替。收到补全相关响铃时提示可能存在多候选或无匹配；正常 TUI 的按键数据仍原样转发。Bash 4+ 连接会加载仅本会话生效的首词补全（Bash 4 使用兼容的 Readline 宏，Bash 5+ 使用首词补全接口）：如果前缀没有系统命令/别名/函数候选，则匹配 Shell 当前目录文件并自动补 `./`，如 `ruo` → `./ruoyi-proxy-linux-spoke`；只补文字，不自动执行。切换目录后按实际新目录匹配，空格和特殊字符由 Readline 转义。Bash 5 已有函数型首词补全会接续使用；Bash 4 保留原有常规补全动作，并在文件匹配时交给原生文件补全；特殊命令型规则或自定义 Tab 宏保留原样。参数补全和 TUI 的 Tab 行为不变。其它 Shell 或 Bash 3 及更早版本保留原生补全。连接后会显示增强是否真正启用，探测失败、注册失败及初始化超时不会静默降级。远程命令参数补全仍取决于 Shell 已有配置，不自动安装软件，不修改启动文件；已有连接需断开重连启用。测试：`node scripts/test-terminal-input.mjs`、`python scripts/test-terminal-completion.py`（需要 Bash 4+，可通过 `TERMINAL_TEST_BASH` 指定；已验证真实 Bash 4.4.12、Bash 5 的默认/兼容路径以及 Emacs/Vi 输入模式）。
- 支持路径跳转、上级目录、文件名筛选与刷新；目录优先显示，最多 2000 项。文件面板目录与终端 Shell 的当前目录独立。终端与 SFTP 不共用一个 SSH 会话；同目录自动刷新在后台完成，不禁用文件列表。重复刷新请求合并，刷新不会覆盖用户正在进行的目录切换。相关回归测试：`node scripts/test-terminal-files.mjs`。
- 普通文件可点击「预览」读取 UTF-8 文本，最多 128 KiB；不支持二进制、符号链接或其他编码。预览内容只在界面显示，不自动发送给模型。
- 「上传文件」支持多选本机文件，上传至文件面板当前目录。普通新文件直接传输；同名覆盖、敏感位置须确认。上传任务显示已传输字节数、结果和取消按钮；失败不自动重试。
- AI 模式下左侧完整切换为聊天，不显示后台命令、命令回显或原始输出；AI 通过当前 SSH 会话操作并整理结论，可连续追问。退出时停止自动操作、清除终端屏幕和滚动历史并恢复手动输入，右侧文件面板不变。
- 放行依据是操作影响，不是是否含管道、引号或是否在简单白名单中：进程/Java 服务/端口查询、日志筛选、只读命令管道及组合自动执行，即使模型误标敏感也不重复确认。普通目录创建、项目文件新增编辑复制移动等低风险操作按模型判断直接执行；普通文件写入命令在覆盖前备份。删除、服务启停、杀进程、权限账号变更、敏感文件修改及外发数据等仍确认。后台保留确定性高风险检查；复杂语法本身不等于风险，但无法解析的动态执行仍需确认。
- 敏感操作在聊天内展示目的、影响和同意/取消按钮，具体命令默认折叠。取消后不执行，不自动重复请求同一操作。
- 「选文件给 AI」仅提供用户选择的本机文件路径，不立即上传；再输入如“把选中的压缩包上传到 `/srv/app/release.zip`”。也可直接在目标中提供本机绝对路径和远程完整目标路径。
- 终端 AI 的上传是命令操作之外的明确例外，复用 SFTP 执行层。每次上传先展示来源、目标及覆盖影响，确认后启动，等待传输真实完成才继续后续命令。主聊天 AI/MCP 原有的 `sftp_upload` 工具保持可用。
- 模型使用设置中的当前配置。执行期间仅提供当前一步经过清理、限长的必要输出，加上本任务的滚动工作摘要；已消费的命令输出不会累计。每个模型响应将已确认事实、关键证据、明确标记的推测/待处理事项和相关绝对路径提炼到 `memory`。
- 任务完成后跨轮只保留最终摘要、相关路径、已确认工作目录和检查时间，最多最近 4 个任务；不保留命令回显、原始日志或完整执行链。聊天显示记录与模型记忆分开管理。易变的进程、端口、服务状态按需重新查询，旧摘要不当作实时结果。停止、取消或失败也只保留精简进度与阻塞项。
- 操作目标、上述摘要/必要输出及提供的本机文件路径会发送给模型；上传文件本体不发送。上下文整理测试：`node scripts/test-terminal-agent-context.mjs`。
- 收起文件面板不停止上传；关闭窗口仅断开交互终端，已启动传输可能继续运行。不需要的任务请先取消；取消或失败时，目录上传中已完成的文件可能仍然保留。

## Codex / Cursor 接入

复用现有 `shiguang-mcp.exe`，**拾光必须正在运行**。已有 MCP 配置的客户端重启 MCP 后即可看到新增工具，无需再配置一个服务。stdio 子进程通过带令牌的本机回环桥接调用桌面应用，不向局域网提供监听。

开发构建：

```powershell
cargo build --manifest-path src-tauri/Cargo.toml --bin shiguang-mcp
```

Codex `config.toml`：

```toml
[mcp_servers.shiguang]
command = "D:\\openspace\\deskHelper\\src-tauri\\target\\debug\\shiguang-mcp.exe"
```

Cursor `mcp.json`：

```json
{
  "mcpServers": {
    "shiguang": {
      "command": "D:\\openspace\\deskHelper\\src-tauri\\target\\debug\\shiguang-mcp.exe"
    }
  }
}
```

## 工具

| 工具 | 参数和用途 |
| --- | --- |
| `list_servers` | 列出连接，按名称、地址和分组选择，再使用稳定的 `id` |
| `get_server` | `server_id`，读取连接资料，不返回密码 |
| `create_server` | `name,host,username,auth` 必填；`port` 默认 22；可提供 `group,note,private_key,secret` |
| `update_server` | `server_id` 和需要修改的字段；未提供字段不变；`secret` 省略保留，空串清除 |
| `delete_server` | `server_id`，删除连接与密码，不操作远程文件；运行中任务存在时拒绝 |
| `test_server_connection` | `server_id`；返回 connected 或 needs_trust；未信任时不发送凭据 |
| `ssh_exec` | `server_id,command`，可选 `workdir,timeout_seconds`；超时默认 600 秒，最大 86400 秒 |
| `sftp_list` | `server_id,remote_path`，返回解析后的绝对 `path`、`entries`、`truncated`；目录最多返回 2000 项 |
| `sftp_stat` | `server_id,remote_path`，读取大小、权限、时间和类型 |
| `sftp_upload` / `sftp_download` | `server_id,local_path,remote_path`，可选 `overwrite` 默认 false；本地路径必须为绝对路径 |
| `list_remote_tasks` | 列出远程任务摘要 |
| `get_remote_task` | `task_id`，可选 `output_offset` 按字节读取；省略时返回日志尾部 |
| `wait_remote_task` | `task_id`，可选 `timeout_seconds`（默认 30，最大 60）和 `output_offset` |
| `cancel_remote_task` | `task_id`，请求中止并断开本地 SSH 连接 |

`ssh_exec` 和传输操作立即返回 `task_id`。后续始终使用原任务编号等待；`running` 不代表完成，`failed/cancelled/unknown` 不代表远程操作未发生。`wait_remote_task` 返回 `task`、`output`、`next_offset`、`has_more`。传 `output_offset=0` 从日志开头读取，后续传上次的 `next_offset`；不传则只读尾部。stdout/stderr 合并，保留退出码；每项命令日志上限 64 MiB。

AI/MCP 的 `ssh_exec` 每次命令使用独立 Shell，与桌面交互终端隔离，Shell 语法及 `workdir` 切换面向 POSIX 服务器。需要持续状态的操作请在同一次命令里完成。AI 直接调用服务器工具，不必再委托拾光模型转述命令。

## 执行约定

- 最多并行 8 项命令/传输；运行中的服务器连接不能修改或删除。
- 取消、超时、客户端断线、应用退出均不能保证远程进程已经停止。需要先查实际进程或结果，不能自动重试部署等有副作用的命令。
- 应用重启后，原运行中任务标记为 `unknown`，不会自动重新执行。启动恢复最近 200 项任务摘要。
- 文件通过同目录临时文件传输，成功后改名；默认拒绝覆盖。目录传输按文件提交，中途失败时已完成文件保留，无法整目录回滚。
- 部分 SFTP v3 服务器不支持直接覆盖改名，此时先暂存旧文件再提交新文件；替换期间目标路径会短暂不存在。替换失败会尝试恢复旧文件，断线导致恢复失败时错误会标明旧文件备份路径。
- 上传覆盖会尽量沿用原文件权限，新文件默认 0644。符号链接和特殊文件不参与递归传输；不支持的 Windows 文件名返回错误，避免静默改名。
- 覆盖、远程命令的授权由调用方根据用户意图判断；MCP 桥接令牌和 SSH 主机指纹不代替用户授权。信任的本地 MCP 客户端可使用已保存连接的权限。

## 验证

```powershell
npm run build
cargo check --manifest-path src-tauri/Cargo.toml --bins --lib
cargo test --manifest-path src-tauri/Cargo.toml --lib
```

本机集成测试使用 `scripts/ssh-test-fixture.py`，只监听 `127.0.0.1`，生成一次性账号和密钥，不访问真实服务器。将 paramiko 安装到 `%APPDATA%/com.deskhelper.win/temp/ssh-test-python` 后运行：

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --lib servers::tests::loopback_ssh_sftp -- --ignored --nocapture
```
