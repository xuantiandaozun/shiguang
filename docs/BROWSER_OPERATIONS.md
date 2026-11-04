# 通用网页操作库（第一批）

## 目标与边界

把历史记录里反复编写的定位、输入事件、下拉选择、等待和数据提取收进受测试的共享实现。不是按 React/Vue 分裂成两套工具，也不直接改框架私有状态。

- 基础操作：`browser-extension/page-api.js`，保留既有 click/type、受控输入原生 setter、富文本输入等逻辑。
- 语义操作：`browser-extension/page-operations.js`，挂在 `window.__dh`，有独立 `operationsVersion`。
- 工具协议/等待调度：`src-tauri/src/browser/operations.rs`。
- 扩展 debugger、scripting 隔离世界、桌面 CDP 都按 Readability → page-api → page-operations 顺序注入。

## AI 工具

### browser_capabilities

报告已实现动作、等待条件、当前控件特征和限制。特征存在不代表站点或组件完全兼容；不承诺仅凭识别 Vue/React 就能操作所有控件。

### browser_action

```json
{"action":"fill","target":{"label":"标题","scope":"#editor"},"text":"结论"}
```

```json
{"action":"select","target":{"role":"combobox","label":"地区"},"text":"大阪"}
```

```json
{"action":"set_checked","target":{"css":"#agree"},"checked":true}
```

- `locate`：返回最多 12 个候选，保留既有 ref，不重置整个 ref 表。
- `click` / `fill`：唯一定位后检查禁用、遮挡，再复用基础操作。
- `select`：原生 select 精确匹配；自定义选择器只处理 ARIA 关联 listbox（无关联时要求唯一可见 listbox）里的唯一可见 option。不自动翻页或搜索；不直接调用私有 React/Vue handler。
- `set_checked`：先读状态，仅在不同状态时点击，随后读回。不能通过点击取消单选项。
- target 字段在桌面端规范化（action 与 wait）：`text`/`aria`/`aria_label`/`title` → `name`，`selector` → `css`，`id` → `css:[id="…"]`，`testid` → `css:[data-testid="…"]`，字符串 ref 转数字；别名与已有字段冲突、或出现其它未知字段时直接报错，不再静默忽略（之前 `{"text":…,"role":"option"}` 会只剩 role，导致歧义）。
- 目标滚动后仍不在视口内（折叠区、隐藏容器、fixed 在屏幕外）返回 `code=offscreen`，与被浮层遮挡的 `covered` 区分。
- 文字定位精确匹配（空白归一化），条件之间为 AND；`scope` 是唯一 CSS 或当前 ref。无目标、重名、失效、禁用、遮挡均不能静默退回全页首条。
- `verified=false` 不等于业务成功。自定义选项若在点击后消失、无法获取 aria-selected，不伪造成功，必须独立读回。

### browser_wait

```json
{"target":{"css":"#results"},"condition":"text","text":"查询完成","timeout_ms":8000}
```

支持 visible / hidden / enabled / text（包含）/ value（精确）。默认 5000ms，最多 15000ms。`timeout_ms=0` 只观察一次，传输最多等待一秒。Rust 每 250ms 单次观察，取消和总截止时间覆盖网络调用与轮询休眠；页面不挂长期 JS 定时器。等待不点击、不重试业务动作。超时不意味着之前的提交未生效。

### browser_extract

```json
{"kind":"table","scope":"#results","limit":20}
```

```json
{"kind":"list","row_selector":".product-card","limit":10,"fields":[
  {"name":"title","selector":"h3"},
  {"name":"url","selector":"a","attribute":"href"},
  {"name":"price","selector":".price"}
]}
```

`kind=fields` 读取唯一 scope 内的单组字段。字段缺失返回 null，不推测价格/币种。默认 20 行，最多 100 行、12 个字段，每格 500 字符，总正文预算 16000 字符；截断显式标记。table 最多每行 20 列。只读取已渲染可见内容，不自动滚动、分页或认定广告真实性。

## 真实输入引擎（第二批，扩展 0.2.0）

扩展从"转发器"变成浏览器运行时：点击、输入、按键、上传都通过 `chrome.debugger` 发出 CDP `Input.*` 事件（`isTrusted=true`），React 受控输入、ProseMirror/Slate/Lexical 等编辑器、只认真人点击的组件都按真人操作处理，不再需要 evaluate 补丁。

分工：

- `browser-extension/runtime.js`（后台）：每个标签一个 `TabRuntime`。负责发送真实输入、跟踪网络请求/导航/原生对话框、等待页面安静、汇报效果、多步执行。只依赖注入的 `send`/`page`，扩展和 e2e 测试共用同一份代码。
- `browser-extension/page-runtime.js`（页面）：`window.__dh.rt`。唯一定位、可操作性（可见/可用/位置稳定/未遮挡，遮挡时换点或报告遮挡者）、焦点与读回校验、页面状态采集。
- `page-api.js` v11：ref 记录元素指纹，被框架重渲染替换后按指纹唯一重定位；不唯一或找不到才报失效。

动作流程：定位 → 滚动并等位置稳定 → 命中检测 → 真实鼠标/键盘 → 等页面安静（DOM 300ms 无状态变化、无 4 秒内发起的未完成请求、非加载中，默认最长 3 秒）→ 返回 `effect`。

`effect` 字段：`url`/`navigated`、`title`、`dialogs_opened`/`dialogs_closed`、`messages`（toast/alert/live region 新出现的文字）、`errors`（新出现的表单错误）、`focus`、`requests`（XHR/Fetch/Document，只含方法、路径、状态码，不含查询参数和正文）、`new_tabs`、`native_dialog`、`settled`/`waited_ms`。

`browser_action` 新增：

- `press`（`key` 如 `Enter`、`Control+A`、`Shift+Enter`；给 target 时先聚焦）、`double_click`、`hover`。
- `upload`：target 是按钮时拦截文件选择框（`Page.fileChooserOpened`）后 `DOM.setFileInputFiles`；target 是隐藏的 `<input type=file>` 时可用 `css` 直接指定。路径在桌面端校验，相对路径按应用 temp 目录解析。扩展需要在 `chrome://extensions` 开启「允许访问文件网址」，否则返回明确提示。
- `dialog`：处理 alert/confirm/prompt。对话框打开期间页面 JS 被阻塞，引擎拒绝其它动作直到处理。
- `steps`：最多 20 步顺序执行，第一个失败（含 `expect` 未满足、原生对话框）即停，返回 `stopped_at`。
- `expect`：`text`/`gone_text`/`url_contains`，动作后在 `timeout_ms`（默认 5 秒）内轮询。
- `newline`：富文本换行默认 `shift_enter`（聊天框回车可能直接发送）；文章编辑器分段用 `enter`；textarea 总是插入换行；单行输入框把换行替换为空格并说明。

`browser_click` / `browser_type` 在扩展调试通道下自动走引擎；强制 `channel=extension` 或无法附着调试器时退回旧的页面内实现。独立 CDP 通道（9222/9223）仍是旧实现，引擎专属动作会返回 `engine_unavailable`。

页面脚本注入改为按内容指纹判断：页面已有同一份脚本时每次调用一次往返，缺失或版本变化才注入（之前每次调用都重新注入约 100KB）。

## 按画面定位与 Jev（扩展 0.2.1）

`target.visual`：页面结构里找不到目标时（纯图标按钮、画布、无文字控件），用一句话描述外观和位置。流程：引擎截取当前视口（CDP `Page.captureScreenshot`，按像素比缩放到最长边 ≤1500px）→ Jev 同一请求问一道 Box（位置）和一道 Noul（是否存在，因为 Box 总会返回一个框）→ 存在概率 ≥0.6 且框置信度 ≥0.3 才采信 → 框中心按视口 CSS 尺寸换算成坐标 → 引擎按坐标发真实鼠标事件（`point`，页面侧 `describePoint` 报告该点实际元素），照常等待页面安静并返回 effect，结果附带 `located`（概率、框、坐标）。

- 只看当前可见区域；目标在屏幕外会判为不存在，先滚动。
- 含 `visual` 的多步由桌面端逐步执行，每个画面定位都在前一步之后重新截图。`action=locate` 配 `visual` 只返回位置不点击。
- 独立 CDP 通道不支持（需要扩展调试通道截图）。图片与 Box 是 TypeSafe 预览能力，请求带 `TypeSafe-Danger-Zone-Images: true`，格式可能随版本变化。

System One 调用层（`system_one.rs`）：

- `ask()` 一次请求多道题（choice/noul/score/box），支持图片资产；429/529 在剩余时间足够时退避重试一次；用量统一记为 `system_one_<来源>`（route/browser/context/browser_fast/visual），模型名取响应里实际作答的版本。
- 修复：`TokenUsage` 识别 `input_tokens`/`output_tokens`，此前 Jev 用量全部被当作空记录丢弃。
- 浏览器动作复核的说明更新到真实输入引擎的全部动作（含 steps、visual）。
- 快速循环：选下一步的同一请求里，为每个点击候选加一道"点了会不会造成不可撤销的外部变化"的是非题；选中的候选概率 ≥0.5 就交回主模型。原有关键词表保留，两者叠加只会更严格。

实测（jev-1.14.0，测试页截图 758×482）：带文字按钮（英文、中文描述）存在概率 0.99–1.0、框置信度 0.91–0.92，中心落在按钮内；纯 SVG 垃圾桶图标滚动到可见区域后 0.98/0.89，中心落在图标内并成功点击；不存在的目标 0.01–0.14。图标在视口外时正确判为不存在（0.24），门槛阻止了误点。

## 安全与升级

- 这些工具仍走现有 tools::execute 及调用方的安全链路；不会通过换工具绕过主代理的浏览器复核。委托任务沿用其原有执行边界。
- 走专用 `page_operation` 传输协议，方法名只允许 capabilities/action/probe/extract；参数作为 JSON 数据传递。scripting 通道使用静态函数调用，不依赖动态 eval，也不需要先调用快照才能注入操作库。
- 旧工具注册表在启动 seeding 时增量补齐新工具，保留已有描述编辑和禁用状态；browser 类别发现包含全部四项。未被用户改过（revision=1）的内置定义在读取时替换为当前代码版本，新版说明和参数才能到达模型；改过的保持原样。
- 扩展端 JS 不会因仅更换 EXE 就自动热更新。使用扩展通道时需更新/重新加载项目中的浏览器扩展；分发 ZIP 会包含新文件。CDP 的 JS 随 EXE 编译。
- 第一批不自动穿透 iframe/Shadow DOM，不宣称完整覆盖 Ant Design/Element Plus/各类编辑器。后续应按真实失败样本添加适配器和回归测试，而非收藏未经验证的站点脚本。

## 检查

- `npm run test:browser-operations`：9 项 jsdom/模拟扩展通道测试，覆盖无快照冷启动注入、静态 scripting 调用、结构化错误保留、定位、歧义、禁用、遮挡、输入事件、幂等勾选、原生/ARIA 选择、等待观察和有界提取。几何和命中检测是模拟的，不等价于真实浏览器端到端验收。
- `npm run test:browser-runtime`：13 项单元测试。页面侧（jsdom）：ref 重定位、遮挡/禁用、读回校验、状态采集；引擎（假 CDP）：鼠标序列、换行方式、上传、多步停止、expect、原生对话框、请求等待、按键解析。
- `npm run test:browser-e2e`：11 项真实浏览器测试（另有 1 项真实 Jev 测试，设置 `TYPESAFE_API_KEY` 才运行；国内网络需走代理：`NODE_USE_ENV_PROXY=1 HTTPS_PROXY=http://127.0.0.1:7897 NO_PROXY=127.0.0.1,localhost`，直连返回 451 "not available in your region"）。前 10 项：启动独立无头 Chrome（临时用户目录），用与扩展相同的 runtime.js 驱动：受控输入（并证明旧实现会被回滚）、聊天框换行不误发送、模型驱动编辑器、只认真人点击的按钮、遮挡检测、重渲染后 ref 重定位、请求等待与提示、两种上传、自定义下拉、原生 confirm、多步与 expect。找不到 Chrome/Edge 时跳过，可用 `CHROME_PATH` 指定。
- `cargo test --manifest-path src-tauri/Cargo.toml --lib`：包括工具发现/旧注册表、参数隔离、等待超时与取消、上传路径校验、快速循环遇到失败交回主模型。
- 后续用真实 React/Vue 页面回放验证成功率、工具往返次数和临时 evaluate 次数；没有现场验证的数据不要宣称收益比例。
