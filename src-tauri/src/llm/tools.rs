use crate::db::{Db, PlanCategory};
use crate::organizer::{executor, rules, scanner};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use tauri::{AppHandle, Emitter, Manager};

pub fn definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "scan_desktop",
                "description": "扫描目录，返回文件和文件夹清单（名称/相对路径/类型/大小/修改时间/层级）。默认扫描用户桌面并深入子文件夹读取内容；用户指定其它目录（如 D 盘某路径）时用 path 参数。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "要扫描的目录：绝对路径（如 D:\\docs）或相对桌面的文件夹名。留空则扫描桌面。" },
                        "recursive": { "type": "boolean", "description": "是否深入读取子文件夹内容，默认 true" },
                        "depth": { "type": "integer", "description": "最大层数：1=只列顶层，3=深入两层子文件夹（默认），上限 6" }
                    },
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "读取指定文件的内容。支持文本文件（txt/md/log/json/csv/代码等，自动识别 UTF-8/GBK 等编码）、Office 文档（docx/xlsx/pptx，提取纯文本）和 PDF（提取文本层；扫描件无文本层时会提示）。大文件默认只返回开头部分：需要更多内容时用 offset 续读；用户明确要求完整内容时才设 full=true。图片请改用 read_image；zip/exe 等二进制文件不能读内容，请改用 get_file_info。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文件路径：绝对路径（如 D:\\docs\\a.txt）或相对桌面；临时目录文件用 temp/文件名" },
                        "offset": { "type": "integer", "description": "字符偏移量，从该位置继续读取，默认 0" },
                        "max_chars": { "type": "integer", "description": "本次最多返回的字符数，默认 4000，上限 20000" },
                        "full": { "type": "boolean", "description": "读取完整内容（仍有 100000 字符安全上限），默认 false" }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_file_info",
                "description": "查询文件或文件夹的属性：类型/大小/创建时间/修改时间/访问时间/只读/隐藏等（即 Windows 属性对话框中的字段）。zip/exe/图片/音视频等无法读取内容的文件用它。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文件或文件夹路径：绝对路径或相对桌面" }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "create_file",
                "description": "创建文本/代码文件（txt/md/log/json/csv/xml/yaml/html/css/js/ts/py/java/go/rs/sql/sh/bat/ps1 等文本类格式），内容以 UTF-8 写入；父目录不存在时自动创建。相对路径默认写入应用临时目录（禁止把草稿/中间产物堆到桌面）；用户明确要求交付到桌面时用绝对路径或 desktop/文件名；也可写 temp/文件名。目标已存在时默认报错，overwrite=true 才会覆盖（覆盖前自动备份原文件）。不能创建 exe/docx/xlsx/pdf 等二进制或 Office 格式。要修改已有文件请用 edit_file。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文件路径：相对路径→临时目录；temp/x 或 临时/x→临时目录；desktop/x 或 桌面/x→桌面；绝对路径原样（如 D:\\\\docs\\\\a.py）" },
                        "content": { "type": "string", "description": "完整文件内容" },
                        "overwrite": { "type": "boolean", "description": "文件已存在时是否覆盖（自动备份原文件），默认 false" }
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "clear_temp_files",
                "description": "清空应用临时目录内的全部文件与子目录（保留目录本身）。仅在本轮用过临时文件、任务收尾并已征得用户同意后调用；用户说先留着则不要调用。",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit_file",
                "description": "编辑已有的文本/代码文件（保持原文件编码，编辑前自动备份原文件）。三种模式：replace（默认）把 old_text 精确替换为 new_text——old_text 必须与文件内容完全一致（含缩进换行）且唯一匹配，多处匹配时需补充上下文或设 all=true 全部替换；append 在文件末尾追加 content；prepend 在文件开头插入 content。仅支持文本类格式，Office/PDF/二进制文件不支持。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文件路径：绝对路径，或相对桌面；临时目录文件可用 temp/文件名" },
                        "mode": { "type": "string", "enum": ["replace", "append", "prepend"], "description": "编辑模式，默认 replace" },
                        "old_text": { "type": "string", "description": "replace 模式：要被替换的原文（必须与文件内容完全一致，含缩进换行）" },
                        "new_text": { "type": "string", "description": "replace 模式：替换后的内容" },
                        "content": { "type": "string", "description": "append / prepend 模式：要插入的内容" },
                        "all": { "type": "boolean", "description": "replace 模式：old_text 出现多处时是否全部替换，默认 false" }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_image",
                "description": "用云端视觉模型理解图片内容（布局、含义、图表分析等）。纯提取文字请优先用免费的本地 ocr_image。支持 png/jpg/jpeg/gif/webp/bmp。需要在设置中配置视觉模型 API Key。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "图片路径：绝对路径或相对桌面" },
                        "question": { "type": "string", "description": "针对图片的具体问题，如「这个报错是什么意思」。留空则做全面描述" }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "ocr_image",
                "description": "本地 OCR 提取图片中的文字（百度 PaddleOCR PP-OCRv5 mobile，离线、免费、无需 API Key）。适合截图/扫描件/证件等「只要文字」的场景。首次调用会自动下载约 15MB 模型。需要理解画面含义时改用 read_image。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "图片路径：绝对路径或相对桌面" }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "browser_navigate",
                "description": "在浏览器中打开网址。默认在当前标签页打开，new_tab=true 时开新标签。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "完整网址，如 https://www.example.com" },
                        "new_tab": { "type": "boolean", "description": "是否在新标签页打开，默认 false" }
                    },
                    "required": ["url"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "browser_snapshot",
                "description": "获取当前页面的文本快照：交互元素（链接/按钮/输入框/下拉框等）带 [编号]，之后 browser_click/browser_type 用编号引用。操作页面前必须先获取快照；页面变化后编号失效，需要重新获取。页面过大截断时，先用全量快照找到目标容器（弹窗/表单/列表），再用 scope 聚焦该容器获取局部快照——局部快照编号从 1 重排，候选更少定位更准。只要读文章/新闻正文请用 browser_read，不要用本工具硬抠全文。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "max_chars": { "type": "integer", "description": "快照最大字符数，默认 8000" },
                        "scope": { "description": "可选：聚焦局部。传上次快照中容器元素的编号（数字），或 CSS 选择器（字符串，如 .msg-form、dialog）。只遍历该元素子树，编号从 1 重排" }
                    },
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "browser_read",
                "description": "用 Readability 抽取当前页可读正文（剥离导航/广告/侧栏），返回纯文本。适合总结文章、摘录新闻/博客/文档页。不返回可点击编号——要操作页面请用 browser_snapshot。应用页、登录页、信息流等非文章结构可能提取失败，此时改用 snapshot 或截图。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "max_chars": { "type": "integer", "description": "正文最大字符数，默认 12000，上限 100000" }
                    },
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "browser_click",
                "description": "点击快照中编号对应的元素（链接/按钮等）。返回 changed 表示点击后页面是否有可见变化：changed=false 说明点击可能未生效，应重新快照确认目标或换方式；covered_by 表示元素被遮挡。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "ref": { "type": "integer", "description": "browser_snapshot 返回的元素编号" }
                    },
                    "required": ["ref"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "browser_type",
                "description": "在快照中编号对应的输入框/文本域/下拉框/富文本编辑器（contenteditable，如 ProseMirror，快照中带「可编辑」标注）中输入文字。带焦点校验和读回校验：返回 value 是输入框实际读回的内容，可用于确认填对了位置；校验失败会返回 error，此时不要当作成功继续下一步。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "ref": { "type": "integer", "description": "browser_snapshot 返回的元素编号" },
                        "text": { "type": "string", "description": "要输入的内容；下拉框填选项文字" },
                        "clear": { "type": "boolean", "description": "输入前是否清空原有内容，默认 true" }
                    },
                    "required": ["ref", "text"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "browser_scroll",
                "description": "滚动当前页面。自动定位实际滚动容器（页面内嵌列表也能滚），返回 moved 表示是否真的滚动了：moved=false 说明没滚动，可按 hint 处理。滚动列表后编号会变化，需重新快照。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "direction": { "type": "string", "enum": ["up", "down", "left", "right"], "description": "默认 down" },
                        "amount": { "type": "integer", "description": "滚动像素，默认 600" },
                        "ref": { "type": "integer", "description": "可选：快照中某个元素的编号，滚动它所在的滚动容器（填表/操作内嵌列表时用）" }
                    },
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "browser_tabs",
                "description": "列出浏览器所有标签页（id/标题/网址/是否当前操作页）。",
                "parameters": { "type": "object", "properties": {}, "required": [] }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "browser_activate_tab",
                "description": "切换到指定标签页进行后续操作。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer", "description": "browser_tabs 返回的 id" }
                    },
                    "required": ["id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "browser_screenshot",
                "description": "截取当前页面保存为 PNG 并返回路径。只要文字用 ocr_image；要理解画面用 read_image。",
                "parameters": { "type": "object", "properties": {}, "required": [] }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "browser_evaluate",
                "description": "在当前页面执行 JavaScript 并返回结果，用于精确提取页面数据等高级操作。CSP 受限站点（如 X）会自动降级到 CDP 通道执行，也可用 channel 参数强制指定。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "expression": { "type": "string", "description": "JS 表达式，如 document.querySelectorAll('h2').length" },
                        "channel": {
                            "type": "string",
                            "enum": ["auto", "extension", "debugger", "cdp"],
                            "description": "执行通道，默认 auto（扩展优先，失败自动降级）。extension=仅扩展 scripting（不降级）；debugger=扩展内嵌调试通道（同标签绕过 CSP，会显示调试提示条）；cdp=独立 CDP 调试实例（9222/9223，可能无登录态）"
                        }
                    },
                    "required": ["expression"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "browser_status",
                "description": "查看浏览器连接通道状态（扩展桥 / CDP 调试实例 / 未连接）。浏览器工具异常时用它诊断。",
                "parameters": { "type": "object", "properties": {}, "required": [] }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "propose_organization",
                "description": "生成桌面整理方案并交给用户在界面上确认。方案只是提议，用户确认后系统才会移动/删除文件。分类的 action 为 delete 时文件会被移入回收站（可恢复），同样需要用户确认后才会执行。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "summary": { "type": "string", "description": "方案一句话概述" },
                        "categories": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": { "type": "string", "description": "分类名，如 图片、文档；删除分类建议命名为「移入回收站」" },
                                    "action": {
                                        "type": "string",
                                        "enum": ["move", "delete"],
                                        "description": "move=移动到分类文件夹（默认）；delete=移入回收站。仅当用户明确要求删除或文件明显无用时才用 delete"
                                    },
                                    "files": {
                                        "type": "array",
                                        "items": { "type": "string" },
                                        "description": "该分类下的文件名（与 scan_desktop 返回的 name 一致）"
                                    }
                                },
                                "required": ["name", "files"]
                            }
                        }
                    },
                    "required": ["categories"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "add_todo",
                "description": "新建待办事项/提醒。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "description": "待办标题" },
                        "note": { "type": "string", "description": "备注，可选" },
                        "due_at": { "type": "string", "description": "截止时间，本地时间格式 YYYY-MM-DD HH:MM:SS，可选" },
                        "repeat_rule": { "type": "string", "enum": ["none", "daily", "weekly"], "description": "重复规则，默认 none" },
                        "priority": { "type": "integer", "enum": [0, 1, 2], "description": "0 低 1 中 2 高，默认 1" },
                        "remind_mode": { "type": "string", "enum": ["notify", "popup", "popup_input"], "description": "提醒方式，默认 notify（仅系统通知）。popup=弹窗提醒（用户明确要弹窗/强提醒时用）；popup_input=弹窗带输入框，用户填的内容会作为聊天消息发回给你处理——适合「提醒我写日报/周报，到时让我填内容」这类需要用户提交材料的提醒" }
                    },
                    "required": ["title"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_todos",
                "description": "查询待办事项列表。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "filter": { "type": "string", "enum": ["pending", "done", "all"], "description": "默认 pending" }
                    },
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "complete_todo",
                "description": "把待办标记为完成。",
                "parameters": {
                    "type": "object",
                    "properties": { "id": { "type": "integer", "description": "待办 id" } },
                    "required": ["id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "snooze_todo",
                "description": "把待办提醒延后若干分钟。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer" },
                        "minutes": { "type": "integer", "description": "延后分钟数，默认 10" }
                    },
                    "required": ["id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_operation_history",
                "description": "查询桌面整理的操作批次历史（用于回答或撤销）。",
                "parameters": { "type": "object", "properties": {}, "required": [] }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "undo_batch",
                "description": "撤销某个整理批次，把该批次移动的文件移回原位置。",
                "parameters": {
                    "type": "object",
                    "properties": { "batch_id": { "type": "string" } },
                    "required": ["batch_id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "create_rule",
                "description": "创建自动整理规则：桌面新文件命中规则时自动移动到目标分类文件夹。当用户表达“以后这类文件都……”时使用。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "规则名" },
                        "match_type": { "type": "string", "enum": ["ext", "keyword", "regex"], "description": "匹配方式" },
                        "pattern": { "type": "string", "description": "匹配模式：ext 用逗号分隔扩展名如 pdf,docx；keyword 用逗号分隔关键词；regex 写正则" },
                        "target_name": { "type": "string", "description": "目标分类名（建在整理根目录下），如 图片" }
                    },
                    "required": ["name", "match_type", "pattern", "target_name"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "toggle_rule",
                "description": "启用或停用某条自动整理规则。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer" },
                        "enabled": { "type": "boolean" }
                    },
                    "required": ["id", "enabled"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_profile",
                "description": "查看用户个人信息：固定字段（姓名/性别/出生年月/手机/邮箱/城市）+ AI 维护的自由条目（工作经历/自媒体号/项目描述等）。任务需要本人信息但对话中没有提供时调用。",
                "parameters": { "type": "object", "properties": {}, "required": [] }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "save_profile_entry",
                "description": "保存/更新用户个人信息的自由条目（按 label 查重覆盖）。用户在聊天中透露了可长期复用的个人信息时主动保存：工作经历、技能、学历、自媒体账号、开发项目、作品链接、常用署名等。label 用简短名词（如「工作经历」「B站账号」），content 写具体内容（200字以内）。姓名/性别/出生年月等固定字段由用户在设置页维护，不要用本工具保存。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "label": { "type": "string", "description": "条目名，如 工作经历 / 自媒体账号 / 项目经历" },
                        "content": { "type": "string", "description": "具体内容，保留长期有效信息，丢弃一次性细节" }
                    },
                    "required": ["label", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "delete_profile_entry",
                "description": "按 id 删除一条个人信息自由条目（先用 list_profile 获取 id）。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer" }
                    },
                    "required": ["id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run_subagent",
                "description": "把「需要多步操作、大量阅读」的子任务整体委托给子代理。子代理在独立上下文中工作（可用 scan_desktop/read_file/get_file_info/ocr_image/read_image/list_todos 等只读工具），你看不到它的中间过程，只能拿到它返回的最终结论——适合「读完这些文件给我汇总」「分析这个目录里的内容」这类会消耗大量对话上下文的任务。子任务描述必须具体、自包含（子代理看不到本对话）。子代理不能操作浏览器、不能写文件、不能再委托。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "子任务描述：具体、自包含，写清要做什么、结论里要包含什么" },
                        "context": { "type": "string", "description": "可选：子代理需要的背景信息（关键文件路径、目标格式、用户偏好等）" }
                    },
                    "required": ["task"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "同步执行一条 Windows 命令并等待结束（默认 60 秒超时）。输出超长时只返回开头与结尾片段（中间省略），完整输出写入日志文件——适合很快完成的命令（如 git status、dir、ipconfig 等查询类）。避免用 && 串联过多命令，前段输出可能被省略，重要输出请分开执行。命令经 cmd /c 运行，PowerShell 语法（@()、$_ 等）必须先包一层 powershell -NoProfile -Command。耗时命令一律改用 run_command_background。删除/格式化/改系统配置等破坏性命令必须先向用户说明并征得同意。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "要执行的命令（经 cmd /c 运行；PowerShell 语法用 powershell -NoProfile -Command 包裹）" },
                        "workdir": { "type": "string", "description": "工作目录（绝对路径）。默认桌面；若命令会写中间文件，必须设为系统提示词中的临时目录" },
                        "timeout_secs": { "type": "integer", "description": "超时秒数，默认 60，上限 600；超时自动终止" },
                        "tail_chars": { "type": "integer", "description": "返回输出的末尾字符数，默认 2000，上限 8000" }
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run_command_background",
                "description": "在后台执行 Windows 命令（构建/下载/批量处理/启动服务等耗时任务），立即返回 task_id，不阻塞对话。输出实时写入日志文件、不占对话上下文；之后用 check_task 查询进度并只取需要的输出片段。删除/格式化/改系统配置等破坏性命令必须先向用户说明并征得同意。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "要执行的命令（经 cmd /c 运行）" },
                        "label": { "type": "string", "description": "任务备注名（如「构建前端」），方便用户识别" },
                        "workdir": { "type": "string", "description": "工作目录（绝对路径）。默认桌面；若命令会写中间文件，必须设为系统提示词中的临时目录" }
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "check_task",
                "description": "查询后台任务的状态与输出。默认只返回输出的末尾 tail_chars 字符；给 pattern 时只返回包含该关键字的最近 50 行——按需要取片段，不要把整个日志拉进上下文。任务未完成时稍后可再次查询。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "string", "description": "run_command_background 返回的 task_id" },
                        "tail_chars": { "type": "integer", "description": "返回输出末尾字符数，默认 2000，上限 8000" },
                        "pattern": { "type": "string", "description": "可选：只取包含该关键字的行（如 error、失败、完成），与 tail_chars 二选一" }
                    },
                    "required": ["task_id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_tasks",
                "description": "列出所有后台任务（id/状态/命令/起止时间/日志路径，不含输出内容）。",
                "parameters": { "type": "object", "properties": {}, "required": [] }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "stop_task",
                "description": "停止一个正在运行的后台任务（结束整棵进程树）。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "string", "description": "要停止的任务 id" }
                    },
                    "required": ["task_id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_system_info",
                "description": "查询本机硬件与运行状态，返回结构化 JSON：操作系统、CPU、内存、磁盘、GPU、电池、进程占用。也可用 run_command 自行执行 systeminfo / Get-CimInstance 等命令达到同样目的；本工具只是省去拼命令和解析输出。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "category": {
                            "type": "string",
                            "enum": ["overview", "cpu", "memory", "disk", "gpu", "battery", "process"],
                            "description": "overview=整机概览（默认）；cpu/memory/disk/gpu/battery=单项；process=进程列表"
                        },
                        "sort_by": {
                            "type": "string",
                            "enum": ["memory", "cpu"],
                            "description": "仅 process：排序字段，默认 memory"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "仅 process：返回前 N 个，默认 15，上限 50"
                        }
                    },
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_skills",
                "description": "列出已安装 Skills（含 scope=internal|external、启用状态）。目录通常已在对话末尾；需核对全量或确认某技能是否存在时调用。",
                "parameters": { "type": "object", "properties": {}, "required": [] }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "load_skill",
                "description": "加载某个 Skill 的完整 SKILL.md。任务匹配目录中的项时先调用再执行。内部技能只读；不要凭摘要猜步骤。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "技能名（与目录中的 name 一致）" }
                    },
                    "required": ["name"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "create_skill",
                "description": "创建或覆盖一条【外部】Skill。内部技能禁止覆盖。完整完成用户目标后，用它沉淀可复用路径（description 写触发场景，body 写步骤与坑点，丢弃一次性文案/数字/时间）；中途失败不要调用。用户要求新建/改外部技能时也用它。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "技能标识名，建议小写英文+连字符，如 git-auto-commit-zh" },
                        "description": { "type": "string", "description": "触发场景描述：什么时候该用这个技能（会进目录摘要，写清楚触发词）" },
                        "body": { "type": "string", "description": "技能正文 Markdown：步骤、约束、示例等" }
                    },
                    "required": ["name", "description", "body"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "delete_skill",
                "description": "删除一条【外部】Skill（不可恢复）。内部技能禁止删除。仅用户明确要求时调用。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "要删除的技能名" }
                    },
                    "required": ["name"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "manage_skill",
                "description": "管理 Skill：enable/disable（内外部均可）、scan/sync 从本机 Claude/Codex/Cursor 导入为外部技能。不能用 sync 覆盖内部技能。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["enable", "disable", "scan", "sync"],
                            "description": "enable/disable 需 name；scan 扫描外部候选；sync 导入到本地"
                        },
                        "name": { "type": "string", "description": "enable/disable 时的技能名" },
                        "source": {
                            "type": "string",
                            "enum": ["claude", "codex", "cursor", "cursor-builtin"],
                            "description": "sync/scan 可选：只处理该来源；留空则全部"
                        },
                        "names": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "sync 可选：只同步这些技能名；留空则同步该来源下全部"
                        },
                        "overwrite": {
                            "type": "boolean",
                            "description": "sync 时若本地已有同名技能是否覆盖，默认 false（跳过）"
                        }
                    },
                    "required": ["action"]
                }
            }
        }
    ])
}

/// 从全量工具定义中按名称挑出子集（子代理等受限场景使用）
pub fn definitions_for(names: &[&str]) -> Value {
    let all = definitions();
    let Some(arr) = all.as_array() else {
        return json!([]);
    };
    Value::Array(
        arr.iter()
            .filter(|t| {
                t.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .map(|n| names.contains(&n))
                    .unwrap_or(false)
            })
            .cloned()
            .collect(),
    )
}

pub async fn execute(
    app: &AppHandle,
    name: &str,
    args: &Value,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Value> {
    let state = app.state::<crate::AppState>();
    let db = &state.db;
    match name {
        "scan_desktop" => {
            let recursive = args
                .get("recursive")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let depth = args
                .get("depth")
                .and_then(|v| v.as_u64())
                .map(|d| (d as usize).clamp(1, 6))
                .unwrap_or(if recursive { 3 } else { 1 });
            let path_arg = args
                .get("path")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let (root, skip) = match path_arg {
                Some(p) => {
                    let cand = Path::new(p);
                    let full = if cand.is_absolute() {
                        cand.to_path_buf()
                    } else {
                        scanner::desktop_dir()?.join(cand)
                    };
                    (full, None)
                }
                None => {
                    let skip = crate::commands::organize_root_skip(db);
                    (scanner::desktop_dir()?, skip)
                }
            };
            let items = scanner::scan_path(&root, depth, 300, skip)?;
            Ok(json!({
                "root": root.to_string_lossy().replace('\\', "/"),
                "depth": depth,
                "count": items.len(),
                "items": items,
            }))
        }
        "read_file" => {
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let path = crate::reader::resolve_path(app, raw)?;
            let offset = args
                .get("offset")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let max_chars = args
                .get("max_chars")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let full = args
                .get("full")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let res = crate::reader::read_file(&path, offset, max_chars, full)?;
            Ok(serde_json::to_value(&res)?)
        }
        "get_file_info" => {
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let path = crate::reader::resolve_path(app, raw)?;
            Ok(serde_json::to_value(&crate::reader::file_info(&path)?)?)
        }
        "create_file" => {
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 content"))?;
            let overwrite = args
                .get("overwrite")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            crate::writer::create_file(app, raw, content, overwrite)
        }
        "clear_temp_files" => {
            let before = crate::tempfs::clear(app)?;
            Ok(json!({
                "ok": true,
                "path": before.path,
                "removed_files": before.file_count,
                "removed_dirs": before.dir_count,
                "freed_bytes": before.total_bytes,
            }))
        }
        "edit_file" => {
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let mode = args
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("replace");
            crate::writer::edit_file(
                app,
                raw,
                mode,
                args.get("old_text").and_then(|v| v.as_str()),
                args.get("new_text").and_then(|v| v.as_str()),
                args.get("content").and_then(|v| v.as_str()),
                args.get("all").and_then(|v| v.as_bool()).unwrap_or(false),
            )
        }
        "read_image" => {
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let path = crate::reader::resolve_path(app, raw)?;
            let question = args.get("question").and_then(|v| v.as_str());
            let settings = crate::commands::load_settings(db);
            if settings.vision_api_key.trim().is_empty() {
                bail!("尚未配置视觉模型 API Key。纯提取文字可改用 ocr_image（本地免费）；要看图理解请在「设置 → 图像识别」填写视觉 Key。");
            }
            let http = reqwest::Client::new();
            let text = crate::llm::vision::recognize_image(
                &http,
                &settings.vision_base_url,
                &settings.vision_api_key,
                &settings.vision_model,
                &path,
                question,
            )
            .await?;
            Ok(json!({
                "path": path.to_string_lossy().replace('\\', "/"),
                "model": settings.vision_model,
                "result": text,
            }))
        }
        "ocr_image" => {
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let path = crate::reader::resolve_path(app, raw)?;
            let state = app.state::<crate::AppState>();
            // ONNX 推理为同步阻塞；在异步工具路径里用 block_in_place 避免卡住整 runtime
            let result = tokio::task::block_in_place(|| state.ocr.recognize(&path))?;
            Ok(serde_json::to_value(&result)?)
        }
        n if n.starts_with("browser_") => browser_tool(app, n, args).await,
        "propose_organization" => propose_organization(app, db, args),
        "add_todo" => {
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 title"))?;
            let due = args
                .get("due_at")
                .and_then(|v| v.as_str())
                .and_then(crate::commands::normalize_due);
            let todo = db.insert_todo(
                title.trim(),
                args.get("note").and_then(|v| v.as_str()).unwrap_or(""),
                due.as_deref(),
                args.get("repeat_rule")
                    .and_then(|v| v.as_str())
                    .unwrap_or("none"),
                args.get("priority").and_then(|v| v.as_i64()).unwrap_or(1),
                &crate::commands::normalize_remind_mode(
                    args.get("remind_mode").and_then(|v| v.as_str()),
                ),
            )?;
            let _ = app.emit("todos-changed", ());
            Ok(json!({ "ok": true, "todo": todo }))
        }
        "list_todos" => {
            let filter = args
                .get("filter")
                .and_then(|v| v.as_str())
                .unwrap_or("pending");
            let todos = db.list_todos(filter)?;
            Ok(json!({ "count": todos.len(), "todos": todos }))
        }
        "complete_todo" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("缺少 id"))?;
            db.set_todo_done(id, true)?;
            let _ = app.emit("todos-changed", ());
            Ok(json!({ "ok": true }))
        }
        "snooze_todo" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("缺少 id"))?;
            let minutes = args
                .get("minutes")
                .and_then(|v| v.as_i64())
                .unwrap_or(10);
            let new_due = (chrono::Local::now() + chrono::Duration::minutes(minutes))
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();
            db.snooze(id, &new_due)?;
            let _ = app.emit("todos-changed", ());
            Ok(json!({ "ok": true, "new_due": new_due }))
        }
        "get_operation_history" => {
            let batches = db.list_batches()?;
            Ok(json!({ "count": batches.len(), "batches": batches }))
        }
        "undo_batch" => {
            let batch_id = args
                .get("batch_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 batch_id"))?;
            let count = executor::undo_batch(db, batch_id)?;
            let _ = app.emit("history-changed", ());
            Ok(json!({ "ok": true, "restored": count }))
        }
        "create_rule" => {
            let name_s = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 name"))?;
            let match_type = args
                .get("match_type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 match_type"))?;
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 pattern"))?;
            let target_name = args
                .get("target_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 target_name"))?;
            if !["ext", "keyword", "regex"].contains(&match_type) {
                bail!("match_type 必须是 ext / keyword / regex");
            }
            if match_type == "regex" {
                regex::Regex::new(pattern).map_err(|e| anyhow!("正则无效: {}", e))?;
            }
            let target = crate::commands::resolve_target_folder(db, target_name)
                .map_err(|e| anyhow!(e))?;
            let id = db.upsert_rule(None, name_s.trim(), match_type, pattern.trim(), &target)?;
            let _ = app.emit("rules-changed", ());
            Ok(json!({ "ok": true, "rule_id": id, "target_folder": target }))
        }
        "toggle_rule" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("缺少 id"))?;
            let enabled = args
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            db.toggle_rule(id, enabled)?;
            let _ = app.emit("rules-changed", ());
            Ok(json!({ "ok": true }))
        }
        "execute_plan" => Ok(json!({
            "message": "执行整理需要用户在聊天面板的方案卡片上点击「确认执行」，请提示用户操作。"
        })),
        "list_profile" => {
            let settings = crate::commands::load_settings(db);
            let entries = db.pf_list()?;
            Ok(json!({
                "fixed": {
                    "真实姓名": settings.profile_name,
                    "自媒体号名称": settings.profile_alias,
                    "性别": settings.profile_gender,
                    "出生年月": settings.profile_birth,
                    "手机": settings.profile_phone,
                    "邮箱": settings.profile_email,
                    "所在城市": settings.profile_city,
                },
                "entries": entries,
                "hint": "固定字段为空时引导用户去「设置 → 个人信息」填写；自由条目可由你通过 save_profile_entry 维护。对外默认用自媒体号名称，真实姓名仅招聘等实名场景使用。",
            }))
        }
        "save_profile_entry" => {
            let label = args
                .get("label")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("缺少 label"))?;
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("缺少 content"))?;
            let content: String = content.chars().take(500).collect();
            let id = db.pf_upsert(label, &content)?;
            let _ = app.emit("profile-changed", ());
            Ok(json!({
                "ok": true,
                "id": id,
                "message": format!("个人信息条目「{}」已保存，后续需要时会自动加载", label),
            }))
        }
        "delete_profile_entry" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("缺少 id"))?;
            db.pf_delete(id)?;
            let _ = app.emit("profile-changed", ());
            Ok(json!({ "ok": true, "deleted_id": id }))
        }
        "run_subagent" => {
            let task = args
                .get("task")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("缺少 task"))?;
            let context = args.get("context").and_then(|v| v.as_str());
            let text = crate::llm::subagent::run(app, task, context, cancel).await?;
            Ok(json!({
                "result": text,
                "note": "以上是子代理返回的最终结论；它的中间过程未占用本对话上下文。",
            }))
        }
        n @ ("run_command" | "run_command_background" | "check_task" | "list_tasks"
        | "stop_task") => {
            if !crate::commands::load_settings(db).command_tools_enabled {
                bail!("命令执行功能未启用。请用户在主窗口「设置 → 后台任务与命令执行」中打开开关。");
            }
            command_tool(app, n, args).await
        }
        "get_system_info" => {
            let category = args
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("overview");
            let sort_by = args
                .get("sort_by")
                .and_then(|v| v.as_str())
                .unwrap_or("memory");
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(15)
                .clamp(1, 50) as usize;
            crate::machine::query(category, sort_by, limit).await
        }
        "list_skills" => {
            let skills = app.state::<crate::AppState>().skills.list();
            Ok(json!({ "count": skills.len(), "skills": skills }))
        }
        "load_skill" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("缺少 name"))?;
            let content = app.state::<crate::AppState>().skills.load(name)?;
            Ok(json!({
                "name": name,
                "content": content,
                "note": "以上是技能完整说明，请按其步骤执行；不要偏离技能约束。",
            }))
        }
        "create_skill" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 name"))?;
            let description = args
                .get("description")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 description"))?;
            let body = args
                .get("body")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 body"))?;
            let info = app
                .state::<crate::AppState>()
                .skills
                .create(name, description, body)?;
            let _ = app.emit("skills-changed", ());
            Ok(json!({ "ok": true, "skill": info }))
        }
        "delete_skill" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 name"))?;
            app.state::<crate::AppState>().skills.delete(name)?;
            let _ = app.emit("skills-changed", ());
            Ok(json!({ "ok": true, "deleted": name }))
        }
        "manage_skill" => {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 action"))?;
            match action {
                "enable" | "disable" => {
                    let name = args
                        .get("name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow!("enable/disable 需要 name"))?;
                    let info = app
                        .state::<crate::AppState>()
                        .skills
                        .set_enabled(name, action == "enable")?;
                    let _ = app.emit("skills-changed", ());
                    Ok(json!({ "ok": true, "skill": info }))
                }
                "scan" => {
                    let source = args.get("source").and_then(|v| v.as_str());
                    let mut list = app.state::<crate::AppState>().skills.scan_external();
                    if let Some(sf) = source.map(str::trim).filter(|s| !s.is_empty()) {
                        list.retain(|e| e.source == sf);
                    }
                    Ok(json!({
                        "count": list.len(),
                        "external": list,
                        "note": "用 manage_skill(action=sync, source=..., names=[...]) 导入；overwrite=true 可覆盖本地同名。",
                    }))
                }
                "sync" => {
                    let source = args.get("source").and_then(|v| v.as_str());
                    let names: Option<Vec<String>> = args.get("names").and_then(|v| {
                        v.as_array().map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                    });
                    let overwrite = args
                        .get("overwrite")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let result = app.state::<crate::AppState>().skills.sync_from(
                        source,
                        names.as_deref(),
                        overwrite,
                    )?;
                    let _ = app.emit("skills-changed", ());
                    Ok(result)
                }
                other => bail!("未知 action: {}（enable/disable/scan/sync）", other),
            }
        }
        other => bail!("未知工具: {}", other),
    }
}

/// 后台任务/命令类工具的统一入口（都走 TaskManager，输出落日志文件）
async fn command_tool(app: &AppHandle, name: &str, args: &Value) -> Result<Value> {
    let state = app.state::<crate::AppState>();
    let tasks = &state.tasks;
    match name {
        "run_command" => {
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 command"))?;
            let workdir = args.get("workdir").and_then(|v| v.as_str());
            let timeout = args
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(60)
                .clamp(5, 600);
            let tail_chars = args
                .get("tail_chars")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let info = tasks.run_sync(app, command, workdir, timeout).await?;
            let (output, truncated) = tasks.head_tail(&info.id, tail_chars).unwrap_or_default();
            Ok(json!({
                "task_id": info.id,
                "status": info.status,
                "exit_code": info.exit_code,
                "output": output.trim(),
                "truncated": truncated,
                "note": if truncated {
                    "输出超长，已截取开头与结尾片段（中间省略）；完整输出在日志文件中，可用 check_task 按 pattern 关键字过滤取需要的部分。"
                } else {
                    "以上是完整输出。"
                },
                "log_path": info.log_path,
            }))
        }
        "run_command_background" => {
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 command"))?;
            let label = args.get("label").and_then(|v| v.as_str());
            let workdir = args.get("workdir").and_then(|v| v.as_str());
            let info = tasks.start_command(app, command, label, workdir)?;
            Ok(json!({
                "task_id": info.id,
                "pid": info.pid,
                "status": info.status,
                "message": "命令已在后台执行，输出写入日志文件、不占对话上下文。用 check_task 查询状态并只取需要的输出片段（末尾或按关键字过滤）；用 stop_task 停止。",
            }))
        }
        "check_task" => {
            let id = args
                .get("task_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 task_id"))?;
            let info = tasks.get(id).ok_or_else(|| anyhow!("任务不存在: {}", id))?;
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let tail_chars = args
                .get("tail_chars")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let (mode, output) = match pattern {
                Some(p) => {
                    let hits = tasks.grep(id, p)?;
                    (
                        format!("匹配「{}」的最近 {} 行", p, hits.len()),
                        hits.join("\n"),
                    )
                }
                None => {
                    let t = tasks.tail(id, tail_chars)?;
                    ("末尾片段".to_string(), t.trim().to_string())
                }
            };
            Ok(json!({
                "task_id": info.id,
                "label": info.label,
                "status": info.status,
                "exit_code": info.exit_code,
                "started_at": info.started_at,
                "finished_at": info.finished_at,
                "output_mode": mode,
                "output": output,
                "log_path": info.log_path,
                "hint": if info.status == "running" {
                    "任务仍在运行，可稍后再次 check_task；输出只取了片段，完整内容在日志文件中。"
                } else {
                    "任务已结束；如需更多输出，用不同 pattern 或更大 tail_chars 再查，完整内容在日志文件中。"
                },
            }))
        }
        "list_tasks" => {
            let list = tasks.list();
            Ok(json!({ "count": list.len(), "tasks": list }))
        }
        "stop_task" => {
            let id = args
                .get("task_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 task_id"))?;
            let info = tasks.stop(app, id)?;
            Ok(json!({ "ok": true, "task_id": info.id, "status": info.status }))
        }
        _ => bail!("未知命令工具: {}", name),
    }
}

fn propose_organization(app: &AppHandle, db: &Db, args: &Value) -> Result<Value> {
    let summary = args
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let cats = args
        .get("categories")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if cats.is_empty() {
        bail!("方案为空：categories 不能为空");
    }

    let settings = crate::commands::load_settings(db);
    let root = Path::new(&settings.organize_root);
    let desktop = scanner::desktop_dir()?;

    let mut categories: Vec<PlanCategory> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for c in cats {
        let name = c
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("未分类")
            .to_string();
        let files_raw = c
            .get("files")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut files = Vec::new();
        for f in files_raw {
            let Some(fname) = f.as_str() else { continue };
            // 只保留文件名，防止路径注入
            let base = Path::new(fname)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if base.is_empty() || !seen.insert(base.clone()) {
                continue;
            }
            if base == "desktop.ini" {
                continue;
            }
            if !desktop.join(&base).exists() {
                continue;
            }
            files.push(base);
        }
        if files.is_empty() {
            continue;
        }
        let action = match c.get("action").and_then(|v| v.as_str()) {
            Some("delete") => "delete".to_string(),
            _ => "move".to_string(),
        };
        let target_folder = if action == "delete" {
            String::new()
        } else {
            let safe: String = name
                .chars()
                .map(|ch| if "<>:\"/\\|?*".contains(ch) { '_' } else { ch })
                .collect();
            root.join(safe).to_string_lossy().to_string()
        };
        categories.push(PlanCategory {
            name,
            action,
            target_folder,
            files,
        });
    }

    if categories.is_empty() {
        bail!("方案为空：没有可整理的有效文件");
    }

    db.cancel_pending_plans()?;
    let plan_id = db.insert_plan(&summary, &categories)?;
    let plan = db.get_plan(plan_id)?;
    let _ = app.emit("plan-proposed", &plan);
    let total: usize = categories.iter().map(|c| c.files.len()).sum();
    Ok(json!({
        "plan_id": plan_id,
        "file_count": total,
        "message": "方案已生成并展示给用户，请等待用户在界面上确认或取消，不要重复调用本工具。"
    }))
}

#[allow(dead_code)]
pub fn apply_rule_to_path(db: &Db, path: &Path) -> Result<Option<(String, String)>> {
    rules::apply_to_file(db, path)
}

async fn browser_tool(app: &AppHandle, name: &str, args: &Value) -> Result<Value> {
    let state = app.state::<crate::AppState>();
    let action = name.trim_start_matches("browser_");
    if action == "status" {
        return Ok(state.browser.status().await);
    }
    if action == "screenshot" {
        let v = state.browser.call("screenshot", args.clone()).await?;
        let b64 = v
            .get("png_base64")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("截图失败：未返回图像数据"))?;
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| anyhow!("截图数据解码失败: {}", e))?;
        let dir = app
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join("screenshots");
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(format!(
            "shot-{}.png",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        std::fs::write(&file, &bytes)?;
        return Ok(json!({
            "saved": file.to_string_lossy().replace('\\', "/"),
            "channel": v.get("channel").cloned().unwrap_or(Value::Null),
            "hint": "截图已保存。提取文字用 ocr_image（本地免费）；理解画面用 read_image。",
        }));
    }
    state.browser.call(action, args.clone()).await
}
