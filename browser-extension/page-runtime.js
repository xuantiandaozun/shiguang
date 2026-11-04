// 拾光真实输入引擎的页面侧（与 runtime.js 配合）。
// 只做页面里才能判断的事：唯一定位、可操作性（可见/可用/位置稳定/未遮挡）、
// 焦点与读回校验、页面状态采集。鼠标键盘事件由扩展后台通过 chrome.debugger
// 的 Input.* 发出（isTrusted=true），任何框架都按真人操作处理。
(() => {
  const api = window.__dh;
  const RT_VER = 2;
  if (!api || !api.ops || (api.rt && api.rt.version === RT_VER)) return;
  const { unique, summary, remember, visible, enabled, label, checked, text, norm } = api.ops;
  const short = (value, n) => {
    const s = norm(value);
    return s.length > n ? s.slice(0, n) + "…" : s;
  };

  // DOM 变化计数，供后台判断页面是否安静。只记时间和次数，不保留节点。
  // 属性只看状态类，style/class 动画不算"还在变化"。
  if (!window.__dhMut) {
    const state = { count: 0, last: performance.now() };
    try {
      new MutationObserver((records) => {
        state.count += records.length;
        state.last = performance.now();
      }).observe(document.documentElement, {
        childList: true, subtree: true, characterData: true, attributes: true,
        attributeFilter: ["aria-expanded", "aria-selected", "aria-checked", "aria-hidden", "aria-busy",
          "aria-invalid", "disabled", "hidden", "open", "value", "href", "src"],
      });
    } catch { /* 文档尚未就绪：下次注入再安装 */ }
    window.__dhMut = state;
  }

  const kindOf = (el) => {
    if (!el) return "none";
    if (el.tagName === "INPUT") {
      if (el.type === "file") return "file";
      return ["checkbox", "radio", "button", "submit", "reset", "image", "range", "color"].includes(el.type) ? "other" : "input";
    }
    if (el.tagName === "TEXTAREA") return "textarea";
    if (el.tagName === "SELECT") return "select";
    if (el.isContentEditable) return "contenteditable";
    return "other";
  };
  const TEXT_KINDS = ["input", "textarea", "contenteditable"];
  const editingHost = (el) => {
    let host = el;
    while (host && host.parentElement && host.parentElement.isContentEditable) host = host.parentElement;
    return host;
  };
  // 包含关系穿透 Shadow DOM。
  const owns = (el, node) => {
    for (let n = node; n; n = n.parentNode || n.host) if (n === el) return true;
    return false;
  };
  const deepActive = () => {
    let a = document.activeElement;
    for (let i = 0; i < 8 && a; i++) {
      if (a.shadowRoot && a.shadowRoot.activeElement) { a = a.shadowRoot.activeElement; continue; }
      if (a.tagName === "IFRAME") {
        try {
          const inner = a.contentDocument && a.contentDocument.activeElement;
          if (inner) { a = inner; continue; }
        } catch { /* 跨域 iframe 无法读取 */ }
      }
      break;
    }
    return a;
  };
  const deepHit = (x, y) => {
    let hit = document.elementFromPoint(x, y);
    while (hit && hit.shadowRoot) {
      const inner = hit.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === hit) break;
      hit = inner;
    }
    return hit;
  };
  const hitOwned = (el, hit) => !!hit && (owns(el, hit) || (hit.closest && hit.closest("label") && hit.closest("label").control === el));

  // 后台标签页 requestAnimationFrame 会暂停，用定时器兜底，避免卡住。
  const frame = () => new Promise((resolve) => {
    let done = false;
    const finish = () => { if (!done) { done = true; resolve(); } };
    requestAnimationFrame(finish);
    setTimeout(finish, 60);
  });
  async function waitStable(el) {
    let prev = el.getBoundingClientRect();
    for (let i = 0; i < 8; i++) {
      await frame();
      const r = el.getBoundingClientRect();
      if (Math.abs(r.x - prev.x) < 1 && Math.abs(r.y - prev.y) < 1 && Math.abs(r.width - prev.width) < 1 && Math.abs(r.height - prev.height) < 1) return true;
      prev = r;
    }
    return false;
  }

  // 在元素可见部分里找一个确实落在它身上的点：中心被图标/角标挡住时换点，
  // 整体被浮层盖住时报告遮挡者，绝不把点击落到别的元素上。
  function clickablePoint(el) {
    const r = el.getBoundingClientRect();
    const left = Math.max(r.left, 0), right = Math.min(r.right, window.innerWidth);
    const top = Math.max(r.top, 0), bottom = Math.min(r.bottom, window.innerHeight);
    if (right - left < 1 || bottom - top < 1) return { ok: false, offscreen: true };
    const fractions = [[0.5, 0.5], [0.3, 0.5], [0.7, 0.5], [0.5, 0.3], [0.5, 0.7], [0.15, 0.5], [0.85, 0.5]];
    let blocker = null;
    for (const [fx, fy] of fractions) {
      const x = left + (right - left) * fx;
      const y = top + (bottom - top) * fy;
      const hit = deepHit(x, y);
      if (hitOwned(el, hit)) return { ok: true, x: Math.round(x * 100) / 100, y: Math.round(y * 100) / 100 };
      blocker ||= hit;
    }
    return { ok: false, by: blocker ? api.describe(blocker) : "未知元素" };
  }

  function resolveTarget(target) {
    if (!target || typeof target !== "object" || Array.isArray(target)) return { error: "target 必须是定位对象", code: "invalid_request" };
    const onlyRef = target.ref != null && Object.keys(target).every((k) => k === "ref");
    if (onlyRef) {
      const before = window.__dhRelocated || 0;
      const el = api.ref(target.ref);
      if (!el) return { error: `元素 [${target.ref}] 已不在页面上（页面已变化且无法唯一重定位），请重新 find/snapshot`, code: "stale" };
      return { el, relocated: (window.__dhRelocated || 0) > before };
    }
    try {
      const found = unique(target);
      return found.el ? { el: found.el } : found;
    } catch (e) {
      return { error: String(e.message || e), code: "invalid_request" };
    }
  }

  const editableWithin = (el) => {
    if (TEXT_KINDS.includes(kindOf(el)) || kindOf(el) === "select") return el;
    const nested = Array.from(el.querySelectorAll('input:not([type="hidden"]):not([type="file"]), textarea, [contenteditable]:not([contenteditable="false"])'))
      .find((n) => visible(n) && TEXT_KINDS.includes(kindOf(n)));
    return nested || el;
  };

  // 上传入口的 <input type=file> 通常是隐藏的，可见性定位找不到；按 CSS 唯一匹配文件框。
  function hiddenFileInput(target) {
    if (!target || typeof target.css !== "string") return null;
    let matches;
    try { matches = Array.from(document.querySelectorAll(target.css)).filter((el) => kindOf(el) === "file"); } catch { return null; }
    return matches.length === 1 ? matches[0] : null;
  }

  async function prepare({ target, intent = "click" } = {}) {
    let found = resolveTarget(target);
    if (!found.el && intent === "upload") {
      const file = hiddenFileInput(target);
      if (file) found = { el: file };
    }
    if (!found.el) return found;
    let el = intent === "type" ? editableWithin(found.el) : found.el;
    const extra = found.relocated ? { relocated: true } : {};
    if (intent === "upload" && kindOf(el) === "file") {
      window.__dhRtTarget = el;
      return { ok: true, file_input: true, target: summary(el), ref: remember(el), ...extra };
    }
    if (!enabled(el)) return { error: "目标已禁用，未操作", code: "disabled", target: summary(el) };
    el.scrollIntoView({ block: "center", inline: "center", behavior: "instant" });
    const stable = await waitStable(el);
    if (!visible(el)) return { error: "目标不可见，未操作", code: "hidden", target: summary(el) };
    const point = clickablePoint(el);
    if (!point.ok && point.offscreen) return { error: "目标滚动后仍不在可视区域（可能在折叠区或隐藏容器里），未操作；先展开所在区域，或换一个可见的同名目标", code: "offscreen", target: summary(el) };
    if (!point.ok) return { error: `目标被「${point.by}」遮挡，未操作；先关闭遮挡层或滚动后再试`, code: "covered", covered_by: point.by, target: summary(el) };
    window.__dhRtTarget = el;
    const active = deepActive();
    return {
      ok: true, x: point.x, y: point.y, target: summary(el), ref: remember(el), kind: kindOf(el),
      focused: !!active && (active === el || owns(el, active)), ...(stable ? {} : { moving: true }), ...extra,
    };
  }

  function focusState() {
    const a = deepActive();
    const kind = kindOf(a);
    const t = window.__dhRtTarget;
    window.__dhRtEditing = TEXT_KINDS.includes(kind) ? a : null;
    return {
      kind, editable: TEXT_KINDS.includes(kind), focus: a ? api.describe(a) : "body",
      same_as_target: !!t && !!a && (a === t || owns(t, a) || owns(a, t)),
      readonly: !!a && (!!a.readOnly || a.getAttribute("aria-readonly") === "true"),
    };
  }

  // clear=true 选中全部内容（随后插入的文字会替换选区）；否则把光标移到末尾。
  function selectAll({ clear = true } = {}) {
    const a = window.__dhRtEditing;
    if (!a || !a.isConnected) return { ok: false };
    const kind = kindOf(a);
    if (kind === "input" || kind === "textarea") {
      try {
        if (clear) a.select();
        else a.setSelectionRange(a.value.length, a.value.length);
      } catch { if (clear) a.select(); }
      return { ok: true, had_content: !!a.value };
    }
    const host = editingHost(a);
    const doc = host.ownerDocument;
    const range = doc.createRange();
    range.selectNodeContents(host);
    if (!clear) range.collapse(false);
    const sel = doc.getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
    return { ok: true, had_content: !!norm(host.innerText) };
  }

  const placeholderTexts = (el) => {
    const out = [];
    for (const node of [el, ...el.querySelectorAll("[data-placeholder],[aria-placeholder],[placeholder],[class*='placeholder' i]")]) {
      for (const attr of ["placeholder", "data-placeholder", "aria-placeholder"]) {
        const v = node.getAttribute && node.getAttribute(attr);
        if (v) out.push(norm(v));
      }
      if (node !== el && /placeholder/i.test(String(node.className || ""))) out.push(norm(node.textContent));
    }
    return out.filter(Boolean);
  };

  // 每个非空行按顺序出现在读回内容里即视为成功：编辑器归一化空白、
  // 合并空行、占位文字残留都不算失败；顺序错乱、缺行或旧内容未清除才算。
  function verifyText({ text: expected = "", clear = true } = {}) {
    const a = window.__dhRtEditing;
    if (!a || !a.isConnected) return { verified: false, reason: "输入目标已不在页面上" };
    const kind = kindOf(a);
    const host = kind === "contenteditable" ? editingHost(a) : a;
    const actual = kind === "contenteditable" ? (host.innerText || "") : String(a.value ?? "");
    const isPassword = a.tagName === "INPUT" && a.type === "password";
    const flat = norm(actual);
    const lines = String(expected).split("\n").map(norm).filter(Boolean);
    let pos = 0;
    let matched = 0;
    let missing = null;
    for (const line of lines) {
      const at = flat.indexOf(line, pos);
      if (at < 0) { missing ||= line; continue; }
      matched += 1;
      pos = at + line.length;
    }
    const placeholderLen = placeholderTexts(host).filter((p) => flat.includes(p)).reduce((n, p) => n + p.length, 0);
    const expectedLen = norm(lines.join(" ")).length;
    const leftover = clear && flat.length - expectedLen - placeholderLen > Math.max(20, expectedLen * 0.2);
    const verified = matched === lines.length && (lines.length > 0 || flat.length <= placeholderLen) && !leftover;
    return {
      verified, kind, length: actual.length,
      preview: isPassword ? "（密码已隐藏）" : short(actual, 80),
      ...(missing ? { missing: short(missing, 60), matched_lines: matched, total_lines: lines.length } : {}),
      ...(leftover ? { reason: "原有内容可能没有清除" } : missing ? { reason: "读回内容缺少部分文字" } : {}),
    };
  }

  const MESSAGE_SEL = [
    '[role="alert"]', '[role="status"]', '[aria-live="assertive"]', '[aria-live="polite"]',
    '[class*="toast" i]', '[class*="snackbar" i]', '[class*="notification-notice" i]', '[class*="message-notice" i]',
    ".ant-message", ".el-message", ".semi-toast", ".arco-message", ".byte-message",
  ].join(",");
  const ERROR_SEL = '[class*="error" i]:not(svg):not(path), [class*="invalid" i]:not(input):not(textarea), [aria-invalid="true"] + *';
  const visibleTexts = (selector, max, len) => {
    const out = [];
    for (const el of document.querySelectorAll(selector)) {
      if (out.length >= max) break;
      if (!visible(el)) continue;
      const t = norm(el.innerText ?? el.textContent);
      if (t && t.length <= len && !out.includes(t)) out.push(t);
    }
    return out;
  };
  const dialogName = (d) => short(label(d) || (d.querySelector("h1,h2,h3,[class*='title' i]") || {}).innerText || text(d), 60);

  function observe() {
    const dialogs = Array.from(document.querySelectorAll('[role="dialog"],[role="alertdialog"],[aria-modal="true"],dialog[open]'))
      .filter(visible).slice(0, 6).map(dialogName);
    const active = deepActive();
    return {
      url: location.href, title: document.title, ready: document.readyState, dialogs,
      messages: visibleTexts(MESSAGE_SEL, 6, 200),
      errors: visibleTexts(ERROR_SEL, 5, 120),
      focus: active && active !== document.body ? api.describe(active) : "body",
      mut: window.__dhMut ? window.__dhMut.count : 0,
    };
  }

  function quiet() {
    const m = window.__dhMut || { last: 0, count: 0 };
    return { age: performance.now() - m.last, ready: document.readyState, count: m.count };
  }

  function targetState({ ref } = {}) {
    const el = ref != null ? api.ref(ref) : window.__dhRtTarget;
    if (!el || !el.isConnected) return { connected: false };
    const isPassword = el.tagName === "INPUT" && el.type === "password";
    return {
      connected: true, checked: checked(el), expanded: el.getAttribute("aria-expanded"),
      selected: el.getAttribute("aria-selected"), disabled: !enabled(el),
      value: isPassword ? null : short(el.value ?? "", 80), text: short(el.innerText || el.textContent || "", 80),
    };
  }

  function expect(cond = {}) {
    const details = {};
    let met = true;
    if (cond.url_contains != null) {
      details.url = location.href;
      met &&= location.href.includes(String(cond.url_contains));
    }
    if (cond.text != null || cond.gone_text != null) {
      const body = norm(document.body ? document.body.innerText : "");
      if (cond.text != null) { details.text_found = body.includes(norm(cond.text)); met &&= details.text_found; }
      if (cond.gone_text != null) { details.gone = !body.includes(norm(cond.gone_text)); met &&= details.gone; }
    }
    return { met, ...details };
  }

  // 截图裁剪用：视口 CSS 尺寸、滚动位置与像素比。
  function viewport() {
    return { w: window.innerWidth, h: window.innerHeight, dpr: window.devicePixelRatio || 1, scrollX: window.scrollX, scrollY: window.scrollY };
  }

  const INTERACTIVE_ANCESTOR = 'a,button,input,textarea,select,label,summary,[role],[contenteditable]:not([contenteditable="false"]),[tabindex],[onclick]';
  // 按画面定位得到的是视口坐标：报告该点上实际是什么元素（取最近的可交互祖先），
  // 不滚动——截图之后页面没动，坐标才对得上。
  function describePoint({ x, y } = {}) {
    if (!Number.isFinite(x) || !Number.isFinite(y)) return { error: "point 需要数字 x/y", code: "invalid_request" };
    if (x < 0 || y < 0 || x > window.innerWidth || y > window.innerHeight) return { error: "坐标在视口之外，页面可能已滚动，请重新定位", code: "out_of_view" };
    const hit = deepHit(x, y);
    if (!hit) return { error: "该位置没有元素", code: "not_found" };
    const el = (hit.closest && hit.closest(INTERACTIVE_ANCESTOR)) || hit;
    if (!enabled(el)) return { error: "该位置的元素已禁用，未操作", code: "disabled", target: summary(el) };
    window.__dhRtTarget = el;
    const active = deepActive();
    return { ok: true, target: summary(el), ref: remember(el), kind: kindOf(el), focused: !!active && (active === el || owns(el, active)) };
  }

  function fileNames() {
    const el = window.__dhRtTarget;
    return el && el.files ? Array.from(el.files).map((f) => f.name) : null;
  }

  api.rt = {
    version: RT_VER, prepare, focusState, selectAll, verifyText, observe, quiet, targetState, expect, fileNames, viewport, describePoint,
    currentTarget: () => window.__dhRtTarget || null,
    legacyAction: (args) => api.action(args),
  };
})();
