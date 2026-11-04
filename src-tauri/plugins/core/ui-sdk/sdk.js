// 拾光插件界面 SDK。插件页面里这样引入：
//   <link rel="stylesheet" href="/__sdk/theme.css">
//   <script src="/__sdk/sdk.js"></script>
// 页面运行在隔离的 iframe 里，不能直接访问 IPC、本机文件或网络；一切经宿主转发并检查权限。
(function () {
  const pending = new Map();
  const listeners = new Map();
  let seq = 0;
  let info = null;
  let readyResolve;
  const ready = new Promise((r) => (readyResolve = r));

  function call(method, params) {
    const id = ++seq;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      window.parent.postMessage({ __sg: true, type: "call", id, method, params }, "*");
    });
  }

  window.addEventListener("message", (event) => {
    if (event.source !== window.parent) return;
    const msg = event.data;
    if (!msg || msg.__sg !== true) return;
    if (msg.type === "result") {
      const p = pending.get(msg.id);
      if (!p) return;
      pending.delete(msg.id);
      if (msg.error) p.reject(new Error(msg.error));
      else p.resolve(msg.value);
    } else if (msg.type === "init") {
      info = msg.info;
      applyTheme(msg.theme);
      readyResolve(info);
    } else if (msg.type === "event") {
      (listeners.get(msg.name) || []).forEach((cb) => {
        try { cb(msg.payload); } catch (e) { console.error(e); }
      });
    } else if (msg.type === "theme") {
      applyTheme(msg.theme);
    }
  });

  function applyTheme(theme) {
    if (!theme) return;
    for (const [k, v] of Object.entries(theme)) document.documentElement.style.setProperty(k, v);
  }

  // 内容高度变化时通知宿主，iframe 自动撑开。
  const reportSize = () => window.parent.postMessage({ __sg: true, type: "resize", height: document.documentElement.scrollHeight }, "*");
  new ResizeObserver(reportSize).observe(document.documentElement);

  window.shiguang = {
    /** 宿主就绪后返回 { pluginId, slot, item, params } */
    ready: () => ready,
    get info() { return info; },
    /** 调用本插件的工具，或 permissions 声明过的 tools:<name> */
    callTool: (name, args) => call("callTool", { name, args: args || {} }),
    /** 插件设置（按 settings schema，含默认值） */
    getSettings: () => call("getSettings", {}),
    saveSettings: (values) => call("saveSettings", { values }),
    /** 发一条消息给拾光（需要 chat 权限） */
    sendChat: (text) => call("sendChat", { text }),
    /** 用系统浏览器打开 http(s) 链接（需要 open_url 权限） */
    openUrl: (url) => call("openUrl", { url }),
    /** 打开本插件声明的独立窗口（ui.windows 的 route），query 形如 "id=1" */
    openWindow: (route, query) => call("openWindow", { route, query }),
    /** 页内提示 */
    toast: (text) => call("toast", { text }),
    /** 订阅宿主事件：plugins-changed，或本插件通过 sg 事件发出的 plugin:<id>:<name> */
    on: (name, cb) => {
      if (!listeners.has(name)) listeners.set(name, []);
      listeners.get(name).push(cb);
      call("subscribe", { name }).catch(() => {});
      return () => listeners.set(name, (listeners.get(name) || []).filter((x) => x !== cb));
    },
  };
  window.parent.postMessage({ __sg: true, type: "hello" }, "*");
})();
