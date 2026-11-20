---
name: plugin-authoring
description: >
  用户要给拾光新增功能、小程序、独立弹窗、新页面、新工具、组合已有功能、自动化钩子，
  创建/移除桌面快捷方式，或修改/修复/回滚某个插件时加载。
  说明插件与小程序能做什么、如何用 plugin_write 编写验证、以及如何用 applet_open / applet_shortcut 打开独立窗口和创建桌面快捷方式。
---

# 编写拾光插件与小程序

拾光的功能都以插件提供，任意插件的界面（`ui.applets` / `ui.tabs` / `ui.windows`）和内置功能（如服务器管理、待办事项、录屏截图、工作区等）都可以**作为独立小程序窗口多开**，并**在 Windows 桌面创建带专属图标的快捷启动方式**。

用户想要一个**以后反复使用**的新能力、新页面或功能组合小程序时，写一个用户插件；一次性的事直接用现有工具做，不要建插件。如果用户只是想把**已有功能**（如服务器管理、待办）放到桌面或独立打开，不需要写插件，直接用 `applet_list` / `applet_open` / `applet_shortcut`。

## 先判断

- 只是把已有功能独立打开或放到桌面 → 直接调用 `applet_list` 查 `plugin_id` / `item_id`，再用 `applet_open` 或 `applet_shortcut`（不要自己用命令写 `.lnk`）。
- 只是做法/经验 → 用 Skill（create_skill），不用插件。
- 需要新的可调用能力（查某个 API、算某个数、管理某类数据）→ 插件工具（script / http / command 执行器）。
- 需要独立小程序 / 新功能页面 / 组合已有功能的工作台 → **默认只声明 `ui.applets`**（完全独立的弹窗小程序，不要往拾光主窗口/设置页的 `ui.tabs` 塞标签；只有用户明确要求放进主窗口标签栏时才写 `ui.tabs`）。优先声明式 `view`；交互复杂再写 `entry` HTML 页面。组合已有内置能力（如服务器 + 待办 + 文件）只需在 `plugin.json` 声明 `permissions: ["tools:<工具名>", ...]`，视图和脚本即可调用 `callTool`。
- 每次某个工具调用前后都要自动检查/补充 → hooks。
- 需要改动内核代码（Rust）或内置插件 → 不能自己改，告诉用户需要交给编码智能体（delegate_to_agent）在仓库里改。

## 流程

1. `plugin_list` / `applet_list` 看现有插件和小程序入口，避免重复；需要时 `plugin_read` 看内置插件的写法。
2. `plugin_read` 读取 core 插件的 `PLUGIN_GUIDE.md`（完整规范：清单、执行器、脚本 API、视图块、SDK、`ui.applets`）。
3. 设计：插件 id、工具（名称全局唯一，加前缀）、需要的最小权限、数据存哪（脚本用 `sg.store`）、图标（`icon` 可用内置图标名如 `server`/`todo`/`files`/`usage`/`ai`/`plug`，或插件内 PNG 路径）。
4. `plugin_write` 一次提交全部文件；`summary` 用一句话告诉用户这个插件/小程序做什么。校验失败按 problems 修正后重交。
5. 用户确认生效后：
   - 有新工具就**实际调用一次**确认结果正确；
   - 是小程序就调用 `applet_open` 以独立窗口打开给用户看；
   - 若用户要求放到桌面，调用 `applet_shortcut`（会自动弹确认卡并生成带专属图标的 `.lnk`，不要用 shell 自己造快捷方式）。
6. 出问题：读日志里的报错，用 `plugin_write` 修正；改坏了用 `plugin_rollback` 回到上个版本。

## 注意

- 工具 description 是给模型看的：写清楚用途、何时用、参数含义和限制。
- 不要用 create_file / edit_file 直接写插件目录，必须走 plugin_write（有校验、确认和版本快照）。
- 不要用 run_command / PowerShell 自己在桌面创建 `.lnk`，必须走 `applet_shortcut`（统一格式、专属图标、自动记录、可一键移除）。
- 不要申请用不到的权限；涉及发送、删除、付款的工具要在描述里写明需要用户确认。
- 密钥类配置放 settings（format=password），不要写死在脚本里。
