// 插件注册表：从后端读取插件清单，汇总各挂载点（标签页、窗口、工具文案、斜杠命令）。
// 文件变化时后端发 plugins-changed，这里自动刷新，界面随之更新。
import { create } from "zustand";
import { ipc, onEvent } from "../lib/ipc";
import type { PluginInfo, PluginManifestList, PluginSlashCommand, PluginUiItem } from "../lib/ipc";

export interface MountedItem extends PluginUiItem {
  tool?: string;
  plugin: PluginInfo;
  /** 唯一键：插件 id + 项 id/route */
  key: string;
}

interface PluginState {
  list: PluginManifestList | null;
  error: string | null;
  load: () => Promise<void>;
}

export const usePluginStore = create<PluginState>((set) => ({
  list: null,
  error: null,
  load: async () => {
    try {
      const list = await ipc.listPlugins();
      set({ list, error: null });
    } catch (e) {
      set({ error: String(e) });
    }
  },
}));

let started = false;
/** 首次使用时加载并订阅变化（每个窗口一次）。 */
export function startPluginRegistry() {
  if (started) return;
  started = true;
  void usePluginStore.getState().load();
  void onEvent<number>("plugins-changed", () => void usePluginStore.getState().load());
}

function enabledPlugins(list: PluginManifestList | null): PluginInfo[] {
  return (list?.plugins ?? []).filter((p) => p.enabled);
}

function itemsOf(list: PluginManifestList | null, slot: "tabs" | "windows" | "tool_cards"): MountedItem[] {
  const out: MountedItem[] = [];
  for (const plugin of enabledPlugins(list)) {
    const items = plugin.ui?.[slot] as Array<PluginUiItem & { tool?: string }> | undefined;
    if (!Array.isArray(items)) continue;
    for (const item of items) {
      const id = item.id ?? item.route ?? item.tool ?? String(out.length);
      out.push({ ...item, plugin, key: `${plugin.id}:${id}` });
    }
  }
  return out.sort((a, b) => (a.order ?? 1000) - (b.order ?? 1000) || a.key.localeCompare(b.key));
}

export function usePluginTabs(): MountedItem[] {
  const list = usePluginStore((s) => s.list);
  return itemsOf(list, "tabs");
}

export function usePluginWindows(): MountedItem[] {
  const list = usePluginStore((s) => s.list);
  return itemsOf(list, "windows");
}

/** 工具结果卡片：{ tool, entry | view }，聊天里该工具成功后显示。 */
export function usePluginToolCards(): Array<MountedItem & { tool?: string }> {
  const list = usePluginStore((s) => s.list);
  return itemsOf(list, "tool_cards");
}

export function useToolLabels(): Record<string, string> {
  return usePluginStore((s) => s.list?.tool_labels) ?? EMPTY;
}
const EMPTY: Record<string, string> = {};

export function usePluginSlashCommands(): Array<PluginSlashCommand & { plugin: string }> {
  const list = usePluginStore((s) => s.list);
  const out: Array<PluginSlashCommand & { plugin: string }> = [];
  for (const plugin of enabledPlugins(list)) {
    for (const cmd of plugin.ui?.slash_commands ?? []) {
      if (cmd?.trigger) out.push({ ...cmd, plugin: plugin.id });
    }
  }
  return out;
}

/** plugin:// 协议下某个插件文件的地址（WebView2 上是 http://plugin.localhost/...）。 */
export function pluginUrl(pluginId: string, rel: string): string {
  const cut = rel.search(/[?#]/);
  const file = cut >= 0 ? rel.slice(0, cut) : rel;
  const suffix = cut >= 0 ? rel.slice(cut) : "";
  const path = `${encodeURIComponent(pluginId)}/${file.split("/").map(encodeURIComponent).join("/")}${suffix}`;
  const isWindows = navigator.userAgent.includes("Windows");
  return isWindows ? `http://plugin.localhost/${path}` : `plugin://localhost/${path}`;
}
