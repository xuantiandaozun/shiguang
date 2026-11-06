//! 控件树的纯数据部分：控件类型名、快照渲染、目标匹配。与 COM 无关，便于测试。

use serde::Serialize;

pub const P_INVOKE: u32 = 1;
pub const P_VALUE: u32 = 2;
pub const P_TOGGLE: u32 = 4;
pub const P_EXPAND: u32 = 8;
pub const P_SELECT: u32 = 16;
pub const P_SCROLL: u32 = 32;
pub const P_TEXT: u32 = 64;

pub const BUTTON: i32 = 50000;
pub const CHECKBOX: i32 = 50002;
pub const COMBOBOX: i32 = 50003;
pub const EDIT: i32 = 50004;
pub const HYPERLINK: i32 = 50005;
pub const IMAGE: i32 = 50006;
pub const LIST_ITEM: i32 = 50007;
pub const LIST: i32 = 50008;
pub const MENU: i32 = 50009;
pub const MENU_ITEM: i32 = 50011;
pub const RADIO: i32 = 50013;
pub const SCROLLBAR: i32 = 50014;
pub const TEXT: i32 = 50020;
pub const TREE_ITEM: i32 = 50024;
pub const CUSTOM: i32 = 50025;
pub const GROUP: i32 = 50026;
pub const THUMB: i32 = 50027;
pub const DATA_ITEM: i32 = 50029;
pub const DOCUMENT: i32 = 50030;
pub const WINDOW: i32 = 50032;
pub const PANE: i32 = 50033;
pub const TITLEBAR: i32 = 50037;
pub const SEPARATOR: i32 = 50038;

/// (类型 id, 英文名, 中文名, 额外别名)
const ROLES: &[(i32, &str, &str, &[&str])] = &[
    (50000, "button", "按钮", &[]),
    (50001, "calendar", "日历", &[]),
    (50002, "checkbox", "复选框", &["check"]),
    (50003, "combobox", "下拉框", &["dropdown", "select", "组合框"]),
    (50004, "edit", "输入框", &["textbox", "input", "编辑框", "文本框"]),
    (50005, "hyperlink", "链接", &["link"]),
    (50006, "image", "图片", &["img"]),
    (50007, "listitem", "列表项", &["option"]),
    (50008, "list", "列表", &["listbox"]),
    (50009, "menu", "菜单", &[]),
    (50010, "menubar", "菜单栏", &[]),
    (50011, "menuitem", "菜单项", &[]),
    (50012, "progressbar", "进度条", &["progress"]),
    (50013, "radiobutton", "单选框", &["radio", "单选按钮"]),
    (50014, "scrollbar", "滚动条", &[]),
    (50015, "slider", "滑块", &[]),
    (50016, "spinner", "微调框", &["spinbutton"]),
    (50017, "statusbar", "状态栏", &["status"]),
    (50018, "tabcontrol", "标签页组", &["tablist"]),
    (50019, "tabitem", "标签页", &["tab"]),
    (50020, "text", "文本", &["label", "statictext"]),
    (50021, "toolbar", "工具栏", &[]),
    (50022, "tooltip", "提示", &[]),
    (50023, "tree", "树", &["treeview"]),
    (50024, "treeitem", "树节点", &[]),
    (50025, "custom", "自定义控件", &[]),
    (50026, "group", "分组", &[]),
    (50027, "thumb", "拖动柄", &[]),
    (50028, "datagrid", "数据表", &["grid"]),
    (50029, "dataitem", "数据项", &["cell", "row", "单元格"]),
    (50030, "document", "文档", &["editor"]),
    (50031, "splitbutton", "分割按钮", &[]),
    (50032, "window", "窗口", &["dialog", "对话框"]),
    (50033, "pane", "面板", &[]),
    (50034, "header", "表头", &[]),
    (50035, "headeritem", "表头项", &["columnheader", "列头"]),
    (50036, "table", "表格", &[]),
    (50037, "titlebar", "标题栏", &[]),
    (50038, "separator", "分隔线", &[]),
    (50039, "semanticzoom", "缩放视图", &[]),
    (50040, "appbar", "应用栏", &[]),
];

pub fn role_name(id: i32) -> &'static str {
    ROLES.iter().find(|r| r.0 == id).map(|r| r.2).unwrap_or("控件")
}

/// 角色名（中英文、别名，不区分大小写）→ 类型 id
pub fn role_id(name: &str) -> Option<i32> {
    let n = name.trim().to_lowercase().replace(['_', '-', ' '], "");
    ROLES
        .iter()
        .find(|r| r.1 == n || r.2 == n || r.3.iter().any(|a| *a == n))
        .map(|r| r.0)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Info {
    pub control: i32,
    pub name: String,
    pub automation_id: String,
    pub class: String,
    pub rect: Option<[i32; 4]>,
    pub enabled: bool,
    pub offscreen: bool,
    pub focused: bool,
    pub password: bool,
    pub value: Option<String>,
    pub readonly: bool,
    pub toggle: Option<i32>,
    pub expand: Option<i32>,
    pub selected: Option<bool>,
    pub patterns: u32,
    #[serde(skip)]
    pub children: Vec<usize>,
}

impl Info {
    fn actionable(&self) -> bool {
        self.patterns & (P_INVOKE | P_VALUE | P_TOGGLE | P_EXPAND | P_SELECT) != 0
    }
}

/// 不展开子树的类型：标题栏（最小化/关闭按钮）、滚动条
pub fn skip_subtree(control: i32) -> bool {
    matches!(control, TITLEBAR | SCROLLBAR | THUMB)
}

/// 值得输出的节点：可交互的控件、带名字的文字和容器。没有名字的布局容器透明（子节点上提）。
fn meaningful(n: &Info) -> bool {
    match n.control {
        BUTTON | CHECKBOX | COMBOBOX | EDIT | HYPERLINK | LIST_ITEM | MENU_ITEM | RADIO | TREE_ITEM | DATA_ITEM | DOCUMENT | 50015 | 50016
        | 50019 | 50031 | 50035 => true,
        SEPARATOR => false,
        TEXT | IMAGE | GROUP | PANE | CUSTOM | WINDOW | LIST | MENU | 50010 | 50017 | 50018 | 50021 | 50023 | 50028 | 50034 | 50036
        | 50012 => !n.name.trim().is_empty() || n.actionable(),
        _ => n.actionable() || !n.name.trim().is_empty(),
    }
}

fn clip(s: &str, max: usize) -> String {
    let flat: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let flat = flat.trim();
    if flat.chars().count() <= max {
        flat.to_string()
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

/// 单行描述：类型 "名称" 及状态
pub fn describe(n: &Info) -> String {
    let mut s = role_name(n.control).to_string();
    let name = clip(&n.name, 80);
    if !name.is_empty() {
        s.push_str(&format!(" \"{name}\""));
    }
    if !n.automation_id.is_empty() && n.automation_id.len() <= 40 && n.automation_id != n.name {
        s.push_str(&format!(" id={}", n.automation_id));
    }
    if n.patterns & P_VALUE != 0 {
        if n.password {
            s.push_str(" 值=***");
        } else if let Some(v) = n.value.as_deref().filter(|v| !v.trim().is_empty() && v.trim() != n.name.trim()) {
            s.push_str(&format!(" 值=\"{}\"", clip(v, 60)));
        }
    }
    let mut flags: Vec<&str> = Vec::new();
    if !n.enabled {
        flags.push("不可用");
    }
    if n.readonly && n.patterns & P_VALUE != 0 && matches!(n.control, EDIT | COMBOBOX) {
        flags.push("只读");
    }
    match n.toggle {
        Some(0) => flags.push("未勾选"),
        Some(1) => flags.push("已勾选"),
        Some(2) => flags.push("部分勾选"),
        _ => {}
    }
    match n.expand {
        Some(0) => flags.push("已折叠"),
        Some(1) | Some(2) => flags.push("已展开"),
        _ => {}
    }
    if n.selected == Some(true) {
        flags.push("已选中");
    }
    if n.focused {
        flags.push("焦点");
    }
    if n.offscreen {
        flags.push("屏幕外");
    }
    if n.patterns & P_TEXT != 0 && matches!(n.control, DOCUMENT | EDIT) {
        flags.push("可读全文");
    }
    if !flags.is_empty() {
        s.push_str(&format!(" ({})", flags.join("，")));
    }
    s
}

#[derive(Debug, Clone)]
pub struct RenderOpts {
    pub max_chars: usize,
    pub query: Option<String>,
    pub include_offscreen: bool,
    pub max_depth: Option<usize>,
}

#[derive(Debug, Default)]
pub struct Rendered {
    pub text: String,
    /// 编号 → 节点下标（编号 = 下标 + 1）
    pub refs: Vec<usize>,
    pub truncated: bool,
    pub truncated_tree: bool,
    pub total_nodes: usize,
    pub interactive: usize,
}

fn matches_text(n: &Info, q: &str) -> bool {
    let q = q.to_lowercase();
    n.name.to_lowercase().contains(&q)
        || n.automation_id.to_lowercase().contains(&q)
        || (!n.password && n.value.as_deref().is_some_and(|v| v.to_lowercase().contains(&q)))
        || role_name(n.control) == q
}

pub fn render(nodes: &[Info], roots: &[(usize, String)], opts: &RenderOpts) -> Rendered {
    let mut out = Rendered { total_nodes: nodes.len(), ..Default::default() };
    let query = opts.query.as_deref().map(str::trim).filter(|q| !q.is_empty());
    for (root, label) in roots {
        if out.truncated {
            break;
        }
        if !out.text.is_empty() {
            out.text.push('\n');
        }
        out.text.push_str(&format!("== {label} ==\n"));
        // (节点, 缩进, 深度, 祖先路径, 父节点名称)
        let mut stack: Vec<(usize, usize, usize, Vec<String>, String)> = vec![(*root, 0, 0, Vec::new(), String::new())];
        while let Some((idx, indent, depth, path, parent_name)) = stack.pop() {
            let n = &nodes[idx];
            let is_root = idx == *root;
            if !is_root && n.rect.is_none() && !opts.include_offscreen && n.children.is_empty() {
                continue;
            }
            // 屏幕外的节点本身不显示，但子节点照常检查：WinUI 的弹出层容器会误报屏幕外，里面的菜单项却是可见的
            let hidden = !is_root && n.offscreen && !opts.include_offscreen
                // 按钮/菜单项里重复自身名字的文字标签
                || (n.control == TEXT && !parent_name.is_empty() && n.name.trim() == parent_name);
            let show = !is_root && !hidden && meaningful(n) && query.map_or(true, |q| matches_text(n, q));
            let mut child_indent = indent;
            let mut child_path = path.clone();
            if show {
                let line = if query.is_some() && !path.is_empty() {
                    format!("[{}] {}  ‹ {}\n", out.refs.len() + 1, describe(n), path.join(" › "))
                } else {
                    format!("{}[{}] {}\n", "  ".repeat(indent), out.refs.len() + 1, describe(n))
                };
                if out.text.len() + line.len() > opts.max_chars {
                    out.truncated = true;
                    break;
                }
                out.text.push_str(&line);
                out.refs.push(idx);
                if n.actionable() {
                    out.interactive += 1;
                }
                if query.is_none() {
                    child_indent += 1;
                }
            }
            if !is_root && !n.name.trim().is_empty() && meaningful(n) && !n.actionable() {
                child_path.push(clip(&n.name, 20));
            }
            if opts.max_depth.is_some_and(|d| depth >= d) {
                continue;
            }
            let own_name = n.name.trim().to_string();
            for &c in n.children.iter().rev() {
                stack.push((c, child_indent, depth + 1, child_path.clone(), own_name.clone()));
            }
        }
    }
    out
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub name: Option<String>,
    pub roles: Option<Vec<i32>>,
    pub automation_id: Option<String>,
    pub class: Option<String>,
    pub index: Option<usize>,
    pub include_offscreen: bool,
}

impl Query {
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(n) = &self.name {
            parts.push(format!("名为「{n}」"));
        }
        if let Some(r) = &self.roles {
            parts.push(format!("的{}", r.iter().map(|&i| role_name(i)).collect::<Vec<_>>().join("/")));
        }
        if let Some(a) = &self.automation_id {
            parts.push(format!("（id={a}）"));
        }
        if let Some(c) = &self.class {
            parts.push(format!("（类名 {c}）"));
        }
        if parts.is_empty() {
            "控件".into()
        } else {
            format!("{}控件", parts.join(""))
        }
    }
}

fn visible_nodes(nodes: &[Info], roots: &[(usize, String)], include_offscreen: bool) -> Vec<usize> {
    let mut out = Vec::new();
    for (root, _) in roots {
        let mut stack = vec![*root];
        while let Some(i) = stack.pop() {
            if i == *root || !nodes[i].offscreen || include_offscreen {
                out.push(i);
            }
            for &c in nodes[i].children.iter().rev() {
                stack.push(c);
            }
        }
    }
    out
}

/// 按条件找节点：名称先精确（忽略大小写和首尾空白）再包含；默认忽略屏幕外，找不到时再算上屏幕外
pub fn find(nodes: &[Info], roots: &[(usize, String)], q: &Query) -> Vec<usize> {
    for include_offscreen in [q.include_offscreen, true] {
        let pool = visible_nodes(nodes, roots, include_offscreen);
        let base: Vec<usize> = pool
            .into_iter()
            .filter(|&i| {
                let n = &nodes[i];
                q.roles.as_ref().map_or(true, |r| r.contains(&n.control))
                    && q.automation_id.as_ref().map_or(true, |a| n.automation_id == *a)
                    && q.class.as_ref().map_or(true, |c| n.class.eq_ignore_ascii_case(c))
            })
            .collect();
        let hits = match q.name.as_deref().map(|s| s.trim().to_lowercase()) {
            None => base,
            Some(name) => {
                let exact: Vec<usize> = base.iter().copied().filter(|&i| nodes[i].name.trim().to_lowercase() == name).collect();
                if !exact.is_empty() {
                    exact
                } else {
                    base.into_iter().filter(|&i| nodes[i].name.to_lowercase().contains(&name)).collect()
                }
            }
        };
        if !hits.is_empty() || include_offscreen {
            return hits;
        }
    }
    Vec::new()
}

/// 找不到时给出相近的候选，帮助模型改条件
pub fn suggest(nodes: &[Info], roots: &[(usize, String)], q: &Query) -> String {
    let pool = visible_nodes(nodes, roots, false);
    let mut cands: Vec<String> = Vec::new();
    if let Some(r) = &q.roles {
        cands = pool.iter().filter(|&&i| r.contains(&nodes[i].control) && !nodes[i].name.trim().is_empty()).take(12).map(|&i| describe(&nodes[i])).collect();
    }
    if cands.is_empty() {
        cands = pool.iter().filter(|&&i| nodes[i].actionable() && !nodes[i].name.trim().is_empty()).take(12).map(|&i| describe(&nodes[i])).collect();
    }
    if cands.is_empty() {
        "这个窗口几乎没有暴露控件信息（自绘界面/游戏/部分 Electron 应用），改用 desktop_screenshot 看画面，再用 target.text（按文字 OCR 定位）、target.visual 或 target.point 操作。".into()
    } else {
        format!("窗口里现有的控件如：{}。可先 desktop_snapshot 查看完整结构。", cands.join("；"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(control: i32, name: &str, children: Vec<usize>) -> Info {
        Info { control, name: name.into(), enabled: true, rect: Some([0, 0, 10, 10]), children, ..Default::default() }
    }

    fn sample() -> Vec<Info> {
        let mut save = node(BUTTON, "保存", vec![]);
        save.patterns = P_INVOKE;
        let mut edit = node(EDIT, "文件名", vec![]);
        edit.patterns = P_VALUE;
        edit.value = Some("a.txt".into());
        let mut hidden = node(BUTTON, "隐藏", vec![]);
        hidden.offscreen = true;
        hidden.patterns = P_INVOKE;
        let mut save2 = node(BUTTON, "保存为模板", vec![]);
        save2.patterns = P_INVOKE;
        vec![
            node(WINDOW, "记事本", vec![1, 5]),
            node(PANE, "", vec![2, 3, 4]), // 无名容器：透明
            save,
            edit,
            hidden,
            save2,
        ]
    }

    #[test]
    fn roles_resolve_in_both_languages() {
        assert_eq!(role_id("Button"), Some(BUTTON));
        assert_eq!(role_id("按钮"), Some(BUTTON));
        assert_eq!(role_id("textbox"), Some(EDIT));
        assert_eq!(role_id("menu_item"), Some(MENU_ITEM));
        assert_eq!(role_id("什么"), None);
    }

    #[test]
    fn render_skips_anonymous_containers_and_offscreen() {
        let nodes = sample();
        let r = render(&nodes, &[(0, "窗口「记事本」".into())], &RenderOpts { max_chars: 4000, query: None, include_offscreen: false, max_depth: None });
        assert_eq!(r.refs, vec![2, 3, 5]);
        assert!(r.text.contains("[1] 按钮 \"保存\""));
        assert!(r.text.contains("[2] 输入框 \"文件名\" 值=\"a.txt\""));
        assert!(!r.text.contains("隐藏"));
        assert!(!r.text.contains("面板"));
    }

    #[test]
    fn render_respects_char_budget() {
        let nodes = sample();
        let r = render(&nodes, &[(0, "w".into())], &RenderOpts { max_chars: 40, query: None, include_offscreen: false, max_depth: None });
        assert!(r.truncated);
        assert_eq!(r.refs.len(), 1);
    }

    #[test]
    fn find_prefers_exact_name_then_contains() {
        let nodes = sample();
        let roots = vec![(0, String::new())];
        let q = Query { name: Some("保存".into()), ..Default::default() };
        assert_eq!(find(&nodes, &roots, &q), vec![2]);
        let q = Query { name: Some("模板".into()), ..Default::default() };
        assert_eq!(find(&nodes, &roots, &q), vec![5]);
        // 只在屏幕外的也能找到
        let q = Query { name: Some("隐藏".into()), ..Default::default() };
        assert_eq!(find(&nodes, &roots, &q), vec![4]);
        let q = Query { roles: Some(vec![EDIT]), ..Default::default() };
        assert_eq!(find(&nodes, &roots, &q), vec![3]);
    }

    #[test]
    fn visible_children_of_offscreen_popup_are_kept() {
        let mut popup = node(WINDOW, "弹出窗口", vec![2]);
        popup.offscreen = true;
        let mut item = node(MENU_ITEM, "新建", vec![]);
        item.patterns = P_INVOKE;
        let nodes = vec![node(WINDOW, "主窗口", vec![1]), popup, item];
        let r = render(&nodes, &[(0, "w".into())], &RenderOpts { max_chars: 4000, query: None, include_offscreen: false, max_depth: None });
        assert_eq!(r.refs, vec![2]);
        let q = Query { name: Some("新建".into()), ..Default::default() };
        assert_eq!(find(&nodes, &[(0, String::new())], &q), vec![2]);
    }

    #[test]
    fn password_values_are_masked() {
        let mut n = node(EDIT, "密码", vec![]);
        n.patterns = P_VALUE;
        n.password = true;
        n.value = Some("secret".into());
        assert!(describe(&n).contains("值=***"));
        assert!(!matches_text(&n, "secret"));
    }
}
