---
name: capability-priority
description: >
  需要查资料、读链接、读文档、朗读/语音，或在「用户自建 Skill / 本地工具 / 浏览器 / DeepSeek 能力」之间拿不准用哪个时加载。
  给出统一的优先级与 DeepSeek 兜底规则。
---

# 能力优先级（本地优先，DeepSeek 兜底）

只有一个对话模式（agent）。DeepSeek 不再是独立模式，只是兜底和特定场景的补充。

## 优先级从高到低

1. **Skill**：目录里有命中的 Skill（外部 = 用户沉淀的流程；内部 = 应用内置规范）先 `load_skill`，按它的步骤做。用户点名某 Skill 必须加载，不要说「我用不了 Skill」。
2. **本地工具**：
   - 用户已打开的页面、需要登录态/Cookie、需要交互或动态渲染 → `browser_*`
   - 本机文件、Office、图片 → `read_file` / `ocr_image` / `word_edit` 等 / 内置 `office-docs`
   - 外部系统（飞书、阿里云、Vercel 等）→ 对应 CLI + 外部 Skill，经 `run_command`
3. **本地网页检索**：`web_search` / `web_fetch`（默认引擎，不用填 `engine`）。
4. **DeepSeek 兜底**：文档解析用 `deepseek_parse_file`（手动，本地解析不了或效果差才用）；联网为自动兜底：本地搜索失败 → DeepSeek Search；本地抓取失败、HTTP 错误或正文过短（反爬/动态渲染）→ DeepSeek Fetch。

## 何时显式用 DeepSeek

- 只在下列情况给 `web_search` / `web_fetch` 传 `engine: "deepseek"`：
  - 本地结果被反爬拦住、内容为空，而该页面不需要登录；
  - 需要带正文摘要的检索（`web_search` 传 `need_content: true`，较慢，仅需要摘要时用）。
- 需要登录态或用户已打开的页面，不要用 DeepSeek，直接用 `browser_*`。

## 不是 agent 工具的 DeepSeek 能力

这些只在界面里提供，不要尝试在任务中调用：消息朗读（TTS）、语音输入（ASR）、截断回复续写、深度思考开关。
配音/旁白类任务用已有的外部 Skill（如 `doubao-tts-voiceover`）或内置 `screen-recording`，不要用界面的 TTS 按钮代替。

## 重叠场景速查

| 场景 | 优先 | 兜底 |
|---|---|---|
| 查公开资料 | `web_search` → `web_fetch` | DeepSeek（自动） |
| 读用户已打开/需登录的页面 | `browser_read` / `browser_snapshot` | 无 |
| 读 PDF/Word/PPT/Excel | `read_file` + 内置 `office-docs` | `deepseek_parse_file`（本地读不出/效果差时；会上传文件，敏感文件先问用户） |
| 图片取字 | `ocr_image`（省 token）或直接看图 | `deepseek_parse_file`（版式复杂、OCR 结果差时） |
| 配音 / 旁白 | 外部 `doubao-tts-voiceover` / 内置 `screen-recording` | 无 |
