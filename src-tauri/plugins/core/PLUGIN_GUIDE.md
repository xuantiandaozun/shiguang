# 拾光插件规范

拾光的功能都以插件提供。一个插件是一个目录，至少包含 `plugin.json`。

- **内置插件**：随程序发布（程序目录 `plugins/`；开发时是仓库 `src-tauri/plugins/`），只读。
- **用户插件**：`%APPDATA%/com.deskhelper.win/plugins/<id>/`。拾光通过 `plugin_write` 新增或修改，改完即生效，每次修改前自动存版本，可 `plugin_rollback`。
- 插件文件变化后约 0.3 秒自动重载：工具从下一轮对话起可用，界面自动刷新，MCP 服务自动重启。
- 用户插件不能与内置插件同 id；要改内置功能的行为，新建插件用 hooks 或新工具实现。

## plugin.json

```json
{
  "id": "expense",                 // 小写字母、数字、- 和 _
  "name": "记账",
  "version": "1.0.0",
  "description": "记录和查询日常开销",
  "order": 200,                    // 排序，越小越靠前
  "permissions": ["script", "tools:create_file"],
  "tools": ["tools.json"],         // 工具文件列表
  "categories": {                  // 能力类别（discover_capabilities 用）
    "expense": { "label": "记账", "description": "记录/查询开销" }
  },
  "prompts": { "system_append": "prompts/rules.md" },   // 追加到系统提示词末尾（慎用，只写必要约束）
  "hooks": [ { "event": "before_tool", "tools": ["run_command"], "script": "hooks.js", "function": "beforeTool" } ],
  "mcp_servers": { "main": { "command": "node", "args": ["{plugin_dir}/server.js"], "import_tools": true, "tool_prefix": "exp_" } },
  "settings": "settings.schema.json",
  "ui": { ... }                    // 见「界面」
}
```

### 权限

| 权限 | 允许 |
|---|---|
| `script` | 工具用 script 执行器 |
| `command` | 工具用 command 执行器（启动本机程序） |
| `net` | http 执行器；脚本里的 `sg.http` |
| `mcp` | 工具用 mcp 执行器 / mcp_servers |
| `browser` | browser_action 执行器 |
| `hooks` | 声明 hooks |
| `tools:<名字>` / `tools:*` | 在脚本、alias、界面里调用其它插件的工具 |
| `chat` | 界面 SDK 的 `sendChat` |
| `open_url` | 界面 SDK 的 `openUrl` |

只声明真正需要的权限。插件自己的工具不需要 `tools:` 权限。

## 工具文件（tools.json）

数组，每项：

```json
{
  "name": "expense_add",            // 全局唯一，字母数字 _ -
  "label": "记一笔账",               // 聊天里显示的进行式文案
  "description": "记录一笔开销……",   // 给模型看：做什么、什么时候用、注意什么
  "parameters": { "type": "object", "properties": { "amount": {"type": "number"} }, "required": ["amount"] },
  "category": "expense",            // 主类别，缺省为插件 id
  "executor": { "kind": "script", "file": "expense.js", "function": "add" }
}
```

### 执行器

| kind | 配置 | 说明 |
|---|---|---|
| `script` | `file`, `function` | QuickJS 运行插件脚本，见下文 |
| `command` | `argv`（数组，支持模板）, `stdin`=`json`/`none`, `timeout_seconds`, `cwd` | 启动程序；参数 JSON 写入 stdin；stdout 是 JSON 则原样返回，否则返回 `{output}`；退出码非 0 视为失败 |
| `http` | `method`, `url`（模板，参数自动 URL 编码）, `headers`, `body`=`args`, `timeout_seconds` | 只支持 http/https |
| `mcp` | `server`, `tool` | 调用本插件 mcp_servers 里的服务 |
| `alias` | `target`, `args`（默认参数） | 用默认参数包装另一个工具，需要 `tools:<target>` |
| `browser_action` | `action`, `extension_only` | 浏览器扩展动作 |
| `builtin` | — | 内核实现，只有内置插件可用 |

模板占位符：`{plugin_dir}`、`{args_json}`、`{args.字段}`。

### 脚本（script 执行器与 hooks）

```js
// 顶层函数或 module.exports 都可以，支持 async。每次调用都是全新的隔离环境，限时 5 分钟（钩子 10 秒）、内存 64MB。
module.exports.add = (args, sg) => {
  const list = sg.store.get("items") || [];
  list.push({ amount: args.amount, at: new Date().toISOString() });
  sg.store.set("items", list);
  return { ok: true, count: list.length };
};
```

`sg` 提供（都是同步调用）：

| 方法 | 说明 |
|---|---|
| `sg.callTool(name, args)` | 调用工具，需要 `tools:<name>` 权限（本插件工具除外） |
| `sg.http({method, url, headers, json, body})` | 返回 `{status, text, json}`，需要 `net` |
| `sg.readText(path)` | 读取插件目录内的文件 |
| `sg.store.get(key)` / `sg.store.set(key, value)` | 插件私有的持久化存储 |
| `sg.settings()` | 插件设置（含 schema 默认值） |
| `sg.log(...)` / `console.log` | 写入应用日志 |

返回值会作为工具结果交给模型；抛出异常即工具失败。

### 钩子

- `before_tool`：输入 `{tool, args, session_id}`；返回 `{block: "原因"}` 拦截，`{args: {...}}` 改写参数，`null` 放行。
- `after_tool`：输入 `{tool, args, result, session_id}`；返回 `{result: ...}` 替换结果，`{note: "..."}` 给模型追加提示，`null` 不处理。
- 钩子出错只记日志，不影响工具本身。钩子会在每次匹配的工具调用时运行，`tools` 尽量写具体。

## 界面（ui）

```json
"ui": {
  "tabs": [ { "id": "expense", "label": "记账", "icon": "usage", "order": 45, "view": "ui/view.json" } ],
  "windows": [ { "route": "expense-chart", "title": "开销图表", "width": 720, "height": 520, "entry": "ui/chart.html" } ],
  "tool_cards": [ { "tool": "expense_summary", "view": "ui/card.json" } ],
  "slash_commands": [ { "trigger": "记账", "description": "快速记一笔", "prompt": "帮我记一笔账：" } ],
  "tool_labels": { "old_tool": "旧工具文案" }
}
```

- `tabs`：主窗口标签页。`icon` 可用 plug / todo / server / files / skill / usage / settings / ai / history。
- `windows`：独立窗口，界面里用 `shiguang.openWindow("expense-chart")` 打开。
- `tool_cards`：该工具成功后，在聊天里渲染卡片；界面收到 `params.result`。
- `slash_commands`：聊天输入 `/记账` 时把 prompt 填进输入框。
- 每个挂载项三选一：`view`（声明式 JSON，推荐）、`entry`（HTML 页面）、`component`（仅内置插件）。

### 声明式视图（view）

```json
{
  "title": "记账",
  "blocks": [
    { "type": "row", "blocks": [
      { "type": "stat", "label": "本月合计", "value": "{{data.total}}", "source": { "tool": "expense_summary" } }
    ]},
    { "type": "form", "fields": [
        { "key": "amount", "label": "金额", "type": "number" },
        { "key": "note", "label": "备注" }
      ],
      "submit": { "label": "记一笔", "tool": "expense_add", "args": { "amount": "{{form.amount}}", "note": "{{form.note}}" } } },
    { "type": "table", "source": { "tool": "expense_list" }, "path": "items",
      "columns": [ { "key": "at", "label": "时间" }, { "key": "amount", "label": "金额" } ],
      "row_actions": [ { "label": "删除", "tool": "expense_delete", "args": { "id": "{{row.id}}" }, "confirm": "删除这一笔？" } ] }
  ]
}
```

块类型：`section`（title, blocks）、`row`、`text`（text, muted）、`markdown`、`stat`（label, value）、`table`（columns, row_actions）、`list`（title, subtitle 模板, row_actions）、`form`（fields: text/textarea/number/select/checkbox/datetime；submit；reset）、`button`（label, action=工具调用或 `"refresh"`）、`json`。

- `source: {tool, args}`：先调用工具取数据，结果（可用 `path` 取子字段）在模板里叫 `data`。
- 模板：`{{data.x}}`、`{{row.x}}`、`{{form.x}}`、`{{params.x}}`；整串只有一个占位符时保留原类型（数字仍是数字）。
- 工具调用后默认刷新整个视图（`"refresh": false` 关闭）。
- 视图只能调用本插件的工具，或 permissions 里声明的 `tools:<名字>`。

### 代码页面（entry）

运行在隔离 iframe 里：不能访问 IPC、本机文件、外网，只能通过 SDK。

```html
<!doctype html>
<html><head>
  <meta charset="utf-8">
  <link rel="stylesheet" href="/__sdk/theme.css">
  <script src="/__sdk/sdk.js"></script>
</head><body>
  <div class="card"><h1>记账</h1><div id="out" class="muted">加载中…</div></div>
  <script>
    shiguang.ready().then(async (info) => {
      const r = await shiguang.callTool("expense_summary", {});
      document.getElementById("out").textContent = "本月合计：" + r.total;
    });
  </script>
</body></html>
```

SDK（`window.shiguang`，都返回 Promise）：`ready()`（返回 `{pluginId, slot, params}`）、`callTool(name, args)`、`getSettings()`、`saveSettings(values)`、`sendChat(text)`（chat 权限）、`openUrl(url)`（open_url 权限）、`openWindow(route, query)`、`toast(text)`、`on(event, cb)`（plugins-changed / todos-changed / workspaces-changed / sessions-changed / llm-message-done）。

`theme.css` 提供 `.card .row .muted button.primary button.danger table input` 等基础样式和颜色变量（`--ink --muted --accent --border`），与主窗口一致。页面高度自动适应内容。资源路径用相对路径（如 `./app.js`）或 `/<插件id>/...`；React 等框架打包成静态文件放进插件目录即可（Vite 设 `base: "./"`）。

## 设置（settings.schema.json）

```json
{ "type": "object", "properties": {
  "currency": { "type": "string", "title": "货币", "enum": ["CNY", "USD"], "default": "CNY" },
  "budget": { "type": "number", "title": "月预算" },
  "token": { "type": "string", "title": "API Token", "format": "password" }
}}
```

在「插件」页显示为表单；脚本用 `sg.settings()`、页面用 `shiguang.getSettings()` 读取。支持 string（enum、format=textarea/password）、number/integer、boolean。

## MCP 服务

`mcp_servers.<名字>`：`command`、`args`（支持 `{plugin_dir}`）、`env`、`cwd`、`import_tools`（自动把 tools/list 注册为工具）、`tool_prefix`、`category`。也可以不自动导入，在 tools.json 里用 `{"kind":"mcp","server":"名字","tool":"原工具名"}` 自己写描述。

## 写插件的流程（拾光）

1. `plugin_list` 看有没有现成插件；`plugin_read` 参考内置插件写法。
2. 工具描述写清楚做什么、何时用、参数含义；能用声明式视图就不写代码页面。
3. `plugin_write` 一次提交全部文件（新插件必须含 plugin.json），`summary` 写给用户看。
4. 校验不通过会返回 problems，修正后重交；通过后用户确认（有界面可以先预览）。
5. 生效后实际调用一次新工具验证；出错用 `plugin_write` 修正，或 `plugin_rollback`。
