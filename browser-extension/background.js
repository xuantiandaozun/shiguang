// 拾光浏览器桥：连接桌面端本地 WebSocket 服务，执行浏览器操作命令。
const WS_URL = "ws://127.0.0.1:17893";
let ws = null;
let reconnectTimer = null;

function ensureConnected() {
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  connect();
}

function connect() {
  try {
    ws = new WebSocket(WS_URL);
  } catch {
    scheduleReconnect();
    return;
  }
  ws.onopen = () => console.log("[拾光] 已连接桌面助手");
  ws.onclose = () => {
    ws = null;
    scheduleReconnect();
  };
  ws.onerror = () => {
    try { ws && ws.close(); } catch { /* ignore */ }
  };
  ws.onmessage = (ev) => handleMessage(ev.data);
}

function scheduleReconnect() {
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    ensureConnected();
  }, 5000);
}

// MV3 service worker 空闲约 30 秒会被终止，定时器也会随之消失。
// alarms 是唯一能在 SW 休眠后重新唤醒它的机制——桌面端不是随时运行的，
// 唤醒时补一次重连，保证拾光启动后不用手动刷新扩展也能自动连上。
chrome.alarms.create("dh-keepalive", { periodInMinutes: 0.5 });
chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === "dh-keepalive") ensureConnected();
});

// 浏览器启动/扩展装好时唤醒 SW 并立即连接：
// 桌面端拉起默认浏览器后只等约 15 秒，光靠 alarms（30s 周期）可能赶不上
chrome.runtime.onStartup.addListener(() => ensureConnected());
chrome.runtime.onInstalled.addListener(() => ensureConnected());

function send(obj) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(obj));
}

// MV3 service worker 保活：周期 ping（桌面端回 pong，WebSocket 活动会重置 SW 空闲计时）
setInterval(() => {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send("ping");
}, 20000);

connect();

// ---- 内嵌 CDP 通道（chrome.debugger）----
// 用于 CSP 受限站点（如 X）：Runtime.evaluate 走 DevTools 特权，豁免页面 CSP。
// 代价：附着期间 Chrome 顶部显示「正在调试此浏览器」提示条。
// 会话按标签保持，避免每次调用 attach/detach 导致提示条闪烁；SW 重启后 Set 丢失，
// 重新 attach 时报 "already attached" 视为成功即可。
const attachedTabs = new Set();
chrome.debugger.onDetach.addListener((source) => {
  if (source && source.tabId) attachedTabs.delete(source.tabId);
});

async function ensureDebugger(tabId) {
  if (attachedTabs.has(tabId)) return;
  try {
    await chrome.debugger.attach({ tabId }, "1.3");
  } catch (e) {
    if (!/already attached/i.test(String((e && e.message) || e))) throw e;
  }
  attachedTabs.add(tabId);
}

// 通道能力型错误：本通道做不到、换通道才可能做到（CSP 拦截 / 注入被拒 / 调试器无法附着）。
// 业务错误（元素不存在、没有活动标签页等）不在此列，不做降级。
function isChannelError(e) {
  const m = String((e && e.message) || e).toLowerCase();
  return /evalerror|unsafe-eval|refused to evaluate|content security policy|cannot access|cannot script|cannot be scripted|cannot attach|not allowed/.test(m);
}

async function debuggerEval(tabId, expression) {
  await ensureDebugger(tabId);
  const r = await chrome.debugger.sendCommand({ tabId }, "Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: true,
  });
  if (r.exceptionDetails) {
    const d = r.exceptionDetails;
    const desc = (d.exception && (d.exception.description || d.exception.value)) || d.text;
    throw new Error(desc ? String(desc).split("\n")[0] : "页面 JS 执行异常");
  }
  return r.result && r.result.value !== undefined ? r.result.value : null;
}

// debugger / CDP 注入源：Readability + page-api（与桌面端 cdp.rs 同构）
let pageApiSrc = null;
async function getPageApiSrc() {
  if (!pageApiSrc) {
    const [readability, pageApi] = await Promise.all([
      fetch(chrome.runtime.getURL("readability.js")).then((r) => r.text()),
      fetch(chrome.runtime.getURL("page-api.js")).then((r) => r.text()),
    ]);
    pageApiSrc = readability + "\n" + pageApi;
  }
  return pageApiSrc;
}

async function handleMessage(raw) {
  let req;
  try {
    req = JSON.parse(raw);
  } catch {
    return;
  }
  try {
    const data = await handle(req.action, req.params || {});
    send({ id: req.id, ok: true, data });
  } catch (e) {
    send({ id: req.id, ok: false, error: String((e && e.message) || e) });
  }
}

async function activeTab() {
  const tabs = await chrome.tabs.query({ active: true, currentWindow: true });
  const tab = tabs && tabs[0];
  if (!tab) throw new Error("没有活动标签页");
  return tab;
}

function waitLoad(tabId, ms) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      chrome.tabs.onUpdated.removeListener(listener);
      reject(new Error("页面加载超时"));
    }, ms);
    function listener(id, info) {
      if (id === tabId && info.status === "complete") {
        clearTimeout(timer);
        chrome.tabs.onUpdated.removeListener(listener);
        resolve();
      }
    }
    chrome.tabs.onUpdated.addListener(listener);
  });
}

// 在页面隔离世界中调用 page-api.js 注入的 window.__dh 方法
const CALLS = {
  snapshot: (maxChars, scope) => window.__dh.snapshot(maxChars, scope),
  read: (maxChars) => window.__dh.read(maxChars),
  click: (ref) => window.__dh.click(ref),
  type: (ref, text, clear) => window.__dh.type(ref, text, clear),
  scroll: (dir, amount, ref) => window.__dh.scroll(dir, amount, ref),
  info: () => window.__dh.info(),
};

async function runInTab(tabId, action, args) {
  await chrome.scripting.executeScript({
    target: { tabId },
    files: ["readability.js", "page-api.js"],
  });
  const [r] = await chrome.scripting.executeScript({ target: { tabId }, func: CALLS[action], args });
  if (r && r.error) throw new Error(r.error.message || String(r.error));
  const res = r ? r.result : null;
  if (res && typeof res === "object" && typeof res.error === "string") throw new Error(res.error);
  return res;
}

// debugger 通道：把 page-api.js 源码注入主世界后调用 window.__dh（与桌面端 cdp.rs 同构）
async function runInTabViaDebugger(tabId, action, args) {
  const src = await getPageApiSrc();
  const res = await debuggerEval(tabId, `${src}\n;window.__dh.${action}(...${JSON.stringify(args)});`);
  if (res && typeof res === "object" && typeof res.error === "string") throw new Error(res.error);
  return res;
}

// 默认隔离世界执行；命中通道能力型错误（CSP/注入被拒）时自动降级到 debugger 通道
async function runInTabAuto(tabId, action, args) {
  try {
    return await runInTab(tabId, action, args);
  } catch (e) {
    if (!isChannelError(e)) throw e;
    const res = await runInTabViaDebugger(tabId, action, args);
    if (res && typeof res === "object") res.via = "debugger";
    return res;
  }
}

// 在主世界执行用户 JS（可访问页面变量）
async function evalInPage(tabId, expression) {
  const [r] = await chrome.scripting.executeScript({
    target: { tabId },
    world: "MAIN",
    func: (expr) => {
      try {
        const v = eval(expr);
        return { ok: true, value: v === undefined ? null : v };
      } catch (e) {
        return { ok: false, error: String(e) };
      }
    },
    args: [expression],
  });
  if (r && r.error) throw new Error(r.error.message || String(r.error));
  const res = r ? r.result : null;
  if (res && res.ok === false) throw new Error(res.error);
  return res ? res.value : null;
}

async function handle(action, p) {
  switch (action) {
    case "navigate": {
      if (!p.url) throw new Error("缺少 url");
      let tab;
      if (p.new_tab) {
        tab = await chrome.tabs.create({ url: p.url, active: true });
      } else {
        tab = await activeTab();
        await chrome.tabs.update(tab.id, { url: p.url });
      }
      await waitLoad(tab.id, 15000).catch(() => {});
      return runInTabAuto(tab.id, "info", []);
    }
    case "snapshot": {
      const t = await activeTab();
      const snapshot = await runInTabAuto(t.id, "snapshot", [p.max_chars || 8000, p.scope ?? null]);
      const info = await runInTabAuto(t.id, "info", []);
      const out = { title: info.title, url: info.url, snapshot };
      if (info.via === "debugger") out.via = "debugger";
      return out;
    }
    case "read": {
      const t = await activeTab();
      const article = await runInTabAuto(t.id, "read", [p.max_chars || 12000]);
      const info = await runInTabAuto(t.id, "info", []);
      const out = {
        url: info.url,
        title: article.title || info.title || "",
        byline: article.byline || "",
        siteName: article.siteName || "",
        excerpt: article.excerpt || "",
        length: article.length,
        returned_chars: article.returned_chars,
        truncated: !!article.truncated,
        content: article.content,
      };
      if (article.hint) out.hint = article.hint;
      if (article.via === "debugger" || info.via === "debugger") out.via = "debugger";
      return out;
    }
    case "click": {
      const t = await activeTab();
      return runInTabAuto(t.id, "click", [p.ref]);
    }
    case "type": {
      const t = await activeTab();
      return runInTabAuto(t.id, "type", [p.ref, p.text ?? "", p.clear !== false]);
    }
    case "scroll": {
      const t = await activeTab();
      return runInTabAuto(t.id, "scroll", [p.direction || "down", p.amount || 600, p.ref ?? null]);
    }
    case "tabs": {
      const tabs = await chrome.tabs.query({});
      return {
        tabs: tabs.map((t) => ({ id: t.id, title: t.title || "", url: t.url || "", active: !!t.active })),
      };
    }
    case "activate_tab": {
      await chrome.tabs.update(p.id, { active: true });
      const t = await chrome.tabs.get(p.id);
      await chrome.windows.update(t.windowId, { focused: true });
      return { ok: true };
    }
    case "screenshot": {
      const t = await activeTab();
      const dataUrl = await chrome.tabs.captureVisibleTab(t.windowId, { format: "png" });
      return { png_base64: String(dataUrl).split(",")[1] || "" };
    }
    case "evaluate": {
      const t = await activeTab();
      const expr = p.expression || "";
      const ch = p.channel || "auto";
      // channel=debugger：跳过 scripting 直接走 CDP；channel=extension：禁止降级
      if (ch === "debugger") {
        return { result: await debuggerEval(t.id, expr), via: "debugger" };
      }
      try {
        return { result: await evalInPage(t.id, expr) };
      } catch (e) {
        if (ch === "extension" || !isChannelError(e)) throw e;
        return { result: await debuggerEval(t.id, expr), via: "debugger" };
      }
    }
    case "info": {
      const t = await activeTab();
      return runInTabAuto(t.id, "info", []);
    }
    default:
      throw new Error("未知动作: " + action);
  }
}
