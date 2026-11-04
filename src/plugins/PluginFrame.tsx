// 插件代码界面的宿主：隔离 iframe（无同源、无 IPC），通过 postMessage 转发 SDK 调用并检查权限。
import { useEffect, useRef, useState } from "react";
import { ipc, onEvent } from "../lib/ipc";
import type { PluginInfo } from "../lib/ipc";
import { pluginUrl } from "./registry";

/** 插件页面可以订阅的宿主事件（另加 plugin:<自己的 id>:*）。 */
const SUBSCRIBABLE = new Set(["plugins-changed", "todos-changed", "workspaces-changed", "sessions-changed", "llm-message-done"]);

const THEME_VARS = ["--page", "--surface", "--ink", "--muted", "--accent"];

function currentTheme(): Record<string, string> {
  const style = getComputedStyle(document.body);
  const out: Record<string, string> = {};
  for (const name of THEME_VARS) {
    const value = style.getPropertyValue(name).trim();
    if (value) out[name] = value;
  }
  return out;
}

export default function PluginFrame({
  plugin,
  entry,
  slot,
  params,
  minHeight = 320,
  fill = false,
}: {
  plugin: PluginInfo;
  entry: string;
  slot: string;
  params?: Record<string, unknown>;
  minHeight?: number;
  /** true 时铺满父容器（独立窗口），否则按内容高度撑开 */
  fill?: boolean;
}) {
  const ref = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(minHeight);
  const [toast, setToast] = useState("");
  const [reloadKey, setReloadKey] = useState(0);
  const paramsKey = JSON.stringify(params ?? {});
  const permsKey = plugin.permissions.join(",");

  useEffect(() => {
    const unlisteners: Array<() => void> = [];
    const has = (perm: string) => permsKey.split(",").includes(perm);
    const post = (msg: Record<string, unknown>) => ref.current?.contentWindow?.postMessage({ __sg: true, ...msg }, "*");

    const handle = async (method: string, p: Record<string, any>): Promise<unknown> => {
      switch (method) {
        case "callTool":
          return ipc.pluginCallTool(plugin.id, String(p.name ?? ""), (p.args ?? {}) as Record<string, unknown>);
        case "getSettings":
          return ipc.getPluginSettings(plugin.id);
        case "saveSettings":
          return ipc.savePluginSettings(plugin.id, (p.values ?? {}) as Record<string, unknown>);
        case "sendChat":
          if (!has("chat")) throw new Error(`插件 ${plugin.id} 没有 chat 权限`);
          await ipc.showChat();
          return ipc.sendChat(String(p.text ?? ""));
        case "openUrl": {
          if (!has("open_url")) throw new Error(`插件 ${plugin.id} 没有 open_url 权限`);
          const url = String(p.url ?? "");
          if (!/^https?:\/\//i.test(url)) throw new Error("只能打开 http/https 链接");
          return ipc.openExternal(url);
        }
        case "openWindow":
          return ipc.openPluginWindow(plugin.id, String(p.route ?? ""), p.query ? String(p.query) : undefined);
        case "toast":
          setToast(String(p.text ?? "").slice(0, 200));
          window.setTimeout(() => setToast(""), 2500);
          return null;
        case "subscribe": {
          const name = String(p.name ?? "");
          if (!SUBSCRIBABLE.has(name) && !name.startsWith(`plugin:${plugin.id}:`)) throw new Error(`不能订阅事件 ${name}`);
          const un = await onEvent<unknown>(name, (payload) => post({ type: "event", name, payload }));
          unlisteners.push(un);
          return null;
        }
        default:
          throw new Error(`未知方法 ${method}`);
      }
    };

    const onMessage = (event: MessageEvent) => {
      if (!ref.current || event.source !== ref.current.contentWindow) return;
      const msg = event.data;
      if (!msg || msg.__sg !== true) return;
      if (msg.type === "hello") {
        post({ type: "init", info: { pluginId: plugin.id, slot, params: JSON.parse(paramsKey) }, theme: currentTheme() });
      } else if (msg.type === "resize" && !fill) {
        const h = Number(msg.height);
        if (Number.isFinite(h)) setHeight(Math.max(minHeight, Math.min(h + 4, 20000)));
      } else if (msg.type === "call") {
        handle(String(msg.method), (msg.params ?? {}) as Record<string, any>)
          .then((value) => post({ type: "result", id: msg.id, value }))
          .catch((e) => post({ type: "result", id: msg.id, error: String(e?.message ?? e) }));
      }
    };
    window.addEventListener("message", onMessage);
    // 插件文件变化后刷新页面。
    let unChanged: (() => void) | undefined;
    onEvent<number>("plugins-changed", () => setReloadKey((k) => k + 1)).then((u) => (unChanged = u));
    return () => {
      window.removeEventListener("message", onMessage);
      unlisteners.forEach((u) => u());
      unChanged?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [plugin.id, permsKey, slot, paramsKey, minHeight, fill]);

  return (
    <div className="relative" style={fill ? { height: "100%" } : undefined}>
      <iframe
        key={reloadKey}
        ref={ref}
        title={`${plugin.name} · ${slot}`}
        src={pluginUrl(plugin.id, entry)}
        sandbox="allow-scripts allow-forms"
        className="w-full border-0 bg-transparent"
        style={{ height: fill ? "100%" : height }}
      />
      {toast && (
        <div className="absolute bottom-3 left-1/2 -translate-x-1/2 rounded-lg bg-slate-800 px-3 py-1.5 text-xs text-slate-100 shadow">
          {toast}
        </div>
      )}
    </div>
  );
}
