# 服务器与本地 MCP

主窗口「服务器」管理 SSH 连接，可执行非交互命令、浏览远程目录、上传下载文件或目录。拾光内置 AI、Codex 和 Cursor 共用同一连接库及执行层。交互终端、多标签 Shell、跳板机、断点续传和目录同步尚未实现。

## 首次使用

1. 新增连接，填写名称、主机、端口、用户名和认证方式。可使用密码、私钥文件或 SSH Agent。私钥保存本机绝对路径，不复制私钥内容。
2. 点击「测试连接」，通过可信渠道核对 SHA256 主机指纹后信任。首次信任和指纹变化后的重新信任只能在拾光界面完成，AI/MCP 无法直接修改信任。
3. 执行命令或打开远程目录。上传目录的目标是目录根路径，例如本地 `D:\build` → `/srv/app` 会生成 `/srv/app/文件`。目录合并不会清除目标中的其他文件。
4. 编辑连接时点击「查看」读取已保存密码，可以选择复制；「清除」并保存会删除密码内容。编辑时不填写密码会保留原值；更换认证方式且未提供新密码时清空旧凭据。

按用户选择，密码和私钥口令**明文**保存在 `%APPDATA%/com.deskhelper.win/servers.db` 的 `server_secrets` 表中。连接资料、主机指纹和任务摘要也在此数据库，命令输出保存于 `remote-tasks/`。复制数据库即包含密码，请按个人密码文件管理。列表和普通查询不自动返回密码；界面的查看功能通过单独的本地 IPC 读取。

Windows 构建使用 libssh2 的系统加密后端。私钥格式和算法支持取决于该后端；遇到不支持的密钥格式时可先使用密码或兼容的 RSA PEM 私钥。键盘交互 MFA 不在此版本范围内。

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
| `sftp_list` | `server_id,remote_path`，目录最多返回 2000 项 |
| `sftp_stat` | `server_id,remote_path`，读取大小、权限、时间和类型 |
| `sftp_upload` / `sftp_download` | `server_id,local_path,remote_path`，可选 `overwrite` 默认 false；本地路径必须为绝对路径 |
| `list_remote_tasks` | 列出远程任务摘要 |
| `get_remote_task` | `task_id`，可选 `output_offset` 按字节读取；省略时返回日志尾部 |
| `wait_remote_task` | `task_id`，可选 `timeout_seconds`（默认 30，最大 60）和 `output_offset` |
| `cancel_remote_task` | `task_id`，请求中止并断开本地 SSH 连接 |

`ssh_exec` 和传输操作立即返回 `task_id`。后续始终使用原任务编号等待；`running` 不代表完成，`failed/cancelled/unknown` 不代表远程操作未发生。`wait_remote_task` 返回 `task`、`output`、`next_offset`、`has_more`。传 `output_offset=0` 从日志开头读取，后续传上次的 `next_offset`；不传则只读尾部。stdout/stderr 合并，保留退出码；每项命令日志上限 64 MiB。

每次命令使用独立 Shell，Shell 语法及 `workdir` 切换面向 POSIX 服务器。需要持续状态的操作请在同一次命令里完成。AI 直接调用服务器工具，不必再委托拾光模型转述命令。

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
