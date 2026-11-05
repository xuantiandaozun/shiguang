# 操作录屏（设计稿）

> 状态：2026-11-05 P1、P2、截图、背景音乐已提交；P3 桌面录屏代码完成（未提交），等真机测试。

## 字幕样式与代码片段（2026-11-05）

- **字幕样式**（`recorder/subtitle.rs`）：timeline.json 的 `subtitle_style`，预设 classic / variety（综艺花字）/ minimal / karaoke，可覆盖字体、字号、位置、渐变填充、多层描边、投影、重点词颜色、底框、动画（pop / fade / slide / typewriter / karaoke）。字形轮廓转成 tiny-skia 路径再描边、填充，逐帧计算，不依赖浏览器。解说词里【】括起的词按重点词颜色画，`.srt` 里去掉括号。主字体缺字时退回微软雅黑粗体。
- **字体**：内置清单是 Google Fonts 收录的 OFL 免费可商用中文字体（站酷快乐体、站酷庆科黄油体、站酷小薇、马善政毛笔），首次用到时下载到 `app_data/fonts`（GitHub 失败退 jsDelivr；本进程没有代理环境变量时用 Windows 系统代理）。用户也可以把 .ttf/.otf 放进这个目录。下载失败时退回系统字体，导出照常，结果里有 `warnings`。
- **代码片段**（`recorder/scene.rs`）：timeline.json 的 `scenes[]`，每项 `{id, kind: insert|overlay, at, duration, file}`，文件在录制目录的 `scenes/`。
  - insert：作为带编号的定格放进 TimeMap（`TimeMap::tagged`），画面在该源时间让给片段；`narration::plan` 先放片段再排解说，同一时刻的解说排在片段之后。
  - overlay：从 `map.to_output(at)` 开始叠在画面上，PNG 透明，读入时转预乘。
  - 渲染：后台 `--headless=new` 的 Edge/Chrome（设置 `scene_browser` 可指定），自写的小 CDP 客户端。注入脚本接管 rAF / setTimeout / setInterval / Date / performance，并把所有 Web Animations 暂停后设 `currentTime`，所以逐帧确定。页面可定义 `window.renderFrame(t)`；`window.SG_DURATION` 是片段时长。设计宽度固定 1920 CSS px，用 deviceScaleFactor 缩放到输出分辨率。插入片段截 JPEG、叠加片段截 PNG，缓存在 `scene_cache/<id>-<内容哈希>/`，内容和尺寸不变就复用。
  - 内置模板（插件资源 `assets/scenes/*.html`，`{{变量|默认值}}` 转义填入，`{{{变量}}}` 作为 JSON 字符串给脚本）：title、chapter、bullets、ending、code（insert）；lower_third、callout、sticker（overlay）。
  - `record_edit` 的 scenes 项可给 `template + vars` 或 `html`，由 `edit_timeline` 落成文件后只存 `file`；`record_scene_preview` 把片段在几个时间点截图拼成一张图给模型看。
- 编辑窗口：时间线多一条「代码片段」轨，手动调整里能删片段；导出选项多「字幕样式」；常用要求加了「加片头片尾」「字幕换综艺花字」「加贴纸或标注」。
- 测试：`cargo test --release --lib recorder::scene::tests::headless -- --ignored`（真实浏览器、时间受控）；`SG_SAMPLE_DIR=目录 … recorder::scene::tests::render_samples -- --ignored`（各字幕预设和每个模板出图）；`SG_E2E_DIR=目录 … recorder::e2e::export_with_scenes -- --ignored`（片头 + 贴纸 + 综艺字幕导出 MP4 并截帧）。

## P3 桌面录屏（`src/recorder/desktop.rs`，2026-11-05）

和设计稿的差异：采集先用 GDI BitBlt，不用 WGC。原因是不加新依赖、画面本来就不含系统光标、`WDA_EXCLUDEFROMCAPTURE` 对 BitBlt 同样生效。代价是录窗口时录的是它在屏幕上的位置，被挡住的部分会录到挡在前面的窗口。以后需要「被遮挡也能录」再换 WGC。

- **来源**：`record_start` 的 `source` = `browser`（原来的）| `screen`（鼠标所在屏幕或 `monitor`）| `window`（标题/进程名，切到前台，跟随窗口移动，尺寸固定为开始时的大小）| `region`（rect）| `select`（复用截图选区浮层，`purpose=record`，只返回范围不截图）。
- **画面**：按 `desktop_fps`（默认 20）采集，复用一块 DIB；和上一帧逐像素比较，没变化就不存。变化像素少于画面 0.05%（输入光标闪烁、时钟）的帧标 `minor`，紧凑节奏不把它当「有动静」。超过 2560×1600 按整数倍缩小。3 个线程编码 JPEG，编码跟不上时丢帧但不更新「上一帧」，下一轮会重新检测到变化。
- **事件**（10ms 轮询，无钩子）：`move`（≥30ms 一次）、`down`/`up`、`click`（左键按下抬起位移 ≤8px）、`leave`（离开范围或停在拾光自己的窗口上）、`key`（只记「有输入」，不记按了什么，最多 10 次/秒）、`window`（前台窗口切换，带标题和进程名）。
- **点击目标**：单独线程用 UI Automation `ElementFromPoint` 取控件名称、类型和边框，生成 `target.label`（如「保存（按钮）」）和 `target.bbox`，放大倍数按控件大小算。密码框不取名称。点到输入框会补一个 `focus` 事件，输入时保持放大。识别线程忙时直接发不带目标的点击。
- **拾光窗口**：默认录制期间对拾光所有窗口设 `WDA_EXCLUDEFROMCAPTURE`，停止时恢复；`include_self` 或设置 `record_include_self` 打开时录进去（拍拾光宣传视频）。
- **快捷键**：`record_hotkey`（默认 Ctrl+Alt+R）开始录鼠标所在屏幕 / 停止。全局快捷键线程改成一次注册多个。
- **写盘**：桌面线程自己写 `frames/NNNNNN.jpg`，再发 `rec_frame_saved` 给 `write_loop` 记 `frames.jsonl`（零点取最早的帧，因为编码可能乱序完成）。窗口关闭时发 `rec_end` 自动收尾。
- **没做**：OCR 自动打码（桌面只能录完框选打码）、被遮挡窗口的录制、系统声音 / 麦克风。

## 背景音乐与节奏（2026-11-05）

- **背景音乐**：`timeline.json` 的 `music {path, volume=0.25, duck=0.35, fade_in=1, fade_out=2.5}`。混音在 `narration::write_mix` 里完成：
  - 音乐循环铺满全片；
  - 增益 = 基础音量 × 淡入淡出 × 解说压低。离解说 0.3 秒内平滑过渡，每 10 毫秒计算一次。
  - 不依赖 ffmpeg 的混音滤镜。
- **配乐来源**：配乐目录 `music_dir`（默认「音乐/拾光配乐」，首次用到时自动创建）。`record_timeline` 和编辑器会列出目录里的音频；不绑定任何音乐生成服务。
- **紧凑节奏**：`tighten`（默认开启）。导出时把「没有新画面帧、没有操作事件（不含鼠标移动）、也不在解说时间内」且超过 1 秒的空档，剪到只剩 0.4 秒（`narration::idle_cuts`），再和用户手动剪切合并后排期。用 2026-11-05 那条 140 秒的 README 介绍测算：能剪掉约 22 秒。
- **Skill**：
  - 节奏规则：总长 45–75 秒；每段解说不超过 25 字；每 3–5 秒一个画面动作；开头 3 秒说清是什么。
  - 「先配音后录制」：先生成配音，录制时每个镜头按配音时长推进。
  - 按视频类型选音色：产品介绍不用悬疑、纪录片腔，语速稍快。
  - 配乐选择规则。

## 截图（`src/recorder/screenshot.rs`）

- **方式**：
  - **框选**：先截整个虚拟屏幕（冻结画面），再打开全屏选区窗口 `capture-overlay`（前端 `windows/CaptureOverlay.tsx`）。拖动选区域；鼠标停在窗口上会高亮，单击则截整个窗口。
  - **指定窗口**：`PrintWindow(PW_RENDERFULLCONTENT)`，被遮挡也完整，再按 DWM 可见边框裁掉阴影；得到全黑画面时退回按屏幕区域截。
  - **全屏**：所有显示器。
  - **区域**：按坐标截。
- **拾光自身窗口**：截图期间用 `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` 排除出画面，不会闪烁；`include_self` 可以包含它们（做宣传素材时用）。
- **DPI**：截图相关调用在线程级切到每显示器 DPI 感知，坐标统一是物理像素。
- **输出**：PNG 保存到 `screenshot_dir`（默认「图片/拾光截图」）；剪贴板同时写 CF_DIB 和 PNG 两种格式。
- **全局快捷键**：`RegisterHotKey` 在独立线程上注册（没有用 global-shortcut 插件，它会连带升级 Tauri 依赖）。设置保存后重新注册；被别的程序占用时会提示。
- **AI 工具**：`screenshot`（interactive / window / screen / region）、`list_windows`。
- **界面**：「录屏截图」页顶部的截图区，有最近截图缩略图，可以复制或打开所在位置。
- 窗口枚举、窗口查找、选区浮层也给 P3 桌面录屏用（见上）。

## P2 实际落地（时间线编辑 / 配音 / 字幕）

- `timeline.json` 结构（`src/recorder/doc.rs`），时间都是源时间：
  - `zooms_edited`：false 时导出按事件自动生成放大段，true 时原样使用 `zooms`；
  - `zooms[{start,end,scale,focus?}]`：没有 focus 时跟随光标；
  - `masks[{start,end,rect}]`；
  - `cuts[{start,end}]`；
  - `narration[{at,text,audio?}]`；
  - `burn_subtitles?`。
- 时间映射（`timeline::TimeMap`）：源时间去掉剪切段，再插入定格。
- 配音排期（`narration::plan`）：每段挂在 `at`。上一段没说完时，在 `at` 处定格画面等它说完，两段之间至少隔 0.3 秒；最后一段说完前视频不结束。只有文字没有音频时，按每秒 4 个字估算时长。
- 混音：每段用 ffmpeg 解码成 48k 立体声 PCM，在 Rust 里混成 `narration.wav`，导出时作为第二路输入编码成 AAC。不依赖 ffmpeg 版本的混音滤镜，也不需要 ffprobe。
- 字幕：
  - 解说按句子和长度切成多条，时长按字数分配；
  - 导出时总会写一个同名 `.srt`；
  - `burn_subtitles` 为 true 时，用 ab_glyph 加载系统字体（微软雅黑，退而用黑体、宋体）画在画面底部。
- 快速预览：`record_export {preview:true}`，960 宽、20fps、ultrafast，存成录制目录下的 `preview.mp4`，不记作正式导出。
- 停止录制时，带 note 的 `record_mark` 自动成为解说草稿。
- AI 工具：
  - `record_timeline`：返回标记、点击/输入/页面步骤、当前时间线和 `audio_dir`；
  - `record_edit`：给了哪个字段就整体替换，先校验，通过才保存。
- 配音不内置服务商：Skill 指导拾光通过 run_command 调用户的配音 CLI（比如火山），先看 `--help`，凭据由 CLI 自己管理。音频放进 `audio_dir`，再用 `record_edit` 挂上。
- 界面：「录屏」页每条录制有「编辑」，打开独立的录屏编辑窗口（插件窗口 `recorder-editor`，`multi: true`，每份录屏一个窗口），组件 `RecorderEditor`：
  - 编辑以拾光为主：顶部写一句要求（或点常用要求）→ 填进聊天框由用户确认发送，拾光用 `record_edit` 改完，`recorder-timeline-changed` 事件让编辑窗口自动刷新；
  - 编辑台只保留轻量手动功能：拖动时间轴看原始画面、画面上框选打码、改/删解说文字、删打码/剪切/手动放大段、恢复自动放大、配乐/字幕/节奏选项；改动自动保存；
  - 精细的数值调整（时间点、放大倍数和焦点、配音文件、配乐音量）交给拾光。
- 已知限制：
  - 编辑器显示的是原始画面，放大和光标效果要靠快速预览看；
  - 还没有背景音乐；
  - 字幕样式固定。

## P1 实际落地（与下文方案的差异以这里为准）

- 页面录制脚本放在 **recorder 插件** `plugins/recorder/assets/page-recorder.js`（不在 browser-extension，扩展不需要带它；Rust 随 record_start 下发源码）。
- 打码样式用 `document.adoptedStyleSheets`（不受页面 CSP 限制），旧浏览器才退回 `<style>`。
- 扩展 0.2.2：`record_start` / `record_stop`，画面帧和事件作为 `{event: rec_frame|rec_event|rec_end}` 主动推送；扩展连着时只走扩展（不切到别的浏览器实例），旧扩展会提示重新加载。
- 内核 `src/recorder/`：`mod.rs`（会话、写盘、工具、界面命令、自动收尾）、`timeline.rs`、`render.rs`（tiny-skia，多线程）、`ffmpeg.rs`。
- 编码器自动选择：libx264 > h264_mf > mpeg4。
- 测试：
  - `cargo test --lib recorder::`；
  - `npm run test:recorder-page`（jsdom 跑打码与事件）；
  - `cargo test --release --lib recorder::e2e -- --ignored --nocapture` 用本机 ffmpeg 真实导出（720p 6 秒约 1 秒）。
- 未验证：ffmpeg 自动下载（npmmirror tgz）没有实际跑过，下载并运行外部程序被自动权限拦了；本机已有 winget 装的 ffmpeg，会优先用它。
- 待用户验证：在真实浏览器里重新加载扩展后试录，重点看打码是否覆盖、光标坐标是否对得上（页面缩放不是 100% 时）、导航后是否继续录。

## 已确认的决策

| 问题 | 决定 |
|---|---|
| 处理方式 | 先录原始素材 + 事件轨，导出时再做放大、光标、打码、配音（可反复修改） |
| 配音 | 用火山现成的普通音色；**不写死接口**：由拾光通过命令工具调用火山 CLI 生成音频，录制器只负责把音频片段挂到时间线上。具体做法写进 Skill |
| ffmpeg | 首次使用时下载，不打进安装包 |
| 顺序 | 先做 P1（网页录屏），桌面录屏后续必须做 |
| 宣传视频 | 拾光要能录自己的宣传视频，所以「把拾光窗口排除出画面」必须是开关，不能写死 |

## 分期

| 期 | 内容 |
|---|---|
| P1 | 网页录制（扩展 debugger + 独立 CDP 两条路径）+ 事件轨 + 合成光标 + 自动放大 + 页面源头打码 + 导出 MP4 + 录屏页 |
| P2 | 时间线编辑器（放大段/手动打码/裁剪）+ 配音片段挂载（CLI 生成）+ 字幕 SRT + 拾光按事件轨写解说词初稿 |
| P3 | 桌面窗口录制（Windows Graphics Capture，关系统光标和黄框）+ 全局鼠标钩子 + 本地 OCR 打码 + 拾光窗口排除开关（`SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`） |
| P4 | 拾光自动录演示 / 宣传视频：边操作边 `record_mark`，自动生成放大、解说、配音 |

## 工程目录格式

```
app_data/recordings/<id>/
├─ meta.json        {id,title,source:"web",url,started,ended,frame_count,duration_ms,viewport}
├─ frames/000001.jpg …   原始画面（不带鼠标，仅在页面变化时有帧，帧率不固定）
├─ frames.jsonl     {i, t(毫秒时间戳), w, h, meta(screencast metadata)}
├─ events.jsonl     {t, type:move|down|click|key|scroll|focus|nav|mark, x, y, vw, vh, target:{label,bbox}}
├─ timeline.json    zooms[] / masks[] / cuts[] / narration[{anchor, audio, text}] / 光标样式（P1 由事件自动生成）
└─ audio/           配音片段（P2）
```

导出默认放到 `dirs::video_dir()/拾光录屏/`。

## P1 实现方案（今天已核对过代码）

### 1. 页面脚本 `browser-extension/page-recorder.js`

和其它页面脚本一样放在 browser-extension 目录，由 browser 插件 `assets` 声明（`page_recorder`），Rust 用 `page_asset("page_recorder")` 读取，**通过 record_start 参数把源码和配置下发给扩展**，所以扩展本身不需要带这个文件。

- 事件：pointermove（rAF 节流）、pointerdown/click（带元素 label 和 bbox）、keydown（只记非输入键/快捷键；可打印字符只记「输入」不记内容；密码框完全不记）、scroll、focus。每条带 `t=Date.now()`、`vw/vh=innerWidth/innerHeight`。
- 上报：`Runtime.addBinding("__sgRec")` → 页面调 `window.__sgRec(JSON)` → `Runtime.bindingCalled`。
- 打码在源头做：给元素加 `data-sg-mask`，样式 `filter: blur(8px) !important`。
  - 规则：密码框一律打码；`autocomplete=cc-*`；配置的选择器；正则预设（手机号、身份证、邮箱、银行卡、密钥/token 形式）+ 自定义正则。命中文本节点时打码其 parentElement。
  - MutationObserver 收到变化后用 requestAnimationFrame 合并扫描（只扫新增/变化节点），保证在下一次绘制前打码完成；`input` 事件重扫该输入框。
- 导航后：`Page.addScriptToEvaluateOnNewDocument` 重新安装。

### 2. 扩展 `background.js`（manifest 版本升到 0.2.2，用户需重新加载扩展）

- 新动作 `record_start {session, script, config, max_width, max_height, quality}`：ensureDebugger → Page.enable → Runtime.enable → Runtime.addBinding → addScriptToEvaluateOnNewDocument → 当前页 evaluate 安装 → `Page.startScreencast {format:"jpeg"}`。
- 在已有的 `chrome.debugger.onEvent` 里：`Page.screencastFrame` → `send({event:"rec_frame", session, data, metadata})` 并 `Page.screencastFrameAck`；`Runtime.bindingCalled(name=__sgRec)` → `send({event:"rec_event", session, payload})`；主框架导航 → `rec_event` nav。
- `record_stop`：stopScreencast、移除脚本和 binding、页面里卸载打码。
- 录制期间持续收发消息，MV3 service worker 不会因空闲被停；若中途断开，Rust 侧按帧中断处理并结束录制。

### 3. Rust：扩展桥 `browser/ext.rs`

现在只处理 `{id, ok, data}` 应答。要增加：带 `event` 字段的主动消息转给订阅者（`Inner` 里放 `Mutex<Option<mpsc::UnboundedSender<Value>>>`，加 `subscribe()`）。

### 4. Rust：CDP 路径 `browser/cdp.rs`

chromiumoxide_cdp 0.9.1 里有现成类型：`page::{StartScreencastParams, StartScreencastFormat, EventScreencastFrame(data: Binary, metadata: ScreencastFrameMetadata, session_id), ScreencastFrameAckParams, StopScreencastParams, AddScriptToEvaluateOnNewDocumentParams, RemoveScriptToEvaluateOnNewDocumentParams}`、`runtime::{AddBindingParams, EventBindingCalled}`。用 `page.event_listener::<…>()` 转成和扩展相同格式的 Value 发到同一个通道。`Hub` 加 `record_start/record_stop`：扩展已连接走扩展，否则走 CDP。

### 5. Rust：新模块 `src-tauri/src/recorder/`

- `mod.rs`：`RecorderHub` 放进 `AppState`；`TOOL_NAMES` + `execute()`，在 `llm/tools.rs` 里照 `workspaces::TOOL_NAMES` 的方式分派；Tauri 命令在 `lib.rs` 的 `generate_handler!` 注册；设置用 `plugins::settings::values(app, "recorder")` 读取。
  - 写盘任务：rec_frame → base64 解码写 jpg + frames.jsonl；rec_event → events.jsonl。
  - 录制时长上限（默认 30 分钟）自动停止。
- `timeline.rs`（纯函数，写单测）：
  - 放大段：点击/输入焦点前 0.4s 到后 1.5s，间隔 < 1.2s 的合并；倍数默认 1.8，有 bbox 时按「元素约占画面 40%」算并限制在 1.4–2.4。
  - 镜头：按 1/fps 步长模拟临界阻尼弹簧，同时驱动缩放和中心点；放大时中心跟随平滑后的光标；中心限制在画面内。
  - 光标：移动事件插值后再用更硬的弹簧平滑；点击时光标缩到 0.85，并画 0.4s 扩散波纹。
  - 坐标换算：页面 CSS px × (帧宽 / vw)。
- `render.rs`：tiny-skia 合成（需要新增依赖 `tiny-skia`；`image`、`rayon`、`flate2`、`tar` 已在依赖树里，要用的话在 Cargo.toml 显式声明），流程：
  1. 取 ≤t 的最近源帧（解码缓存）；
  2. 按 timeline 手动打码区域做马赛克；
  3. 按镜头变换画到输出画布；
  4. 在输出空间画合成光标和波纹；
  5. 可选「带边距」风格：渐变背景 + 圆角 + 阴影，宣传视频用。

  多线程按帧号取模分给 worker，主线程按顺序收集，原始 RGBA 写给 ffmpeg stdin：`-f rawvideo -pix_fmt rgba -s WxH -r 30 -i - -c:v libx264 -pix_fmt yuv420p -crf 20 -preset veryfast -movflags +faststart`。输出宽高取偶数。
- `ffmpeg.rs` 按顺序找 ffmpeg：
  1. 设置里指定的路径；
  2. `app_data/tools/ffmpeg/ffmpeg.exe`；
  3. PATH 里的 ffmpeg；
  4. 下载：默认 npmmirror 的 `@ffmpeg-installer/win32-x64`（tgz，约 25MB，国内快），备用 GitHub BtbN 构建；地址可在设置里改。

  注意：这个 ffmpeg 是 4.1 版，带 libx264 和 gdigrab；P3 用 WGC 原生采集，所以不依赖新版 ffmpeg 的 ddagrab。

### 6. 插件 `src-tauri/plugins/recorder/`

- `plugin.json`：类别 `recorder`；ui tab `builtin:RecorderTab`；settings。
- `tools.json`（builtin）：
  - `record_start`：title、可选 url，先打开网页再录；
  - `record_stop`；
  - `record_status`；
  - `record_mark`：label、note，是解说词的锚点；
  - `record_list`；
  - `record_export`：id，可覆盖 zoom/cursor/style/fps/out_path，后台执行并报告进度；
  - `record_delete`。
- `settings.schema.json`：打码预设开关、自定义选择器（textarea）、自定义正则（textarea）、放大倍数、光标大小、fps、画面风格、输出目录、ffmpeg 路径、ffmpeg 下载地址。
- `skills/screen-recording/SKILL.md`：怎么边操作边录、什么时候打标记、导出前提醒用户检查打码；P2 再补配音（调用火山 CLI 生成音频，再挂到时间线）。

### 7. 前端 `src/components/RecorderTab.tsx`

在 `src/plugins/builtin.tsx` 注册。功能：
- 开始/停止；
- 录制列表；
- 导出：进度来自事件 `recorder-export-progress`；
- `<video src={convertFileSrc(out)}>` 预览成片；
- 打开文件夹、删除。

### 已知限制（P1）

- 只录一个标签页；切到后台标签时 screencast 会停帧，按时间戳补帧（画面停住）。
- 跨域 iframe（OOPIF）里的鼠标事件和打码暂不支持。
- 画面只有网页内容，不含浏览器地址栏。

## 防泄露原则（各期通用）

1. 只录目标标签页或窗口，不默认录全屏。
2. 网页在源头打码，桌面靠 OCR 后期打码 + 人工确认。
3. 密码框按键不进事件轨。
4. 导出前必须检查打码。
5. 原始素材只在本地；配音只发送文字，发送前用同一套正则检查。
