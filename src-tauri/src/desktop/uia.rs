//! Windows UI Automation：读取窗口的无障碍树、按编号/名称找控件、用控件模式（Invoke/Value/Toggle…）直接操作。
//! 所有 COM 调用都在一条专用 MTA 线程里串行执行；上一次快照的元素引用也留在这条线程里。

use super::tree::{self, Info, Query, P_EXPAND, P_INVOKE, P_SCROLL, P_SELECT, P_TEXT, P_TOGGLE, P_VALUE};
use anyhow::{anyhow, bail, Context, Result};
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use ::windows::core::BSTR;
use ::windows::Win32::Foundation::{HWND, POINT};
use ::windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use ::windows::Win32::UI::Accessibility::*;

type Job = Box<dyn FnOnce(&mut Ctx) + Send>;

static WORKER: Mutex<Option<Sender<Job>>> = Mutex::new(None);

/// 一次构建出的控件树：nodes 与 els 一一对应
pub struct Built {
    pub nodes: Vec<Info>,
    pub els: Vec<IUIAutomationElement>,
    /// (根节点下标, 区块标题)
    pub roots: Vec<(usize, String)>,
    pub truncated: bool,
}

pub struct Ctx {
    uia: IUIAutomation,
    subtree: IUIAutomationCacheRequest,
    children: IUIAutomationCacheRequest,
    top: IUIAutomationCacheRequest,
    /// 上次快照的编号 → 元素（编号从 1 开始，下标 = 编号 - 1）
    refs: Vec<IUIAutomationElement>,
    ref_infos: Vec<Info>,
    pub ref_hwnd: isize,
}

const MAX_NODES: usize = 6000;

fn spawn() -> Result<Sender<Job>> {
    let (tx, rx) = channel::<Job>();
    let (ready_tx, ready_rx) = channel::<std::result::Result<(), String>>();
    std::thread::Builder::new()
        .name("shiguang-uia".into())
        .spawn(move || {
            unsafe {
                use windows_sys::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
                SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            }
            let mut ctx = match Ctx::new() {
                Ok(c) => {
                    let _ = ready_tx.send(Ok(()));
                    c
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("{e:#}")));
                    return;
                }
            };
            for job in rx {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&mut ctx))).is_err() {
                    log::warn!("UI Automation 任务异常");
                }
            }
        })?;
    ready_rx.recv().map_err(|_| anyhow!("UI Automation 线程启动失败"))?.map_err(|e| anyhow!("UI Automation 不可用：{e}"))?;
    Ok(tx)
}

/// 在 UIA 线程里执行一段操作
pub async fn run<T: Send + 'static>(f: impl FnOnce(&mut Ctx) -> Result<T> + Send + 'static) -> Result<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut job: Option<Job> = Some(Box::new(move |ctx| {
        let _ = tx.send(f(ctx));
    }));
    {
        let mut guard = WORKER.lock().map_err(|_| anyhow!("UI Automation 锁异常"))?;
        // 线程意外退出时重建一次
        for _ in 0..2 {
            if guard.is_none() {
                *guard = Some(spawn()?);
            }
            match guard.as_ref().unwrap().send(job.take().unwrap()) {
                Ok(()) => break,
                Err(e) => {
                    job = Some(e.0);
                    *guard = None;
                }
            }
        }
    }
    rx.await.map_err(|_| anyhow!("UI Automation 操作异常中断"))?
}

fn hwnd(h: isize) -> HWND {
    HWND(h as *mut core::ffi::c_void)
}

fn rect_of(r: ::windows::Win32::Foundation::RECT) -> Option<[i32; 4]> {
    let v = [r.left, r.top, r.right - r.left, r.bottom - r.top];
    (v[2] > 0 && v[3] > 0).then_some(v)
}

unsafe fn info_of(el: &IUIAutomationElement) -> Info {
    let mut i = Info {
        control: el.CachedControlType().map(|c| c.0).unwrap_or(0),
        name: el.CachedName().map(|b| b.to_string()).unwrap_or_default(),
        automation_id: el.CachedAutomationId().map(|b| b.to_string()).unwrap_or_default(),
        class: el.CachedClassName().map(|b| b.to_string()).unwrap_or_default(),
        rect: el.CachedBoundingRectangle().ok().and_then(rect_of),
        enabled: el.CachedIsEnabled().map(|b| b.as_bool()).unwrap_or(true),
        offscreen: el.CachedIsOffscreen().map(|b| b.as_bool()).unwrap_or(false),
        focused: el.CachedHasKeyboardFocus().map(|b| b.as_bool()).unwrap_or(false),
        password: el.CachedIsPassword().map(|b| b.as_bool()).unwrap_or(false),
        ..Default::default()
    };
    if el.GetCachedPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId).is_ok() {
        i.patterns |= P_INVOKE;
    }
    if let Ok(p) = el.GetCachedPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
        i.patterns |= P_VALUE;
        i.value = p.CachedValue().ok().map(|b| b.to_string());
        i.readonly = p.CachedIsReadOnly().map(|b| b.as_bool()).unwrap_or(false);
    }
    if let Ok(p) = el.GetCachedPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId) {
        i.patterns |= P_TOGGLE;
        i.toggle = p.CachedToggleState().ok().map(|s| s.0);
    }
    if let Ok(p) = el.GetCachedPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId) {
        i.patterns |= P_EXPAND;
        i.expand = p.CachedExpandCollapseState().ok().map(|s| s.0);
    }
    if let Ok(p) = el.GetCachedPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId) {
        i.patterns |= P_SELECT;
        i.selected = p.CachedIsSelected().ok().map(|b| b.as_bool());
    }
    if el.GetCachedPatternAs::<IUIAutomationScrollPattern>(UIA_ScrollPatternId).is_ok() {
        i.patterns |= P_SCROLL;
    }
    if el.GetCachedPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId).is_ok() {
        i.patterns |= P_TEXT;
    }
    i
}

unsafe fn children_of(el: &IUIAutomationElement) -> Vec<IUIAutomationElement> {
    let Ok(arr) = el.GetCachedChildren() else { return Vec::new() };
    let n = arr.Length().unwrap_or(0);
    (0..n).filter_map(|i| arr.GetElement(i).ok()).collect()
}

fn invoke_with_timeout(p: IUIAutomationInvokePattern) -> Result<bool> {
    struct SendBox(IUIAutomationInvokePattern);
    // MTA 里的 COM 代理可以跨 MTA 线程使用
    unsafe impl Send for SendBox {}
    let boxed = SendBox(p);
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let boxed = boxed;
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = tx.send(boxed.0.Invoke().map_err(|e| e.message()));
        }
    });
    match rx.recv_timeout(Duration::from_millis(2500)) {
        Ok(Ok(())) => Ok(true),
        Ok(Err(e)) => bail!("触发控件失败：{e}"),
        // 有的程序在 Invoke 里弹模态对话框，调用要等对话框关掉才返回：当作已触发
        Err(_) => Ok(false),
    }
}

impl Ctx {
    fn new() -> Result<Ctx> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
                .or_else(|_| CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER))
                .context("创建 UI Automation 失败")?;
            if let Ok(u2) = ::windows::core::Interface::cast::<IUIAutomation2>(&uia) {
                let _ = u2.SetConnectionTimeout(4000);
                let _ = u2.SetTransactionTimeout(8000);
            }
            let make = |scope: TreeScope| -> Result<IUIAutomationCacheRequest> {
                let req = uia.CreateCacheRequest()?;
                for p in [
                    UIA_NamePropertyId,
                    UIA_ControlTypePropertyId,
                    UIA_BoundingRectanglePropertyId,
                    UIA_IsEnabledPropertyId,
                    UIA_IsOffscreenPropertyId,
                    UIA_AutomationIdPropertyId,
                    UIA_ClassNamePropertyId,
                    UIA_HasKeyboardFocusPropertyId,
                    UIA_IsPasswordPropertyId,
                    UIA_ProcessIdPropertyId,
                    UIA_NativeWindowHandlePropertyId,
                    UIA_ValueValuePropertyId,
                    UIA_ValueIsReadOnlyPropertyId,
                    UIA_ToggleToggleStatePropertyId,
                    UIA_ExpandCollapseExpandCollapseStatePropertyId,
                    UIA_SelectionItemIsSelectedPropertyId,
                ] {
                    req.AddProperty(p)?;
                }
                for p in [
                    UIA_InvokePatternId,
                    UIA_ValuePatternId,
                    UIA_TogglePatternId,
                    UIA_ExpandCollapsePatternId,
                    UIA_SelectionItemPatternId,
                    UIA_ScrollPatternId,
                    UIA_TextPatternId,
                ] {
                    req.AddPattern(p)?;
                }
                req.SetTreeScope(scope)?;
                req.SetAutomationElementMode(AutomationElementMode_Full)?;
                Ok(req)
            };
            let subtree = make(TreeScope_Subtree)?;
            let children = make(TreeScope(TreeScope_Element.0 | TreeScope_Children.0))?;
            let top = make(TreeScope(TreeScope_Element.0 | TreeScope_Children.0))?;
            Ok(Ctx { uia, subtree, children, top, refs: Vec::new(), ref_infos: Vec::new(), ref_hwnd: 0 })
        }
    }

    /// 先整棵子树一次取回（一次跨进程往返）；超时或失败再逐层取并限量
    unsafe fn build_from(&self, root: &IUIAutomationElement, out: &mut Built, label: String) {
        let start = out.nodes.len();
        match root.BuildUpdatedCache(&self.subtree) {
            Ok(cached) => {
                let mut stack = vec![(cached, None::<usize>)];
                while let Some((el, parent)) = stack.pop() {
                    if out.nodes.len() >= MAX_NODES {
                        out.truncated = true;
                        break;
                    }
                    let idx = out.nodes.len();
                    out.nodes.push(info_of(&el));
                    if let Some(p) = parent {
                        out.nodes[p].children.push(idx);
                    }
                    let kids = if tree::skip_subtree(out.nodes[idx].control) { Vec::new() } else { children_of(&el) };
                    out.els.push(el);
                    for k in kids.into_iter().rev() {
                        stack.push((k, Some(idx)));
                    }
                }
            }
            Err(e) => {
                log::info!("整树缓存失败，改为逐层读取：{e}");
                let deadline = Instant::now() + Duration::from_secs(8);
                let mut queue = std::collections::VecDeque::new();
                queue.push_back((root.clone(), None::<usize>));
                while let Some((el, parent)) = queue.pop_front() {
                    if out.nodes.len() - start >= 1500 || Instant::now() > deadline {
                        out.truncated = true;
                        break;
                    }
                    let Ok(cached) = el.BuildUpdatedCache(&self.children) else { continue };
                    let idx = out.nodes.len();
                    out.nodes.push(info_of(&cached));
                    if let Some(p) = parent {
                        out.nodes[p].children.push(idx);
                    }
                    if !tree::skip_subtree(out.nodes[idx].control) {
                        for k in children_of(&cached) {
                            queue.push_back((k, Some(idx)));
                        }
                    }
                    out.els.push(cached);
                }
            }
        }
        if out.nodes.len() > start {
            out.roots.push((start, label));
        }
    }

    /// 目标窗口 + 同进程弹出的菜单 / 属于它的对话框（菜单、下拉列表是独立的顶层窗口，不在主窗口子树里）
    pub fn build(&self, h: isize) -> Result<Built> {
        unsafe {
            let main = self.uia.ElementFromHandle(hwnd(h)).context("读取窗口的无障碍信息失败（窗口可能已关闭）")?;
            let pid = main.CurrentProcessId().unwrap_or(0);
            let mut out = Built { nodes: Vec::new(), els: Vec::new(), roots: Vec::new(), truncated: false };
            if let Ok(desktop) = self.uia.GetRootElementBuildCache(&self.top) {
                for top in children_of(&desktop) {
                    if top.CachedProcessId().unwrap_or(-1) != pid {
                        continue;
                    }
                    let th = top.CachedNativeWindowHandle().map(|w| w.0 as isize).unwrap_or(0);
                    if th == h || th == 0 {
                        continue;
                    }
                    let control = top.CachedControlType().map(|c| c.0).unwrap_or(0);
                    let owner = {
                        use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindow, GW_OWNER};
                        GetWindow(th as _, GW_OWNER) as isize
                    };
                    let popup = control == tree::MENU || (control == tree::LIST && top.CachedClassName().map(|c| c.to_string()).unwrap_or_default() == "ComboLBox");
                    if !(popup || owner == h) {
                        continue;
                    }
                    if top.CachedIsOffscreen().map(|b| b.as_bool()).unwrap_or(false) {
                        continue;
                    }
                    let name = top.CachedName().map(|b| b.to_string()).unwrap_or_default();
                    let label = if popup { "弹出菜单/列表".to_string() } else { format!("对话框「{name}」") };
                    self.build_from(&top, &mut out, label);
                }
            }
            let title = main.CurrentName().map(|b| b.to_string()).unwrap_or_default();
            self.build_from(&main, &mut out, format!("窗口「{title}」"));
            if out.nodes.is_empty() {
                bail!("没有读到任何控件");
            }
            // 主窗口放最前，弹出层随后
            let main_root = out.roots.pop().unwrap();
            out.roots.insert(0, main_root);
            Ok(out)
        }
    }

    pub fn snapshot(&mut self, h: isize, opts: &tree::RenderOpts) -> Result<tree::Rendered> {
        let built = self.build(h)?;
        let mut rendered = tree::render(&built.nodes, &built.roots, opts);
        rendered.truncated_tree = built.truncated;
        self.refs = rendered.refs.iter().map(|&i| built.els[i].clone()).collect();
        self.ref_infos = rendered.refs.iter().map(|&i| built.nodes[i].clone()).collect();
        self.ref_hwnd = h;
        Ok(rendered)
    }

    pub fn by_ref(&self, h: isize, r: usize) -> Result<(IUIAutomationElement, Info)> {
        if self.refs.is_empty() || self.ref_hwnd != h {
            bail!("编号 {r} 不属于这个窗口：先对它调用 desktop_snapshot，或改用 name/role 定位");
        }
        let el = self.refs.get(r.wrapping_sub(1)).ok_or_else(|| anyhow!("编号 {r} 超出上次快照范围（共 {} 个）", self.refs.len()))?;
        let info = self.ref_infos[r - 1].clone();
        unsafe {
            if el.CurrentControlType().is_err() {
                bail!("编号 {r} 的控件已经不存在（界面变了），请重新 desktop_snapshot");
            }
        }
        Ok((el.clone(), info))
    }

    /// 按名称/类型等找唯一控件；多个时用 index（从 1 开始）选
    pub fn find(&self, h: isize, q: &Query) -> Result<(IUIAutomationElement, Info)> {
        let built = self.build(h)?;
        let hits = tree::find(&built.nodes, &built.roots, q);
        match (hits.len(), q.index) {
            (0, _) => bail!("没有找到{}。{}", q.describe(), tree::suggest(&built.nodes, &built.roots, q)),
            (_, Some(i)) if i >= 1 && i <= hits.len() => Ok((built.els[hits[i - 1]].clone(), built.nodes[hits[i - 1]].clone())),
            (_, Some(i)) => bail!("index {i} 超出范围：共 {} 个匹配", hits.len()),
            (1, None) => Ok((built.els[hits[0]].clone(), built.nodes[hits[0]].clone())),
            (n, None) => {
                let list: Vec<String> = hits.iter().take(8).enumerate().map(|(k, &i)| format!("{}. {}", k + 1, tree::describe(&built.nodes[i]))).collect();
                bail!("{}有 {n} 个匹配，请加 role/automation_id 区分，或用 index 指定第几个：{}", q.describe(), list.join("；"))
            }
        }
    }

    /// 只判断是否存在（wait 用）
    pub fn exists(&self, h: isize, q: &Query) -> Result<usize> {
        let built = self.build(h)?;
        Ok(tree::find(&built.nodes, &built.roots, q).len())
    }
}

// ---------- 对单个元素的操作（都在 UIA 线程里调用） ----------

pub fn current_rect(el: &IUIAutomationElement) -> Option<[i32; 4]> {
    unsafe { el.CurrentBoundingRectangle().ok().and_then(rect_of) }
}

/// 鼠标要点的位置：优先控件自报的可点击点，否则取边框中心；在屏幕外时先让它滚动到可见
pub fn click_point(el: &IUIAutomationElement) -> Result<(i32, i32)> {
    unsafe {
        if el.CurrentIsOffscreen().map(|b| b.as_bool()).unwrap_or(false) {
            if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationScrollItemPattern>(UIA_ScrollItemPatternId) {
                let _ = p.ScrollIntoView();
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        let mut pt = POINT::default();
        if el.GetClickablePoint(&mut pt).map(|b| b.as_bool()).unwrap_or(false) {
            return Ok((pt.x, pt.y));
        }
    }
    let [x, y, w, h] = current_rect(el).ok_or_else(|| anyhow!("控件没有可见位置（可能被折叠或在屏幕外）"))?;
    Ok((x + w / 2, y + h / 2))
}

pub fn invoke(el: &IUIAutomationElement) -> Result<Option<&'static str>> {
    unsafe {
        if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) {
            return Ok(Some(if invoke_with_timeout(p)? { "invoke" } else { "invoke（调用未返回，可能打开了模态对话框）" }));
        }
    }
    Ok(None)
}

pub fn select_item(el: &IUIAutomationElement) -> Result<bool> {
    unsafe {
        if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId) {
            p.Select().context("选择失败")?;
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn toggle_state(el: &IUIAutomationElement) -> Option<i32> {
    unsafe {
        el.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
            .ok()
            .and_then(|p| p.CurrentToggleState().ok())
            .map(|s| s.0)
    }
}

pub fn toggle(el: &IUIAutomationElement) -> Result<bool> {
    unsafe {
        if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId) {
            p.Toggle().context("切换失败")?;
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn is_selected(el: &IUIAutomationElement) -> Option<bool> {
    unsafe {
        el.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
            .ok()
            .and_then(|p| p.CurrentIsSelected().ok())
            .map(|b| b.as_bool())
    }
}

/// expand=true 展开，false 折叠；返回操作后的状态（0 折叠 1 展开 2 部分 3 叶子）
pub fn expand(el: &IUIAutomationElement, expand: bool) -> Result<Option<i32>> {
    unsafe {
        let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId) else {
            return Ok(None);
        };
        if expand {
            p.Expand().context("展开失败")?;
        } else {
            p.Collapse().context("折叠失败")?;
        }
        std::thread::sleep(Duration::from_millis(250));
        Ok(p.CurrentExpandCollapseState().ok().map(|s| s.0))
    }
}

pub fn expand_state(el: &IUIAutomationElement) -> Option<i32> {
    unsafe {
        el.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId)
            .ok()
            .and_then(|p| p.CurrentExpandCollapseState().ok())
            .map(|s| s.0)
    }
}

pub fn value(el: &IUIAutomationElement) -> Option<String> {
    unsafe {
        el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            .ok()
            .and_then(|p| p.CurrentValue().ok())
            .map(|b| b.to_string())
    }
}

/// 用 Value 模式直接设值；控件不支持或只读时返回 false
pub fn set_value(el: &IUIAutomationElement, text: &str) -> Result<bool> {
    unsafe {
        let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) else { return Ok(false) };
        if p.CurrentIsReadOnly().map(|b| b.as_bool()).unwrap_or(false) {
            return Ok(false);
        }
        p.SetValue(&BSTR::from(text)).context("设置内容失败")?;
        Ok(true)
    }
}

pub fn focus(el: &IUIAutomationElement) -> bool {
    unsafe { el.SetFocus().is_ok() }
}

/// 用滚动模式滚动；不支持时返回 false。amount 为步数，page=true 按整页
pub fn scroll(el: &IUIAutomationElement, dir: &str, amount: u32, page: bool) -> Result<bool> {
    unsafe {
        let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationScrollPattern>(UIA_ScrollPatternId) else { return Ok(false) };
        let (inc, dec) = if page { (ScrollAmount_LargeIncrement, ScrollAmount_LargeDecrement) } else { (ScrollAmount_SmallIncrement, ScrollAmount_SmallDecrement) };
        let (h, v) = match dir {
            "up" => (ScrollAmount_NoAmount, dec),
            "down" => (ScrollAmount_NoAmount, inc),
            "left" => (dec, ScrollAmount_NoAmount),
            "right" => (inc, ScrollAmount_NoAmount),
            _ => bail!("滚动方向只能是 up/down/left/right"),
        };
        for _ in 0..amount.max(1) {
            if p.Scroll(h, v).is_err() {
                break;
            }
        }
        Ok(true)
    }
}

/// 读控件的文字：文本模式（文档/编辑器全文）→ 值 → 子树里所有名称与值
pub fn read_text(ctx: &Ctx, el: &IUIAutomationElement, max_chars: usize) -> Result<(String, &'static str)> {
    unsafe {
        if let Ok(p) = el.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) {
            if let Ok(range) = p.DocumentRange() {
                if let Ok(t) = range.GetText(max_chars as i32 + 1) {
                    let t = t.to_string();
                    if !t.trim().is_empty() {
                        return Ok((t, "text"));
                    }
                }
            }
        }
        if let Some(v) = value(el).filter(|v| !v.trim().is_empty()) {
            return Ok((v, "value"));
        }
        let cached = el.BuildUpdatedCache(&ctx.subtree).context("读取控件内容失败")?;
        let mut parts: Vec<String> = Vec::new();
        let mut stack = vec![cached];
        let mut total = 0usize;
        while let Some(e) = stack.pop() {
            let i = info_of(&e);
            for s in [Some(i.name.clone()), if i.password { None } else { i.value.clone() }].into_iter().flatten() {
                let s = s.trim().to_string();
                if !s.is_empty() && parts.last() != Some(&s) {
                    total += s.chars().count();
                    parts.push(s);
                }
            }
            if total > max_chars {
                break;
            }
            for k in children_of(&e).into_iter().rev() {
                stack.push(k);
            }
        }
        Ok((parts.join("\n"), "tree"))
    }
}

/// 在元素子树里找下拉选项（展开后的下拉列表有时是独立弹出窗口，再到整个窗口里找）
pub fn find_option(ctx: &Ctx, h: isize, scope: &IUIAutomationElement, option: &str) -> Result<IUIAutomationElement> {
    let q = Query { name: Some(option.to_string()), roles: Some(vec![tree::LIST_ITEM, tree::MENU_ITEM, tree::DATA_ITEM, tree::TREE_ITEM]), ..Default::default() };
    unsafe {
        if let Ok(cached) = scope.BuildUpdatedCache(&ctx.subtree) {
            let mut out = Built { nodes: Vec::new(), els: Vec::new(), roots: Vec::new(), truncated: false };
            let mut stack = vec![(cached, None::<usize>)];
            while let Some((el, parent)) = stack.pop() {
                let idx = out.nodes.len();
                out.nodes.push(info_of(&el));
                if let Some(p) = parent {
                    out.nodes[p].children.push(idx);
                }
                for k in children_of(&el).into_iter().rev() {
                    stack.push((k, Some(idx)));
                }
                out.els.push(el);
            }
            out.roots.push((0, String::new()));
            let hits = tree::find(&out.nodes, &out.roots, &Query { include_offscreen: true, ..q.clone() });
            if let Some(&i) = hits.first() {
                return Ok(out.els[i].clone());
            }
        }
    }
    ctx.find(h, &Query { include_offscreen: true, ..q }).map(|(el, _)| el).with_context(|| format!("展开后没有找到选项「{option}」"))
}
