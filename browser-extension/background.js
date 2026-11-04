// 拾光浏览器桥：连接桌面端本地 WebSocket 服务，执行浏览器操作命令。
// 真实输入引擎（runtime.js）：点击/输入/按键/上传经 chrome.debugger 的 Input.* 发出，
// 动作后等待页面安静并汇报效果；页面侧判定在 page-runtime.js。
importScripts("runtime.js");
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
// 默认优先：Runtime.evaluate 走 DevTools 特权，进页面主世界，能碰到框架内部状态、
// 绕过 CSP，AI 对复杂站点的操控更稳。代价：附着期间 Chrome 顶部显示调试提示条。
// 无法附着时再降级到 scripting 隔离世界。会话按标签保持，避免提示条闪烁；
// SW 重启后 Set 丢失，重新 attach 时报 "already attached" 视为成功即可。
const attachedTabs = new Set();
// 仅在用户/AI 显式开启观察时保存当前标签的脱敏请求目录；不会记录认证头、Cookie 或响应正文。
const networkWatch = new Map();
// 同一标签必须固定执行面，否则 snapshot 的 ref 和 click/evaluate 会落在不同世界。
const tabExec = new Map();
// 每个标签一个真实输入运行时（网络/导航/原生对话框状态随调试会话保存）。
const runtimes = new Map();
// 记录新开的标签，动作后告诉 AI「点击打开了新标签」。
const createdTabs = [];
chrome.tabs.onCreated.addListener((tab) => {
  createdTabs.push({ id: tab.id, opener: tab.openerTabId, url: tab.pendingUrl || tab.url || "", at: Date.now() });
  if (createdTabs.length > 50) createdTabs.shift();
});
chrome.debugger.onDetach.addListener((source) => {
  if (source && source.tabId) {
    attachedTabs.delete(source.tabId);
    tabExec.delete(source.tabId);
    networkWatch.delete(source.tabId);
    const rt = runtimes.get(source.tabId);
    if (rt) rt.reset();
    runtimes.delete(source.tabId);
  }
});

function safeRequestSummary(request) {
  let url;
  try { url = new URL(request.url); } catch { return null; }
  const query_keys = Array.from(url.searchParams.keys()).slice(0, 30);
  let body_keys = [];
  if (request.postData) {
    try {
      const parsed = JSON.parse(request.postData);
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) body_keys = Object.keys(parsed).slice(0, 30);
    } catch { body_keys = ["(非 JSON 请求体，未保存内容)"]; }
  }
  const safePath = url.pathname
    .split("/")
    .map((part) => (/^\d{3,}$/.test(part) || /^[0-9a-f]{8}-[0-9a-f-]{8,}$/i.test(part)) ? ":id" : part)
    .join("/");
  return { method: request.method, url: `${url.origin}${safePath}`, query_keys, body_keys, resource_type: "" };
}

chrome.debugger.onEvent.addListener((source, method, params) => {
  const tabId = source && source.tabId;
  const rt = tabId != null ? runtimes.get(tabId) : null;
  if (rt) rt.onEvent(method, params);
  const watch = tabId != null ? networkWatch.get(tabId) : null;
  if (!watch) return;
  if (method === "Network.requestWillBeSent" && params && params.request) {
    const item = safeRequestSummary(params.request);
    if (!item) return;
    item.resource_type = params.type || "";
    item.request_id = params.requestId;
    item.at = new Date().toISOString();
    watch.items.push(item);
    if (watch.items.length > 80) watch.items.shift();
  }
  if (method === "Network.responseReceived" && params) {
    const item = watch.items.find((x) => x.request_id === params.requestId);
    if (item) { item.status = params.response && params.response.status; item.mime_type = params.response && params.response.mimeType; }
  }
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

async function startNetworkWatch(tabId) {
  await ensureDebugger(tabId);
  await chrome.debugger.sendCommand({ tabId }, "Network.enable", {});
  networkWatch.set(tabId, { started_at: new Date().toISOString(), items: [] });
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

// debugger / CDP 注入源：Readability + page-api + 语义操作 + 运行时页面侧（与桌面端 cdp.rs 同构）
const PAGE_FILES = ["readability.js", "page-api.js", "page-operations.js", "page-runtime.js"];
let pageApiSrc = null;
let pageApiStamp = "";
function stampOf(text) {
  let h = 5381;
  for (let i = 0; i < text.length; i++) h = ((h << 5) + h + text.charCodeAt(i)) | 0;
  return (h >>> 0).toString(36) + "-" + text.length.toString(36);
}
async function getPageApiSrc() {
  if (!pageApiSrc) {
    const parts = await Promise.all(PAGE_FILES.map((f) => fetch(chrome.runtime.getURL(f)).then((r) => r.text())));
    pageApiSrc = parts.join("\n");
    pageApiStamp = stampOf(pageApiSrc);
  }
  return pageApiSrc;
}

// 在主世界执行依赖 window.__dh 的表达式。页面已有同一份脚本时只需一次往返；
// 缺失或版本不同才注入（清掉旧 __dh 保证升级生效，ref 表保留）。
async function callPageApi(tabId, expr) {
  const src = await getPageApiSrc();
  const stamp = JSON.stringify(pageApiStamp);
  const guarded = `(async()=>{if(!(window.__dhSrcStamp===${stamp}&&window.__dh&&window.__dh.rt))return{__dhMissing:true};return{v:await (${expr})};})()`;
  let res = await debuggerEval(tabId, guarded);
  if (res && res.__dhMissing) {
    await debuggerEval(tabId, `window.__dh=undefined;window.__dhVer=undefined;\n${src}\n;window.__dhSrcStamp=${stamp};true`);
    res = await debuggerEval(tabId, guarded);
  }
  if (!res || res.__dhMissing) throw new Error("页面脚本注入失败");
  return res.v === undefined ? null : res.v;
}

function pageCallViaDebugger(tabId, method, arg) {
  return callPageApi(tabId, `window.__dh.rt[${JSON.stringify(method)}](${JSON.stringify(arg ?? null)})`);
}

async function getRuntime(tabId) {
  await ensureDebugger(tabId);
  let rt = runtimes.get(tabId);
  if (!rt) {
    rt = new self.DhRuntime.TabRuntime({
      send: (method, params) => chrome.debugger.sendCommand({ tabId }, method, params || {}),
      page: (method, arg) => pageCallViaDebugger(tabId, method, arg),
      newTabs: async (since) => createdTabs
        .filter((t) => t.at >= since && t.opener === tabId)
        .map(({ id, url }) => ({ id, url })),
    });
    runtimes.set(tabId, rt);
  }
  return rt;
}

// 真实输入引擎；返回 null 表示该标签只能走旧的页面内实现（强制 scripting 或调试器无法附着）。
async function runEngine(tab, payload, channel) {
  if (channel === "extension" || tabExec.get(tab.id) === "extension") return null;
  let rt;
  try {
    rt = await getRuntime(tab.id);
  } catch (e) {
    if (isChannelError(e)) return null;
    throw e;
  }
  tabExec.set(tab.id, "debugger");
  const result = Array.isArray(payload.steps) ? await rt.run(payload.steps) : await rt.act(payload);
  return { ...result, engine: "trusted-input" };
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

function normalizeCookieDomain(domain) {
  return String(domain || "").trim().toLowerCase().replace(/^\.+/, "");
}

function cookieMatchesDomain(cookieDomain, wantedDomain) {
  const actual = normalizeCookieDomain(cookieDomain);
  const wanted = normalizeCookieDomain(wantedDomain);
  return !!wanted && (actual === wanted || actual.endsWith(`.${wanted}`));
}

function serializeCookies(cookies, format) {
  if (format === "json") return JSON.stringify(cookies, null, 2);
  return cookies.map((cookie) => `${cookie.name}=${cookie.value}`).join("\n");
}

function assertLoopbackUrl(rawUrl) {
  let url;
  try { url = new URL(String(rawUrl || "")); } catch { throw new Error("POST 地址无效"); }
  if (url.protocol !== "http:" || !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname)) {
    throw new Error("Cookie 只允许 POST 到本机 HTTP 回环地址");
  }
  return url.href;
}

// 读取页面 JS 无法访问的 Cookie（包括 HttpOnly）。默认不读取全部 Cookie：
// 调用方必须至少给 domains 或 urls；names 可进一步限制 Cookie 名称。
async function exportCookies(p) {
  const domains = Array.isArray(p.domains) ? p.domains.map(normalizeCookieDomain).filter(Boolean) : [];
  const urls = Array.isArray(p.urls) ? p.urls.map((v) => String(v || "").trim()).filter(Boolean) : [];
  const names = new Set(Array.isArray(p.names) ? p.names.map((v) => String(v)) : []);
  if (!domains.length && !urls.length) throw new Error("domains 或 urls 至少提供一项");

  const batches = await Promise.all([
    ...domains.map((domain) => chrome.cookies.getAll({ domain })),
    ...urls.map((url) => chrome.cookies.getAll({ url })),
  ]);
  const unique = new Map();
  for (const cookie of batches.flat()) {
    if (domains.length && !domains.some((domain) => cookieMatchesDomain(cookie.domain, domain))) continue;
    if (names.size && !names.has(cookie.name)) continue;
    const key = `${cookie.storeId}\n${cookie.domain}\n${cookie.path}\n${cookie.name}`;
    unique.set(key, cookie);
  }
  const cookies = Array.from(unique.values()).map((cookie) => ({
    name: cookie.name,
    value: cookie.value,
    domain: cookie.domain,
    path: cookie.path,
    secure: cookie.secure,
    httpOnly: cookie.httpOnly,
    sameSite: cookie.sameSite,
    expirationDate: cookie.expirationDate,
    storeId: cookie.storeId,
  }));

  const output = p.output || "return";
  const format = p.format === "json" ? "json" : "netscape-lite";
  const text = serializeCookies(cookies, format);
  if (output === "return") return { count: cookies.length, cookies };
  if (output === "post") {
    const url = assertLoopbackUrl(p.post_url);
    const response = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": format === "json" ? "application/json" : "text/plain;charset=UTF-8" },
      body: text,
    });
    if (!response.ok) throw new Error(`本地服务返回 HTTP ${response.status}`);
    return { count: cookies.length, names: cookies.map((cookie) => cookie.name), posted_to: url };
  }
  if (output === "download") {
    const mime = format === "json" ? "application/json" : "text/plain";
    const dataUrl = `data:${mime};charset=utf-8,${encodeURIComponent(text)}`;
    const downloadId = await chrome.downloads.download({
      url: dataUrl,
      filename: String(p.filename || `deskhelper-cookies.${format === "json" ? "json" : "txt"}`),
      saveAs: p.save_as !== false,
    });
    return { count: cookies.length, names: cookies.map((cookie) => cookie.name), download_id: downloadId };
  }
  throw new Error(`不支持的 Cookie 输出方式: ${output}`);
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
  read: (maxChars, offset) => window.__dh.read(maxChars, offset),
  find: (query, limit) => window.__dh.find(query, limit),
  request: (method, url, body, headers, maxChars) => window.__dh.request(method, url, body, headers, maxChars),
  click: (ref) => window.__dh.click(ref),
  type: (ref, text, clear) => window.__dh.type(ref, text, clear),
  scroll: (dir, amount, ref) => window.__dh.scroll(dir, amount, ref),
  info: () => window.__dh.info(),
  capabilities: () => window.__dh.capabilities(),
  action: (args) => window.__dh.action(args),
  probe: (args) => window.__dh.probe(args),
  extract: (args) => window.__dh.extract(args),
};

async function runInTab(tabId, action, args) {
  await chrome.scripting.executeScript({
    target: { tabId },
    files: PAGE_FILES,
  });
  const [r] = await chrome.scripting.executeScript({ target: { tabId }, func: CALLS[action], args });
  if (r && r.error) throw new Error(r.error.message || String(r.error));
  const res = r ? r.result : null;
  if (res && typeof res === "object" && typeof res.error === "string" && !["capabilities", "action", "probe", "extract"].includes(action)) throw new Error(res.error);
  return res;
}

// debugger 通道：把 page-api.js 源码注入主世界后调用 window.__dh（与桌面端 cdp.rs 同构）
async function runInTabViaDebugger(tabId, action, args) {
  const res = await callPageApi(tabId, `window.__dh.${action}(...${JSON.stringify(args)})`);
  if (res && typeof res === "object" && typeof res.error === "string" && !["capabilities", "action", "probe", "extract"].includes(action)) throw new Error(res.error);
  return res;
}

function markDebugger(res) {
  if (res && typeof res === "object" && !Array.isArray(res)) res.via = "debugger";
  return res;
}

// 默认 debugger 主世界；无法附着时降级到 scripting 隔离世界。
// 一旦某标签选定执行面就粘住，避免 ref 跨世界失效。channel 可强制指定。
async function runInTabAuto(tabId, action, args, channel) {
  const ch = channel || "auto";
  const runDebugger = async () => {
    const res = markDebugger(await runInTabViaDebugger(tabId, action, args));
    tabExec.set(tabId, "debugger");
    return res;
  };
  const runScript = async () => {
    const res = await runInTab(tabId, action, args);
    tabExec.set(tabId, "extension");
    return res;
  };
  if (ch === "debugger") return runDebugger();
  if (ch === "extension") return runScript();
  if (tabExec.get(tabId) === "extension") return runScript();
  try {
    return await runDebugger();
  } catch (e) {
    if (!isChannelError(e)) throw e;
    return runScript();
  }
}

async function evalViaDebugger(tabId, expression, ref, suppliedArgs) {
  if (ref != null) {
    const wrapped = `(async()=>{const $el=window.__dh.ref(${JSON.stringify(ref)});if(!$el)throw new Error(${JSON.stringify(`元素 [${ref}] 不存在或已失效，请在当前通道重新获取快照`)});const $args=${JSON.stringify(suppliedArgs)};return await (${expression});})()`;
    return callPageApi(tabId, wrapped);
  }
  if (suppliedArgs != null) {
    const wrapped = `(async()=>{const $args=${JSON.stringify(suppliedArgs)};return await (${expression});})()`;
    return debuggerEval(tabId, wrapped);
  }
  return debuggerEval(tabId, expression);
}

// 执行动态 JS（scripting 降级路径）：带 ref 时复用快照所在隔离世界；不带 ref 时进入页面主世界。
async function evalInPage(tabId, expression, ref, suppliedArgs) {
  // 带 ref 时必须与 snapshot 在同一隔离世界执行，才能安全复用元素引用。
  if (ref != null) {
    await chrome.scripting.executeScript({
      target: { tabId },
      files: PAGE_FILES,
    });
    const [r] = await chrome.scripting.executeScript({
      target: { tabId },
      func: async (expr, elementRef, dynamicArgs) => {
        try {
          const $el = window.__dh && window.__dh.ref(elementRef);
          if (!$el) return { ok: false, error: `元素 [${elementRef}] 不存在或已失效，请重新获取快照` };
          const $args = dynamicArgs;
          const v = await eval(expr);
          return { ok: true, value: v === undefined ? null : v };
        } catch (e) {
          return { ok: false, error: String(e) };
        }
      },
      args: [expression, ref, suppliedArgs ?? null],
    });
    if (r && r.error) throw new Error(r.error.message || String(r.error));
    const res = r ? r.result : null;
    if (res && res.ok === false) throw new Error(res.error);
    return res ? res.value : null;
  }
  if (suppliedArgs != null) {
    const [r] = await chrome.scripting.executeScript({
      target: { tabId },
      world: "MAIN",
      func: async (expr, dynamicArgs) => {
        try {
          const $args = dynamicArgs;
          const v = await eval(expr);
          return { ok: true, value: v === undefined ? null : v };
        } catch (e) {
          return { ok: false, error: String(e) };
        }
      },
      args: [expression, suppliedArgs],
    });
    if (r && r.error) throw new Error(r.error.message || String(r.error));
    const res = r ? r.result : null;
    if (res && res.ok === false) throw new Error(res.error);
    return res ? res.value : null;
  }
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
    case "cookies":
      return exportCookies(p);
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
      tabExec.delete(tab.id);
      return runInTabAuto(tab.id, "info", [], p.channel);
    }
    case "snapshot": {
      const t = await activeTab();
      const snapshot = await runInTabAuto(t.id, "snapshot", [p.max_chars || 4000, p.scope ?? null], p.channel);
      const info = await runInTabAuto(t.id, "info", [], p.channel);
      const out = { title: info.title, url: info.url, snapshot };
      if ((snapshot && snapshot.via === "debugger") || info.via === "debugger") out.via = "debugger";
      return out;
    }
    case "read": {
      const t = await activeTab();
      const article = await runInTabAuto(t.id, "read", [p.max_chars || 6000, p.offset || 0], p.channel);
      const info = await runInTabAuto(t.id, "info", [], p.channel);
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
      const r = await runEngine(t, { action: "click", target: { ref: p.ref } }, p.channel);
      return r ? markDebugger(r) : runInTabAuto(t.id, "click", [p.ref], p.channel);
    }
    case "find": {
      const t = await activeTab();
      return runInTabAuto(t.id, "find", [p.query || "", p.limit || 12], p.channel);
    }
    case "network_observe": {
      const t = await activeTab();
      const action = p.action || "list";
      if (action === "start") { await startNetworkWatch(t.id); return { ok: true, started: true, note: "正在观察当前标签页；只保存脱敏后的接口结构。" }; }
      const watch = networkWatch.get(t.id);
      if (action === "stop") { networkWatch.delete(t.id); return { ok: true, stopped: true, captured: watch ? watch.items.length : 0 }; }
      if (!watch) return { active: false, requests: [], hint: "尚未开始观察；先调用 action=start，再在页面上完成一次真实操作。" };
      return { active: true, started_at: watch.started_at, count: watch.items.length, requests: watch.items.map(({ request_id, ...item }) => item) };
    }
    case "request": {
      const t = await activeTab();
      return runInTabAuto(t.id, "request", [p.method || "GET", p.url || "", p.body ?? null, p.headers ?? {}, p.max_chars || 6000], p.channel);
    }
    case "type": {
      const t = await activeTab();
      const step = { action: "type", target: { ref: p.ref }, text: p.text ?? "", clear: p.clear !== false, ...(p.newline ? { newline: p.newline } : {}) };
      const r = await runEngine(t, step, p.channel);
      return r ? markDebugger(r) : runInTabAuto(t.id, "type", [p.ref, p.text ?? "", p.clear !== false], p.channel);
    }
    case "scroll": {
      const t = await activeTab();
      return runInTabAuto(t.id, "scroll", [p.direction || "down", p.amount || 600, p.ref ?? null], p.channel);
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
    case "page_operation": {
      if (!["capabilities", "action", "probe", "extract", "capture"].includes(p.method)) throw new Error("未知页面操作方法");
      const t = await activeTab();
      const args = p.args || {};
      if (p.method === "capture") {
        // 按画面定位用的视口截图，坐标与引擎的真实鼠标同一参照系。
        const rt = await getRuntime(t.id);
        tabExec.set(t.id, "debugger");
        return { result: await rt.capture(args), via: "debugger" };
      }
      if (p.method === "action" && (Array.isArray(args.steps) || (args.action && args.action !== "locate"))) {
        const r = await runEngine(t, args, p.channel);
        if (r) return { result: r, via: "debugger" };
        if (Array.isArray(args.steps) || !["click", "fill", "select", "set_checked"].includes(args.action)) {
          return { result: { error: "该动作需要真实输入引擎（扩展调试通道），当前标签页无法附着调试器", code: "engine_unavailable" } };
        }
      }
      const result = await runInTabAuto(t.id, p.method, [args], p.channel);
      return { result, ...(result?.via ? { via: result.via } : {}) };
    }
    case "evaluate": {
      const t = await activeTab();
      const expr = p.expression || "";
      const ref = p.ref ?? null;
      const suppliedArgs = p.args ?? null;
      const ch = p.channel || "auto";
      const runDebugger = async () => {
        const result = await evalViaDebugger(t.id, expr, ref, suppliedArgs);
        tabExec.set(t.id, "debugger");
        return { result, via: "debugger" };
      };
      const runScript = async () => {
        const result = await evalInPage(t.id, expr, ref, suppliedArgs);
        tabExec.set(t.id, "extension");
        return { result };
      };
      // channel=debugger：只走主世界；channel=extension：只走 scripting，不降级
      if (ch === "debugger") return runDebugger();
      if (ch === "extension") return runScript();
      if (tabExec.get(t.id) === "extension") return runScript();
      try {
        return await runDebugger();
      } catch (e) {
        if (!isChannelError(e)) throw e;
        if (ref != null) {
          throw new Error(`基于 ref 的动态脚本在调试通道失败，且切换通道后编号会失效。请重新获取快照后再执行。原错误：${String((e && e.message) || e)}`);
        }
        return runScript();
      }
    }
    case "info": {
      const t = await activeTab();
      return runInTabAuto(t.id, "info", [], p.channel);
    }
    default:
      throw new Error("未知动作: " + action);
  }
}
