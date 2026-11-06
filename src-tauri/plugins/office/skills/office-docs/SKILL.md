---
name: office-docs
description: >
  处理 Word、Excel、PDF、PPT 文件时加载：写报告/通知/合同并生成 docx，按模板填写，改文档里的文字和表格，
  整理或生成 Excel 表格（写数据、公式、样式），合并/拆分/旋转 PDF，图片合成 PDF，转 PDF 或转换格式，读扫描件。
  说明用哪个工具、标准流程，以及专用工具做不到时的兜底办法。
---

# 办公文档

## 选工具

| 需求 | 工具 |
|---|---|
| 看内容和结构、拿编号 | `office_read` |
| 写一份新 Word | `word_create`（Markdown） |
| 改已有 Word、填模板 | `word_edit` |
| 新建或修改 Excel | `excel_edit`（新建加 `create: true`） |
| PDF 合并、拆分、抽页、删页、旋转、图片转 PDF、页面转图片 | `pdf_edit` |
| 转 PDF、doc→docx、xls→xlsx、pdf→docx、PPT 导出图片 | `office_convert` |

不要为这些常规操作临时写 Python 或 PowerShell 脚本。

## 标准流程

1. 先 `office_read`。改 Word 时编号以它为准；改 Excel 时先确认表头和数据范围。
2. 一次调用里把多个操作放进 `operations`，按顺序执行。Word 的编号始终指读取时的原编号，同一批里插入或删除不会让编号移位。
3. 不给 `output` 就原地保存，并自动备份原文件，结果里会返回备份路径。用户没说要覆盖时，交付物优先另存（给 `output`）。
4. 改完再 `office_read` 核对关键位置。需要看版式时，`office_convert` 转成 PDF，再 `pdf_edit to_images` 后用 `read_image` 查看。
5. 回复里写交付文件的完整绝对路径。

## Word 要点

- 模板：文档里写 `{{客户名}}` 这类占位符，用 `fill` 填写，返回的 `unfilled` 会列出没填到的占位符。
- `replace` 可以跨越格式不同的文字片段匹配，保留第一个片段的格式，页眉页脚也会一起替换。
- `insert` 的 markdown 和 `word_create` 写法相同，可以插入标题、列表、表格、图片。
- 表格：`table_cell` 改单元格，`table_add_row` 以某一行为模板加行，格式会沿用。

## Excel 要点

- 文本形式的编号（"001"、身份证号）按字符串传，不会被转成数字；以 `=` 开头的字符串是公式。
- 公式在 Excel/WPS 打开时才会计算，所以拾光刚写入的公式读回来没有结果。需要结果时用 `office_convert` 转一次（比如转 PDF），或者告知用户。
- 常用组合：`write` 表头和数据 → `style` 表头加粗、填充、加边框 → `width` 自动列宽 → `freeze A2`。

## PDF 要点

- 扫描件（`office_read` 提示没有文字层）：先 `pdf_edit to_images`，再对图片用 `ocr_image`。
- 加密的 PDF 不能处理，需要请用户提供密码或解密后的版本。

## 专用工具做不到时

图表、数据透视表、修订/批注、邮件合并、宏、复杂排版（分栏、文本框、目录更新）这类需求，按下面的顺序找办法：

1. **本机有 Office/WPS**（`office_convert engine=list` 可以查）：用 `run_command` 执行 PowerShell COM 脚本。写法参照 `office_convert` 的做法：
   - 开头 `$app = New-Object -ComObject Word.Application`（或 Excel.Application / PowerPoint.Application；WPS 是 KWPS / KET / KWPP.Application）。
   - 记录 `$pre = $app.Documents.Count`，结束时只有 `$pre -eq 0` 才 `Quit()`，以免关掉用户正在用的实例。
   - 脚本文件要用 UTF-8 BOM 保存，否则中文会乱码。
   - 例如更新目录：`$doc.TablesOfContents.Item(1).Update()`。插入图表：`$ws.Shapes.AddChart2(...)`。
2. **有 Python**（先 `python -c "import docx, openpyxl"` 检查）：python-docx、openpyxl 能做更细的格式控制，例如 openpyxl 的 `openpyxl.chart` 可以画图表。
3. 以上都没有：说明限制，提供替代方案。比如先用 `excel_edit` 准备好数据，请用户在 Excel 里点一下插入图表。

## 安全

- 覆盖用户文件前确认，或者另存。
- 合同、财务数据不要外发，中间产物放在临时目录。
