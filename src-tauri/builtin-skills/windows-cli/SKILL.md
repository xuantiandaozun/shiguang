---
name: windows-cli
description: >
  在 Windows 上用 run_command 执行命令、调用外部程序或编写 PowerShell 时加载。
  说明优先用 argv 避免引号问题、JSON 走 stdin 或文件、以及何时才用 cmd/PowerShell 脚本。
---

# Windows 命令调用

通过 `run_command` 执行外部程序或脚本时使用本技能。不要把它当成某个具体 CLI 的手册；子命令以该程序自己的 `--help` 为准。

## 选择调用方式

按这个顺序选，不要一上来拼一整条命令字符串：

1. **外部程序 → `argv`**。每个参数独立一项，例如 `["git", "commit", "-m", "说明文字"]`。JSON、空格、中文都不必转义。系统直启进程，按 PATHEXT 解析 exe/cmd，不会命中同名 `.ps1`。
2. **结构化正文 → `stdin`**，或先 `create_file` 再把路径作为 argv 的一项。不要把 JSON 拼进命令行。
3. **PowerShell 逻辑**（`$变量`、管道、对象、多行）→ `command` + `shell=powershell`。动态值放 `script_args`，脚本里读 `$DHArgs`。真实路径用 `-LiteralPath`。
4. **cmd 内建命令**（`dir`、`copy`、`for`）→ `command` + `shell=cmd`。
5. 不要用 `cmd /c` 再包一层 PowerShell，也不要 `powershell -Command "..."`。

`argv` 第一项只放程序名。错误：`["git status -sb"]`。正确：`["git", "status", "-sb"]`。

## PowerShell

- 管道无匹配时是 `$null`。取 `.Count` 前必须 `$hits = @($lines | Where-Object { ... })`。
- 不要为了 `.Count` 把 `powershell_strict` 设为 false。
- 调用外部程序时不要 `& 命令名 ...`；改回 `argv`。
- 在脚本里调用原生程序时用 `& $exe @argArray`，不要把参数拼成一个大字符串。

## 读与写

- 需要按中文名称匹配时，让程序输出 JSON 或其它机器可读格式；不要用可能乱码的 markdown/表格做匹配。
- 写操作先用该程序的预览/dry-run（若有），确认对象后再执行；完成后读回校验。
- 子命令、字段名、过滤运算符以 `--help` 和报错列出的合法值为准，不要猜测。
