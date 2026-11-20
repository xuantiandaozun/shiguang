// 独立小程序窗口：渲染任意内置标签页、插件小程序（ui.applets）或用户插件窗口。
// 支持同时打开多个不同工具，也支持右上角一键多开新窗口、固定到桌面。
import { useEffect, useMemo, useState } from "react";
import Icon, { type IconName } from "../components/Icon";
import { ipc, onEvent } from "../lib/ipc";
import type { AppletEntry } from "../lib/ipc";
import PluginMount from "../plugins/PluginMount";
import { findPluginMount, startPluginRegistry, usePluginStore } from "../plugins/registry";

const ICONS = new Set<string>([
  "plus", "history", "close", "maximize", "files", "ai", "clear", "plug",
  "refresh", "upload", "up", "send", "stop", "tab", "todo", "server",
  "skill", "usage", "settings", "video",
]);
const iconOf = (name?: string | null): IconName => (name && ICONS.has(name) ? (name as IconName) : "plug");

const toolbarBtn =
  "inline-flex items-center gap-1 rounded-lg border border-slate-700 px-2.5 py-1 text-xs text-slate-300 hover:bg-slate-800/60 transition disabled:opacity-40";

export default function AppletWindow() {
  const list = usePluginStore((s) => s.list);
  const loadError = usePluginStore((s) => s.error);
  const [appletInfo, setAppletInfo] = useState<AppletEntry | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ text: string; error?: boolean } | null>(null);

  const { pluginId, slot, itemId, extraParams, extraQuery } = useMemo(() => {
    const hash = window.location.hash.replace(/^#\/?/, "");
    const search = new URLSearchParams(hash.split("?")[1] ?? "");
    const pluginId = search.get("plugin") ?? "";
    const slot = search.get("slot") ?? "tab";
    const itemId = search.get("id") ?? pluginId;
    const rest = new URLSearchParams();
    const extraParams: Record<string, string> = {};
    for (const [k, v] of search.entries()) {
      if (k === "plugin" || k === "slot" || k === "id") continue;
      extraParams[k] = v;
      rest.append(k, v);
    }
    return { pluginId, slot, itemId, extraParams, extraQuery: rest.toString() || undefined };
  }, []);

  useEffect(() => startPluginRegistry(), []);

  useEffect(() => {
    let alive = true;
    const refresh = () => {
      ipc
        .listApplets()
        .then((items) => {
          if (!alive) return;
          const found =
            items.find((a) => a.plugin_id === pluginId && a.slot === slot && a.item_id === itemId) ??
            items.find((a) => a.plugin_id === pluginId && a.item_id === itemId) ??
            null;
          setAppletInfo(found);
        })
        .catch(() => {});
    };
    refresh();
    let unlistenApplets: (() => void) | undefined;
    let unlistenPlugins: (() => void) | undefined;
    onEvent("applets-changed", refresh).then((u) => (unlistenApplets = u));
    onEvent("plugins-changed", refresh).then((u) => (unlistenPlugins = u));
    return () => {
      alive = false;
      unlistenApplets?.();
      unlistenPlugins?.();
    };
  }, [pluginId, slot, itemId]);

  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), 3200);
    return () => window.clearTimeout(timer);
  }, [notice]);

  const mounted = useMemo(
    () => findPluginMount(list, pluginId, slot, itemId),
    [list, pluginId, slot, itemId]
  );

  const title = appletInfo?.title ?? mounted?.title ?? mounted?.label ?? mounted?.plugin.name ?? itemId;
  const isIframe = Boolean(mounted && !mounted.plugin.builtin && mounted.entry);
  const mountSlot = slot === "window" || isIframe ? "window" : "tab";

  const handleNewInstance = async () => {
    setBusy(true);
    try {
      await ipc.openAppletWindow(pluginId, itemId, { slot, query: extraQuery, newInstance: true });
    } catch (e) {
      setNotice({ text: String(e), error: true });
    } finally {
      setBusy(false);
    }
  };

  const handleShortcut = async () => {
    setBusy(true);
    try {
      if (appletInfo?.shortcut_path) {
        await ipc.removeAppletShortcut(appletInfo.key);
        setNotice({ text: "已移出桌面快捷方式" });
      } else {
        const rec = await ipc.createAppletShortcut(pluginId, itemId, {
          slot,
          name: title,
          query: extraQuery,
        });
        setNotice({ text: `已发送到桌面：${rec.name}.lnk` });
      }
    } catch (e) {
      setNotice({ text: String(e), error: true });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="h-screen flex flex-col text-slate-200">
      <header className="shrink-0 px-4 py-2.5 border-b border-slate-800 flex items-center justify-between gap-3">
        <div className="flex items-center gap-2 min-w-0">
          <Icon name={iconOf(appletInfo?.icon ?? mounted?.icon)} className="text-sky-400" />
          <span className="text-sm font-semibold text-slate-100 truncate">{title}</span>
          {mounted && (
            <span className="text-[11px] text-slate-500 truncate">
              {mounted.plugin.builtin ? "拾光工具" : mounted.plugin.name}
            </span>
          )}
        </div>
        <div className="flex items-center gap-1.5 shrink-0">
          {notice && (
            <span className={`text-xs mr-1 ${notice.error ? "text-rose-400" : "text-sky-300"}`}>
              {notice.text}
            </span>
          )}
          <button
            className={toolbarBtn}
            disabled={busy || !mounted}
            onClick={handleNewInstance}
            title="再打开一个独立窗口（支持同工具多开）"
          >
            <Icon name="plus" className="w-3.5 h-3.5" />
            新开窗口
          </button>
          <button
            className={toolbarBtn}
            disabled={busy || !mounted}
            onClick={handleShortcut}
            title={appletInfo?.shortcut_path ? `桌面已有：${appletInfo.shortcut_path}（点击移除）` : "在桌面创建独立快捷启动图标"}
          >
            {appletInfo?.shortcut_path ? "已在桌面 · 移除" : "发送到桌面"}
          </button>
          <button
            className={toolbarBtn}
            onClick={() => void ipc.openMain()}
            title="打开拾光主窗口"
          >
            主窗口
          </button>
        </div>
      </header>

      <main className={`flex-1 min-h-0 flex flex-col ${isIframe ? "p-3" : "overflow-y-auto scrollbar-thin p-5"}`}>
        {list === null && !loadError && <div className="text-sm text-slate-500">加载中…</div>}
        {loadError && <div className="text-sm text-rose-400">加载插件清单失败：{loadError}</div>}
        {list !== null && !mounted && (
          <div className="rounded-xl border border-rose-500/40 p-4 text-sm text-rose-400">
            没有找到小程序入口「{pluginId}:{itemId}」，对应插件可能已停用或删除。
          </div>
        )}
        {mounted && (
          <PluginMount
            key={`${mounted.key}:${slot}`}
            item={mounted}
            slot={mountSlot}
            fill={isIframe}
            params={extraParams}
          />
        )}
      </main>
    </div>
  );
}
