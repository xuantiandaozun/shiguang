---
name: windows-cli
description: >
  在 Windows 上用 run_command 执行命令、调用外部程序或编写 PowerShell 时加载。
  说明优先用 argv 避免引号问题、JSON 用 stdin/files 传入、长时间任务用 await 挂起而不是轮询。
---

# Windows 命令调用

通过 `run_command` 执行外部程序或脚本时使用本技能。不要把它当成某个具体 CLI 的手册；子命令以该程序自己的 `--help` 为准。

## 选择调用方式

按这个顺序选，不要一上来拼一整条命令字符串：

1. **外部程序 → `argv`**。每个参数独立一项，例如 `["git", "commit", "-m", "说明文字"]`。JSON、空格、中文都不必转义。系统直启进程，按 PATHEXT 解析 exe/cmd，不会命中同名 `.ps1`。
2. **JSON / 长文本入参 → `stdin` 或 `files`**。不要把 JSON 拼进命令行，也不要先 `create_file` 再读。CLI 要求 `@file` 时，把内容放进 `files`（值为对象/数组会自动写成 JSON），argv 里写 `@payload.json`；未指定 `workdir` 时自动用应用临时目录。
3. **JSON 出参**：让程序输出 JSON。`run_command` 会自动解析 stdout——小结果在 `json` 字段，大结果给 `json_summary`（完整内容在 `json_file`）。不要再把输出重定向到本地文件后 `read_file`。只要子集时用 `json_pointer`（已跑过的任务可用 `check_task` + `json_pointer`，不必重跑）。
4. **PowerShell 逻辑**（`$变量`、管道、对象、多行）→ `command` + `shell=powershell`。动态值放 `script_args`，脚本里读 `$DHArgs`。真实路径用 `-LiteralPath`。
5. **cmd 内建命令**（`dir`、`copy`、`for`）→ `command` + `shell=cmd`。
6. 不要用 `cmd /c` 再包一层 PowerShell，也不要 `powershell -Command "..."`。

`argv` 第一项只放程序名。错误：`["git status -sb"]`。正确：`["git", "status", "-sb"]`。

## 长时间命令和脚本

预计会跑几十秒以上（安装、构建、下载、自己写的 `.ps1` / `.py` / `.bat`）时：

1. 用 `run_command_background`，并设 **`await=true`**。或者先启动再立刻 `await_task`。
2. 当前对话会挂起，**不消耗工具轮次**；任务结束后带着 stdout 自动继续。
3. **不要**用 `check_task` 一轮一轮地问「好了没」。
4. 用户点中断会停止这次等待，并尝试结束该进程。

## PowerShell

- 管道无匹配时是 `$null`。取 `.Count` 前必须 `$hits = @($lines | Where-Object { ... })`。
- 不要为了 `.Count` 把 `powershell_strict` 设为 false。
- 调用外部程序时不要 `& 命令名 ...`；改回 `argv`。
- 在脚本里调用原生程序时用 `& $exe @argArray`，不要把参数拼成一个大字符串。

## 读与写

- 需要按中文名称匹配时，让程序输出 JSON；`run_command` 会解析 stdout。不要用可能乱码的 markdown/表格做匹配，也不要把 JSON 再落到文件里读一遍。
- 写操作先用该程序的预览/dry-run（若有），确认对象后再执行；完成后读回校验。
- 子命令、字段名、过滤运算符以 `--help` 和报错列出的合法值为准，不要猜测。

## 外部参考数据

CLI 返回的**短期内不变**的对照信息（id↔名称、字段定义、选项列表、区域/实例规格）用 `lookup_cache` 保存，下次先 get，不必把原始大段输出再灌进对话。

- key 自己起、保持稳定，例如 `lark.base.<token>.projects`、`aliyun.ecs.regions`。
- value 只放提炼后的短表；密钥、token、当天日程、最新一条动态记录不要写入。
- 默认 7 天有效；对照表被你改过（新建项目、改字段）就 put 覆盖或 delete。
- 会变的业务数据仍实时拉取。
