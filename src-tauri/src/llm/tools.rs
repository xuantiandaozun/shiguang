use crate::db::{Db, PlanCategory};
use crate::organizer::{executor, rules, scanner};
use anyhow::{anyhow, bail, Result};
use chrono::TimeZone;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use tauri::{AppHandle, Emitter, Manager};

pub const DISCOVER_TOOL: &str = "discover_capabilities";

/// 初始只暴露能力发现与 Skill 加载。其余工具由模型围绕当前目标按需激活，
/// 避免几十个无关 schema 同时挤占注意力。
pub fn core_tool_names() -> Vec<String> {
    vec![DISCOVER_TOOL.to_string(), "load_skill".to_string()]
}

fn tools_for_category(category: &str) -> &'static [&'static str] {
    match category {
        "files" => &[
            "scan_desktop",
            "search_files",
            "read_file",
            "get_file_info",
            "create_file",
            "clear_temp_files",
            "edit_file",
            "read_image",
            "ocr_image",
        ],
        "browser" => &[
            "browser_navigate",
            "browser_snapshot",
            "browser_read",
            "browser_click",
            "browser_type",
            "browser_scroll",
            "browser_tabs",
            "browser_activate_tab",
            "browser_screenshot",
            "browser_evaluate",
            "browser_status",
        ],
        "organize" => &[
            "scan_desktop",
            "propose_organization",
            "get_operation_history",
            "undo_batch",
            "create_rule",
            "toggle_rule",
        ],
        "todos" => &["add_todo", "list_todos", "complete_todo", "snooze_todo"],
        "profile" => &["list_profile", "save_profile_entry", "delete_profile_entry"],
        "commands" => &[
            "run_command",
            "run_command_background",
            "check_task",
            "list_tasks",
            "stop_task",
        ],
        "system" => &["get_system_info"],
        "delegation" => &["run_subagent"],
        "skills" => &[
            "list_skills",
            "load_skill",
            "create_skill",
            "delete_skill",
            "manage_skill",
        ],
        _ => &[],
    }
}

fn activate_categories(categories: &[String]) -> Vec<String> {
    let mut names = core_tool_names();
    for category in categories {
        for name in tools_for_category(category) {
            if !names.iter().any(|existing| existing == name) {
                names.push((*name).to_string());
            }
        }
    }
    names
}

pub fn definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "discover_capabilities",
                "description": "围绕当前目标发现并激活完成任务所需的工具，同时检索可能相关的 Skills。当前可见工具不足、你不确定该用什么、或需要换一种实现路径时调用；它不是执行任务本身。一次选齐所有明显相关的能力类别，避免逐个试探。发现后相关工具会从下一轮起可用。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "用一句具体的话描述要完成的结果、涉及的对象和当前缺口，用于匹配 Skills" },
                        "categories": {
                            "type": "array",
                            "items": {
                                "type": "string",
                                "enum": ["files", "browser", "organize", "todos", "profile", "commands", "system", "delegation", "skills"]
                            },
                            "description": "要激活的能力：files=本地文件/图片，browser=网页，organize=桌面整理，todos=待办，profile=个人资料，commands=命令/脚本/CLI，system=硬件与进程，delegation=只读子任务，skills=管理技能"
                        }
                    },
                    "required": ["query", "categories"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "scan_desktop",
                "description": "扫描一个小而明确的目录，返回文件和文件夹清单（名称/相对路径/类型/大小/修改时间/层级）。默认扫描用户桌面。整盘、跨目录、大量文件、空间占用、清理候选等批量任务必须优先使用 search_files 的持久化索引；只有用户明确拒绝维护索引并要求临时扫描时，才用本工具或命令遍历。",
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
                "name": "search_files",
                "description": "本机持久化文件元数据索引（简化版 Everything），是整盘、跨目录、大量文件、空间占用、清理候选和批量筛选任务的首选入口。先用 status/coverage 判断覆盖；完整索引可 search，也可 summarize 按目标目录的一级子项汇总递归占用。没有完整索引时，先向用户说明将后台建立并持续维护文件名、路径、大小和修改时间等元数据，获得明确同意后调用 index（user_confirmed=true）；不要悄悄改用 PowerShell 全盘遍历。用户明确拒绝索引并要求一次性扫描时才回退。ntfs_index 通过 MFT 快速建立仅名称索引，需 UAC，不含可靠大小/时间，不能用于空间分析；ntfs_sync 用于显式提权追赶，ntfs_probe 用于诊断。索引不读取文件内容。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["search", "summarize", "index", "status", "stop", "ntfs_probe", "ntfs_sync", "ntfs_index"], "description": "默认 search；summarize 汇总一级子项递归占用；所有 ntfs_* 动作必须先获得用户对 UAC 的明确同意" },
                        "roots": { "type": "array", "items": { "type": "string" }, "description": "status/coverage、index、search 使用的目标绝对目录；search 可限制范围。相对路径按桌面解析" },
                        "root": { "type": "string", "description": "summarize 要汇总的单个绝对目录，如 D:/" },
                        "volume": { "type": "string", "description": "ntfs_probe/ntfs_sync/ntfs_index 使用的规范盘符根路径，如 C:/" },
                        "query": { "type": "string", "description": "名称或完整路径包含的文字；支持 SQLite 通配符 % 和 _" },
                        "extensions": { "type": "array", "items": { "type": "string" }, "description": "扩展名白名单，如 [\"pdf\",\"docx\"]" },
                        "kind": { "type": "string", "enum": ["file", "directory"], "description": "只找文件或只找目录" },
                        "min_size_mb": { "type": "number", "description": "最小文件大小 MB" },
                        "max_size_mb": { "type": "number", "description": "最大文件大小 MB" },
                        "modified_after": { "type": "string", "description": "修改时间下限，本地时间 YYYY-MM-DD 或 YYYY-MM-DD HH:MM:SS" },
                        "modified_before": { "type": "string", "description": "修改时间上限，本地时间 YYYY-MM-DD 或 YYYY-MM-DD HH:MM:SS" },
                        "sort": { "type": "string", "enum": ["name_asc", "name_desc", "size_asc", "size_desc", "modified_asc", "modified_desc"], "description": "默认 name_asc" },
                        "limit": { "type": "integer", "description": "返回上限；search 默认 100、最大 500，summarize 最大 100；truncated=true 表示还有更多结果" },
                        "user_confirmed": { "type": "boolean", "description": "index 时必填 true，且只能在用户已明确同意开始维护这些目录的索引后设置" }
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
                "description": "创建文本/代码文件（txt/md/log/json/csv/xml/yaml/html/css/js/ts/py/java/go/rs/sql/sh/bat/ps1 等文本类格式）；普通文本以 UTF-8 写入，新建 .ps1 自动带 Windows PowerShell 5.1 兼容的 UTF-8 BOM。父目录不存在时自动创建。相对路径默认写入应用临时目录（禁止把草稿/中间产物堆到桌面）；用户明确要求交付到桌面时用绝对路径或 desktop/文件名；也可写 temp/文件名。目标已存在时默认报错，overwrite=true 才会覆盖（覆盖前自动备份原文件）。一般 PowerShell 操作优先直接用 run_command 的 shell=powershell，无需创建中间脚本。不能创建 exe/docx/xlsx/pdf 等二进制或 Office 格式。要修改已有文件请用 edit_file。",
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
                "description": "观察当前页面的交互结构与状态。快照为控件编号，并标注 role、标签、值、展开/收起、关联浮层、可搜索性、禁用/必填等语义；当前可见的 dialog/listbox/menu/tree/grid 浮层会优先输出，包括 portal 与虚拟列表的可见部分。任何点击、输入、滚动、导航、弹窗变化或 channel 变化后，旧编号都可能失效，应重新观察。打开自定义控件后先取新的全量快照，因为 portal 浮层通常不在触发器子树；定位到新浮层后才用 scope 聚焦。正文阅读用 browser_read。",
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
                "description": "点击快照中编号对应的元素。返回 changed、焦点、展开状态和可见浮层数量的前后观察结果。若出现新浮层或控件展开，下一步重新 snapshot 再决定输入、点击或滚动，不要沿用旧编号猜选项；changed=false 或 covered_by 时根据返回提示调整。",
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
                "description": "向编号控件输入文字。它会通用地解析真实编辑面：控件本身、可编辑后代、aria-controls/owns 关联控件、点击后获得焦点的编辑器或新浮层中的搜索框，适用于组合框、日期/标签选择器和富文本等框架组件。原生 select 支持精确或唯一模糊匹配，多个候选时拒绝猜测。返回 value/resolved_from/opened_control/match_mode 供核验。对自定义选择器，输入成功只代表搜索条件已写入，不代表选项已选中；随后必须重新 snapshot 观察候选并点击、再读回最终值。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "ref": { "type": "integer", "description": "browser_snapshot 返回的元素编号" },
                        "text": { "type": "string", "description": "要输入的内容或搜索词；用户给的是模糊业务名称时可先输入缩小候选，选择动作由后续快照与点击完成" },
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
                "description": "滚动当前交互区域。给 ref 时依次寻找元素最近滚动祖先、aria-controls/owns 关联区域及其浮层滚动容器；不传 ref 时优先当前可见浮层，再选页面主要滚动容器，适用于 portal/虚拟列表。返回容器、前后位置和 moved；滚动后必须重新 snapshot，因为可见内容和编号会变化。",
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
                "description": "在当前页面执行 AI 动态生成的 JavaScript 并返回结构化结果，是 click/type/scroll 在复杂组件、虚拟列表、画布或页面语义不足时的通用补充能力，不绑定特定网站。优先先 snapshot 观察真实状态；先用只读脚本检查 DOM/ARIA/滚动尺寸/候选，再生成最小改动脚本，派发页面需要的事件，并在返回值中报告执行前后状态以便核验。可传 ref，此时表达式中可用 `$el` 引用该快照元素、用 `$args` 读取结构化参数；复杂逻辑写成 `async` IIFE。复用 ref 时必须沿用 snapshot 返回的通道：extension-debugger 对应 debugger，cdp 对应 cdp；页面导航、刷新或切换通道后须重新 snapshot。CSP 受限站点会自动降级到调试通道。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "expression": { "type": "string", "description": "JS 表达式或 IIFE，返回可 JSON 序列化的值。例：`({role:$el?.getAttribute('role'), expanded:$el?.getAttribute('aria-expanded')})`；复杂操作用 `(async()=>{ /* inspect/action/verify */ return {...}; })()`" },
                        "ref": { "type": "integer", "description": "可选：browser_snapshot 的元素编号；传入后表达式可直接使用 `$el`，避免编造脆弱 CSS 选择器" },
                        "args": { "description": "可选：传给动态脚本的 JSON 值；表达式中通过 `$args` 使用，避免手工拼接和转义用户文本" },
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
                "description": "同步执行一条 Windows 命令并等待结束（默认 60 秒超时）。shell=cmd 适合 exe、批处理和 cmd 内建命令；shell=powershell 适合含 `$变量`、管道、对象处理或多行逻辑的 PowerShell 脚本。后端会自动选择 EncodedCommand，超长时安全降级为带 BOM 临时脚本；两者都不经过 cmd 引号嵌套。执行前进行完整 AST 语法检查，默认启用严格变量与错误即停，并传播原生程序失败退出码。动态数据优先放 script_args/environment，避免拼进代码。默认 auto 还能识别高置信度 PowerShell 语法和旧的 powershell -Command 包装。整盘、跨目录或大量文件扫描优先 search_files；没有索引时先提示用户开始维护，用户明确拒绝并要求一次性扫描后才能用本工具遍历。返回 ok=false 或 status=failed/timeout 时必须根据输出修正，不能声称成功。耗时命令改用 run_command_background；破坏性命令必须先向用户说明并征得同意。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "要执行的命令或 PowerShell 脚本正文；PowerShell 模式直接写 `$dirs=...`，不要再包 powershell -Command" },
                        "shell": { "type": "string", "enum": ["auto", "cmd", "powershell"], "description": "解释器，默认 auto。出现 `$变量`、PowerShell 管道/对象或多行脚本时选 powershell；普通 CLI/cmd 命令选 cmd" },
                        "powershell_strict": { "type": "boolean", "description": "PowerShell 是否启用严格变量检查和错误即停，默认 true；只有明确依赖宽松行为的旧脚本才设 false" },
                        "success_exit_codes": { "type": "array", "items": { "type": "integer" }, "description": "被视为成功的退出码，默认 [0]；仅按目标程序文档扩展，例如某些同步/差异工具" },
                        "script_args": { "description": "PowerShell 的结构化 JSON 参数；脚本中通过 `$DHArgs` 读取。用户提供的路径、关键词、文本等动态数据优先放这里，不要拼入脚本字符串" },
                        "environment": { "type": "object", "additionalProperties": { "type": "string" }, "description": "可选环境变量键值；适合向 CLI 安全传递配置，避免把动态值拼进命令。不得包含敏感信息，除非当前任务确实需要" },
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
                "description": "在后台执行 Windows 命令或 PowerShell 脚本（构建、下载、批量处理、启动服务等耗时任务），立即返回 task_id。解释器识别、PowerShell AST 预检、严格错误处理、安全传输、结构化 script_args/environment 和成功退出码规则均与 run_command 相同。输出写入 UTF-8/GB18030 自适应日志；之后用 check_task 查询状态和必要片段，并检查 status、exit_code 与 guidance。破坏性命令必须先向用户说明并征得同意。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "要执行的命令或 PowerShell 脚本正文" },
                        "shell": { "type": "string", "enum": ["auto", "cmd", "powershell"], "description": "解释器，默认 auto；PowerShell 语法和 `$变量` 选 powershell" },
                        "powershell_strict": { "type": "boolean", "description": "PowerShell 严格模式，默认 true；旧脚本兼容时才关闭" },
                        "success_exit_codes": { "type": "array", "items": { "type": "integer" }, "description": "成功退出码，默认 [0]；只依据目标程序的退出码语义扩展" },
                        "script_args": { "description": "PowerShell 结构化 JSON 参数，脚本中通过 `$DHArgs` 读取；避免拼接动态文本" },
                        "environment": { "type": "object", "additionalProperties": { "type": "string" }, "description": "传给子进程的环境变量键值" },
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
pub fn definitions_for<S: AsRef<str>>(names: &[S]) -> Value {
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
                    .map(|n| names.iter().any(|candidate| candidate.as_ref() == n))
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
        "discover_capabilities" => {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|query| !query.is_empty())
                .ok_or_else(|| anyhow!("请具体描述要完成的目标"))?;
            let categories = string_array(args, "categories");
            if categories.is_empty() {
                bail!("至少选择一个能力类别");
            }
            let invalid: Vec<_> = categories
                .iter()
                .filter(|category| tools_for_category(category).is_empty())
                .cloned()
                .collect();
            if !invalid.is_empty() {
                bail!("未知能力类别: {}", invalid.join(", "));
            }
            let file_scan_intent = detect_bulk_file_scan(query);
            let mut effective_categories = categories.clone();
            if file_scan_intent.is_some()
                && !effective_categories
                    .iter()
                    .any(|category| category == "files")
            {
                effective_categories.push("files".to_string());
            }
            let mut activated_tools = activate_categories(&effective_categories);
            if file_scan_intent
                .as_ref()
                .is_some_and(|intent| !intent.direct_scan_override)
            {
                prioritize_index_tools(&mut activated_tools);
            }
            let matched_skills = rank_skills(query, state.skills.list(), 5);
            let has_matched_skills = !matched_skills.is_empty();
            let file_scan_policy = file_scan_intent
                .as_ref()
                .map(|intent| build_file_scan_policy(&state.file_index, intent))
                .transpose()?;
            let next = if let Some(policy) = file_scan_policy.as_ref() {
                policy
                    .get("next")
                    .and_then(Value::as_str)
                    .unwrap_or("批量文件任务优先使用 search_files。")
                    .to_string()
            } else if has_matched_skills {
                "相关工具已激活。若某个 Skill 与当前目标匹配，先 load_skill；再结合当前情境选择最小工具组合。".to_string()
            } else {
                "相关工具已激活。请选择满足目标的最小工具组合；若仍不足，可再次发现其它类别或用 commands 查看本地帮助并组合实现。".to_string()
            };
            Ok(json!({
                "query": query,
                "requested_categories": categories,
                "activated_categories": effective_categories,
                "activated_tools": activated_tools,
                "matched_skills": matched_skills,
                "file_scan_policy": file_scan_policy,
                "next": next,
            }))
        }
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
        "search_files" => {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("search");
            match action {
                "status" => {
                    let roots = string_array(args, "roots");
                    let coverage = state.file_index.coverage(&roots, false)?;
                    let needs_index = !coverage.ready;
                    let indexed_roots = coverage.indexed_roots.clone();
                    Ok(json!({
                        "ok": true,
                        "status": state.file_index.status(),
                        "indexed_roots": indexed_roots,
                        "coverage": coverage,
                        "needs_index": needs_index,
                        "message": if !needs_index {
                            "目标范围已有索引，可直接搜索；空间/大小/时间分析还需 metadata_level=full。"
                        } else {
                            "目标范围尚未维护索引。请先提示用户：建立后会在后台持续维护文件名、路径、大小和修改时间等元数据，不读取文件内容；用户明确同意后再调用 action=index。"
                        }
                    }))
                }
                "stop" => Ok(json!({
                    "stop_requested": state.file_index.stop(),
                    "status": state.file_index.status(),
                })),
                "ntfs_probe" => {
                    let volume = args
                        .get("volume")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| anyhow!("ntfs_probe 需要 volume，例如 C:/"))?;
                    if crate::ntfs_usn::volume_for_path(Path::new(volume)).as_deref()
                        != Some(volume)
                    {
                        bail!("volume 必须是规范盘符根路径，例如 C:/");
                    }
                    Ok(serde_json::to_value(
                        crate::ntfs_helper::probe_elevated(app, volume).await?,
                    )?)
                }
                "ntfs_sync" => {
                    let volume = args
                        .get("volume")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| anyhow!("ntfs_sync 需要 volume，例如 C:/"))?;
                    if crate::ntfs_usn::volume_for_path(Path::new(volume)).as_deref()
                        != Some(volume)
                    {
                        bail!("volume 必须是规范盘符根路径，例如 C:/");
                    }
                    Ok(serde_json::to_value(
                        state.file_index.sync_usn_elevated(app, volume).await?,
                    )?)
                }
                "ntfs_index" => {
                    let volume = args
                        .get("volume")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| anyhow!("ntfs_index 需要 volume，例如 C:/"))?;
                    if crate::ntfs_usn::volume_for_path(Path::new(volume)).as_deref()
                        != Some(volume)
                    {
                        bail!("volume 必须是规范盘符根路径，例如 C:/");
                    }
                    Ok(serde_json::to_value(
                        state
                            .file_index
                            .rebuild_ntfs_elevated(app, volume)
                            .await?,
                    )?)
                }
                "index" => {
                    let raw_roots = string_array(args, "roots");
                    if raw_roots.is_empty() {
                        bail!("index 操作需要 roots，建议只索引任务所需目录；索引整个磁盘可能耗时较长");
                    }
                    if args.get("user_confirmed").and_then(Value::as_bool) != Some(true) {
                        return Ok(json!({
                            "ok": false,
                            "status": "confirmation_required",
                            "confirmation_required": true,
                            "roots": raw_roots,
                            "message": "开始维护索引前需要用户明确同意。请说明：首次建立会后台遍历所选目录，之后持续维护文件名、路径、大小和修改时间等元数据，不读取文件内容。"
                        }));
                    }
                    let desktop = scanner::desktop_dir()?;
                    let roots = raw_roots
                        .into_iter()
                        .map(|raw| {
                            let p = Path::new(&raw);
                            if p.is_absolute() {
                                p.to_path_buf()
                            } else {
                                desktop.join(p)
                            }
                        })
                        .collect();
                    Ok(json!({
                        "ok": true,
                        "message": "文件索引已在后台启动；请稍后用 action=status 查看进度，running=false 后即可搜索",
                        "status": state.file_index.start(roots)?,
                    }))
                }
                "summarize" => {
                    let root = args
                        .get("root")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|root| !root.is_empty())
                        .ok_or_else(|| anyhow!("summarize 需要 root，例如 D:/"))?;
                    let coverage = state
                        .file_index
                        .coverage(&[root.to_string()], true)?;
                    if !coverage.ready {
                        return Ok(index_required_result(
                            coverage,
                            true,
                            "空间占用汇总需要完整元数据索引",
                        ));
                    }
                    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
                    Ok(json!({
                        "ok": true,
                        "summary": state.file_index.summarize_usage(root, limit)?,
                    }))
                }
                "search" => {
                    let roots = string_array(args, "roots");
                    let require_full_metadata = args.get("min_size_mb").is_some()
                        || args.get("max_size_mb").is_some()
                        || args.get("modified_after").is_some()
                        || args.get("modified_before").is_some()
                        || matches!(
                            args.get("sort").and_then(Value::as_str),
                            Some("size_asc" | "size_desc" | "modified_asc" | "modified_desc")
                        );
                    let coverage = state
                        .file_index
                        .coverage(&roots, require_full_metadata)?;
                    if !coverage.ready {
                        return Ok(index_required_result(
                            coverage,
                            require_full_metadata,
                            "目标范围尚无可用于本次批量搜索的索引",
                        ));
                    }
                    let mb = 1024.0 * 1024.0;
                    let min_size_bytes = args
                        .get("min_size_mb")
                        .and_then(|v| v.as_f64())
                        .filter(|v| *v >= 0.0)
                        .map(|v| (v * mb) as u64);
                    let max_size_bytes = args
                        .get("max_size_mb")
                        .and_then(|v| v.as_f64())
                        .filter(|v| *v >= 0.0)
                        .map(|v| (v * mb) as u64);
                    let query = crate::file_index::SearchQuery {
                        roots,
                        text: args
                            .get("query")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                        extensions: string_array(args, "extensions"),
                        kind: args
                            .get("kind")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        min_size_bytes,
                        max_size_bytes,
                        modified_after: optional_local_timestamp(args, "modified_after")?,
                        modified_before: optional_local_timestamp(args, "modified_before")?,
                        sort: args
                            .get("sort")
                            .and_then(|v| v.as_str())
                            .unwrap_or("name_asc")
                            .to_string(),
                        limit: args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize,
                    };
                    Ok(serde_json::to_value(state.file_index.search(query)?)?)
                }
                other => bail!(
                    "未知 action: {}（可选 search/summarize/index/status/stop/ntfs_probe/ntfs_sync/ntfs_index）",
                    other
                ),
            }
        }
        "read_file" => {
            let raw = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let path = crate::reader::resolve_path(app, raw)?;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let max_chars = args
                .get("max_chars")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
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
            let minutes = args.get("minutes").and_then(|v| v.as_i64()).unwrap_or(10);
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
            let target =
                crate::commands::resolve_target_folder(db, target_name).map_err(|e| anyhow!(e))?;
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
        n @ ("run_command"
        | "run_command_background"
        | "check_task"
        | "list_tasks"
        | "stop_task") => {
            if !crate::commands::load_settings(db).command_tools_enabled {
                bail!(
                    "命令执行功能未启用。请用户在主窗口「设置 → 后台任务与命令执行」中打开开关。"
                );
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
                "note": "以上是相关经验与约束。结合当前用户目标、环境证据和风险使用；保留仍适用的稳定约束，情境不符时调整方法并验证结果。",
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

fn string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn integer_array(args: &Value, key: &str) -> Result<Vec<i32>> {
    let Some(value) = args.get(key) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| anyhow!("{} 必须是整数数组", key))?;
    values
        .iter()
        .map(|value| {
            value
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| anyhow!("{} 包含无效退出码", key))
        })
        .collect()
}

fn string_map(args: &Value, key: &str) -> Result<std::collections::HashMap<String, String>> {
    let Some(value) = args.get(key) else {
        return Ok(Default::default());
    };
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("{} 必须是字符串键值对象", key))?;
    if object.len() > 64 {
        bail!("{} 最多包含 64 项", key);
    }
    object
        .iter()
        .map(|(name, value)| {
            if name.is_empty()
                || name.contains('=')
                || name.contains('\0')
                || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                bail!("{} 包含无效环境变量名: {}", key, name);
            }
            let value = value
                .as_str()
                .ok_or_else(|| anyhow!("{} 的值必须都是字符串", key))?;
            if value.contains('\0') {
                bail!("环境变量 {} 的值包含 NUL 字符", name);
            }
            Ok((name.clone(), value.to_string()))
        })
        .collect()
}

/// Skill 描述本身承担触发语义。这里用连续片段重合做轻量本地召回，
/// 不把完整 Skill 目录永久塞进每一轮上下文。
fn rank_skills(query: &str, skills: Vec<crate::skills::SkillInfo>, limit: usize) -> Vec<Value> {
    let query = normalize_match_text(query);
    let query_chars: Vec<char> = query.chars().collect();
    let mut ranked: Vec<(usize, crate::skills::SkillInfo)> = skills
        .into_iter()
        .filter(|skill| skill.enabled)
        .filter_map(|skill| {
            let candidate = normalize_match_text(&format!("{}{}", skill.name, skill.description));
            let mut score = 0usize;
            for width in 2..=6.min(query_chars.len()) {
                for window in query_chars.windows(width) {
                    let fragment: String = window.iter().collect();
                    if candidate.contains(&fragment) {
                        score += width * width;
                    }
                }
            }
            (score > 0).then_some((score, skill))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    ranked
        .into_iter()
        .take(limit)
        .map(|(score, skill)| {
            json!({
                "name": skill.name,
                "description": skill.description,
                "scope": skill.scope,
                "relevance": score,
            })
        })
        .collect()
}

fn normalize_match_text(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(c))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BulkFileScanIntent {
    target_roots: Vec<String>,
    requires_full_metadata: bool,
    direct_scan_override: bool,
}

/// Identify work whose cost grows with an entire drive, many directories, or
/// many files. Capability discovery uses this to make the persistent index the
/// default path instead of merely presenting it beside arbitrary shell tools.
fn detect_bulk_file_scan(query: &str) -> Option<BulkFileScanIntent> {
    let lower = query.to_lowercase();
    let scan_context = [
        "扫描",
        "搜索",
        "查找",
        "筛选",
        "盘点",
        "统计",
        "分析",
        "看看",
        "列出",
        "整理",
        "清理",
        "占用",
        "空间",
        "容量",
        "大文件",
        "重复文件",
        "文件",
        "目录",
        "文件夹",
        "search",
        "scan",
        "find",
        "disk usage",
        "large file",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    let broad_scope = [
        "批量",
        "大量",
        "全部",
        "所有",
        "整个",
        "整体",
        "整盘",
        "全盘",
        "跨目录",
        "多目录",
        "多文件",
        "磁盘",
        "盘符",
        "索引",
        "everything",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    let target_roots = extract_volume_roots(&lower);
    let index_maintenance = lower.contains("索引")
        && (!target_roots.is_empty()
            || ["文件", "目录", "磁盘", "everything"]
                .iter()
                .any(|marker| lower.contains(marker)));
    if (!scan_context && !index_maintenance) || (!broad_scope && target_roots.is_empty()) {
        return None;
    }

    let requires_full_metadata = [
        "占用",
        "空间",
        "容量",
        "大小",
        "大文件",
        "统计",
        "修改时间",
        "最近",
        "最旧",
        "size",
        "usage",
        "modified",
        "largest",
        "oldest",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    let direct_scan_override = [
        "不用索引",
        "不要索引",
        "不建立索引",
        "拒绝索引",
        "直接扫描",
        "临时扫描",
        "只扫一次",
        "一次性扫描",
        "用powershell",
        "用 powershell",
        "用cmd",
        "用 cmd",
        "用命令扫描",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    Some(BulkFileScanIntent {
        target_roots,
        requires_full_metadata,
        direct_scan_override,
    })
}

fn extract_volume_roots(query: &str) -> Vec<String> {
    let chars: Vec<char> = query.chars().collect();
    let mut roots = Vec::new();
    for (index, drive) in chars.iter().copied().enumerate() {
        if !drive.is_ascii_alphabetic() {
            continue;
        }
        let marker = chars[index + 1..]
            .iter()
            .copied()
            .find(|character| !character.is_whitespace());
        if matches!(marker, Some(':' | '盘')) {
            let root = format!("{}:/", drive.to_ascii_uppercase());
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
    }
    roots
}

fn prioritize_index_tools(active_tools: &mut Vec<String>) {
    active_tools.retain(|name| {
        !matches!(
            name.as_str(),
            "scan_desktop" | "run_command" | "run_command_background"
        )
    });
    if !active_tools.iter().any(|name| name == "search_files") {
        active_tools.push("search_files".to_string());
    }
}

fn build_file_scan_policy(
    file_index: &crate::file_index::FileIndex,
    intent: &BulkFileScanIntent,
) -> Result<Value> {
    let coverage = file_index.coverage(&intent.target_roots, intent.requires_full_metadata)?;
    let scope = if intent.target_roots.is_empty() {
        "目标范围".to_string()
    } else {
        intent.target_roots.join("、")
    };
    let next = if intent.direct_scan_override {
        "用户已明确要求不维护索引或只做一次性扫描，可以使用 scan_desktop/commands；仍须检查执行结果，不能把启动当成完成。".to_string()
    } else if coverage.ready {
        if intent.requires_full_metadata {
            format!(
                "批量文件任务必须优先使用 search_files。{} 已有完整元数据索引；空间/占用分析下一步调用 action=summarize，普通批量筛选调用 action=search，不要改用 PowerShell 全量遍历。",
                scope
            )
        } else {
            format!(
                "批量文件任务必须优先使用 search_files。{} 已有可用索引，下一步调用 action=search，不要改用 PowerShell 全量遍历。",
                scope
            )
        }
    } else if intent.target_roots.is_empty() {
        "批量文件任务必须优先使用 search_files，但当前没有覆盖目标范围的可用索引。先向用户确认要维护的目录，并说明首次建立会在后台遍历，之后持续维护文件名、路径、大小和修改时间等元数据，不读取文件内容；用户明确同意后调用 action=index、user_confirmed=true。未同意前不要改用 PowerShell 全量遍历。".to_string()
    } else {
        format!(
            "批量文件任务必须优先使用 search_files，但 {} 尚无可用的{}索引。请先提示用户是否开始维护索引：首次建立会后台遍历，之后持续维护文件名、路径、大小和修改时间等元数据，不读取文件内容。用户本轮已明确同意时，调用 action=index、roots={}、user_confirmed=true；否则先询问，不要改用 PowerShell 全量遍历。",
            scope,
            if intent.requires_full_metadata { "完整元数据" } else { "文件" },
            serde_json::to_string(&intent.target_roots)?
        )
    };
    Ok(json!({
        "intent": "bulk_file_scan",
        "priority_tool": "search_files",
        "target_roots": intent.target_roots,
        "requires_full_metadata": intent.requires_full_metadata,
        "direct_scan_override": intent.direct_scan_override,
        "coverage": coverage,
        "next": next,
    }))
}

fn index_required_result(
    coverage: crate::file_index::IndexCoverage,
    requires_full_metadata: bool,
    reason: &str,
) -> Value {
    json!({
        "ok": false,
        "status": "index_required",
        "index_required": true,
        "requires_full_metadata": requires_full_metadata,
        "coverage": coverage,
        "message": format!(
            "{}。请提示用户开始维护完整文件索引；首次建立会后台遍历所选目录，之后持续维护元数据，不读取文件内容。明确同意后调用 action=index、user_confirmed=true；未同意前不要回退到命令全量扫描。",
            reason
        ),
    })
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    fn skill(name: &str, description: &str) -> crate::skills::SkillInfo {
        crate::skills::SkillInfo {
            name: name.to_string(),
            description: description.to_string(),
            enabled: true,
            scope: "external".to_string(),
            source: "local".to_string(),
            synced_from: String::new(),
            path: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn core_only_exposes_discovery_and_skill_loading() {
        let definitions = definitions_for(&core_tool_names());
        let names: Vec<_> = definitions
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
            .collect();
        assert_eq!(names, vec!["discover_capabilities", "load_skill"]);
    }

    #[test]
    fn categories_activate_only_related_tools() {
        let active = activate_categories(&["files".to_string(), "commands".to_string()]);
        assert!(active.iter().any(|name| name == "read_file"));
        assert!(active.iter().any(|name| name == "run_command"));
        assert!(!active.iter().any(|name| name == "browser_click"));
    }

    #[test]
    fn whole_drive_usage_is_routed_to_persistent_index() {
        let intent = detect_bulk_file_scan("看看 D 盘，整体分析空间使用和目录占用").unwrap();
        assert_eq!(intent.target_roots, vec!["D:/"]);
        assert!(intent.requires_full_metadata);
        assert!(!intent.direct_scan_override);

        let mut active = activate_categories(&[
            "files".to_string(),
            "commands".to_string(),
            "system".to_string(),
        ]);
        prioritize_index_tools(&mut active);
        assert!(active.iter().any(|name| name == "search_files"));
        assert!(active.iter().any(|name| name == "get_system_info"));
        assert!(!active.iter().any(|name| name == "scan_desktop"));
        assert!(!active.iter().any(|name| name == "run_command"));
        assert!(!active.iter().any(|name| name == "run_command_background"));
    }

    #[test]
    fn explicit_one_time_scan_keeps_direct_scan_override() {
        let intent = detect_bulk_file_scan("不要索引，D盘只扫一次，用 PowerShell 统计").unwrap();
        assert_eq!(intent.target_roots, vec!["D:/"]);
        assert!(intent.direct_scan_override);
    }

    #[test]
    fn confirmed_drive_index_maintenance_still_routes_to_file_index() {
        let intent = detect_bulk_file_scan("用户已同意为 D 盘建立索引").unwrap();
        assert_eq!(intent.target_roots, vec!["D:/"]);
        assert!(!intent.direct_scan_override);
    }

    #[test]
    fn search_files_schema_exposes_indexed_usage_summary_and_confirmation() {
        let definitions = definitions_for(&["search_files"]);
        let tool = &definitions.as_array().unwrap()[0];
        let actions = tool
            .pointer("/function/parameters/properties/action/enum")
            .and_then(Value::as_array)
            .unwrap();
        assert!(actions.iter().any(|action| action == "summarize"));
        assert!(tool
            .pointer("/function/parameters/properties/user_confirmed")
            .is_some());
    }

    #[test]
    fn daily_report_query_recalls_daily_report_skill() {
        let found = rank_skills(
            "帮我把今天的工作写到飞书日报",
            vec![
                skill("daily-report-feishu-base", "写日报、日报、飞书日报"),
                skill("reddit-browse-home-feed", "浏览 Reddit 首页"),
            ],
            5,
        );
        assert_eq!(found[0]["name"], "daily-report-feishu-base");
        assert!(found
            .iter()
            .all(|item| item["name"] != "reddit-browse-home-feed"));
    }

    #[test]
    fn browser_evaluate_accepts_snapshot_ref_and_structured_args() {
        let definitions = definitions_for(&["browser_evaluate"]);
        let tool = &definitions.as_array().unwrap()[0];
        assert!(tool
            .pointer("/function/parameters/properties/ref")
            .is_some());
        assert!(tool
            .pointer("/function/parameters/properties/args")
            .is_some());
        assert!(tool
            .pointer("/function/description")
            .and_then(Value::as_str)
            .unwrap()
            .contains("动态生成"));
    }

    #[test]
    fn command_tools_expose_preventive_shell_controls() {
        let definitions = definitions_for(&["run_command", "run_command_background"]);
        for tool in definitions.as_array().unwrap() {
            let properties = tool
                .pointer("/function/parameters/properties")
                .and_then(Value::as_object)
                .unwrap();
            for field in [
                "shell",
                "powershell_strict",
                "success_exit_codes",
                "script_args",
                "environment",
            ] {
                assert!(properties.contains_key(field), "missing {}", field);
            }
        }
    }
}

fn optional_local_timestamp(args: &Value, key: &str) -> Result<Option<i64>> {
    let Some(raw) = args
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    let naive = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .map(|date| date.and_hms_opt(0, 0, 0).expect("midnight is valid"))
        })
        .map_err(|_| {
            anyhow!(
                "{} 时间格式无效，应为 YYYY-MM-DD 或 YYYY-MM-DD HH:MM:SS",
                key
            )
        })?;
    chrono::Local
        .from_local_datetime(&naive)
        .single()
        .map(|dt| Some(dt.timestamp()))
        .ok_or_else(|| anyhow!("{} 对应的本地时间不存在或有歧义", key))
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
            let shell = args.get("shell").and_then(|v| v.as_str());
            let powershell_strict = args
                .get("powershell_strict")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let success_exit_codes = integer_array(args, "success_exit_codes")?;
            let script_args = args.get("script_args").cloned().unwrap_or(Value::Null);
            let environment = string_map(args, "environment")?;
            let timeout = args
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(60)
                .clamp(5, 600);
            let tail_chars = args.get("tail_chars").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let info = tasks
                .run_sync(
                    app,
                    command,
                    workdir,
                    timeout,
                    shell,
                    powershell_strict,
                    &success_exit_codes,
                    &script_args,
                    &environment,
                )
                .await?;
            let (output, truncated) = tasks.head_tail(&info.id, tail_chars).unwrap_or_default();
            let guidance = command_guidance(&info.status, info.exit_code, &output, &info.shell);
            Ok(json!({
                "ok": info.status == "done",
                "task_id": info.id,
                "status": info.status,
                "exit_code": info.exit_code,
                "output": output.trim(),
                "truncated": truncated,
                "execution_context": command_execution_context(&info),
                "guidance": guidance,
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
            let shell = args.get("shell").and_then(|v| v.as_str());
            let powershell_strict = args
                .get("powershell_strict")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let success_exit_codes = integer_array(args, "success_exit_codes")?;
            let script_args = args.get("script_args").cloned().unwrap_or(Value::Null);
            let environment = string_map(args, "environment")?;
            let info = tasks.start_command(
                app,
                command,
                label,
                workdir,
                shell,
                powershell_strict,
                &success_exit_codes,
                &script_args,
                &environment,
            )?;
            Ok(json!({
                "task_id": info.id,
                "pid": info.pid,
                "status": info.status,
                "execution_context": command_execution_context(&info),
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
            let tail_chars = args.get("tail_chars").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
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
                "ok": info.status == "done" || info.status == "running",
                "task_id": info.id,
                "label": info.label,
                "status": info.status,
                "exit_code": info.exit_code,
                "started_at": info.started_at,
                "finished_at": info.finished_at,
                "output_mode": mode,
                "output": output,
                "execution_context": command_execution_context(&info),
                "guidance": command_guidance(&info.status, info.exit_code, &output, &info.shell),
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

fn command_execution_context(info: &crate::tasks::TaskInfo) -> Value {
    let mut context = json!({
        "os": "Windows",
        "shell": info.shell,
        "shell_selection": info.shell_selection,
        "transport": info.transport,
        "success_exit_codes": info.success_exit_codes,
        "python_utf8": true,
        "log_decoding": "UTF-8，失败时回退 GB18030"
    });
    if let Some(obj) = context.as_object_mut() {
        if info.shell == "powershell" {
            obj.insert(
                "launch".into(),
                json!("Windows PowerShell / NoProfile / NonInteractive"),
            );
            obj.insert(
                "quoting".into(),
                json!("脚本不经过 cmd；$变量、引号和中文保持原样"),
            );
            obj.insert(
                "preflight".into(),
                json!("执行前使用 PowerShell AST Parser 检查完整脚本"),
            );
        } else {
            obj.insert("launch".into(), json!("cmd.exe /d /s /c"));
            obj.insert("code_page".into(), json!(65001));
        }
    }
    context
}

/// 把常见命令失败翻译成模型可直接执行的恢复建议，减少盲目重复同一条命令。
fn command_guidance(
    status: &str,
    exit_code: Option<i32>,
    output: &str,
    shell: &str,
) -> Vec<String> {
    let mut hints = Vec::new();
    if status == "timeout" {
        hints.push("命令已超时终止。确认它是否本应长时间运行；是则改用 run_command_background，否则缩小任务范围。".to_string());
    } else if matches!(status, "failed" | "cancelled") {
        hints.push(format!(
            "命令未成功{}。先根据输出定位根因并修正，再决定是否重试；不要把本次执行当作成功。",
            exit_code
                .map(|code| format!("（退出码 {}）", code))
                .unwrap_or_default()
        ));
    }

    let lower = output.to_lowercase();
    if output.contains("__DH_PS_PARSE_ERROR__") {
        hints.push("PowerShell 执行前语法检查未通过，用户脚本尚未开始运行。按标记后的行列和消息修正脚本正文；不要改用多层引号或临时文件绕过检查。".to_string());
    }
    if output.contains("__DH_PS_RUNTIME_ERROR__") {
        hints.push("PowerShell 严格执行阶段失败。未定义变量、非终止错误和异常已被提升为失败；根据标记后的异常与位置修正根因。只有确认是旧脚本兼容问题时才考虑 powershell_strict=false。".to_string());
    }
    if output.contains("__DH_PS_BOOTSTRAP_ERROR__") {
        hints.push("PowerShell 超长脚本的安全传输引导层失败，用户脚本可能尚未开始运行。检查临时文件访问、执行环境或引导错误；不要改回多层命令行引号。".to_string());
    }
    if output.contains('\u{fffd}') || output.contains("��") {
        hints.push("输出仍含乱码替代字符，不能据此做中文名称匹配。优先让命令输出 UTF-8 文件，再用 read_file 核对；必要时查询该 CLI 的编码参数。".to_string());
    }
    if lower.contains("not recognized as an internal or external command")
        || output.contains("不是内部或外部命令")
    {
        hints.push("系统找不到该命令。检查工具是否已安装、可执行文件名是否正确，以及它是否在当前进程的 PATH 中。".to_string());
    }
    if !output.contains("__DH_PS_PARSE_ERROR__")
        && (lower.contains("parsererror") || lower.contains("unexpected token"))
    {
        hints.push(if shell == "powershell" {
            "检测到 PowerShell 解析错误。脚本已通过 EncodedCommand 原样传入，`$变量` 不会被 cmd 展开；请依据报错行检查脚本自身的引号、括号和语法。".to_string()
        } else {
            "检测到 cmd 语法解析错误。若命令实际使用 `$变量`、对象管道或 PowerShell 语法，请把 shell 改为 powershell，并直接传脚本正文，不要再包 powershell -Command。".to_string()
        });
    }
    if lower.contains("access is denied") || output.contains("拒绝访问") {
        hints.push("检测到权限不足。先确认目标和操作范围；只有确实需要提升权限且用户已知情时，才请求授权。".to_string());
    }
    hints
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
