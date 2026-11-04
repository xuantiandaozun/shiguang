// 拾光浏览器运行时（扩展后台侧）。
// 把点击/输入/按键/上传变成真实输入事件（CDP Input.*，isTrusted=true），
// 每个动作后等待页面安静，并汇报实际效果（URL、弹窗、提示、请求、新标签）。
// 与运行环境无关，只依赖注入的 io：
//   io.send(method, params)   CDP 命令；扩展里是 chrome.debugger.sendCommand
//   io.page(method, arg)      调用页面侧 window.__dh.rt[method]（自动注入页面脚本）
//   io.newTabs(sinceMs)       可选：动作期间新开的标签
// CDP 事件通过 runtime.onEvent(method, params) 喂入。
(function (root) {
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const clamp = (n, lo, hi) => Math.max(lo, Math.min(hi, Number.isFinite(Number(n)) ? Number(n) : lo));

  const MODIFIER_BITS = { alt: 1, control: 2, ctrl: 2, meta: 4, cmd: 4, command: 4, shift: 8 };
  const NAMED_KEYS = {
    enter: ["Enter", "Enter", 13, "\r"], tab: ["Tab", "Tab", 9, ""], escape: ["Escape", "Escape", 27, ""],
    esc: ["Escape", "Escape", 27, ""], backspace: ["Backspace", "Backspace", 8, ""], delete: ["Delete", "Delete", 46, ""],
    space: [" ", "Space", 32, " "], arrowup: ["ArrowUp", "ArrowUp", 38, ""], arrowdown: ["ArrowDown", "ArrowDown", 40, ""],
    arrowleft: ["ArrowLeft", "ArrowLeft", 37, ""], arrowright: ["ArrowRight", "ArrowRight", 39, ""],
    home: ["Home", "Home", 36, ""], end: ["End", "End", 35, ""], pageup: ["PageUp", "PageUp", 33, ""], pagedown: ["PageDown", "PageDown", 34, ""],
  };

  function parseKey(spec) {
    const parts = String(spec || "").split("+").map((s) => s.trim()).filter(Boolean);
    if (!parts.length) throw new Error("缺少按键");
    const name = parts.pop();
    let modifiers = 0;
    for (const m of parts) {
      const bit = MODIFIER_BITS[m.toLowerCase()];
      if (!bit) throw new Error(`未知修饰键 ${m}`);
      modifiers |= bit;
    }
    let key, code, keyCode, text;
    const named = NAMED_KEYS[name.toLowerCase()];
    if (named) {
      [key, code, keyCode, text] = named;
    } else if (/^[a-z]$/i.test(name)) {
      const upper = name.toUpperCase();
      key = modifiers & 8 ? upper : name.toLowerCase();
      code = `Key${upper}`; keyCode = upper.charCodeAt(0); text = key;
    } else if (/^[0-9]$/.test(name)) {
      key = name; code = `Digit${name}`; keyCode = name.charCodeAt(0); text = name;
    } else if (/^F([1-9]|1[0-2])$/i.test(name)) {
      key = name.toUpperCase(); code = key; keyCode = 111 + Number(name.slice(1)); text = "";
    } else if ([...name].length === 1) {
      key = name; code = ""; keyCode = 0; text = name;
    } else {
      throw new Error(`未知按键 ${name}`);
    }
    // 带 Ctrl/Alt/Meta 的组合键不产生文字输入（否则 Ctrl+A 会输入字母 a）。
    if (modifiers & 7) text = "";
    return { key, code, keyCode, text, modifiers };
  }

  async function pressKey(send, spec) {
    const k = parseKey(spec);
    const base = { modifiers: k.modifiers, key: k.key, code: k.code, windowsVirtualKeyCode: k.keyCode, nativeVirtualKeyCode: k.keyCode };
    await send("Input.dispatchKeyEvent", { ...base, type: k.text ? "keyDown" : "rawKeyDown", ...(k.text ? { text: k.text, unmodifiedText: k.text } : {}) });
    await send("Input.dispatchKeyEvent", { ...base, type: "keyUp" });
  }

  async function mouse(send, x, y, kind = "click") {
    await send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y, button: "none", buttons: 0 });
    if (kind === "hover") return;
    const clicks = kind === "double_click" ? 2 : 1;
    for (let i = 1; i <= clicks; i++) {
      await send("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", buttons: 1, clickCount: i });
      await send("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", buttons: 0, clickCount: i });
    }
  }

  const shortUrl = (raw) => {
    try {
      const u = new URL(raw);
      const s = u.origin + u.pathname;
      return s.length > 120 ? s.slice(0, 120) + "…" : s;
    } catch {
      return String(raw || "").slice(0, 120);
    }
  };
  const added = (from, to) => (to || []).filter((t) => !(from || []).includes(t));

  function diffObservation(before, after) {
    if (!after) return { page_unreadable: true };
    const effect = {};
    if (!before || after.url !== before.url) effect.url = after.url;
    if (before && after.title !== before.title) effect.title = after.title;
    const opened = added(before && before.dialogs, after.dialogs);
    const closed = added(after.dialogs, before && before.dialogs);
    const messages = added(before && before.messages, after.messages);
    const errors = added(before && before.errors, after.errors);
    if (opened.length) effect.dialogs_opened = opened;
    if (closed.length) effect.dialogs_closed = closed;
    if (messages.length) effect.messages = messages;
    if (errors.length) effect.errors = errors;
    if (before && after.focus !== before.focus) effect.focus = after.focus;
    effect.dom_changed = !before || after.mut !== before.mut;
    return effect;
  }

  function fileError(e) {
    const m = String((e && e.message) || e);
    if (/not allowed|permission/i.test(m)) {
      return `浏览器拒绝扩展读取本地文件：请在 chrome://extensions 打开「拾光浏览器桥」详情，开启「允许访问文件网址」后重试（原错误：${m}）`;
    }
    return `设置上传文件失败：${m}`;
  }

  class RuntimeError extends Error {
    constructor(message, code) { super(message); this.code = code; }
  }

  const ENGINE_ACTIONS = ["click", "double_click", "hover", "type", "fill", "press", "upload", "select", "set_checked", "dialog"];

  class TabRuntime {
    constructor(io, options = {}) {
      this.io = io;
      this.now = options.now || (() => Date.now());
      this.enabled = false;
      this.requests = new Map();
      this.log = [];
      this.dialog = null;
      this.nav = { seq: 0, loading: false };
      this.mainFrameId = null;
      this.waiters = [];
      this.gate = null;
      this.openGate = null;
      // 输入命令用的发送函数：与"出现原生对话框"赛跑（见 input()）。
      this.send = (method, params) => this.input(method, params);
    }

    // 点击/按键触发 alert/confirm 时，CDP 的输入命令要等对话框关闭才返回；
    // 对话框一出现就不再等待该命令，让引擎能汇报并处理对话框。
    input(method, params) {
      const sent = this.io.send(method, params);
      sent.catch(() => {});
      return Promise.race([sent, this.dialogGate()]);
    }

    dialogGate() {
      if (this.dialog) return Promise.resolve();
      if (!this.gate) this.gate = new Promise((resolve) => { this.openGate = resolve; });
      return this.gate;
    }

    reset() {
      this.enabled = false;
      this.requests.clear();
      this.dialog = null;
      this.nav.loading = false;
    }

    async enable() {
      if (this.enabled) return;
      await this.io.send("Page.enable", {});
      await this.io.send("Network.enable", {}).catch(() => {});
      try {
        const tree = await this.io.send("Page.getFrameTree", {});
        this.mainFrameId = tree && tree.frameTree && tree.frameTree.frame && tree.frameTree.frame.id;
      } catch { /* 部分页面取不到，按无父帧判断主帧 */ }
      this.enabled = true;
    }

    isMainFrame(frameId, frame) {
      if (frame) return !frame.parentId;
      return !this.mainFrameId || frameId === this.mainFrameId;
    }

    onEvent(method, p = {}) {
      switch (method) {
        case "Network.requestWillBeSent":
          if (["XHR", "Fetch", "Document"].includes(p.type) && p.request) {
            const r = { method: p.request.method, url: p.request.url, type: p.type, started: this.now(), status: null, failed: false, done: false };
            this.requests.set(p.requestId, r);
            this.log.push(r);
            if (this.log.length > 200) this.log.shift();
          }
          break;
        case "Network.responseReceived": {
          const r = this.requests.get(p.requestId);
          if (r && p.response) r.status = p.response.status;
          break;
        }
        case "Network.loadingFinished":
        case "Network.loadingFailed": {
          const r = this.requests.get(p.requestId);
          if (r) {
            r.done = true;
            r.failed = method === "Network.loadingFailed" && !p.canceled;
            this.requests.delete(p.requestId);
          }
          break;
        }
        case "Page.javascriptDialogOpening":
          this.dialog = { type: p.type, message: String(p.message || "").slice(0, 300) };
          if (this.openGate) { this.openGate(); this.gate = null; this.openGate = null; }
          break;
        case "Page.javascriptDialogClosed":
          this.dialog = null;
          break;
        case "Page.frameStartedLoading":
          if (this.isMainFrame(p.frameId)) this.nav.loading = true;
          break;
        case "Page.frameNavigated":
          if (p.frame && this.isMainFrame(p.frame.id, p.frame)) {
            this.mainFrameId = p.frame.id;
            this.nav.seq += 1;
          }
          break;
        case "Page.frameStoppedLoading":
          if (this.isMainFrame(p.frameId)) this.nav.loading = false;
          break;
        case "Page.loadEventFired":
          this.nav.loading = false;
          break;
        default:
          break;
      }
      for (const w of [...this.waiters]) {
        if (w.method === method) {
          this.waiters.splice(this.waiters.indexOf(w), 1);
          w.resolve(p);
        }
      }
    }

    waitFor(method, ms) {
      return new Promise((resolve) => {
        const waiter = { method, resolve };
        this.waiters.push(waiter);
        setTimeout(() => {
          const i = this.waiters.indexOf(waiter);
          if (i >= 0) { this.waiters.splice(i, 1); resolve(null); }
        }, ms);
      });
    }

    // 目标有两种给法：DOM 定位（target）或按画面定位得到的视口坐标（point）。
    async locate(step, intent) {
      const point = step.point;
      if (point && typeof point === "object") {
        const r = await this.page("describePoint", { x: Number(point.x), y: Number(point.y) });
        return r.error ? r : { ...r, x: Number(point.x), y: Number(point.y), by_point: true };
      }
      return this.page("prepare", { target: step.target, intent });
    }

    // 截取当前视口给视觉模型。按像素比缩放，让最长边不超过 maxSide（Jev 要求 280–1568px）。
    async capture({ max_side: maxSide = 1500 } = {}) {
      await this.enable();
      if (this.dialog) return { error: "页面有未处理的原生对话框", code: "native_dialog" };
      const vp = await this.page("viewport");
      const scale = Math.min(1, maxSide / (Math.max(vp.w, vp.h) * (vp.dpr || 1)));
      const shot = await this.io.send("Page.captureScreenshot", {
        format: "png", captureBeyondViewport: false,
        clip: { x: vp.scrollX, y: vp.scrollY, width: vp.w, height: vp.h, scale },
      });
      return { data: shot.data, format: "png", width: vp.w, height: vp.h, scale, dpr: vp.dpr };
    }

    // 原生 alert/confirm 打开时页面 JS 被阻塞，任何页面调用都会挂住，必须先拦下。
    async page(method, arg, ms = 5000) {
      if (this.dialog) throw new RuntimeError(`页面弹出了原生对话框（${this.dialog.type}）：${this.dialog.message}`, "native_dialog");
      let timer;
      const timeout = new Promise((_, reject) => { timer = setTimeout(() => reject(new RuntimeError("页面无响应", "page_timeout")), ms); });
      try {
        return await Promise.race([this.io.page(method, arg), timeout]);
      } finally {
        clearTimeout(timer);
      }
    }

    // 超过 4 秒的请求视为长连接/轮询，不阻塞"已安静"判断。
    blockingRequests() {
      const now = this.now();
      let n = 0;
      for (const r of this.requests.values()) if (now - r.started < 4000) n += 1;
      return n;
    }

    async settle(maxMs) {
      const started = this.now();
      const deadline = started + maxMs;
      await sleep(Math.min(80, maxMs));
      for (;;) {
        if (this.dialog) return { settled: true, waited_ms: this.now() - started };
        let pageQuiet = false;
        if (!this.nav.loading) {
          try {
            const q = await this.page("quiet", null, 1500);
            pageQuiet = q.age >= 300 && q.ready !== "loading";
          } catch { pageQuiet = false; }
        }
        const pending = this.blockingRequests();
        if (pageQuiet && pending === 0) return { settled: true, waited_ms: this.now() - started };
        if (this.now() >= deadline) return { settled: false, waited_ms: this.now() - started, pending_requests: pending, navigating: this.nav.loading };
        await sleep(120);
      }
    }

    requestsSince(t0) {
      return this.log.filter((r) => r.started >= t0).slice(-8).map((r) => ({
        method: r.method, url: shortUrl(r.url), type: r.type,
        status: r.status != null ? r.status : r.failed ? "failed" : r.done ? null : "pending",
      }));
    }

    async observeSafe() {
      if (this.dialog) return null;
      try { return await this.page("observe", null, 3000); } catch { return null; }
    }

    async checkExpect(expect) {
      const end = this.now() + clamp(expect.timeout_ms == null ? 5000 : expect.timeout_ms, 0, 15000);
      const cond = { url_contains: expect.url_contains, text: expect.text, gone_text: expect.gone_text };
      let last = { met: false };
      for (;;) {
        try { last = await this.page("expect", cond, 3000); } catch (e) { last = { met: false, error: String(e.message || e) }; }
        if (last.met || this.dialog || this.now() >= end) return last;
        await sleep(250);
      }
    }

    async act(rawStep) {
      const step = { ...rawStep, action: rawStep.action === "fill" ? "type" : rawStep.action };
      if (!ENGINE_ACTIONS.includes(step.action)) return { action: step.action, error: `未知动作 ${step.action}`, code: "invalid_request" };
      await this.enable();
      if (this.dialog && step.action !== "dialog") {
        return { action: step.action, error: `页面有未处理的原生对话框（${this.dialog.type}）：${this.dialog.message}`, code: "native_dialog", next: "先用 action=dialog（accept=true/false）处理。" };
      }
      const t0 = this.now();
      const before = step.action === "dialog" ? null : await this.observeSafe();
      const navSeq = this.nav.seq;
      let res;
      try {
        res = await this.perform(step);
      } catch (e) {
        // 动作途中弹出原生对话框说明输入已生效，需要汇报效果；其它异常发生在输入之前。
        const code = (e && e.code) || "runtime_error";
        res = { error: String((e && e.message) || e), code, performed: code === "native_dialog" };
      }
      const { performed, verifyAfter, ...result } = res;
      const out = { action: step.action, ...result };
      if (!performed) return out;
      const settle = await this.settle(clamp(step.settle_ms == null ? 3000 : step.settle_ms, 0, 10000));
      if (verifyAfter && !this.dialog) {
        try { Object.assign(out, await verifyAfter()); } catch (e) { out.verified = false; out.verify_error = String(e.message || e); }
      }
      const after = await this.observeSafe();
      const effect = diffObservation(before, after);
      if (this.nav.seq !== navSeq) effect.navigated = true;
      const requests = this.requestsSince(t0);
      if (requests.length) effect.requests = requests;
      if (this.io.newTabs) {
        const tabs = await this.io.newTabs(t0).catch(() => []);
        if (tabs && tabs.length) effect.new_tabs = tabs;
      }
      if (this.dialog) effect.native_dialog = this.dialog;
      effect.settled = settle.settled;
      effect.waited_ms = settle.waited_ms;
      if (!settle.settled) effect.note = settle.navigating ? "页面仍在加载" : `等待 ${settle.waited_ms}ms 后页面仍在变化`;
      out.effect = effect;
      out.changed = !!(effect.url || effect.navigated || effect.dom_changed || effect.dialogs_opened || effect.dialogs_closed || effect.messages || effect.new_tabs || effect.native_dialog);
      if (step.expect && typeof step.expect === "object") {
        out.expect = await this.checkExpect(step.expect);
        if (!out.expect.met && !out.error) { out.ok = false; out.error = "动作已执行，但预期结果没有出现"; out.code = "expect_unmet"; }
      }
      if (effect.native_dialog) out.next = "页面弹出了原生对话框，用 action=dialog（accept=true/false）处理后再继续。";
      else if (!out.changed && !out.error && step.action === "click") out.hint = "点击后页面没有可观察变化：先核对目标是否正确，不要原样重试。";
      return out;
    }

    async perform(step) {
      const send = this.send;
      switch (step.action) {
        case "click":
        case "double_click":
        case "hover": {
          const prep = await this.locate(step, "click");
          if (prep.error) return prep;
          await mouse(send, prep.x, prep.y, step.action);
          return { performed: true, ok: true, target: prep.target, ref: prep.ref, ...(prep.relocated ? { relocated: true } : {}) };
        }
        case "type":
          return this.typeText(step);
        case "press": {
          parseKey(step.key);
          if (step.target || step.point) {
            const prep = await this.locate(step, "click");
            if (prep.error) return prep;
            if (!prep.focused) await mouse(send, prep.x, prep.y, "click");
          }
          await pressKey(send, step.key);
          return { performed: true, ok: true, key: step.key };
        }
        case "upload":
          return this.upload(step);
        case "set_checked":
          return this.setChecked(step);
        case "select":
          return this.select(step);
        case "dialog": {
          if (!this.dialog) return { error: "当前没有原生对话框", code: "no_dialog" };
          const info = this.dialog;
          await this.io.send("Page.handleJavaScriptDialog", { accept: step.accept !== false, ...(step.text != null ? { promptText: String(step.text) } : {}) });
          this.dialog = null;
          return { performed: true, ok: true, handled: info, accepted: step.accept !== false };
        }
        default:
          return { error: `未知动作 ${step.action}`, code: "invalid_request" };
      }
    }

    async typeText(step) {
      if (typeof step.text !== "string") return { error: "type 需要 text", code: "invalid_request" };
      const send = this.send;
      const prep = await this.locate(step, "type");
      if (prep.error) return prep;
      if (prep.kind === "select") {
        const r = await this.page("legacyAction", { action: "select", target: { ref: prep.ref }, text: step.text });
        return { ...r, performed: !r.error, via: "native_select" };
      }
      let performed = false;
      if (!prep.focused) {
        await mouse(send, prep.x, prep.y, "click");
        performed = true;
        await sleep(60);
      }
      let fs = await this.page("focusState");
      // 一些组件点击后才异步挂载真正的输入框。
      if (!fs.editable) { await sleep(300); fs = await this.page("focusState"); }
      if (!fs.editable) return { performed, error: `点击后没有获得可输入焦点（当前焦点：${fs.focus}）`, code: "not_editable", target: prep.target };
      if (fs.readonly) return { performed, error: "获得焦点的控件是只读的", code: "readonly", target: prep.target };
      const clear = step.clear !== false;
      await this.page("selectAll", { clear });
      let text = step.text;
      let note;
      if (fs.kind === "input" && /\n/.test(text)) {
        text = text.replace(/\s*\n+\s*/g, " ");
        note = "单行输入框不支持换行，已把换行替换为空格";
      }
      // 富文本里回车可能直接发送消息（聊天框），默认用 Shift+Enter 换行；
      // 文章编辑器需要分段时显式传 newline=enter。
      const newline = step.newline || (fs.kind === "contenteditable" ? "shift_enter" : "insert");
      const lines = text.split("\n");
      if (clear && !text) await pressKey(send, "Backspace");
      for (let i = 0; i < lines.length; i++) {
        if (lines[i]) await send("Input.insertText", { text: lines[i] });
        if (i < lines.length - 1) {
          if (fs.kind === "textarea" || newline === "insert") await send("Input.insertText", { text: "\n" });
          else await pressKey(send, newline === "enter" ? "Enter" : "Shift+Enter");
        }
      }
      const v = await this.page("verifyText", { text, clear });
      return {
        performed: true, ok: !!v.verified, verified: !!v.verified, kind: fs.kind, target: prep.target, ref: prep.ref, value: v.preview,
        ...(v.verified ? {} : { error: v.reason || "输入后读回不一致", code: "unverified", missing: v.missing }),
        ...(note ? { note } : {}),
      };
    }

    async upload(step) {
      const files = step.files;
      if (!Array.isArray(files) || !files.length) return { error: "upload 需要 files（本机绝对路径数组）", code: "invalid_request" };
      const send = this.io.send;
      const input = this.send;
      const prep = await this.locate(step, "upload");
      if (prep.error) return prep;
      try {
        if (prep.file_input) {
          const evaluated = await send("Runtime.evaluate", { expression: "window.__dh.rt.currentTarget()", returnByValue: false });
          const objectId = evaluated && evaluated.result && evaluated.result.objectId;
          if (!objectId) return { error: "无法取得文件输入框", code: "upload_failed" };
          await send("DOM.setFileInputFiles", { files, objectId });
        } else {
          await send("Page.setInterceptFileChooserDialog", { enabled: true });
          try {
            const opened = this.waitFor("Page.fileChooserOpened", 4000);
            await mouse(input, prep.x, prep.y, "click");
            const chooser = await opened;
            if (!chooser) return { performed: true, error: "点击后没有出现文件选择框，目标可能不是上传入口", code: "no_file_chooser", target: prep.target };
            await send("DOM.setFileInputFiles", { files, backendNodeId: chooser.backendNodeId });
          } finally {
            await send("Page.setInterceptFileChooserDialog", { enabled: false }).catch(() => {});
          }
        }
      } catch (e) {
        return { performed: true, error: fileError(e), code: "upload_failed", target: prep.target };
      }
      return { performed: true, ok: true, files: files.length, target: prep.target };
    }

    async setChecked(step) {
      if (typeof step.checked !== "boolean") return { error: "set_checked 需要布尔 checked", code: "invalid_request" };
      const prep = await this.locate(step, "click");
      if (prep.error) return prep;
      const before = await this.page("targetState", { ref: prep.ref });
      if (before.checked == null) return { error: "目标不支持勾选状态", code: "unsupported", target: prep.target };
      if (before.checked === step.checked) return { ok: true, verified: true, changed: false, checked: before.checked, target: prep.target };
      await mouse(this.send, prep.x, prep.y, "click");
      return {
        performed: true, ok: true, target: prep.target,
        verifyAfter: async () => {
          const after = await this.page("targetState", { ref: prep.ref });
          const ok = after.checked === step.checked;
          return { ok, verified: ok, checked: after.checked, ...(ok ? {} : { error: "勾选状态没有按预期改变", code: "unverified" }) };
        },
      };
    }

    async select(step) {
      if (typeof step.text !== "string" || !step.text.trim()) return { error: "select 需要精确选项 text", code: "invalid_request" };
      const send = this.send;
      const control = await this.locate(step, "click");
      if (control.error) return control;
      if (control.kind === "select") {
        const r = await this.page("legacyAction", { action: "select", target: { ref: control.ref }, text: step.text });
        return { ...r, performed: !r.error, via: "native_select" };
      }
      const state = await this.page("targetState", { ref: control.ref });
      if (state.expanded !== "true") {
        await mouse(send, control.x, control.y, "click");
        await this.settle(1500);
      }
      const optionTarget = { role: "option", name: step.text, ...(step.option_scope ? { scope: step.option_scope } : {}) };
      const option = await this.page("prepare", { target: optionTarget, intent: "click" });
      if (option.error) {
        const reason = option.code === "ambiguous" ? `选项「${step.text}」不唯一，用 option_scope 限定浮层` : `没有找到可见选项「${step.text}」；可搜索的控件先 type 关键词再 select`;
        return { performed: true, error: reason, code: option.code, candidates: option.candidates, opened_control: true };
      }
      await mouse(send, option.x, option.y, "click");
      return {
        performed: true, ok: true, target: control.target, option: option.target,
        verifyAfter: async () => {
          const after = await this.page("targetState", { ref: control.ref });
          const shown = `${after.value || ""} ${after.text || ""}`;
          const ok = shown.includes(step.text.trim());
          return { verified: ok, shown: shown.trim().slice(0, 80), ...(ok ? {} : { hint: "控件上读不到所选文字，用局部快照核验最终值，不要重复提交" }) };
        },
      };
    }

    // 顺序执行多步，遇到第一个失败（含预期未满足、原生对话框）就停，并说明停在哪一步。
    async run(steps) {
      if (!Array.isArray(steps) || !steps.length) return { error: "steps 不能为空", code: "invalid_request" };
      if (steps.length > 20) return { error: "单次最多 20 步", code: "invalid_request" };
      const results = [];
      for (let i = 0; i < steps.length; i++) {
        const r = await this.act(steps[i] || {});
        results.push(r);
        if (r.error) return { ok: false, completed: i, total: steps.length, stopped_at: i, error: `第 ${i + 1} 步失败：${r.error}`, steps: results };
        if (this.dialog) return { ok: false, completed: i + 1, total: steps.length, stopped_at: i + 1, error: "页面弹出了原生对话框", steps: results };
      }
      return { ok: true, completed: steps.length, total: steps.length, steps: results };
    }
  }

  const api = { TabRuntime, parseKey, pressKey, mouse, diffObservation, ENGINE_ACTIONS };
  root.DhRuntime = api;
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(typeof self !== "undefined" ? self : globalThis);
