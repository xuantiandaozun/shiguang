import Icon, { type IconName } from "../components/Icon";
import { useEffect, useMemo, useState } from "react";
import { cacheHitRatio, formatPercent, formatTokenCount } from "../components/UsageTab";
import appIcon from "../assets/brand-mark.svg";
import { ipc, onEvent } from "../lib/ipc";
import type { AppletEntry, LlmUsageTotals } from "../lib/ipc";
import { startPluginRegistry, usePluginStore, usePluginTabs } from "../plugins/registry";
import type { MountedItem } from "../plugins/registry";
import PluginMount from "../plugins/PluginMount";

const ICONS = new Set<string>(["plus", "history", "close", "maximize", "files", "ai", "clear", "plug", "refresh", "upload", "up", "send", "stop", "tab", "todo", "server", "skill", "usage", "settings", "video"]);
const iconOf = (item: MountedItem): IconName => (item.icon && ICONS.has(item.icon) ? (item.icon as IconName) : "plug");

/** 标签页都由插件声明（plugin.json 的 ui.tabs）；同一 group 的页在顶部显示切换链接。 */
export default function MainWindow() {
  const tabs = usePluginTabs();
  const loaded = usePluginStore((s) => s.list !== null);
  const loadError = usePluginStore((s) => s.error);
  const [tabKey, setTabKey] = useState<string | null>(null);
  const [today, setToday] = useState<LlmUsageTotals | null>(null);
  const [applets, setApplets] = useState<AppletEntry[]>([]);
  const [appletBusy, setAppletBusy] = useState(false);
  const [appletNotice, setAppletNotice] = useState<{ text: string; error?: boolean } | null>(null);

  useEffect(() => startPluginRegistry(), []);

  const navTabs = useMemo(() => tabs.filter((t) => t.nav !== false), [tabs]);
  const current = tabs.find((t) => t.key === tabKey) ?? navTabs[0] ?? null;
  const byId = (id: string) => tabs.find((t) => t.id === id || t.key === id);
  const group = current?.group ? tabs.filter((t) => t.group === current.group) : [];

  useEffect(() => {
    const load = () => {
      ipc
        .getLlmUsageStats()
        .then((s) => setToday(s.today.totals))
        .catch(() => {});
    };
    load();
    let unlisten: (() => void) | undefined;
    onEvent("llm-usage-changed", load).then((u) => (unlisten = u));
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    const loadApplets = () => {
      ipc.listApplets().then(setApplets).catch(() => {});
    };
    loadApplets();
    let u1: (() => void) | undefined;
    let u2: (() => void) | undefined;
    onEvent("applets-changed", loadApplets).then((u) => (u1 = u));
    onEvent("plugins-changed", loadApplets).then((u) => (u2 = u));
    return () => {
      u1?.();
      u2?.();
    };
  }, []);

  useEffect(() => {
    if (!appletNotice) return;
    const t = window.setTimeout(() => setAppletNotice(null), 3200);
    return () => window.clearTimeout(t);
  }, [appletNotice]);

  // 启动参数 --open main:<tab> 切换到指定标签
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    onEvent<string>("main-switch-tab", (target) => {
      const hit = tabs.find((t) => t.id === target || t.key === target || t.plugin.id === target);
      if (hit) setTabKey(hit.key);
    }).then((u) => (unlisten = u));
    return () => unlisten?.();
  }, [tabs]);

  // 拾光编写插件后请求预览：切到该插件的第一个标签页，没有则打开插件管理页。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    onEvent<{ id: string }>("plugin-preview", ({ id }) => {
      const list = usePluginStore.getState();
      void list.load().then(() => {
        const own = usePluginStore.getState().list?.plugins.find((p) => p.id === id)?.ui?.tabs?.[0];
        setTabKey(own ? `${id}:${own.id ?? own.route ?? "0"}` : "plugins:plugins");
      });
    }).then((u) => (unlisten = u));
    return () => unlisten?.();
  }, []);

  const todayTokens = today?.total_tokens ?? 0;
  const todayRequests = today?.requests ?? 0;
  const todayRatio = today ? cacheHitRatio(today) : null;
  const usageTab = byId("usage");
  const currentTabId = current?.id ?? current?.route ?? "";
  const currentApplet = current
    ? applets.find((a) => a.plugin_id === current.plugin.id && a.slot === "tab" && a.item_id === currentTabId) ?? null
    : null;

  const popOutCurrent = async (newInstance = false) => {
    if (!current || !currentTabId) return;
    setAppletBusy(true);
    try {
      await ipc.openAppletWindow(current.plugin.id, currentTabId, { slot: "tab", newInstance });
    } catch (e) {
      setAppletNotice({ text: String(e), error: true });
    } finally {
      setAppletBusy(false);
    }
  };

  const toggleDesktopShortcut = async () => {
    if (!current || !currentTabId) return;
    setAppletBusy(true);
    try {
      if (currentApplet?.shortcut_path) {
        await ipc.removeAppletShortcut(currentApplet.key);
        setAppletNotice({ text: "已移除桌面快捷方式" });
      } else {
        const rec = await ipc.createAppletShortcut(current.plugin.id, currentTabId, {
          slot: "tab",
          name: current.title ?? current.label ?? current.plugin.name,
        });
        setAppletNotice({ text: `已发送到桌面：${rec.name}.lnk` });
      }
    } catch (e) {
      setAppletNotice({ text: String(e), error: true });
    } finally {
      setAppletBusy(false);
    }
  };

  return (
    <div className="h-full flex flex-col text-slate-200">
      <header className="shrink-0 px-5 pt-4 pb-3 border-b border-slate-800 flex items-center justify-between gap-3">
        <div className="flex items-center gap-2.5">
          <img src={appIcon} alt="拾光" className="w-7 h-7 rounded-full" draggable={false} />
          <div>
            <div className="text-sm font-semibold text-slate-100">拾光 · AI 桌面助手</div>
            <div className="text-[11px] text-slate-500">关闭此窗口不会退出程序，将最小化到系统托盘</div>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {appletNotice && (
            <span className={`text-xs ${appletNotice.error ? "text-rose-400" : "text-sky-300"}`}>
              {appletNotice.text}
            </span>
          )}
          {current && currentTabId && (
            <div className="flex items-center gap-1.5">
              <button
                disabled={appletBusy}
                onClick={() => void popOutCurrent(false)}
                className="rounded-lg border border-slate-700 px-2.5 py-1 text-xs text-slate-300 hover:bg-slate-800/60 transition disabled:opacity-40"
                title={`将「${current.label ?? current.plugin.name}」在独立弹窗中打开`}
              >
                独立窗口
              </button>
              <button
                disabled={appletBusy}
                onClick={() => void toggleDesktopShortcut()}
                className="rounded-lg border border-slate-700 px-2.5 py-1 text-xs text-slate-300 hover:bg-slate-800/60 transition disabled:opacity-40"
                title={
                  currentApplet?.shortcut_path
                    ? `桌面已有：${currentApplet.shortcut_path}（点击移除）`
                    : `在桌面创建「${current.label ?? current.plugin.name}」快捷启动方式`
                }
              >
                {currentApplet?.shortcut_path ? "已在桌面 · 移除" : "发送到桌面"}
              </button>
            </div>
          )}
          {todayRequests > 0 && usageTab && (
            <button
              onClick={() => setTabKey(usageTab.key)}
              className="text-right rounded-lg px-2.5 py-1.5 hover:bg-slate-800/80 transition"
              title="查看模型用量"
            >
              <div className="text-xs text-slate-200 tabular-nums">
                今日 {formatTokenCount(todayTokens)}
              </div>
              <div className="text-[11px] text-slate-500">
                缓存命中 {formatPercent(todayRatio)}
              </div>
            </button>
          )}
        </div>
      </header>

      <nav className="shrink-0 flex gap-1 px-5 pt-3 overflow-x-auto">
        {navTabs.map((t) => {
          const active = current?.key === t.key;
          return (
            <button
              key={t.key}
              onClick={() => setTabKey(t.key)}
              className={`px-4 py-2 rounded-t-lg text-sm transition whitespace-nowrap ${
                active
                  ? "bg-slate-800/80 text-sky-300 font-medium"
                  : "text-slate-400 hover:text-slate-200 hover:bg-slate-800/40"
              }`}
            >
              <Icon name={iconOf(t)} className="mr-2 align-middle" />{t.label ?? t.plugin.name}
            </button>
          );
        })}
      </nav>

      <main className="flex-1 overflow-y-auto scrollbar-thin bg-slate-800/50 mx-5 mb-5 rounded-b-lg rounded-tr-lg p-4">
        {group.length > 1 && (
          <div className="organize-links" aria-label="任务类型">
            {group.map((t) => (
              <button key={t.key} onClick={() => setTabKey(t.key)}>
                {t.group_label ?? t.label}
              </button>
            ))}
          </div>
        )}
        {!loaded && !loadError && <div className="text-sm text-slate-500">加载中…</div>}
        {loadError && <div className="text-sm text-rose-400">插件清单加载失败：{loadError}</div>}
        {current && <PluginMount key={current.key} item={current} slot="tab" />}
      </main>
    </div>
  );
}
