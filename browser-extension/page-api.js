// 拾光页面操作 API（单一实现，两个通道共用）：
// - 浏览器扩展 debugger：由 background.js 通过 chrome.debugger 注入（主世界，默认）
// - 浏览器扩展 scripting：chrome.scripting 注入（隔离世界，debugger 无法附着时降级）
// - CDP 通道：由桌面端 Rust 代码 include_str! 嵌入后通过 Page.evaluate 注入（主世界）
// 幂等：同版本重复注入直接返回；版本升级时覆盖旧引用重新注入。
//
// v2 变更：
// - 元素检测：语义选择器之外补充启发式（tabindex / cursor:pointer / React props
//   的 onClick/onPress），覆盖 RN Web 等无语义框架的 Pressable；祖先已标注则
//   启发式后代跳过，避免 cursor 继承导致嵌套元素重复编号。
// - 快照语义：输入控件补充 name/id/关联 label 文本/所属表单，表单与弹窗输出
//   分组行，降低纯文本 LLM 选错元素的概率。
// - click：完整指针事件序列（RN Web Responder 系统需要按压序列），中心点遮挡
//   检测，MutationObserver + URL 观测点击是否生效，返回 changed 供决策。
// - type：聚焦校验（失败时模拟点击再聚焦）、写入读回校验，结果回显目标描述。
// - scroll：自动定位真实滚动容器（或按 ref 找最近可滚动祖先），位移校验，
//   wheel 事件兜底，如实返回 moved。
// v3 变更：
// - 快照两遍遍历：打开的弹窗（role=dialog/aria-modal/dialog[open]）优先于 body。
//   弹窗常 portal 到 body 末尾，正文庞大的页面（如 LinkedIn feed）会先把字符预算
//   耗尽，导致弹窗里的编辑器被截断丢失；当前焦点所在的可编辑元素也保证进快照。
// - contenteditable 检测放宽为 [contenteditable]:not(false)，快照标注「可编辑」
//   及 data-placeholder/aria-placeholder。
// - type 对富文本编辑器改用选区 + execCommand('insertText')：走编辑器自己的
//   beforeinput 管线（ProseMirror/Slate 依赖它同步内部 model），不再直接改
//   innerText 破坏编辑器内部结构；读回校验对空白/换行归一化容错。
// v4 变更：
// - snapshot 支持 scope 参数（元素编号或 CSS 选择器）：只遍历目标容器子树、
//   编号从 1 重排；全量快照截断时附全页可交互元素总数并引导用 scope 聚焦。
// v5 变更：
// - read：用 Mozilla Readability 抽取正文纯文本（导航/广告/侧栏剥离），供读文章/
//   总结；与 snapshot（操作页）职责分离。依赖同世界先注入的 Readability 构造函数。
// v6 变更：
// - snapshot 补充展开状态、弹层关系、可搜索性、禁用/必填等交互语义，并优先输出
//   当前可见的 dialog/listbox/menu/tree/grid 等浮层，覆盖 portal 和虚拟列表。
// - type 可从触发器/组合控件通用解析到实际可编辑后代、关联控件或新打开浮层中的
//   输入框；原生 select 支持唯一模糊匹配，歧义时返回候选而非猜测。
// - scroll 会沿 aria-controls/aria-owns 等控件关系寻找浮层中的真实滚动容器。
(() => {
  const DH_VER = 10;
  if (window.__dh && window.__dhVer === DH_VER) return;

  const INTERACTIVE_SEL = [
    "a", "button", "input", "textarea", "select", "summary",
    '[role="button"]', '[role="link"]', '[role="menuitem"]', '[role="tab"]',
    '[role="checkbox"]', '[role="radio"]', '[role="switch"]', '[role="textbox"]',
    '[role="combobox"]', '[role="option"]',
    '[contenteditable]:not([contenteditable="false"])', "[onclick]",
  ].join(",");
  const SKIP_TAGS = new Set([
    "SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "SVG", "PATH",
    "META", "LINK", "HEAD", "BR", "HR",
  ]);
  const POPUP_SEL = [
    '[role="dialog"]', '[aria-modal="true"]', 'dialog[open]',
    '[role="listbox"]', '[role="menu"]', '[role="tree"]', '[role="grid"]',
  ].join(',');

  const isEditable = (el) => !!el && (
    el.isContentEditable || el.matches('input:not([type="hidden"]), textarea, select')
  );

  function controlledElements(el) {
    if (!el) return [];
    const ids = `${el.getAttribute('aria-controls') || ''} ${el.getAttribute('aria-owns') || ''}`
      .trim().split(/\s+/).filter(Boolean);
    return ids.map((id) => document.getElementById(id)).filter(Boolean);
  }

  function visibleMatches(root, selector) {
    if (!root) return [];
    const found = [];
    if (root.matches && root.matches(selector) && visibleStyle(root)) found.push(root);
    root.querySelectorAll(selector).forEach((el) => {
      if (visibleStyle(el)) found.push(el);
    });
    return found;
  }

  // 返回 false（不可见）或计算样式（供后续 cursor 判断复用，避免重复取值）
  const visibleStyle = (el) => {
    const st = getComputedStyle(el);
    if (st.display === "none" || st.visibility === "hidden" || st.visibility === "collapse") return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 || r.height > 0 ? st : false;
  };

  function isPopupLayer(el) {
    const st = visibleStyle(el);
    if (!st) return false;
    const role = el.getAttribute('role');
    if (role === 'dialog' || role === 'listbox' || role === 'menu' || el.matches('[aria-modal="true"], dialog[open]')) {
      return true;
    }
    // tree/grid 既可能是弹出选择器，也可能是页面主体。只有定位成浮层，或被当前
    // 展开的控件通过 aria-controls/owns 关联时，才按浮层优先处理。
    if (role === 'tree' || role === 'grid') {
      if (st.position === 'fixed' || st.position === 'absolute') return true;
      return Array.from(document.querySelectorAll('[aria-expanded="true"]'))
        .some((control) => controlledElements(control).includes(el));
    }
    return false;
  }

  const visiblePopupElements = () =>
    Array.from(document.querySelectorAll(POPUP_SEL)).filter(isPopupLayer);

  const short = (s, n) => {
    s = (s || "").replace(/\s+/g, " ").trim();
    return s.length > n ? s.slice(0, n) + "…" : s;
  };

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  // RN Web 等框架：事件挂在 React 内部 props 上，DOM 属性完全不可见
  const hasReactPressHandler = (el) => {
    const keys = Object.keys(el);
    for (let i = 0; i < keys.length; i++) {
      const k = keys[i];
      if (k.startsWith("__reactProps$")) {
        const p = el[k];
        return !!(p && (p.onClick || p.onPress || p.onTap || p.onPressIn));
      }
    }
    return false;
  };

  // 无语义元素的可交互推断；返回命中原因（用于快照标注置信度）或 null
  const heuristicReason = (el, st) => {
    if (el.tabIndex >= 0) return "tabindex";
    if (st && st.cursor === "pointer") return "手型光标";
    if (hasReactPressHandler(el)) return "React事件";
    return null;
  };

  const ownTextOf = (el) =>
    Array.from(el.childNodes)
      .filter((c) => c.nodeType === 3)
      .map((c) => c.textContent)
      .join(" ");

  // 输入控件的关联文本：标准 label → aria-labelledby → 包裹 label → 前置兄弟/父级文本
  function labelTextOf(el) {
    if (el.labels && el.labels.length) return el.labels[0].innerText || "";
    const lb = el.getAttribute("aria-labelledby");
    if (lb) {
      const t = lb
        .split(/\s+/)
        .map((id) => {
          const n = document.getElementById(id);
          return n ? n.innerText : "";
        })
        .join(" ")
        .trim();
      if (t) return t;
    }
    const wrap = el.closest("label");
    if (wrap) return wrap.innerText || "";
    const sib = el.previousElementSibling;
    if (sib && sib.innerText && sib.innerText.trim().length < 60) return sib.innerText;
    const p = el.parentElement;
    if (p) {
      const own = ownTextOf(p).trim();
      if (own && own.length < 60) return own;
      const ps = p.previousElementSibling;
      if (ps && ps.innerText && ps.innerText.trim().length < 60) return ps.innerText;
    }
    return "";
  }

  function labelOf(el) {
    const tag = el.tagName.toLowerCase();
    const parts = [];
    if (tag === "a" && el.href) parts.push(short(el.href, 80));
    if (tag === "input") {
      parts.push("type=" + (el.type || "text"));
      if (el.placeholder) parts.push("placeholder=" + short(el.placeholder, 30));
      if (el.value && !["password", "checkbox", "radio"].includes(el.type)) {
        parts.push("value=" + short(el.value, 40));
      }
      if (el.checked) parts.push("checked");
    }
    if (tag === "select") {
      const o = el.selectedOptions && el.selectedOptions[0];
      if (o) parts.push("selected=" + short(o.textContent, 30));
    }
    if (el.isContentEditable) {
      parts.push("可编辑");
      const ph = el.getAttribute("data-placeholder") || el.getAttribute("aria-placeholder");
      if (ph) parts.push("placeholder=" + short(ph, 30));
    }
    if (["input", "textarea", "select"].includes(tag)) {
      if (el.name) parts.push("name=" + short(el.name, 24));
      if (el.id) parts.push("id=" + short(el.id, 24));
      const lt = labelTextOf(el);
      if (lt) parts.push("标签=" + short(lt, 30));
      const f = el.closest("form");
      if (f) {
        parts.push(
          "表单=" + short(f.getAttribute("name") || f.id || f.getAttribute("aria-label") || f.action || "", 40)
        );
      }
    }
    const role = el.getAttribute("role");
    if (role) parts.push("role=" + role);
    if (role === "combobox" || role === "button" || el.hasAttribute("aria-haspopup")) {
      const lt = labelTextOf(el);
      if (lt && !parts.some((part) => part.startsWith("标签="))) {
        parts.push("标签=" + short(lt, 30));
      }
    }
    const expanded = el.getAttribute("aria-expanded");
    if (expanded === "true" || expanded === "false") {
      parts.push("状态=" + (expanded === "true" ? "已展开" : "已收起"));
    }
    const popup = el.getAttribute("aria-haspopup");
    if (popup && popup !== "false") parts.push("弹层=" + (popup === "true" ? "menu" : popup));
    const controls = el.getAttribute("aria-controls") || el.getAttribute("aria-owns");
    if (controls) parts.push("关联=" + short(controls, 40));
    const activeDescendant = el.getAttribute("aria-activedescendant");
    if (activeDescendant) parts.push("当前候选=" + short(activeDescendant, 40));
    const autocomplete = el.getAttribute("aria-autocomplete");
    if (autocomplete && autocomplete !== "none") parts.push("可搜索=" + autocomplete);
    if (role === "combobox" && (isEditable(el) || visibleMatches(el, 'input, [role="textbox"]').length)) {
      parts.push("可输入搜索");
    }
    if (role === "option") {
      const selected = el.getAttribute("aria-selected");
      if (selected === "true") parts.push("已选择");
      const pos = el.getAttribute("aria-posinset");
      const size = el.getAttribute("aria-setsize");
      if (pos || size) parts.push(`位置=${pos || "?"}/${size || "?"}`);
    }
    if (el.matches(':disabled, [aria-disabled="true"]')) parts.push("已禁用");
    if (el.matches('[required], [aria-required="true"]')) parts.push("必填");
    if (el.matches('[readonly], [aria-readonly="true"]')) parts.push("只读");
    const aria = el.getAttribute("aria-label");
    if (aria) parts.push("aria=" + short(aria, 40));
    return parts.filter(Boolean).join(" ");
  }

  // 供 click/type 返回值确认目标用：简短描述元素身份
  function describeEl(el) {
    const tag = el.tagName.toLowerCase();
    const text = short(ownTextOf(el) || el.innerText || "", 30);
    const meta = labelOf(el);
    return `<${tag}>${text ? " " + text : ""}${meta ? " (" + meta + ")" : ""}`;
  }

  // 生成页面文本快照：交互元素标注 [编号]，编号存入 window.__dhRefs 供 click/type 引用。
  // 全量模式两遍遍历：先走打开的弹窗，再走 body 其余部分。弹窗常 portal 到 body 末尾，
  // 正文庞大的页面（如 LinkedIn feed）会先把字符预算耗尽，导致弹窗被截断丢失。
  // scope 模式：只遍历指定元素子树，编号从 1 重排——先全量定位容器、再局部细看，
  // AI 面对的候选从几百个降到十几个，选择精度显著提升。
  function buildSnapshot(maxChars, scopeRoot) {
    const refs = {};
    const numberedMap = new Map();
    let count = 0;
    let total = 0;
    let truncated = false;
    const lines = [];

    const pushLine = (depth, line) => {
      total += line.length;
      if (total > maxChars) {
        truncated = true;
        lines.push("…(页面过大已截断)");
        return false;
      }
      lines.push("  ".repeat(Math.min(depth, 20)) + line);
      return true;
    };

    const numberEl = (el) => {
      const existing = numberedMap.get(el);
      if (existing) return existing;
      count += 1;
      numberedMap.set(el, count);
      refs[count] = el;
      return count;
    };

    // markedAnc：祖先已被编号时，启发式命中的后代不再编号
    // （cursor:pointer 是继承属性，不去重会把 RN 页面整棵子树都编号）
    const walk = (el, depth, markedAnc, skipPopups) => {
      if (truncated || !el || el.nodeType !== 1 || SKIP_TAGS.has(el.tagName)) return;
      if (skipPopups && el.matches(POPUP_SEL) && isPopupLayer(el)) return;
      const st = visibleStyle(el);
      if (!st) return;
      const alreadyNumbered = numberedMap.has(el);
      const semantic = el.matches(INTERACTIVE_SEL);
      let heuristic = null;
      if (!semantic && !markedAnc && !alreadyNumbered) heuristic = heuristicReason(el, st);
      const interactive = semantic || !!heuristic;
      const tag = el.tagName.toLowerCase();
      const ownText = ownTextOf(el);
      let line = null;
      if (interactive && !alreadyNumbered) {
        const n = numberEl(el);
        const label = short(ownText || el.innerText || "", 60);
        const meta = labelOf(el);
        line = `[${n}] <${tag}>${label ? " " + label : ""}${meta ? "  (" + meta + ")" : ""}`;
        if (heuristic) line += `  <启发式:${heuristic}>`;
      } else if (!alreadyNumbered && tag === "form") {
        const desc = el.getAttribute("name") || el.id || el.getAttribute("aria-label") || el.action || "";
        line = `▼ 表单${desc ? " " + short(desc, 40) : ""}`;
      } else if (!alreadyNumbered && (el.getAttribute("role") === "dialog" || el.hasAttribute("aria-modal") || tag === "dialog")) {
        const desc = el.getAttribute("aria-label") || "";
        line = `▼ 弹窗${desc ? " " + short(desc, 40) : ""}`;
      } else if (tag === "img") {
        line = `[img] ${short(el.alt, 60)}`;
      } else if (ownText.trim()) {
        line = short(ownText, 120);
      }
      if (line !== null && !pushLine(depth, line)) return;
      const nextDepth = depth + (line !== null ? 1 : 0);
      const childMarked = markedAnc || interactive;
      if (el.shadowRoot) Array.from(el.shadowRoot.children).forEach((c) => walk(c, nextDepth, childMarked, skipPopups));
      Array.from(el.children).forEach((c) => walk(c, nextDepth, childMarked, skipPopups));
    };

    if (scopeRoot) {
      // 局部快照：头部说明聚焦对象，然后只遍历其子树
      pushLine(0, `◇ 局部快照，范围: ${describeEl(scopeRoot)}（编号已重排，仅本范围内有效）`);
      walk(scopeRoot, 1, false, false);
      window.__dhRefs = refs;
      if (count === 0 && !truncated) lines.push("(范围内没有检测到可交互元素)");
      return lines.join("\n");
    }

    // 第一遍：当前可见浮层优先。自定义下拉通常 portal 到 body 末尾，若仍按正文顺序
    // 遍历，选项会被大页面字符预算吞掉。嵌套浮层随外层遍历，不重复。
    const walkedPopups = [];
    document.querySelectorAll(POPUP_SEL).forEach((popup) => {
      if (truncated || !isPopupLayer(popup)) return;
      if (walkedPopups.some((walked) => walked.contains(popup))) return;
      walkedPopups.push(popup);
      const role = popup.getAttribute('role') || popup.tagName.toLowerCase();
      pushLine(0, `◆ 当前可见浮层 role=${role}${popup.id ? ` id=${popup.id}` : ''}`);
      walk(popup, 1, false, false);
    });

    // 当前焦点所在的可编辑元素必须进快照（即使不在弹窗里、即使页面巨大）
    const ae = document.activeElement;
    if (
      !truncated && ae && ae !== document.body && !numberedMap.has(ae) &&
      (ae.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(ae.tagName)) &&
      visibleStyle(ae)
    ) {
      const n = numberEl(ae);
      const tag = ae.tagName.toLowerCase();
      const label = short(ae.innerText || "", 60);
      const meta = labelOf(ae);
      pushLine(0, `[${n}] <${tag}>${label ? " " + label : ""}${meta ? "  (" + meta + ")" : ""}  ⬅当前焦点`);
    }

    // 第二遍：body 其余部分（跳过已遍历的弹窗子树）
    walk(document.body, 0, false, true);

    if (truncated) {
      // 告知总量，引导用 scope 聚焦局部而不是反复全量重试
      const rough = document.querySelectorAll(INTERACTIVE_SEL).length;
      lines.push(`（全页约有 ${rough} 个可交互元素，建议用 scope 参数聚焦目标容器（弹窗/表单/列表的编号或 CSS 选择器）获取局部快照）`);
    }
    window.__dhRefs = refs;
    return lines.join("\n") || "(页面为空)";
  }

  function getRef(ref) {
    const el = (window.__dhRefs || {})[ref];
    if (!el || !el.isConnected) return null;
    return el;
  }

  // 只返回匹配目标的紧凑候选，避免为了找一个“提交”按钮而把整页导航和卡片交给模型。
  // 仍把候选写入 ref 表，可直接 click/type，不需要再取一次全页快照。
  function findElements(query, limit) {
    const needle = String(query || "").trim().toLocaleLowerCase();
    if (!needle) return { error: "缺少查找文字" };
    const tokens = needle.split(/\s+/).filter(Boolean);
    const refs = {};
    const results = [];
    const seen = new Set();
    const all = Array.from(document.querySelectorAll(INTERACTIVE_SEL));
    for (const el of all) {
      if (results.length >= Math.max(1, Math.min(Number(limit) || 12, 30))) break;
      if (!visibleStyle(el) || seen.has(el)) continue;
      const label = describeEl(el);
      const haystack = `${label} ${el.getAttribute('aria-label') || ''} ${el.getAttribute('title') || ''}`.toLocaleLowerCase();
      if (!tokens.every((token) => haystack.includes(token))) continue;
      seen.add(el);
      const ref = results.length + 1;
      refs[ref] = el;
      results.push({ ref, element: label });
    }
    window.__dhRefs = refs;
    return {
      query: String(query),
      count: results.length,
      candidates: results,
      hint: results.length === 0 ? "未找到可见匹配项；换更短的关键词或使用 browser_snapshot 查看局部区域。" : undefined,
    };
  }

  async function sameOriginRequest(method, rawUrl, body, headers, maxChars) {
    const upper = String(method || "GET").toUpperCase();
    if (!["GET", "POST", "PUT", "PATCH", "DELETE"].includes(upper)) return { error: "仅支持 GET/POST/PUT/PATCH/DELETE" };
    let target;
    try { target = new URL(String(rawUrl || ""), location.href); } catch { return { error: "接口地址无效" }; }
    if (target.origin !== location.origin) return { error: "为保护当前登录态，只允许调用当前页面同源接口" };
    const safeHeaders = {};
    for (const [key, value] of Object.entries(headers || {})) {
      const lower = String(key).toLowerCase();
      if (["authorization", "cookie", "set-cookie", "proxy-authorization"].includes(lower)) return { error: `禁止手工传入 ${key}；认证将仅由当前浏览器会话自动携带` };
      if (["content-type", "accept", "x-requested-with"].includes(lower)) safeHeaders[key] = String(value);
    }
    const init = { method: upper, credentials: "same-origin", headers: safeHeaders };
    // 常见站点把 CSRF 值放在当前页 meta 中；仅在页面内部自动附带，不返回给模型。
    if (upper !== "GET" && !Object.keys(safeHeaders).some((key) => key.toLowerCase() === "x-csrf-token")) {
      const csrf = document.querySelector('meta[name="csrf-token"], meta[name="csrf_token"]');
      const token = csrf && csrf.getAttribute("content");
      if (token) init.headers["X-CSRF-Token"] = token;
    }
    if (body != null && upper !== "GET") {
      if (typeof body === "string") init.body = body;
      else { if (!Object.keys(safeHeaders).some((key) => key.toLowerCase() === "content-type")) init.headers["Content-Type"] = "application/json"; init.body = JSON.stringify(body); }
    }
    let response;
    try { response = await fetch(target.href, init); } catch (e) { return { error: "接口请求失败: " + String((e && e.message) || e) }; }
    const text = await response.text();
    const cap = Math.max(500, Math.min(Number(maxChars) || 6000, 20000));
    const chars = Array.from(text);
    let preview = chars.slice(0, cap).join("");
    try { preview = JSON.stringify(JSON.parse(text), null, 2); preview = Array.from(preview).slice(0, cap).join(""); } catch { /* 保留文本预览 */ }
    return { ok: response.ok, status: response.status, url: target.href, content_type: response.headers.get("content-type") || "", returned_chars: Array.from(preview).length, truncated: chars.length > cap, body: preview };
  }

  function centerOf(el) {
    const r = el.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
  }

  // RN Web 的 Pressable 需要完整按压序列才触发 onPress；
  // 合成 click 事件的 activation behavior（链接跳转/勾选切换）不受 isTrusted 影响
  function firePressSequence(el, x, y) {
    const opts = {
      bubbles: true, cancelable: true, composed: true, view: window,
      clientX: x, clientY: y, button: 0, buttons: 1,
    };
    const pe = (type) => {
      if (window.PointerEvent) el.dispatchEvent(new PointerEvent(type, opts));
    };
    const me = (type) => el.dispatchEvent(new MouseEvent(type, opts));
    pe("pointerover"); me("mouseover");
    pe("pointermove"); me("mousemove");
    pe("pointerdown"); me("mousedown");
    try { el.focus && el.focus(); } catch { /* ignore */ }
    pe("pointerup"); me("mouseup");
    me("click");
  }

  // 向上找最近的可滚动祖先（RN Web 的 ScrollView/FlatList 都是内层 overflow 容器）
  function nearestScrollable(el, horizontal) {
    let cur = el.parentElement;
    while (cur && cur !== document.documentElement) {
      const st = getComputedStyle(cur);
      const overflow = horizontal ? st.overflowX : st.overflowY;
      if (/(auto|scroll)/.test(overflow)) {
        const room = horizontal
          ? cur.scrollWidth - cur.clientWidth
          : cur.scrollHeight - cur.clientHeight;
        if (room > 8) return cur;
      }
      cur = cur.parentElement;
    }
    return null;
  }

  function asScrollable(el, horizontal) {
    if (!el) return null;
    const st = getComputedStyle(el);
    const overflow = horizontal ? st.overflowX : st.overflowY;
    const room = horizontal ? el.scrollWidth - el.clientWidth : el.scrollHeight - el.clientHeight;
    return /(auto|scroll)/.test(overflow) && room > 8 ? el : null;
  }

  function relatedScrollable(el, horizontal) {
    for (const related of controlledElements(el)) {
      const direct = asScrollable(related, horizontal);
      if (direct) return direct;
      const descendants = related.querySelectorAll('*');
      for (let i = 0; i < Math.min(descendants.length, 3000); i++) {
        const candidate = asScrollable(descendants[i], horizontal);
        if (candidate && visibleStyle(candidate)) return candidate;
      }
    }
    const role = el && el.getAttribute && el.getAttribute('role');
    const hasPopup = el && el.getAttribute && el.getAttribute('aria-haspopup');
    if (role === 'combobox' || (hasPopup && hasPopup !== 'false')) {
      const popups = visiblePopupElements();
      for (let i = popups.length - 1; i >= 0; i--) {
        const popup = popups[i];
        const direct = asScrollable(popup, horizontal);
        if (direct) return direct;
        const descendants = popup.querySelectorAll('*');
        for (let j = 0; j < Math.min(descendants.length, 3000); j++) {
          const candidate = asScrollable(descendants[j], horizontal);
          if (candidate && visibleStyle(candidate)) return candidate;
        }
      }
    }
    return null;
  }

  function activePopupScrollable(horizontal) {
    const popups = visiblePopupElements();
    for (let i = popups.length - 1; i >= 0; i--) {
      const popup = popups[i];
      const direct = asScrollable(popup, horizontal);
      if (direct) return direct;
      const descendants = popup.querySelectorAll('*');
      for (let j = 0; j < Math.min(descendants.length, 3000); j++) {
        const candidate = asScrollable(descendants[j], horizontal);
        if (candidate && visibleStyle(candidate)) return candidate;
      }
    }
    return null;
  }

  async function resolveEditable(el) {
    if (isEditable(el)) return { target: el, adapted: false, opened: false };
    const nested = visibleMatches(el, 'input:not([type="hidden"]), textarea, select, [contenteditable]:not([contenteditable="false"])')[0];
    if (nested) return { target: nested, adapted: true, opened: false };
    for (const related of controlledElements(el)) {
      const candidate = visibleMatches(related, 'input:not([type="hidden"]), textarea, select, [contenteditable]:not([contenteditable="false"])')[0];
      if (candidate) return { target: candidate, adapted: true, opened: false };
    }

    // 对任何被当作输入目标的非编辑元素，先执行一次标准按压，再从实际焦点、关联
    // 控件和新出现的浮层中找编辑面。这适用于组合框、日期选择器、标签选择器等，
    // 不绑定具体 UI 框架或站点。
    const beforePopups = new Set(visiblePopupElements());
    el.scrollIntoView({ block: 'center', behavior: 'instant' });
    const { x, y } = centerOf(el);
    firePressSequence(el, x, y);
    await sleep(220);
    if (isEditable(document.activeElement) && visibleStyle(document.activeElement)) {
      return { target: document.activeElement, adapted: true, opened: true };
    }
    for (const related of controlledElements(el)) {
      const candidate = visibleMatches(related, 'input:not([type="hidden"]), textarea, select, [contenteditable]:not([contenteditable="false"])')[0];
      if (candidate) return { target: candidate, adapted: true, opened: true };
    }
    const popups = visiblePopupElements();
    const ordered = popups.filter((popup) => !beforePopups.has(popup)).concat(popups.filter((popup) => beforePopups.has(popup)));
    for (const popup of ordered) {
      const candidate = visibleMatches(popup, 'input:not([type="hidden"]), textarea, [contenteditable]:not([contenteditable="false"])')[0];
      if (candidate) return { target: candidate, adapted: true, opened: true };
    }
    return { target: null, adapted: true, opened: popups.length > beforePopups.size };
  }

  // 页面级最佳滚动容器：取视口内面积最大的可滚动元素；window 可滚且没有
  // 足够大的内层容器时返回 null（表示用 window 滚动）
  function bestScrollable(horizontal) {
    const se = document.scrollingElement || document.documentElement;
    const windowRoom = horizontal
      ? se.scrollWidth - se.clientWidth
      : se.scrollHeight - se.clientHeight;
    const all = document.body ? document.body.getElementsByTagName("*") : [];
    const n = Math.min(all.length, 8000);
    let best = null;
    let bestArea = 0;
    for (let i = 0; i < n; i++) {
      const el = all[i];
      if (SKIP_TAGS.has(el.tagName)) continue;
      const st = getComputedStyle(el);
      const overflow = horizontal ? st.overflowX : st.overflowY;
      if (!/(auto|scroll)/.test(overflow)) continue;
      const room = horizontal
        ? el.scrollWidth - el.clientWidth
        : el.scrollHeight - el.clientHeight;
      if (room <= 8) continue;
      const r = el.getBoundingClientRect();
      if (r.bottom < 0 || r.top > innerHeight || r.right < 0 || r.left > innerWidth) continue;
      const area =
        Math.max(0, Math.min(r.width, innerWidth)) * Math.max(0, Math.min(r.height, innerHeight));
      if (area > bestArea) {
        bestArea = area;
        best = el;
      }
    }
    // 内层容器够大（覆盖视口 30%+）优先于 window；很小但 window 不可滚时也只能用它
    if (best && bestArea >= innerWidth * innerHeight * 0.3) return best;
    if (windowRoom > 8) return null;
    return best;
  }

  function describeContainer(el) {
    if (!el) return "window";
    const tag = el.tagName.toLowerCase();
    const id = el.id ? "#" + el.id : "";
    const cls = typeof el.className === "string" && el.className.trim()
      ? "." + el.className.trim().split(/\s+/).slice(0, 2).join(".")
      : "";
    return short(tag + id + cls, 50);
  }

  window.__dhVer = DH_VER;
  window.__dh = {
    // 给 browser_evaluate 的 ref 模式使用；返回真实 DOM 元素，不做站点专用封装。
    ref: (ref) => getRef(ref),
    // scope：数字=上次快照中的元素编号，字符串=CSS 选择器；只遍历该元素子树
    snapshot: (maxChars, scope) => {
      if (scope == null || scope === "") return buildSnapshot(maxChars || 8000, null);
      const root = typeof scope === "number" ? getRef(scope) : document.querySelector(String(scope));
      if (!root) return { error: `聚焦目标 ${JSON.stringify(scope)} 不存在或已失效，请重新获取快照` };
      return buildSnapshot(maxChars || 8000, root);
    },

    // 抽取可读正文（文章/新闻/文档页）。会 clone 文档再解析，不改动当前页面 DOM。
    read: (maxChars, offset) => {
      const cap = Math.max(500, Math.min(Number(maxChars) || 12000, 100000));
      if (typeof Readability !== "function") {
        return { error: "Readability 未加载，无法提取正文" };
      }
      let article;
      try {
        article = new Readability(document.cloneNode(true)).parse();
      } catch (e) {
        return { error: "正文提取失败: " + String((e && e.message) || e) };
      }
      const raw = article && article.textContent ? String(article.textContent) : "";
      const cleaned = raw.replace(/[ \t]+\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim();
      if (!cleaned) {
        return {
          error:
            "未能提取到正文（可能是应用页、登录页、列表流或非文章结构）。操作页面请用 browser_snapshot；看画面用 browser_screenshot。",
        };
      }
      const chars = Array.from(cleaned);
      const start = Math.max(0, Math.min(Number(offset) || 0, chars.length));
      const truncated = chars.length > start + cap;
      const content = chars.slice(start, start + cap).join("");
      const out = {
        title: (article.title || document.title || "").trim(),
        byline: (article.byline || "").trim(),
        siteName: (article.siteName || "").trim(),
        excerpt: (article.excerpt || "").trim(),
        length: chars.length,
        offset: start,
        returned_chars: Array.from(content).length,
        truncated,
        content,
      };
      if (truncated) {
        out.hint = `正文共 ${chars.length} 字符，当前返回第 ${start + 1}–${start + Array.from(content).length} 字。用 offset 继续读取，不要直接把 max_chars 拉得很大；要操作页面请用 browser_snapshot。`;
      }
      return out;
    },

    find: (query, limit) => findElements(query, limit),

    request: (method, url, body, headers, maxChars) => sameOriginRequest(method, url, body, headers, maxChars),

    click: async (ref) => {
      const el = getRef(ref);
      if (!el) return { error: `元素 [${ref}] 不存在或已失效，请重新获取快照` };
      el.scrollIntoView({ block: "center", behavior: "instant" });
      const { x, y } = centerOf(el);
      // 遮挡检测：sticky 头/弹层盖住目标时，中心点命中的其实是遮挡层
      const top = document.elementFromPoint(x, y);
      const coveredBy = top && top !== el && !el.contains(top) ? describeEl(top) : null;

      const urlBefore = location.href;
      const activeBefore = document.activeElement;
      const expandedBefore = el.getAttribute('aria-expanded');
      const visiblePopupsBefore = visiblePopupElements().length;
      let mutated = false;
      const mo = new MutationObserver(() => { mutated = true; });
      mo.observe(document.body, { childList: true, subtree: true, attributes: true, characterData: true });

      firePressSequence(el, x, y);
      await sleep(500);
      mo.disconnect();

      const urlChanged = location.href !== urlBefore;
      const focusChanged = document.activeElement !== activeBefore;
      const expandedAfter = el.getAttribute('aria-expanded');
      const visiblePopupsAfter = visiblePopupElements().length;
      const changed = mutated || urlChanged || focusChanged;
      const out = {
        ok: true,
        changed,
        target: describeEl(el),
        observed: {
          expanded_before: expandedBefore,
          expanded_after: expandedAfter,
          visible_popups_before: visiblePopupsBefore,
          visible_popups_after: visiblePopupsAfter,
          focus: describeEl(document.activeElement || document.body),
        },
      };
      if (urlChanged) out.url = location.href;
      if (coveredBy) {
        out.covered_by = coveredBy;
        out.hint = `元素中心被「${coveredBy}」遮挡，点击可能落在遮挡层上；可先关闭遮挡、滚动调整位置后重试`;
      } else if (!changed) {
        out.hint = "点击后页面无可见变化，可能未生效：可重新快照确认目标、滚动后再点，或用 browser_evaluate 直接触发页面逻辑";
      } else if (visiblePopupsAfter > visiblePopupsBefore || expandedAfter === 'true') {
        out.next = "控件已展开或出现新浮层。重新获取快照观察其中的输入框、选项和滚动状态，再决定输入、点击或滚动；不要沿用旧编号猜测。";
      }
      return out;
    },

    type: async (ref, text, clear) => {
      const source = getRef(ref);
      if (!source) return { error: `元素 [${ref}] 不存在或已失效，请重新获取快照` };
      const resolved = await resolveEditable(source);
      const el = resolved.target;
      if (!el) {
        return {
          error: `元素 [${ref}] 本身不可输入，且打开/检查其关联控件和当前浮层后仍未找到可编辑区域。`,
          target: describeEl(source),
          observed: {
            role: source.getAttribute('role') || '',
            expanded: source.getAttribute('aria-expanded'),
            controls: source.getAttribute('aria-controls') || source.getAttribute('aria-owns') || '',
          },
          next: "重新获取快照查看打开后的浮层与焦点；若页面语义不足，用 browser_evaluate 依据 role/aria/text 生成最小 DOM 脚本，并返回执行后的可观察状态。",
        };
      }
      el.scrollIntoView({ block: "center", behavior: "instant" });
      el.focus();
      // 焦点校验：弹层/RN 拦截 focus 时，先模拟真实按压再聚焦（点击会触发页面自身的焦点管理）
      if (document.activeElement !== el && !el.contains(document.activeElement)) {
        const { x, y } = centerOf(el);
        firePressSequence(el, x, y);
        el.focus();
      }
      const focused = document.activeElement === el || el.contains(document.activeElement);
      if (!focused) {
        return {
          error: `元素 [${ref}] 无法获得焦点（可能被遮挡或已禁用），未输入任何内容；当前焦点在 ${describeEl(document.activeElement || document.body)}`,
        };
      }

      let expected;
      let ceHandled = false; // 富文本编辑器已自行处理事件时，跳过下方的共享事件派发
      let matchMode = "input";
      if (el.isContentEditable) {
        const sel = window.getSelection();
        const range = document.createRange();
        range.selectNodeContents(el);
        sel.removeAllRanges();
        sel.addRange(range);
        if (clear === false) sel.collapseToEnd();
        // execCommand 走编辑器自己的 beforeinput 管线（ProseMirror/Slate 依赖它
        // 同步内部 model；直接改 innerText 会破坏内部结构，导致字数统计/提交异常）
        let inserted = false;
        try { inserted = document.execCommand("insertText", false, text); } catch (e) { inserted = false; }
        if (inserted) {
          ceHandled = true;
        } else {
          // 兜底：选区直接插入文本节点 + 手工派发事件
          const r = sel.rangeCount ? sel.getRangeAt(0) : null;
          if (r) {
            r.deleteContents();
            const node = document.createTextNode(text);
            r.insertNode(node);
            r.setStartAfter(node);
            r.collapse(true);
            sel.removeAllRanges();
            sel.addRange(r);
          } else {
            el.innerText = (clear === false ? el.innerText : "") + text;
          }
        }
        expected = clear === false ? (el.innerText || "") : text;
      } else if (el.tagName === "SELECT") {
        const wanted = String(text).trim().toLocaleLowerCase();
        const options = Array.from(el.options);
        let opt = options.find((o) => o.value === text || o.textContent.trim() === text);
        if (!opt) opt = options.find((o) => o.textContent.trim().toLocaleLowerCase() === wanted);
        if (!opt) {
          const fuzzy = options.filter((o) => {
            const label = o.textContent.trim().toLocaleLowerCase();
            return label.includes(wanted) || wanted.includes(label);
          });
          if (fuzzy.length === 1) {
            opt = fuzzy[0];
            matchMode = "unique_fuzzy";
          } else if (fuzzy.length > 1) {
            return {
              error: `下拉框中「${text}」有多个模糊匹配，不能替用户猜测`,
              candidates: fuzzy.slice(0, 20).map((o) => short(o.textContent.trim(), 80)),
              next: "根据用户掌握的业务名称选择精确候选；关键信息不足时询问用户。",
            };
          }
        }
        if (!opt) {
          return {
            error: `下拉框没有找到「${text}」`,
            candidates: options.slice(0, 20).map((o) => short(o.textContent.trim(), 80)),
            next: options.length > 20 ? "选项较多；可缩小关键词或用 browser_evaluate 检查完整 options。" : "核对目标文字后重试。",
          };
        }
        el.value = opt.value;
        expected = opt.value;
        if (matchMode === "input") matchMode = "exact";
      } else {
        try {
          expected = clear === false ? (el.value || "") + text : text;
          const proto = el.tagName === "TEXTAREA" ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
          const desc = Object.getOwnPropertyDescriptor(proto, "value");
          if (desc && desc.set) desc.set.call(el, expected);
          else el.value = expected;
        } catch (e) {
          return { error: `元素 [${ref}] 不是可输入控件` };
        }
      }
      if (!ceHandled) {
        el.dispatchEvent(new InputEvent("beforeinput", { bubbles: true, inputType: "insertText", data: text }));
        el.dispatchEvent(new Event("input", { bubbles: true }));
        el.dispatchEvent(new Event("change", { bubbles: true }));
      }

      // 读回校验：防止设值被框架重置（受控组件拒绝输入、maxlength 截断等）；
      // 富文本编辑器会归一化首尾空白/换行，比较时容错
      const actual = el.isContentEditable ? el.innerText : el.value;
      const compact = (s) => (s || "").replace(/\s+/g, " ").trim();
      const pass = el.isContentEditable
        ? compact(actual) === compact(expected)
          || compact(actual).endsWith(compact(text))
          || (compact(text).length >= 8 && compact(actual).includes(compact(text)))
        : actual === expected;
      if (!pass) {
        return {
          error: `输入校验失败：期望「${short(expected, 40)}」但实际读回「${short(actual, 40)}」。目标元素：${describeEl(el)}`,
        };
      }
      const isPwd = el.tagName === "INPUT" && el.type === "password";
      return {
        ok: true,
        focused: true,
        value: isPwd ? "（密码已隐藏）" : short(actual, 60),
        target: describeEl(el),
        resolved_from: resolved.adapted ? describeEl(source) : null,
        opened_control: resolved.opened,
        match_mode: matchMode,
        next: resolved.adapted
          ? "已在控件实际编辑面完成输入。重新获取快照观察候选、校验信息或下一步动作；不要仅凭输入成功推断已完成选择。"
          : undefined,
      };
    },

    scroll: async (direction, amount, ref) => {
      const d = amount || 600;
      const delta = { up: [0, -d], down: [0, d], left: [-d, 0], right: [d, 0] }[direction] || [0, d];
      const [dx, dy] = delta;
      const horizontal = dx !== 0;

      // 定位滚动容器：ref 指定 → 最近可滚动祖先；否则页面级最佳容器
      let container = null;
      const anchor = ref != null ? getRef(ref) : null;
      if (ref != null && !anchor) return { error: `元素 [${ref}] 不存在或已失效，请重新获取快照` };
      if (anchor) container = nearestScrollable(anchor, horizontal) || relatedScrollable(anchor, horizontal);
      if (!container && !anchor) container = activePopupScrollable(horizontal) || bestScrollable(horizontal);

      const posBefore = container
        ? { x: container.scrollLeft, y: container.scrollTop }
        : { x: window.scrollX, y: window.scrollY };
      const cur = () =>
        container
          ? { x: container.scrollLeft, y: container.scrollTop }
          : { x: window.scrollX, y: window.scrollY };
      const movedOf = (p) => p.x !== posBefore.x || p.y !== posBefore.y;

      let method = "scrollBy";
      if (container) container.scrollBy({ left: dx, top: dy, behavior: "instant" });
      else window.scrollBy(dx, dy);
      let pos = cur();
      let moved = movedOf(pos);

      // 兜底：容器监听 wheel 的自定义滚动（部分 RN Web 列表/轮播只吃 wheel）
      if (!moved) {
        const host = container || document.documentElement;
        const r = host.getBoundingClientRect();
        host.dispatchEvent(new WheelEvent("wheel", {
          bubbles: true, cancelable: true,
          clientX: r.left + r.width / 2, clientY: r.top + r.height / 2,
          deltaX: dx, deltaY: dy, deltaMode: 0,
        }));
        await sleep(160);
        pos = cur();
        moved = movedOf(pos);
        if (moved) method = "wheel";
      }

      const out = {
        ok: true,
        moved,
        method,
        container: describeContainer(container),
        before: horizontal ? posBefore.x : posBefore.y,
        after: horizontal ? pos.x : pos.y,
      };
      if (!moved) {
        out.hint = "滚动未生效：该方向可能已到底/到顶，或内容用变换动画滚动（如轮播）。可用 ref 指定列表内元素精确定位容器，或用 browser_evaluate 检查容器 scrollHeight";
      }
      return out;
    },

    info: () => ({ url: location.href, title: document.title }),
  };
})();
