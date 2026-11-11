//! 桌面软件操控：读取窗口的 UI Automation 无障碍树，按编号/名称定位控件，
//! 优先用控件模式（不抢鼠标、窗口在后台也能操作），不行再用真实鼠标键盘。
//! 读不到结构的界面（自绘、游戏、部分 Electron）用截图 + 本地 OCR 按文字定位、Jev 按画面定位或直接给坐标。

pub mod tree;
#[cfg(windows)]
mod input;
#[cfg(windows)]
mod uia;

pub const TOOL_NAMES: &[&str] = &["desktop_window", "desktop_snapshot", "desktop_action", "desktop_screenshot"];

#[cfg(not(windows))]
pub async fn execute(_app: &tauri::AppHandle, _name: &str, _args: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    anyhow::bail!("桌面软件操控只支持 Windows")
}

#[cfg(not(windows))]
pub async fn element_rect(_hwnd: isize, _target: &serde_json::Value) -> anyhow::Result<([i32; 4], String)> {
    anyhow::bail!("按控件截图只支持 Windows")
}

#[cfg(windows)]
pub use imp::{element_rect, execute};
#[cfg(all(test, windows))]
use imp::{perform, Target};

#[cfg(windows)]
mod imp {
    use super::input;
    use super::tree::{self, Query, RenderOpts};
    use super::uia;
    use crate::recorder::screenshot::{self, WinInfo};
    use anyhow::{anyhow, bail, Context, Result};
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::time::{Duration, Instant};
    use tauri::{AppHandle, Manager};

    /// 最近操作过的窗口：不写 window 时默认沿用
    static LAST_HWND: AtomicIsize = AtomicIsize::new(0);

    fn str_arg<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
        v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
    }

    fn settings(app: &AppHandle) -> Value {
        crate::plugins::settings::values(app, "desktop")
    }

    fn blocked_list(app: &AppHandle) -> Vec<String> {
        settings(app)
            .get("blocked_processes")
            .and_then(Value::as_str)
            .unwrap_or("")
            .split([',', '，', '\n', ';'])
            .map(|s| s.trim().trim_end_matches(".exe").to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// 同一软件的常见叫法（进程名 / 英文名 / 中文名 / 开始菜单快捷方式名），小写
    const APP_ALIASES: &[&[&str]] = &[
        &["weixin", "wechat", "微信"],
        &["wxwork", "wecom", "企业微信"],
        &["qq", "qqnt"],
        &["dingtalk", "钉钉"],
        &["feishu", "lark", "飞书"],
        &["wps", "wps office"],
        &["winword", "word"],
        &["excel"],
        &["powerpnt", "powerpoint"],
        &["notepad", "记事本"],
        &["calc", "calculatorapp", "calculator", "计算器"],
    ];
    /// 本来就该每次开新窗口的程序，不做"已在运行就唤出"的处理
    const MULTI_WINDOW: &[&str] = &["explorer", "资源管理器", "文件资源管理器", "cmd", "powershell", "pwsh", "wt"];
    /// 系统和常驻组件，不算"托盘里运行的软件"
    const SYSTEM_PROCS: &[&str] = &[
        "explorer", "svchost", "applicationframehost", "textinputhost", "systemsettings", "shellexperiencehost",
        "startmenuexperiencehost", "searchhost", "searchapp", "msedgewebview2", "runtimebroker", "ctfmon", "dwm", "sihost",
        "taskhostw", "conhost", "widgets", "widgetservice", "lockapp", "securityhealthsystray", "csrss", "winlogon",
        "rundll32", "dllhost", "smartscreen", "phoneexperiencehost", "crossdeviceresume", "",
    ];

    fn proc_stem(process: &str) -> String {
        process.to_lowercase().trim_end_matches(".exe").to_string()
    }

    /// open 的目标若是一个软件（而不是网址、文档），返回可能的进程名 / 快捷方式名
    fn app_terms(target: &str) -> Option<Vec<String>> {
        let t = target.trim().trim_matches('"');
        let lower = t.to_lowercase();
        let is_path = t.contains('\\') || t.contains('/');
        // 网址、ms-settings: 这类协议（排除 C:\ 盘符路径）
        if lower.contains("://") || (t.contains(':') && !is_path) {
            return None;
        }
        let path = std::path::Path::new(t);
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase());
        if !matches!(ext.as_deref(), None | Some("exe") | Some("lnk")) {
            return None; // 文档、图片等交给默认程序打开
        }
        let stem = path.file_stem()?.to_string_lossy().to_lowercase();
        if MULTI_WINDOW.contains(&stem.as_str()) {
            return None;
        }
        let mut terms = vec![stem.clone()];
        if let Some(group) = APP_ALIASES.iter().find(|g| g.contains(&stem.as_str())) {
            terms.extend(group.iter().map(|s| s.to_string()).filter(|s| *s != stem));
        }
        Some(terms)
    }

    /// 已在运行的同名软件：可见窗口和隐藏（托盘）主窗口
    fn running_windows(terms: &[String]) -> (Vec<WinInfo>, Vec<WinInfo>) {
        let hit = |w: &WinInfo| terms.iter().any(|t| *t == proc_stem(&w.process));
        let visible: Vec<WinInfo> = screenshot::list_windows(false).into_iter().filter(|w| hit(w)).collect();
        let me = std::process::id();
        let hidden = screenshot::win::list_hidden_windows().into_iter().filter(|w| w.pid != me && hit(w)).collect();
        (visible, hidden)
    }

    /// 隐藏窗口里最像主窗口的：面积最大的那个
    fn main_hidden(hidden: &[WinInfo]) -> Option<&WinInfo> {
        hidden.iter().max_by_key(|w| (w.rect[2].max(0) as i64) * (w.rect[3].max(0) as i64))
    }

    /// 有隐藏主窗口、却没有任何可见窗口的软件，多半缩在托盘里运行
    fn background_apps(visible: &[WinInfo]) -> Vec<Value> {
        let me = std::process::id();
        let shown: std::collections::HashSet<u32> = visible.iter().map(|w| w.pid).collect();
        let hidden = screenshot::win::list_hidden_windows();
        let mut by_proc: Vec<(String, Vec<WinInfo>)> = Vec::new();
        for w in hidden {
            let stem = proc_stem(&w.process);
            if w.pid == me || shown.contains(&w.pid) || SYSTEM_PROCS.contains(&stem.as_str()) {
                continue;
            }
            // 同一软件的多个进程只要有一个窗口可见，就不算后台
            if visible.iter().any(|v| proc_stem(&v.process) == stem) {
                continue;
            }
            match by_proc.iter_mut().find(|(p, _)| *p == stem) {
                Some((_, ws)) => ws.push(w),
                None => by_proc.push((stem, vec![w])),
            }
        }
        // 只给进程名：隐藏窗口里哪个是主窗口说不准，交给 open 去判断和唤出
        by_proc.into_iter().filter_map(|(_, ws)| ws.first().map(|w| json!(w.process))).take(30).collect()
    }

    /// 在开始菜单里按名字找快捷方式（程序名不在 PATH / App Paths 里时用）
    fn start_menu_find(terms: &[String]) -> Option<std::path::PathBuf> {
        let mut roots = Vec::new();
        if let Ok(p) = std::env::var("ProgramData") {
            roots.push(std::path::PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs"));
        }
        if let Ok(p) = std::env::var("APPDATA") {
            roots.push(std::path::PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs"));
        }
        let mut links = Vec::new();
        fn walk(dir: &std::path::Path, depth: u32, out: &mut Vec<std::path::PathBuf>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && depth > 0 {
                    walk(&p, depth - 1, out);
                } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) {
                    out.push(p);
                }
            }
        }
        for r in &roots {
            walk(r, 3, &mut links);
        }
        let name = |p: &std::path::PathBuf| p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        // 先找名字完全一致的，再找包含的；跳过卸载/帮助类快捷方式
        let skip = |n: &str| ["卸载", "uninstall", "帮助", "help", "readme"].iter().any(|k| n.contains(k));
        for t in terms {
            if let Some(p) = links.iter().find(|p| name(p) == *t) {
                return Some(p.clone());
            }
        }
        for t in terms.iter().filter(|t| t.chars().count() >= 2) {
            if let Some(p) = links.iter().find(|p| {
                let n = name(p);
                n.contains(t.as_str()) && !skip(&n)
            }) {
                return Some(p.clone());
            }
        }
        None
    }

    fn win_json(w: &WinInfo) -> Value {
        json!({"hwnd": w.hwnd, "title": w.title, "process": w.process, "rect": w.rect, "minimized": w.minimized})
    }

    /// window 参数：数字为窗口句柄，字符串为标题或进程名的一部分；不给则沿用上次的窗口，再不行取最上层的窗口
    fn resolve_window(app: &AppHandle, raw: Option<&Value>) -> Result<WinInfo> {
        let all = screenshot::list_windows(false);
        let w = match raw {
            Some(Value::Number(n)) => {
                let h = n.as_i64().unwrap_or(0);
                all.iter()
                    .find(|w| w.hwnd == h)
                    .cloned()
                    .or_else(|| screenshot::win::list_hidden_windows().into_iter().find(|w| w.hwnd == h))
                    .ok_or_else(|| anyhow!("窗口句柄 {h} 不存在或已关闭，用 desktop_window 重新列出"))?
            }
            Some(Value::String(s)) if !s.trim().is_empty() => {
                if let Ok(h) = s.trim().parse::<i64>() {
                    if let Some(w) = all.iter().find(|w| w.hwnd == h) {
                        return guard(app, w.clone());
                    }
                }
                screenshot::find_window(s, false)?.0
            }
            _ => {
                let last = LAST_HWND.load(Ordering::Relaxed) as i64;
                all.iter()
                    .find(|w| w.hwnd == last)
                    .or_else(|| all.iter().find(|w| !w.minimized))
                    .cloned()
                    .ok_or_else(|| anyhow!("没有可操作的窗口"))?
            }
        };
        guard(app, w)
    }

    fn guard(app: &AppHandle, w: WinInfo) -> Result<WinInfo> {
        let process = w.process.trim_end_matches(".exe").to_lowercase();
        if blocked_list(app).iter().any(|b| *b == process) {
            bail!("「{}」（{}）在桌面操控黑名单里，拾光不会操作它。需要的话请用户在 插件 → 桌面操控 设置里移除", w.title, w.process);
        }
        if input::blocked_by_elevation(w.pid) {
            bail!("「{}」（{}）以管理员权限运行，系统不允许普通权限的拾光操作它。可让用户以管理员身份重启拾光，或改为普通权限启动该软件", w.title, w.process);
        }
        LAST_HWND.store(w.hwnd as isize, Ordering::Relaxed);
        Ok(w)
    }

    fn temp_png(app: &AppHandle, prefix: &str) -> Result<std::path::PathBuf> {
        let dir = crate::tempfs::ensure_temp_dir(app)?.join("desktop");
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join(format!("{prefix}-{}.png", chrono::Local::now().format("%Y%m%d-%H%M%S-%3f"))))
    }

    fn capture(app: &AppHandle, w: &WinInfo, prefix: &str) -> Result<(std::path::PathBuf, image::RgbaImage)> {
        if w.minimized {
            input::show(w.hwnd as isize, "restore")?;
            std::thread::sleep(Duration::from_millis(300));
        }
        let fresh = screenshot::list_windows(false).into_iter().find(|x| x.hwnd == w.hwnd).unwrap_or_else(|| w.clone());
        let img = screenshot::capture_window_best(&fresh)?;
        let path = temp_png(app, prefix)?;
        img.save(&path).context("保存截图失败")?;
        Ok((path, img))
    }

    // ---------- 工具入口 ----------

    pub async fn execute(app: &AppHandle, name: &str, args: &Value) -> Result<Value> {
        match name {
            "desktop_window" => window_tool(app, args).await,
            "desktop_snapshot" => snapshot_tool(app, args).await,
            "desktop_action" => action_tool(app, args).await,
            "desktop_screenshot" => screenshot_tool(app, args).await,
            other => bail!("未知桌面工具 {other}"),
        }
    }

    async fn window_tool(app: &AppHandle, args: &Value) -> Result<Value> {
        let action = str_arg(args, "action").unwrap_or("list");
        match action {
            "list" => {
                let all = screenshot::list_windows(false);
                let fg = input::foreground() as i64;
                let list: Vec<Value> = all
                    .iter()
                    .map(|w| {
                        let mut v = win_json(w);
                        if w.hwnd == fg {
                            v["foreground"] = json!(true);
                        }
                        v
                    })
                    .collect();
                let mut v = json!({"count": list.len(), "windows": list, "note": "按层叠顺序从上到下。其它工具的 window 参数可写标题/进程名的一部分，或 hwnd 数字"});
                let bg = background_apps(&all);
                if !bg.is_empty() {
                    v["background"] = json!(bg);
                    v["background_note"] = json!("这些进程在运行但没有可见窗口（多半缩在托盘里）。要用其中的软件时用 action=open（target 写进程名或软件名）唤出，open 不会重复启动");
                }
                Ok(v)
            }
            "open" => {
                let target = str_arg(args, "target").ok_or_else(|| anyhow!("open 需要 target：程序名（notepad、calc、winword、微信）、完整路径、文件或网址（ms-settings:）"))?;
                let force_new = args.get("force_new").and_then(Value::as_bool).unwrap_or(false);
                let terms = app_terms(target);
                // 软件已经在运行：唤出已有窗口，不再启动第二个（有的软件会开出第二个实例，比如微信会弹登录页）
                if let (Some(terms), false) = (&terms, force_new || str_arg(args, "args").is_some()) {
                    let (visible, hidden) = running_windows(terms);
                    let reuse = |w: WinInfo, how: &str| -> Result<Value> {
                        let w = screenshot::list_windows(false).into_iter().find(|x| x.hwnd == w.hwnd).unwrap_or(w);
                        let w = guard(app, w)?;
                        Ok(json!({"ok": true, "already_running": true, "how": how, "window": win_json(&w),
                            "note": "软件已在运行，已切到它的窗口，没有重新启动。确实要再开一个用 force_new=true"}))
                    };
                    if let Some(w) = visible.iter().find(|w| !w.minimized).or(visible.first()).cloned() {
                        input::activate(w.hwnd as isize)?;
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        return reuse(w, "activated");
                    }
                    if let Some(w) = main_hidden(&hidden).cloned() {
                        guard(app, w.clone())?;
                        let shown = input::unhide(w.hwnd as isize);
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        if shown.is_ok() && screenshot::win::is_visible(w.hwnd as isize) {
                            return reuse(w, "restored_from_tray");
                        }
                        return Ok(json!({"ok": false, "already_running": true, "window": win_json(&w),
                            "error": format!("「{}」已在后台（托盘）运行，但没能把窗口唤出来。没有重新启动它，以免开出第二个实例", w.title),
                            "next": "请用户点一下托盘图标，或用软件自己的快捷键唤出（微信默认 Ctrl+Alt+W），然后 action=list 确认"}));
                    }
                    // 进程在跑但找不到能唤出的窗口（比如正在启动、只有后台服务进程）
                    let mut sys = sysinfo::System::new();
                    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
                    let me = std::process::id();
                    if let Some(p) = sys.processes().values().find(|p| p.pid().as_u32() != me && terms.contains(&proc_stem(&p.name().to_string_lossy()))) {
                        return Ok(json!({"ok": false, "already_running": true, "process": p.name().to_string_lossy(),
                            "error": format!("{} 已在运行，但找不到它的窗口。没有重新启动它，以免开出第二个实例", p.name().to_string_lossy()),
                            "next": "可能还在启动，稍等后 action=list 再看；或请用户点托盘图标唤出。用户明确要多开才用 force_new=true"}));
                    }
                }
                let before: Vec<i64> = screenshot::list_windows(false).iter().map(|w| w.hwnd).collect();
                let mut launched = target.to_string();
                if let Err(e) = input::shell_open(target, str_arg(args, "args"), str_arg(args, "workdir")) {
                    // 程序名不在 PATH / App Paths 里：去开始菜单找同名快捷方式
                    let lnk = terms.as_deref().filter(|_| !target.contains('\\') && !target.contains('/')).and_then(start_menu_find);
                    let Some(lnk) = lnk else { return Err(e) };
                    launched = lnk.to_string_lossy().into_owned();
                    input::shell_open(&launched, str_arg(args, "args"), str_arg(args, "workdir"))?;
                }
                let target = launched.as_str();
                let wait = args.get("wait_ms").and_then(Value::as_u64).unwrap_or(8000).min(30000);
                let deadline = Instant::now() + Duration::from_millis(wait);
                while Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    if let Some(w) = screenshot::list_windows(false).into_iter().find(|w| !before.contains(&w.hwnd)) {
                        // 窗口刚出现时标题可能还没设好，稍等一下再取
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        let w = screenshot::list_windows(false).into_iter().find(|x| x.hwnd == w.hwnd).unwrap_or(w);
                        let w = guard(app, w)?;
                        return Ok(json!({"ok": true, "opened": target, "window": win_json(&w)}));
                    }
                }
                Ok(json!({"ok": true, "opened": target, "window": null, "note": "已启动，但等待期间没有出现新窗口（可能软件已在运行、启动较慢或只在托盘）。用 action=list 查看"}))
            }
            "focus" | "close" | "minimize" | "maximize" | "restore" | "move" => {
                let w = resolve_window(app, args.get("window"))?;
                let h = w.hwnd as isize;
                match action {
                    "focus" if !screenshot::win::is_visible(h) => input::unhide(h)?,
                    "focus" => input::activate(h)?,
                    "close" => input::close(h)?,
                    "move" => {
                        let r: Vec<i32> = args
                            .get("rect")
                            .and_then(Value::as_array)
                            .map(|a| a.iter().filter_map(Value::as_f64).map(|f| f as i32).collect())
                            .unwrap_or_default();
                        let [x, y, ww, hh] = r[..] else { bail!("move 需要 rect [x, y, 宽, 高]（屏幕物理像素）") };
                        input::move_window(h, [x, y, ww, hh])?;
                    }
                    other => input::show(h, other)?,
                }
                tokio::time::sleep(Duration::from_millis(400)).await;
                let after = screenshot::list_windows(false).into_iter().find(|x| x.hwnd == w.hwnd);
                let mut v = json!({"ok": true, "action": action, "window": win_json(&w)});
                match after {
                    Some(a) => {
                        v["now"] = win_json(&a);
                        v["maximized"] = json!(input::is_maximized(h));
                        v["foreground"] = json!(input::foreground() == h);
                    }
                    None if action == "close" => v["closed"] = json!(true),
                    None => {}
                }
                if action == "close" && input::exists(h) {
                    v["note"] = json!("窗口还在：可能弹出了「是否保存」之类的确认框，用 desktop_snapshot 查看");
                }
                Ok(v)
            }
            other => bail!("未知 action {other}（list/open/focus/close/minimize/maximize/restore/move）"),
        }
    }

    async fn snapshot_tool(app: &AppHandle, args: &Value) -> Result<Value> {
        let w = resolve_window(app, args.get("window"))?;
        let opts = RenderOpts {
            max_chars: args.get("max_chars").and_then(Value::as_u64).unwrap_or(5000).clamp(500, 30000) as usize,
            query: str_arg(args, "query").map(str::to_string),
            include_offscreen: args.get("include_offscreen").and_then(Value::as_bool).unwrap_or(false),
            max_depth: args.get("max_depth").and_then(Value::as_u64).map(|d| d as usize),
        };
        let h = w.hwnd as isize;
        let started = Instant::now();
        let r = uia::run(move |ctx| ctx.snapshot(h, &opts)).await?;
        let mut v = json!({
            "window": win_json(&w),
            "snapshot": r.text,
            "refs": r.refs.len(),
            "interactive": r.interactive,
            "elapsed_ms": started.elapsed().as_millis() as u64,
        });
        if r.truncated {
            v["truncated"] = json!(true);
            v["hint"] = json!("快照超出字数上限被截断：用 query 只看相关控件，或加大 max_chars");
        }
        if r.truncated_tree {
            v["tree_truncated"] = json!(true);
        }
        if r.interactive <= 2 {
            v["sparse"] = json!(true);
            v["hint"] = json!("这个窗口几乎没有暴露可操作控件（自绘界面/游戏/部分 Electron 应用）。改用 desktop_screenshot（可加 ocr=true）看画面，再用 target.text / target.visual / target.point 操作");
        }
        Ok(v)
    }

    async fn screenshot_tool(app: &AppHandle, args: &Value) -> Result<Value> {
        let w = resolve_window(app, args.get("window"))?;
        let (path, img) = capture(app, &w, "window")?;
        let state = app.state::<crate::AppState>();
        let vision = crate::commands::load_settings(&state.db).model_supports_vision;
        let mut v = json!({
            "path": path.to_string_lossy().replace('\\', "/"),
            "size": [img.width(), img.height()],
            "window": win_json(&w),
            "native_image": vision,
            "coordinates": "图上像素坐标（左上角为 0,0）可直接作为 desktop_action 的 target.point",
        });
        if args.get("ocr").and_then(Value::as_bool).unwrap_or(false) {
            let boxes = tokio::task::block_in_place(|| state.ocr.recognize_boxes(&path))?;
            let lines: Vec<Value> = boxes.iter().take(300).map(|b| json!({"text": b.text, "rect": b.rect})).collect();
            v["ocr"] = json!(lines);
        } else if !vision {
            v["hint"] = json!("当前模型不能直接看图：要知道画面上有什么，加 ocr=true 取文字及位置，或用 read_image 让视觉模型描述");
        }
        Ok(v)
    }

    // ---------- desktop_action ----------

    /// 已解析好的目标
    #[derive(Clone)]
    pub(super) enum Target {
        None,
        Ref(usize),
        Query(Query),
        /// 屏幕绝对坐标（物理像素）及定位说明
        Screen(i32, i32, Value),
    }

    const TARGET_FIELDS: &[&str] = &["ref", "name", "role", "automation_id", "class", "index", "text", "visual", "point"];

    fn parse_query(t: &serde_json::Map<String, Value>) -> Result<Option<Query>> {
        let name = t.get("name").and_then(Value::as_str).map(str::to_string);
        let roles = match t.get("role").and_then(Value::as_str) {
            Some(r) => Some(vec![tree::role_id(r).ok_or_else(|| anyhow!("不认识的 role「{r}」：可用 button/edit/checkbox/combobox/listitem/menuitem/tabitem/treeitem/hyperlink/text/document/dataitem 等，或中文 按钮/输入框/复选框/下拉框…"))?]),
            None => None,
        };
        let automation_id = t.get("automation_id").and_then(Value::as_str).map(str::to_string);
        let class = t.get("class").and_then(Value::as_str).map(str::to_string);
        if name.is_none() && roles.is_none() && automation_id.is_none() && class.is_none() {
            return Ok(None);
        }
        Ok(Some(Query { name, roles, automation_id, class, index: t.get("index").and_then(Value::as_u64).map(|i| i as usize), include_offscreen: false }))
    }

    /// 控件在屏幕上的边框（物理像素）与描述，给截图按控件裁剪用。
    /// element 同 desktop_action 的 target：ref，或 name / role / automation_id / class（+index）
    pub async fn element_rect(hwnd: isize, element: &Value) -> Result<([i32; 4], String)> {
        let t = element.as_object().ok_or_else(|| anyhow!("element 必须是对象，如 {{\"name\": \"设置\", \"role\": \"button\"}}"))?;
        let by_ref = match t.get("ref") {
            Some(r) => Some(r.as_u64().or_else(|| r.as_str().and_then(|s| s.trim().parse().ok())).ok_or_else(|| anyhow!("ref 必须是 desktop_snapshot 里的数字编号"))? as usize),
            None => None,
        };
        let q = match by_ref {
            Some(_) => None,
            None => Some(parse_query(t)?.ok_or_else(|| anyhow!("element 至少给 ref、name、role、automation_id 之一"))?),
        };
        uia::run(move |ctx| {
            let (el, info) = match (by_ref, &q) {
                (Some(r), _) => ctx.by_ref(hwnd, r)?,
                (None, Some(q)) => ctx.find(hwnd, q)?,
                _ => unreachable!(),
            };
            uia::scroll_into_view(&el);
            let what = tree::describe(&info);
            let rect = uia::current_rect(&el).ok_or_else(|| anyhow!("{what} 没有可见位置（可能被折叠或在屏幕外）"))?;
            Ok((rect, what))
        })
        .await
    }

    async fn resolve_target(app: &AppHandle, w: &WinInfo, raw: Option<&Value>) -> Result<Target> {
        let Some(raw) = raw.filter(|v| !v.is_null()) else { return Ok(Target::None) };
        let t = raw.as_object().ok_or_else(|| anyhow!("target 必须是对象，如 {{\"ref\": 3}} 或 {{\"name\": \"保存\", \"role\": \"button\"}}"))?;
        if let Some(bad) = t.keys().find(|k| !TARGET_FIELDS.contains(&k.as_str())) {
            bail!("target 不支持字段 {bad}；可用 ref / name+role+automation_id+class(+index) / text / visual / point");
        }
        if let Some(r) = t.get("ref") {
            let r = r.as_u64().or_else(|| r.as_str().and_then(|s| s.trim().parse().ok())).ok_or_else(|| anyhow!("ref 必须是快照里的数字编号"))?;
            return Ok(Target::Ref(r as usize));
        }
        if let Some(p) = t.get("point").and_then(Value::as_array) {
            let xy: Vec<f64> = p.iter().filter_map(Value::as_f64).collect();
            let [x, y] = xy[..] else { bail!("point 需要 [x, y]：desktop_screenshot 截图上的像素坐标") };
            let [wx, wy, ww, wh] = w.rect;
            if x < 0.0 || y < 0.0 || x > ww as f64 || y > wh as f64 {
                bail!("point ({x}, {y}) 超出窗口范围 {ww}×{wh}");
            }
            return Ok(Target::Screen(wx + x.round() as i32, wy + y.round() as i32, json!({"point": [x, y]})));
        }
        if let Some(text) = t.get("text").and_then(Value::as_str) {
            let index = t.get("index").and_then(Value::as_u64).map(|i| i as usize);
            return ocr_locate(app, w, text, index).await;
        }
        if let Some(desc) = t.get("visual").and_then(Value::as_str) {
            return visual_locate(app, w, desc).await;
        }
        match parse_query(t)? {
            Some(q) => Ok(Target::Query(q)),
            None => bail!("target 至少给一个定位条件"),
        }
    }

    fn norm(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase()
    }

    /// 截图 + 本地 OCR，按文字找位置（先整行相等，再包含）
    async fn ocr_locate(app: &AppHandle, w: &WinInfo, text: &str, index: Option<usize>) -> Result<Target> {
        let (path, _) = capture(app, w, "ocr")?;
        let state = app.state::<crate::AppState>();
        let boxes = tokio::task::block_in_place(|| state.ocr.recognize_boxes(&path))?;
        let want = norm(text);
        let mut hits: Vec<&crate::ocr::OcrBox> = boxes.iter().filter(|b| norm(&b.text) == want).collect();
        if hits.is_empty() {
            hits = boxes.iter().filter(|b| norm(&b.text).contains(&want)).collect();
        }
        let pick = match (hits.len(), index) {
            (0, _) => {
                let seen: Vec<&str> = boxes.iter().take(30).map(|b| b.text.as_str()).collect();
                bail!("画面上没有识别到文字「{text}」。识别到的有：{}", seen.join("、"));
            }
            (n, Some(i)) if i >= 1 && i <= n => hits[i - 1],
            (n, Some(i)) => bail!("index {i} 超出范围：共 {n} 处「{text}」"),
            (1, None) => hits[0],
            (n, None) => {
                let list: Vec<String> = hits.iter().take(8).enumerate().map(|(k, b)| format!("{}. 「{}」@({}, {})", k + 1, b.text, b.rect[0], b.rect[1])).collect();
                bail!("画面上有 {n} 处「{text}」，用 index 指定第几个（按从上到下、从左到右）：{}", list.join("；"));
            }
        };
        let [x, y, bw, bh] = pick.rect;
        // 整行比目标长时（如「文件 编辑 查看」），按目标在行内的位置估算横坐标
        let line = norm(&pick.text);
        let cx = match line.find(&want) {
            Some(pos) if line.chars().count() > want.chars().count() => {
                let before = line[..pos].chars().count() as f64;
                let total = line.chars().count() as f64;
                let len = want.chars().count() as f64;
                x as f64 + bw as f64 * (before + len / 2.0) / total
            }
            _ => x as f64 + bw as f64 / 2.0,
        };
        let cy = y + bh / 2;
        Ok(Target::Screen(w.rect[0] + cx.round() as i32, w.rect[1] + cy, json!({"ocr_text": pick.text, "rect": pick.rect})))
    }

    /// 截图交给 Jev 找目标
    async fn visual_locate(app: &AppHandle, w: &WinInfo, desc: &str) -> Result<Target> {
        use base64::Engine as _;
        let state = app.state::<crate::AppState>();
        let settings = crate::commands::load_settings(&state.db);
        if !crate::system_one::configured(&settings) {
            bail!("按画面定位需要在 设置 → 智能加速 里启用并配置 TypeSafe（Jev）。也可以先 desktop_screenshot 看图，再用 target.point 或 target.text");
        }
        let (_, img) = capture(app, w, "visual")?;
        let (iw, ih) = (img.width(), img.height());
        if iw < 280 || ih < 280 {
            bail!("窗口太小（{iw}×{ih}），按画面定位要求每边至少 280 像素");
        }
        let scale = (1568.0 / iw.max(ih) as f64).min(1.0);
        let small = if scale < 1.0 {
            image::imageops::resize(&img, (iw as f64 * scale) as u32, (ih as f64 * scale) as u32, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        if small.width() < 280 || small.height() < 280 {
            bail!("窗口长宽比太极端，无法按画面定位，请改用 target.point 或 target.text");
        }
        let mut png = Vec::new();
        small.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let found = crate::system_one::locate_in_screenshot(Some(app), &reqwest::Client::new(), &settings, &b64, desc).await?;
        if found.present < 0.5 || found.bbox.confidence.is_some_and(|c| c < 0.3) {
            bail!("画面上没有把握找到「{desc}」（存在度 {:.2}）。换个描述，或 desktop_screenshot 看图后用 target.point", found.present);
        }
        let (x, y) = found.bbox.center_in(iw as f64, ih as f64);
        Ok(Target::Screen(w.rect[0] + x.round() as i32, w.rect[1] + y.round() as i32, json!({"visual": desc, "present": found.present, "point": [x.round(), y.round()]})))
    }

    struct Before {
        windows: Vec<WinInfo>,
        title: String,
    }

    fn observe(w: &WinInfo) -> Before {
        let windows = screenshot::list_windows(false);
        let title = windows.iter().find(|x| x.hwnd == w.hwnd).map(|x| x.title.clone()).unwrap_or_default();
        Before { windows, title }
    }

    /// 操作后的可观察变化：新弹出/关闭的窗口、前台窗口、标题变化
    fn effect(w: &WinInfo, before: &Before) -> Value {
        let after = screenshot::list_windows(false);
        let opened: Vec<Value> = after.iter().filter(|a| !before.windows.iter().any(|b| b.hwnd == a.hwnd)).map(|a| json!({"hwnd": a.hwnd, "title": a.title, "process": a.process})).collect();
        let closed: Vec<Value> = before.windows.iter().filter(|b| !after.iter().any(|a| a.hwnd == b.hwnd)).map(|b| json!({"title": b.title, "process": b.process})).collect();
        let fg = input::foreground() as i64;
        let mut v = json!({});
        if !opened.is_empty() {
            v["opened_windows"] = json!(opened);
        }
        if !closed.is_empty() {
            v["closed_windows"] = json!(closed);
        }
        match after.iter().find(|a| a.hwnd == w.hwnd) {
            None => v["target_closed"] = json!(true),
            Some(a) if a.title != before.title => v["title_changed"] = json!(a.title),
            _ => {}
        }
        if let Some(f) = after.iter().find(|a| a.hwnd == fg) {
            if f.hwnd != w.hwnd {
                v["foreground"] = json!(f.title);
            }
        }
        v
    }

    async fn action_tool(app: &AppHandle, args: &Value) -> Result<Value> {
        let mut w = resolve_window(app, args.get("window"))?;
        // 最小化的窗口尺寸只剩标题栏，坐标和截图都对不上，先还原
        if w.minimized {
            input::show(w.hwnd as isize, "restore")?;
            tokio::time::sleep(Duration::from_millis(400)).await;
            if let Some(fresh) = screenshot::list_windows(false).into_iter().find(|x| x.hwnd == w.hwnd) {
                w = fresh;
            }
        }
        let Some(steps) = args.get("steps").and_then(Value::as_array) else {
            let before = observe(&w);
            let mut r = run_step(app, &w, args).await?;
            let settle = args.get("settle_ms").and_then(Value::as_u64).unwrap_or(400).min(10000);
            if !matches!(str_arg(args, "action"), Some("read" | "locate" | "wait")) {
                tokio::time::sleep(Duration::from_millis(settle)).await;
                r["effect"] = effect(&w, &before);
            }
            r["window"] = json!(w.title);
            return Ok(r);
        };
        if steps.len() > 20 {
            bail!("steps 最多 20 步");
        }
        let total = steps.len();
        let before = observe(&w);
        let mut results = Vec::new();
        for (i, step) in steps.iter().enumerate() {
            let r = run_step(app, &w, step).await;
            let settle = step.get("settle_ms").and_then(Value::as_u64).unwrap_or(250).min(10000);
            tokio::time::sleep(Duration::from_millis(settle)).await;
            match r {
                Ok(v) => results.push(v),
                Err(e) => {
                    results.push(json!({"ok": false, "error": format!("{e:#}")}));
                    return Ok(json!({"ok": false, "completed": i, "total": total, "error": format!("第 {} 步失败：{e:#}", i + 1), "steps": results, "effect": effect(&w, &before), "window": w.title}));
                }
            }
            if !input::exists(w.hwnd as isize) && i + 1 < total {
                return Ok(json!({"ok": false, "completed": i + 1, "total": total, "error": "目标窗口已关闭，后续步骤未执行", "steps": results, "effect": effect(&w, &before)}));
            }
        }
        Ok(json!({"ok": true, "completed": total, "total": total, "steps": results, "effect": effect(&w, &before), "window": w.title}))
    }

    async fn run_step(app: &AppHandle, w: &WinInfo, step: &Value) -> Result<Value> {
        let action = str_arg(step, "action").ok_or_else(|| anyhow!("缺少 action"))?.to_string();
        if action == "wait" {
            return wait_step(app, w, step).await;
        }
        let target = resolve_target(app, w, step.get("target")).await?;
        let to = match step.get("to") {
            Some(t) if action == "drag" => Some(resolve_target(app, w, Some(t)).await?),
            _ => None,
        };
        let step = step.clone();
        let (h, pid) = (w.hwnd as isize, w.pid);
        let rect = w.rect;
        uia::run(move |ctx| perform(ctx, h, pid, rect, &action, target, to, &step)).await
    }

    async fn wait_step(app: &AppHandle, w: &WinInfo, step: &Value) -> Result<Value> {
        let timeout = step.get("timeout_ms").and_then(Value::as_u64).unwrap_or(5000).min(30000);
        let gone = step.get("gone").and_then(Value::as_bool).unwrap_or(false);
        let t = step.get("target").and_then(Value::as_object).ok_or_else(|| anyhow!("wait 需要 target（name/role… 或 text）"))?;
        let deadline = Instant::now() + Duration::from_millis(timeout);
        let started = Instant::now();
        let by_text = t.get("text").and_then(Value::as_str).map(str::to_string);
        let q = if by_text.is_none() { parse_query(t)?.ok_or_else(|| anyhow!("wait 的 target 用 name/role/automation_id/class 或 text"))? } else { Query::default() };
        loop {
            let found = match &by_text {
                Some(text) => ocr_locate(app, w, text, Some(1)).await.is_ok(),
                None => {
                    let (h, q) = (w.hwnd as isize, q.clone());
                    uia::run(move |ctx| ctx.exists(h, &q)).await.unwrap_or(0) > 0
                }
            };
            if found != gone {
                return Ok(json!({"ok": true, "action": "wait", "waited_ms": started.elapsed().as_millis() as u64}));
            }
            if Instant::now() >= deadline {
                bail!("等待 {} ms 后{}仍{}", timeout, by_text.as_deref().map(|t| format!("文字「{t}」")).unwrap_or_else(|| q.describe()), if gone { "存在" } else { "未出现" });
            }
            tokio::time::sleep(Duration::from_millis(if by_text.is_some() { 800 } else { 400 })).await;
        }
    }

    // ---------- UIA 线程里的执行 ----------

    fn bring_front(h: isize, pid: u32) -> Result<()> {
        if input::window_pid(input::foreground()) != pid {
            input::activate(h)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn perform(ctx: &mut uia::Ctx, h: isize, pid: u32, rect: [i32; 4], action: &str, target: Target, to: Option<Target>, p: &Value) -> Result<Value> {
        let located = match &target {
            Target::Ref(r) => Some(ctx.by_ref(h, *r)?),
            Target::Query(q) => Some(ctx.find(h, q)?),
            _ => None,
        };
        let screen = match &target {
            Target::Screen(x, y, info) => Some((*x, *y, info.clone())),
            _ => None,
        };
        let mut out = json!({"ok": true, "action": action});
        if let Some((_, info)) = &located {
            out["target"] = json!(tree::describe(info));
        }
        if let Some((_, _, info)) = &screen {
            out["located"] = info.clone();
        }
        let el = located.as_ref().map(|(e, _)| e);
        let info = located.as_ref().map(|(_, i)| i);
        let force_mouse = str_arg(p, "method") == Some("mouse");
        let point = |what: &Option<(i32, i32, Value)>| -> Result<(i32, i32)> {
            if let Some((x, y, _)) = what {
                return Ok((*x, *y));
            }
            match el {
                Some(e) => uia::click_point(e),
                None => Ok((rect[0] + rect[2] / 2, rect[1] + rect[3] / 2)),
            }
        };
        let needs_target = || -> Result<()> {
            if located.is_none() && screen.is_none() {
                bail!("{action} 需要 target");
            }
            Ok(())
        };
        let mut method = "mouse";
        match action {
            "click" => {
                needs_target()?;
                let mut done = false;
                if let (Some(e), Some(i), false) = (el, info, force_mouse) {
                    if !i.enabled {
                        bail!("{} 当前不可用（灰色）", tree::describe(i));
                    }
                    if let Some(m) = uia::invoke(e)? {
                        method = m;
                        done = true;
                    } else if i.patterns & tree::P_SELECT != 0 && matches!(i.control, tree::LIST_ITEM | tree::TREE_ITEM | tree::DATA_ITEM | tree::RADIO | 50019) {
                        done = uia::select_item(e)?;
                        method = "select";
                    } else if i.patterns & tree::P_TOGGLE != 0 {
                        done = uia::toggle(e)?;
                        method = "toggle";
                    } else if i.patterns & tree::P_EXPAND != 0 && matches!(i.control, tree::MENU_ITEM | tree::COMBOBOX | tree::TREE_ITEM | 50031) {
                        let open = uia::expand_state(e) != Some(1);
                        uia::expand(e, open)?;
                        method = if open { "expand" } else { "collapse" };
                        done = true;
                    }
                }
                if !done {
                    let (x, y) = point(&screen)?;
                    bring_front(h, pid)?;
                    input::click(x, y, input::Button::Left, 1)?;
                    method = "mouse";
                }
            }
            "double_click" | "right_click" | "middle_click" | "hover" => {
                needs_target()?;
                let (x, y) = point(&screen)?;
                bring_front(h, pid)?;
                match action {
                    "double_click" => input::click(x, y, input::Button::Left, 2)?,
                    "right_click" => input::click(x, y, input::Button::Right, 1)?,
                    "middle_click" => input::click(x, y, input::Button::Middle, 1)?,
                    _ => input::move_to(x, y)?,
                }
            }
            "drag" => {
                needs_target()?;
                let from = point(&screen)?;
                let dest = match to.ok_or_else(|| anyhow!("drag 需要 to（目标位置，写法同 target）"))? {
                    Target::Screen(x, y, _) => (x, y),
                    Target::Ref(r) => uia::click_point(&ctx.by_ref(h, r)?.0)?,
                    Target::Query(q) => uia::click_point(&ctx.find(h, &q)?.0)?,
                    Target::None => bail!("drag 需要 to"),
                };
                bring_front(h, pid)?;
                input::drag(from, dest)?;
            }
            "type" => {
                let text = p.get("text").and_then(Value::as_str).ok_or_else(|| anyhow!("type 需要 text"))?;
                let editable_box = info.is_some_and(|i| matches!(i.control, tree::EDIT | tree::COMBOBOX));
                let clear = p.get("clear").and_then(Value::as_bool).unwrap_or(editable_box);
                let use_keys = str_arg(p, "method") == Some("keys") || force_mouse;
                let mut done = false;
                // 不清空时只对普通输入框整值写入；文档/富文本改用键盘在光标处输入，避免整篇覆盖丢格式
                if let (Some(e), false, true) = (el, use_keys, clear || editable_box) {
                    let expected = if clear { text.to_string() } else { format!("{}{}", uia::value(e).unwrap_or_default(), text) };
                    if uia::set_value(e, &expected)? {
                        let password = info.is_some_and(|i| i.password);
                        let got = uia::value(e);
                        if password || got.as_deref().map(same_text_form) == Some(same_text_form(&expected)) {
                            method = "value";
                            done = true;
                        } else if !clear {
                            // 已整值写入但读回不同：不再用键盘重复追加，交给模型核对
                            method = "value";
                            done = true;
                            out["warning"] = json!("写入后读回的内容与预期不一致，请核对 value");
                        }
                    }
                }
                if !done {
                    bring_front(h, pid)?;
                    if let Some(e) = el {
                        if !uia::focus(e) {
                            let (x, y) = uia::click_point(e)?;
                            input::click(x, y, input::Button::Left, 1)?;
                        }
                    } else if let Some((x, y, _)) = &screen {
                        input::click(*x, *y, input::Button::Left, 1)?;
                    }
                    if clear {
                        input::press("Ctrl+A")?;
                        input::press("Delete")?;
                    }
                    input::type_text(text)?;
                    method = "keys";
                }
                if let Some(e) = el {
                    if !info.is_some_and(|i| i.password) {
                        if let Some(v) = uia::value(e) {
                            out["value"] = json!(v.chars().take(500).collect::<String>());
                        }
                    }
                }
            }
            "press" => {
                let key = str_arg(p, "key").ok_or_else(|| anyhow!("press 需要 key，如 Ctrl+S、Enter、Alt+F4、「Alt F」（依次按）"))?;
                bring_front(h, pid)?;
                if let Some(e) = el {
                    uia::focus(e);
                } else if let Some((x, y, _)) = &screen {
                    input::click(*x, *y, input::Button::Left, 1)?;
                }
                input::press(key)?;
                method = "keys";
            }
            "select" => {
                needs_target()?;
                let e = el.ok_or_else(|| anyhow!("select 需要按 ref 或 name/role 定位的控件"))?;
                match str_arg(p, "option") {
                    None => {
                        if !uia::select_item(e)? {
                            bail!("这个控件不支持选择；下拉框请给 option");
                        }
                        method = "select";
                    }
                    Some(option) => {
                        let st = uia::expand_state(e);
                        if st.is_some() && st != Some(1) {
                            uia::expand(e, true)?;
                        }
                        match uia::find_option(ctx, h, e, option) {
                            Ok(item) => {
                                if uia::select_item(&item)? {
                                    method = "select";
                                } else if uia::invoke(&item)?.is_some() {
                                    method = "invoke";
                                } else {
                                    let (x, y) = uia::click_point(&item)?;
                                    bring_front(h, pid)?;
                                    input::click(x, y, input::Button::Left, 1)?;
                                }
                                if uia::expand_state(e) == Some(1) {
                                    let _ = uia::expand(e, false);
                                }
                            }
                            Err(err) => {
                                // 可编辑下拉框：直接写值
                                if uia::set_value(e, option)? {
                                    method = "value";
                                } else {
                                    let _ = uia::expand(e, false);
                                    return Err(err);
                                }
                            }
                        }
                        if let Some(v) = uia::value(e) {
                            out["value"] = json!(v);
                        }
                    }
                }
            }
            "set_checked" => {
                needs_target()?;
                let want = p.get("checked").and_then(Value::as_bool).ok_or_else(|| anyhow!("set_checked 需要 checked: true/false"))?;
                let e = el.ok_or_else(|| anyhow!("set_checked 需要按 ref 或 name/role 定位的控件"))?;
                if uia::toggle_state(e).is_some() {
                    for _ in 0..3 {
                        if uia::toggle_state(e) == Some(want as i32) {
                            break;
                        }
                        uia::toggle(e)?;
                    }
                    method = "toggle";
                    if uia::toggle_state(e) != Some(want as i32) {
                        bail!("切换后状态仍不对");
                    }
                } else if uia::is_selected(e).is_some() {
                    if want && uia::is_selected(e) != Some(true) {
                        uia::select_item(e)?;
                    } else if !want && uia::is_selected(e) == Some(true) {
                        bail!("单选框不能直接取消，请选择同组的另一项");
                    }
                    method = "select";
                } else {
                    bail!("这个控件不是复选框/单选框");
                }
                out["checked"] = json!(want);
            }
            "expand" | "collapse" => {
                let e = el.ok_or_else(|| anyhow!("{action} 需要按 ref 或 name/role 定位的控件"))?;
                match uia::expand(e, action == "expand")? {
                    Some(s) => out["state"] = json!(match s { 0 => "已折叠", 1 => "已展开", 2 => "部分展开", _ => "无子项" }),
                    None => bail!("这个控件不支持展开/折叠"),
                }
                method = "expand_collapse";
            }
            "scroll" => {
                let dir = str_arg(p, "direction").unwrap_or("down");
                let amount = p.get("amount").and_then(Value::as_u64).unwrap_or(3).clamp(1, 50) as u32;
                let page = p.get("page").and_then(Value::as_bool).unwrap_or(false);
                if let (Some(e), false) = (el, force_mouse) {
                    if uia::scroll(e, dir, amount, page)? {
                        method = "scroll_pattern";
                        return finish(out, method);
                    }
                }
                let (x, y) = point(&screen)?;
                let n = amount as i32 * if page { 5 } else { 1 };
                let (v, hz) = match dir {
                    "up" => (n, 0),
                    "down" => (-n, 0),
                    "left" => (0, -n),
                    "right" => (0, n),
                    _ => bail!("direction 只能是 up/down/left/right"),
                };
                bring_front(h, pid)?;
                input::wheel(x, y, v, hz)?;
            }
            "focus" => {
                let e = el.ok_or_else(|| anyhow!("focus 需要按 ref 或 name/role 定位的控件"))?;
                if !uia::focus(e) {
                    bail!("控件拒绝获得焦点");
                }
                method = "focus";
            }
            "read" => {
                let max = p.get("max_chars").and_then(Value::as_u64).unwrap_or(8000).clamp(200, 100000) as usize;
                let (text, how) = match el {
                    Some(e) => uia::read_text(ctx, e, max)?,
                    None => {
                        let root = ctx.build(h)?;
                        let (el0, _) = (root.els[root.roots[0].0].clone(), ());
                        uia::read_text(ctx, &el0, max)?
                    }
                };
                let total = text.chars().count();
                out["text"] = json!(text.chars().take(max).collect::<String>());
                if total > max {
                    out["truncated"] = json!(true);
                }
                method = how;
            }
            "locate" => {
                needs_target()?;
                if let Some(e) = el {
                    out["rect"] = json!(uia::current_rect(e));
                    if let Some([x, y, ww, hh]) = uia::current_rect(e) {
                        out["rect_in_window"] = json!([x - rect[0], y - rect[1], ww, hh]);
                    }
                }
                method = "none";
            }
            other => bail!("未知 action {other}（click/double_click/right_click/hover/drag/type/press/select/set_checked/expand/collapse/scroll/focus/read/locate/wait）"),
        }
        finish(out, method)
    }

    /// 比较时忽略换行符差异（CRLF / CR / LF）
    fn same_text_form(s: &str) -> String {
        s.replace("\r\n", "\n").replace('\r', "\n")
    }

    fn finish(mut out: Value, method: &str) -> Result<Value> {
        out["method"] = json!(method);
        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn app_terms_cover_aliases_and_skip_non_apps() {
            let t = app_terms("WeChat").unwrap();
            assert!(t.contains(&"weixin".into()) && t.contains(&"微信".into()));
            assert_eq!(app_terms(r"C:\Program Files\Tencent\Weixin\Weixin.exe").unwrap()[0], "weixin");
            assert!(app_terms("微信").unwrap().contains(&"wechat".into()));
            assert!(app_terms("ms-settings:display").is_none());
            assert!(app_terms("https://example.com").is_none());
            assert!(app_terms(r"D:\docs\a.docx").is_none());
            assert!(app_terms("explorer").is_none());
            assert_eq!(app_terms("mytool.exe").unwrap(), vec!["mytool".to_string()]);
        }

        /// 只读：打印本机后台软件、微信的运行窗口和开始菜单快捷方式
        /// cargo test --lib desktop::imp::tests::inspect_running -- --ignored --nocapture
        #[test]
        #[ignore]
        fn inspect_running() {
            let all = screenshot::list_windows(false);
            println!("background: {}", serde_json::to_string_pretty(&background_apps(&all)).unwrap());
            let terms = app_terms("WeChat").unwrap();
            let (v, h) = running_windows(&terms);
            println!("visible: {v:?}\nhidden: {h:?}\nmain hidden: {:?}", main_hidden(&h));
            println!("start menu: {:?}", start_menu_find(&terms));
        }
    }
}

/// 真实桌面联调：cargo test --lib desktop::e2e -- --ignored --nocapture
#[cfg(all(test, windows))]
mod e2e {
    use super::tree::{self, Query, RenderOpts};
    use super::{input, perform, uia, Target};
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn notepad_roundtrip() {
        let before: Vec<i64> = crate::recorder::screenshot::list_windows(false).iter().map(|w| w.hwnd).collect();
        input::shell_open("notepad.exe", None, None).unwrap();
        let mut win = None;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            win = crate::recorder::screenshot::list_windows(false).into_iter().find(|w| !before.contains(&w.hwnd) && w.process.eq_ignore_ascii_case("notepad.exe"));
            if win.is_some() {
                break;
            }
        }
        let w = win.expect("记事本窗口没出现");
        // 无论测试成败都关掉自己打开的窗口
        struct CloseOnDrop(isize);
        impl Drop for CloseOnDrop {
            fn drop(&mut self) {
                let _ = input::close(self.0);
            }
        }
        let _closer = CloseOnDrop(w.hwnd as isize);
        tokio::time::sleep(Duration::from_millis(800)).await;
        let h = w.hwnd as isize;
        let started = std::time::Instant::now();
        let snap = uia::run(move |ctx| ctx.snapshot(h, &RenderOpts { max_chars: 6000, query: None, include_offscreen: false, max_depth: None })).await.unwrap();
        println!("快照 {} ms，{} 个编号，{} 个可操作：\n{}", started.elapsed().as_millis(), snap.refs.len(), snap.interactive, snap.text);
        assert!(snap.interactive > 3);

        let typed = uia::run(move |ctx| {
            let q = Query { roles: Some(vec![tree::DOCUMENT, tree::EDIT]), ..Default::default() };
            let (el, info) = ctx.find(h, &Query { index: Some(1), ..q })?;
            println!("编辑区：{}", tree::describe(&info));
            let ok = uia::set_value(&el, "拾光桌面操控测试\nline2")?;
            let (text, how) = uia::read_text(ctx, &el, 2000)?;
            let _ = uia::set_value(&el, "");
            Ok((ok, text, how))
        })
        .await
        .unwrap();
        println!("set_value={} 读回({})：{:?}", typed.0, typed.2, typed.1);
        assert!(typed.1.contains("拾光桌面操控测试"));

        let menu = uia::run(move |ctx| {
            let (el, info) = ctx.find(h, &Query { name: Some("文件".into()), ..Default::default() }).or_else(|_| ctx.find(h, &Query { name: Some("File".into()), ..Default::default() }))?;
            Ok(tree::describe(&info) + &format!(" @{:?}", uia::current_rect(&el)))
        })
        .await;
        println!("菜单：{menu:?}");

        // 点「文件」：走展开模式，弹出的菜单应作为独立区块出现在快照里
        let (rect, pid) = (w.rect, w.pid);
        let clicked = uia::run(move |ctx| {
            let q = Query { automation_id: Some("File".into()), ..Default::default() };
            let r = perform(ctx, h, pid, rect, "click", Target::Query(q), None, &serde_json::json!({}))?;
            std::thread::sleep(Duration::from_millis(500));
            let snap = ctx.snapshot(h, &RenderOpts { max_chars: 3000, query: Some("新建".into()), include_offscreen: false, max_depth: None })?;
            Ok((r, snap.text))
        })
        .await
        .unwrap();
        println!("点击结果：{}\n查询「新建」：\n{}", clicked.0, clicked.1);
        assert!(clicked.1.contains("新建"));
        input::press("Escape").unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}
