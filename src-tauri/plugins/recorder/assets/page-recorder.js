// 拾光录屏：页面侧脚本（扩展 debugger 与独立 CDP 共用，由桌面端随 record_start 下发）。
// 1. 事件轨：鼠标、点击、按键（不含输入内容）、滚动、焦点 → window.__sgRec(JSON)（CDP Runtime.addBinding）。
// 2. 源头打码：给敏感元素加 data-sg-mask，在下一次绘制前完成，录到的画面里就没有明文。
(function () {
  if (window.__sgRecInstall) return;

  const PRESETS = {
    phone: "(?<!\\d)1[3-9]\\d{9}(?!\\d)",
    idcard: "(?<![0-9A-Za-z])\\d{17}[0-9Xx](?![0-9A-Za-z])",
    email: "[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\\.[A-Za-z]{2,}",
    bankcard: "(?<!\\d)(?:\\d[ -]?){15,18}\\d(?!\\d)",
    secret: "\\b(?:sk|ak|pk|ghp|gho|xox[abp])[-_][A-Za-z0-9_-]{16,}|\\beyJ[A-Za-z0-9_-]{10,}\\.[A-Za-z0-9_-]{10,}|\\b[A-Fa-f0-9]{32,}\\b",
  };
  const SENSITIVE_INPUTS = 'input[type="password"],input[autocomplete^="cc-"],input[autocomplete="one-time-code"]';
  const INTERACTIVE = 'a,button,input,select,textarea,label,summary,[role="button"],[role="link"],[role="menuitem"],[role="tab"],[role="checkbox"],[role="option"],[onclick],[contenteditable=""],[contenteditable="true"]';
  const SKIP_TAGS = { SCRIPT: 1, STYLE: 1, NOSCRIPT: 1, TEMPLATE: 1, SVG: 1 };
  const STYLE_ID = "__sg-rec-style";

  let state = null;

  function install(cfg) {
    if (state) uninstall();
    cfg = cfg || {};
    const presets = cfg.presets || {};
    const sources = Object.keys(PRESETS).filter((k) => presets[k] !== false).map((k) => PRESETS[k]);
    for (const p of cfg.patterns || []) {
      try { new RegExp(p); sources.push(p); } catch { /* 忽略写错的正则 */ }
    }
    const selectors = [SENSITIVE_INPUTS].concat((cfg.selectors || []).filter((s) => {
      try { document.createDocumentFragment().querySelector(s); return true; } catch { return false; }
    }));
    state = {
      cfg,
      re: sources.length ? new RegExp(sources.join("|")) : null,
      selector: selectors.join(","),
      blur: Math.max(4, Number(cfg.blur) || 8),
      observer: null,
      pending: new Set(),
      raf: 0,
      listeners: [],
      sheet: null,
      styleEl: null,
      top: window.top === window,
    };
    ensureStyle();
    scan(document);
    state.observer = new MutationObserver(onMutations);
    state.observer.observe(document, { childList: true, subtree: true, characterData: true });
    on(document, "input", (e) => queue(e.target), true);
    if (state.top && cfg.events !== false) bindEvents();
    return true;
  }

  function uninstall() {
    if (!state) return true;
    if (state.observer) state.observer.disconnect();
    if (state.raf) cancelAnimationFrame(state.raf);
    for (const [target, type, fn, capture] of state.listeners) target.removeEventListener(type, fn, capture);
    document.querySelectorAll("[data-sg-mask]").forEach((el) => el.removeAttribute("data-sg-mask"));
    document.querySelectorAll("sg-mask").forEach((el) => el.replaceWith(...el.childNodes));
    if (state.sheet) document.adoptedStyleSheets = document.adoptedStyleSheets.filter((x) => x !== state.sheet);
    if (state.styleEl) state.styleEl.remove();
    state = null;
    return true;
  }

  function on(target, type, fn, capture) {
    target.addEventListener(type, fn, { capture: !!capture, passive: true });
    state.listeners.push([target, type, fn, !!capture]);
  }

  // ---------- 打码 ----------

  function maskCss() {
    const b = state.blur;
    // filter 对行内元素不一定生效，再用透明文字 + 模糊阴影兜底
    return `[data-sg-mask],sg-mask{filter:blur(${b}px)!important;color:transparent!important;` +
      `text-shadow:0 0 ${b}px rgba(0,0,0,.55)!important;-webkit-text-fill-color:transparent!important;caret-color:transparent!important}` +
      `sg-mask{display:inline-block}`;
  }

  // 优先用可构造样式表：不受页面 CSP 的 style-src 限制（内联 <style> 在严格 CSP 下会被拦，打码就失效了）
  function ensureStyle() {
    if (state.sheet || state.styleEl) return;
    try {
      const sheet = new CSSStyleSheet();
      sheet.replaceSync(maskCss());
      document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
      state.sheet = sheet;
      return;
    } catch { /* 旧浏览器：退回 <style> */ }
    const root = document.documentElement;
    if (!root) return;
    const style = document.createElement("style");
    style.id = STYLE_ID;
    style.textContent = maskCss();
    (document.head || root).appendChild(style);
    state.styleEl = style;
  }

  function onMutations(records) {
    for (const r of records) {
      if (r.type === "characterData") queue(r.target);
      else r.addedNodes.forEach(queue);
    }
  }

  // 合并到下一帧绘制前统一扫描：rAF 回调在绘制前执行，新内容不会以明文画出来
  function queue(node) {
    if (!state || !node) return;
    state.pending.add(node);
    if (!state.raf) {
      state.raf = requestAnimationFrame(() => {
        state.raf = 0;
        const nodes = Array.from(state.pending);
        state.pending.clear();
        ensureStyle();
        for (const n of nodes) if (n.isConnected) scan(n);
      });
    }
  }

  function scan(root) {
    if (!state) return;
    if (root.nodeType === Node.TEXT_NODE) return checkText(root);
    if (root.nodeType !== Node.ELEMENT_NODE && root.nodeType !== Node.DOCUMENT_NODE) return;
    if (root.nodeType === Node.ELEMENT_NODE) {
      if (SKIP_TAGS[root.tagName] || root.tagName === "SG-MASK") return;
      if (root.matches(state.selector)) mark(root);
      checkValue(root);
    }
    root.querySelectorAll(state.selector).forEach(mark);
    root.querySelectorAll("input,textarea").forEach(checkValue);
    if (!state.re) return;
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let n;
    while ((n = walker.nextNode())) checkText(n);
  }

  function checkValue(el) {
    if (!state.re || !(el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement)) return;
    if (el.value && state.re.test(el.value)) mark(el);
  }

  function checkText(node) {
    const text = node.nodeValue;
    if (!state.re || !text || text.length < 5 || !state.re.test(text)) return;
    const parent = node.parentElement;
    if (!parent || SKIP_TAGS[parent.tagName] || parent.closest("[data-sg-mask],sg-mask")) return;
    // 父元素太大（文字直接挂在大容器下）时只包住这段文字，免得整页糊掉
    const r = parent.getBoundingClientRect();
    if (r.width * r.height > innerWidth * innerHeight * 0.3) {
      const wrap = document.createElement("sg-mask");
      node.replaceWith(wrap);
      wrap.appendChild(node);
    } else {
      mark(parent);
    }
  }

  function mark(el) {
    if (!el.hasAttribute("data-sg-mask")) el.setAttribute("data-sg-mask", "");
  }

  function isMasked(el) {
    return !!(el && el.closest && el.closest("[data-sg-mask],sg-mask"));
  }

  // ---------- 事件轨 ----------

  function emit(ev) {
    if (typeof window.__sgRec !== "function") return;
    ev.t = Date.now();
    ev.vw = innerWidth;
    ev.vh = innerHeight;
    try { window.__sgRec(JSON.stringify(ev)); } catch { /* binding 不可用时忽略 */ }
  }

  function describe(el) {
    if (!el || el.nodeType !== 1) return null;
    const target = el.closest(INTERACTIVE) || el;
    const r = target.getBoundingClientRect();
    let label = "";
    if (isMasked(target) || target.matches(SENSITIVE_INPUTS)) label = "（已打码）";
    else {
      label = target.getAttribute("aria-label") || target.getAttribute("placeholder") || target.getAttribute("title") || "";
      if (!label) label = (target.innerText || target.textContent || "").trim();
      if (!label && target instanceof HTMLInputElement && /^(button|submit|reset)$/.test(target.type)) label = target.value;
      label = label.replace(/\s+/g, " ").slice(0, 40);
      if (label && state.re && state.re.test(label)) label = "（已打码）";
    }
    return {
      tag: target.tagName.toLowerCase(),
      role: target.getAttribute("role") || "",
      label,
      bbox: [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)],
    };
  }

  function bindEvents() {
    let moveRaf = 0;
    let last = null;
    on(window, "pointermove", (e) => {
      last = e;
      if (moveRaf) return;
      moveRaf = requestAnimationFrame(() => {
        moveRaf = 0;
        if (last) emit({ type: "move", x: last.clientX, y: last.clientY });
      });
    }, true);
    on(window, "pointerdown", (e) => emit({ type: "down", x: e.clientX, y: e.clientY, button: e.button }), true);
    on(window, "pointerup", (e) => emit({ type: "up", x: e.clientX, y: e.clientY, button: e.button }), true);
    on(window, "click", (e) => emit({ type: "click", x: e.clientX, y: e.clientY, target: describe(e.target) }), true);
    on(document, "pointerleave", () => emit({ type: "leave" }), true);
    on(window, "keydown", (e) => {
      const t = e.target;
      if (t && t.matches && t.matches(SENSITIVE_INPUTS)) return; // 密码框按键完全不记
      const mods = [e.ctrlKey && "Ctrl", e.metaKey && "Meta", e.altKey && "Alt", e.shiftKey && "Shift"].filter(Boolean);
      const printable = e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey;
      emit({ type: "key", key: printable ? "char" : e.key, mods });
    }, true);
    on(window, "focusin", (e) => {
      const t = e.target;
      if (t && t.matches && t.matches("input,textarea,select,[contenteditable],[contenteditable=true]")) {
        emit({ type: "focus", target: describe(t) });
      }
    }, true);
    let scrollAt = 0;
    on(window, "scroll", () => {
      const now = Date.now();
      if (now - scrollAt < 100) return;
      scrollAt = now;
      emit({ type: "scroll", sx: scrollX, sy: scrollY });
    }, true);
    emit({ type: "page", url: location.href, title: document.title });
  }

  window.__sgRecInstall = install;
  window.__sgRecUninstall = uninstall;
})();
