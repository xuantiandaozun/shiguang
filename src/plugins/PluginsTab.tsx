// 主窗口「插件」页：查看、启停、设置、回滚插件；显示加载错误与安全模式。
import { useEffect, useState } from "react";
import { ipc } from "../lib/ipc";
import type { PluginInfo, PluginVersion } from "../lib/ipc";
import { startPluginRegistry, usePluginStore } from "./registry";
import { SchemaFields } from "./SchemaForm";

const button = "rounded-lg border border-slate-700 px-3 py-1.5 text-xs hover:bg-slate-800 disabled:opacity-50";

export default function PluginsTab() {
  const list = usePluginStore((s) => s.list);
  const loadError = usePluginStore((s) => s.error);
  const [selected, setSelected] = useState<string | null>(null);
  const [error, setError] = useState("");

  useEffect(() => startPluginRegistry(), []);

  const plugins = list?.plugins ?? [];
  const current = plugins.find((p) => p.id === selected) ?? null;
  const userPlugins = plugins.filter((p) => !p.builtin);
  const builtinPlugins = plugins.filter((p) => p.builtin);

  const act = async (fn: () => Promise<unknown>) => {
    setError("");
    try {
      await fn();
      await usePluginStore.getState().load();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="text-sm text-slate-400">
          功能都以插件形式提供。内置插件只读；用户插件可以让拾光在聊天里新增或修改（改完即生效，可回滚）。
        </div>
        <div className="flex gap-2">
          <button className={button} onClick={() => act(() => ipc.reloadPlugins())}>重新加载</button>
          <button className={button} onClick={() => act(() => ipc.openPluginsDir())}>打开用户插件目录</button>
        </div>
      </div>

      {list?.safe_mode && (
        <div className="rounded-xl border border-amber-500/50 bg-amber-500/10 p-3 text-sm flex flex-wrap items-center justify-between gap-2">
          <span>安全模式：上次启动没有正常完成，本次只加载内置插件，用户插件已暂停。</span>
          <button className={button} onClick={() => act(() => ipc.setPluginSafeMode(false))}>恢复加载用户插件</button>
        </div>
      )}
      {(error || loadError) && <div className="text-sm text-rose-400">{error || loadError}</div>}
      {!!list?.errors.length && (
        <div className="rounded-xl border border-rose-500/40 p-3 space-y-1">
          <div className="text-sm font-medium text-rose-400">插件加载问题</div>
          {list.errors.map((e, i) => (
            <div key={i} className="text-xs text-slate-400">
              <span className="text-slate-200">{e.plugin}</span>：{e.message}
              <div className="text-slate-500 break-all">{e.path}</div>
            </div>
          ))}
        </div>
      )}

      <div className="grid gap-4 md:grid-cols-[280px_1fr]">
        <div className="space-y-4">
          <Group title={`用户插件（${userPlugins.length}）`} plugins={userPlugins} selected={selected} onSelect={setSelected} empty="还没有。可以在聊天里说「帮我做一个 xx 插件」。" />
          <Group title={`内置插件（${builtinPlugins.length}）`} plugins={builtinPlugins} selected={selected} onSelect={setSelected} />
        </div>
        <div>{current ? <Detail key={current.id} plugin={current} onAct={act} /> : <div className="text-sm text-slate-500">选择左侧插件查看详情。</div>}</div>
      </div>
    </div>
  );
}

function Group({ title, plugins, selected, onSelect, empty }: { title: string; plugins: PluginInfo[]; selected: string | null; onSelect: (id: string) => void; empty?: string }) {
  return (
    <div>
      <div className="mb-2 text-xs text-slate-500">{title}</div>
      {plugins.length === 0 && empty && <div className="text-xs text-slate-500">{empty}</div>}
      <div className="space-y-1">
        {plugins.map((p) => (
          <button
            key={p.id}
            onClick={() => onSelect(p.id)}
            className={`w-full rounded-lg border px-3 py-2 text-left transition ${selected === p.id ? "border-sky-500 bg-sky-500/10" : "border-slate-700 hover:bg-slate-800/50"}`}
          >
            <div className="flex items-center justify-between gap-2">
              <span className={`text-sm ${p.enabled ? "text-slate-100" : "text-slate-500 line-through"}`}>{p.name}</span>
              <span className="text-[11px] text-slate-500">{p.id}</span>
            </div>
            <div className="text-[11px] text-slate-500">
              {p.tools.length} 个工具{p.ui && (p.ui.tabs?.length || p.ui.windows?.length) ? " · 有界面" : ""}
              {!p.enabled && " · 已停用"}
            </div>
          </button>
        ))}
      </div>
    </div>
  );
}

function Detail({ plugin, onAct }: { plugin: PluginInfo; onAct: (fn: () => Promise<unknown>) => Promise<void> }) {
  const [settings, setSettings] = useState<Record<string, unknown> | null>(null);
  const [saved, setSaved] = useState(false);
  const [versions, setVersions] = useState<PluginVersion[]>([]);

  useEffect(() => {
    if (plugin.settings_schema) ipc.getPluginSettings(plugin.id).then(setSettings).catch(() => setSettings({}));
    if (!plugin.builtin) ipc.pluginVersions(plugin.id).then(setVersions).catch(() => setVersions([]));
  }, [plugin.id, plugin.settings_schema, plugin.builtin]);

  return (
    <div className="space-y-4">
      <div className="rounded-xl border border-slate-700 p-4 space-y-2">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div>
            <div className="text-base font-semibold text-slate-100">{plugin.name}</div>
            <div className="text-xs text-slate-500">
              {plugin.id} · v{plugin.version || "?"} · {plugin.builtin ? "内置（只读）" : "用户插件"}
            </div>
          </div>
          {plugin.id !== "core" && (
            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={plugin.enabled} onChange={(e) => onAct(() => ipc.setPluginEnabled(plugin.id, e.target.checked))} />
              启用
            </label>
          )}
        </div>
        {plugin.description && <div className="text-sm text-slate-400">{plugin.description}</div>}
        {plugin.permissions.length > 0 && <div className="text-xs text-slate-500">权限：{plugin.permissions.join("、")}</div>}
        {plugin.tools.length > 0 && (
          <div className="text-xs text-slate-500 break-words">工具：{plugin.tools.join("、")}</div>
        )}
      </div>

      {plugin.settings_schema && settings && (
        <div className="rounded-xl border border-slate-700 p-4 space-y-3">
          <div className="text-sm font-semibold text-slate-100">设置</div>
          <SchemaFields schema={plugin.settings_schema} value={settings} onChange={(v) => (setSettings(v), setSaved(false))} />
          <div className="flex items-center gap-2">
            <button className={button} onClick={() => onAct(async () => (setSettings(await ipc.savePluginSettings(plugin.id, settings)), setSaved(true)))}>
              保存
            </button>
            {saved && <span className="text-xs text-slate-500">已保存</span>}
          </div>
        </div>
      )}

      {!plugin.builtin && (
        <div className="rounded-xl border border-slate-700 p-4 space-y-2">
          <div className="text-sm font-semibold text-slate-100">历史版本</div>
          {versions.length === 0 && <div className="text-xs text-slate-500">还没有历史版本。每次修改前会自动保存一份。</div>}
          {versions.map((v) => (
            <div key={v.version} className="flex flex-wrap items-center justify-between gap-2 text-xs">
              <span className="text-slate-300">
                {v.at ?? v.version} <span className="text-slate-500">{v.reason}</span>
              </span>
              <button
                className={button}
                onClick={() => {
                  if (window.confirm(`把 ${plugin.name} 回滚到这个版本？当前版本会先存进历史。`)) {
                    void onAct(async () => {
                      await ipc.rollbackPlugin(plugin.id, v.version);
                      setVersions(await ipc.pluginVersions(plugin.id));
                    });
                  }
                }}
              >
                回滚到此版本
              </button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
