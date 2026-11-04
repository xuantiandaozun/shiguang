// 声明式插件界面：插件只写 JSON，宿主用统一组件渲染，按钮/表单/数据都绑定到插件工具。
// 规范见 docs/PLUGINS.md「声明式视图」。
import { useCallback, useEffect, useState } from "react";
import { ipc, onEvent } from "../lib/ipc";
import type { PluginInfo } from "../lib/ipc";
import Markdown from "../components/Markdown";

type Json = any;

interface ToolCall {
  tool: string;
  args?: Record<string, Json>;
  /** 执行前确认的提示 */
  confirm?: string;
  /** 执行后刷新整个视图（默认 true） */
  refresh?: boolean;
}

interface Block {
  type: string;
  [key: string]: Json;
}

/** 取值：a.b.0.c */
function pick(data: Json, path?: string): Json {
  if (!path) return data;
  return path.split(".").reduce((acc, key) => (acc == null ? undefined : acc[key]), data);
}

/** {{row.x}} / {{form.y}} / {{data.z}} 模板。整串只有一个占位符时保留原类型。 */
function fill(value: Json, scope: Record<string, Json>): Json {
  if (typeof value === "string") {
    const whole = value.match(/^\{\{\s*([\w.]+)\s*\}\}$/);
    if (whole) return pick(scope, whole[1]);
    return value.replace(/\{\{\s*([\w.]+)\s*\}\}/g, (_, path) => {
      const v = pick(scope, path);
      return v == null ? "" : typeof v === "object" ? JSON.stringify(v) : String(v);
    });
  }
  if (Array.isArray(value)) return value.map((v) => fill(v, scope));
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, fill(v, scope)]));
  }
  return value;
}

const input = "w-full rounded-lg border border-slate-700 bg-slate-900 px-2.5 py-1.5 text-sm";
const button = "rounded-lg border border-slate-700 px-3 py-1.5 text-sm hover:bg-slate-800 disabled:opacity-50";

export default function DeclarativeView({ plugin, view, params }: { plugin: PluginInfo; view: string; params?: Record<string, unknown> }) {
  const [spec, setSpec] = useState<Json>(null);
  const [error, setError] = useState("");
  const [version, setVersion] = useState(0);

  useEffect(() => {
    ipc
      .readPluginView(plugin.id, view)
      .then((s) => {
        setSpec(s);
        setError("");
      })
      .catch((e) => setError(String(e)));
  }, [plugin.id, view, version]);

  useEffect(() => {
    let un: (() => void) | undefined;
    onEvent<number>("plugins-changed", () => setVersion((v) => v + 1)).then((u) => (un = u));
    return () => un?.();
  }, []);

  if (error) return <div className="text-sm text-rose-400">界面加载失败：{error}</div>;
  if (!spec) return <div className="text-sm text-slate-500">加载中…</div>;
  return (
    <div className="space-y-4">
      {spec.title && <div className="text-base font-semibold text-slate-100">{spec.title}</div>}
      {spec.description && <div className="text-sm text-slate-400">{spec.description}</div>}
      <Blocks plugin={plugin} blocks={spec.blocks ?? []} scope={{ params: params ?? {} }} refreshAll={() => setVersion((v) => v + 1)} version={version} />
    </div>
  );
}

function useRunner(plugin: PluginInfo, refreshAll: () => void) {
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const run = useCallback(
    async (call: ToolCall, scope: Record<string, Json>) => {
      if (call.confirm && !window.confirm(fill(call.confirm, scope))) return undefined;
      setBusy(true);
      setMessage("");
      try {
        const result = await ipc.pluginCallTool(plugin.id, call.tool, fill(call.args ?? {}, scope));
        if (call.refresh !== false) refreshAll();
        return result;
      } catch (e) {
        setMessage(String(e));
        return undefined;
      } finally {
        setBusy(false);
      }
    },
    [plugin.id, refreshAll]
  );
  return { run, busy, message };
}

function Blocks({ plugin, blocks, scope, refreshAll, version }: { plugin: PluginInfo; blocks: Block[]; scope: Record<string, Json>; refreshAll: () => void; version: number }) {
  return (
    <>
      {blocks.map((block, i) => (
        <BlockView key={i} plugin={plugin} block={block} scope={scope} refreshAll={refreshAll} version={version} />
      ))}
    </>
  );
}

/** 有 source 的块先调工具取数据，结果放进 scope.data。 */
function useSource(plugin: PluginInfo, source: ToolCall | undefined, scope: Record<string, Json>, version: number) {
  const [data, setData] = useState<Json>(undefined);
  const [error, setError] = useState("");
  const key = JSON.stringify([source, version]);
  useEffect(() => {
    if (!source) return;
    let alive = true;
    ipc
      .pluginCallTool(plugin.id, source.tool, fill(source.args ?? {}, scope))
      .then((d) => alive && (setData(d), setError("")))
      .catch((e) => alive && setError(String(e)));
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [plugin.id, key]);
  return { data, error };
}

function BlockView({ plugin, block, scope, refreshAll, version }: { plugin: PluginInfo; block: Block; scope: Record<string, Json>; refreshAll: () => void; version: number }) {
  const { data, error } = useSource(plugin, block.source, scope, version);
  const { run, busy, message } = useRunner(plugin, refreshAll);
  const [form, setForm] = useState<Record<string, Json>>(() => Object.fromEntries((block.fields ?? []).map((f: Json) => [f.key, f.default ?? (f.type === "checkbox" ? false : "")])));
  const local = block.source ? { ...scope, data: pick(data, block.path) } : scope;
  if (block.source && error) return <div className="text-sm text-rose-400">{error}</div>;
  if (block.source && data === undefined) return <div className="text-sm text-slate-500">加载中…</div>;

  switch (block.type) {
    case "section":
      return (
        <section className="rounded-xl border border-slate-700 p-4 space-y-3">
          {block.title && <div className="text-sm font-semibold text-slate-100">{fill(block.title, local)}</div>}
          <Blocks plugin={plugin} blocks={block.blocks ?? []} scope={local} refreshAll={refreshAll} version={version} />
        </section>
      );
    case "row":
      return (
        <div className="flex flex-wrap gap-3 items-start">
          <Blocks plugin={plugin} blocks={block.blocks ?? []} scope={local} refreshAll={refreshAll} version={version} />
        </div>
      );
    case "text":
      return <div className={`text-sm ${block.muted ? "text-slate-500" : "text-slate-300"}`}>{String(fill(block.text ?? "", local))}</div>;
    case "markdown":
      return <Markdown content={String(fill(block.text ?? "", local))} />;
    case "stat":
      return (
        <div className="rounded-xl border border-slate-700 px-4 py-3 min-w-[120px]">
          <div className="text-xs text-slate-500">{fill(block.label, local)}</div>
          <div className="text-xl font-semibold text-slate-100 tabular-nums">{String(fill(block.value, local) ?? "")}</div>
        </div>
      );
    case "json":
      return <pre className="text-xs overflow-auto rounded-lg border border-slate-700 p-3">{JSON.stringify(local.data ?? fill(block.value, local), null, 2)}</pre>;
    case "button":
      return (
        <div>
          <button className={button} disabled={busy} onClick={() => (block.action === "refresh" ? refreshAll() : run(block.action, local))}>
            {fill(block.label, local)}
          </button>
          {message && <div className="mt-1 text-xs text-rose-400">{message}</div>}
        </div>
      );
    case "table":
    case "list": {
      const rows: Json[] = Array.isArray(local.data) ? local.data : [];
      if (rows.length === 0) return <div className="text-sm text-slate-500">{block.empty ?? "暂无数据"}</div>;
      if (block.type === "list") {
        return (
          <ul className="space-y-2">
            {rows.map((row, i) => (
              <li key={i} className="rounded-lg border border-slate-700 px-3 py-2">
                <div className="text-sm text-slate-200">{String(fill(block.title ?? "{{row.title}}", { ...local, row }))}</div>
                {block.subtitle && <div className="text-xs text-slate-500">{String(fill(block.subtitle, { ...local, row }))}</div>}
                <RowActions actions={block.row_actions} scope={{ ...local, row }} run={run} busy={busy} />
              </li>
            ))}
          </ul>
        );
      }
      const columns: Json[] = block.columns ?? Object.keys(rows[0] ?? {}).map((key) => ({ key, label: key }));
      return (
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="text-left text-xs text-slate-500">
                {columns.map((c) => (
                  <th key={c.key} className="py-1.5 pr-3 font-normal">{c.label ?? c.key}</th>
                ))}
                {block.row_actions && <th />}
              </tr>
            </thead>
            <tbody>
              {rows.map((row, i) => (
                <tr key={i} className="border-t border-slate-700 align-top">
                  {columns.map((c) => {
                    const v = pick(row, c.key);
                    return <td key={c.key} className="py-1.5 pr-3 text-slate-300">{v == null ? "" : typeof v === "object" ? JSON.stringify(v) : String(v)}</td>;
                  })}
                  {block.row_actions && (
                    <td className="py-1.5 whitespace-nowrap">
                      <RowActions actions={block.row_actions} scope={{ ...local, row }} run={run} busy={busy} />
                    </td>
                  )}
                </tr>
              ))}
            </tbody>
          </table>
          {message && <div className="mt-1 text-xs text-rose-400">{message}</div>}
        </div>
      );
    }
    case "form":
      return (
        <form
          className="space-y-3"
          onSubmit={async (e) => {
            e.preventDefault();
            const result = await run(block.submit, { ...local, form });
            if (result !== undefined && block.reset !== false) {
              setForm(Object.fromEntries((block.fields ?? []).map((f: Json) => [f.key, f.default ?? (f.type === "checkbox" ? false : "")])));
            }
          }}
        >
          {(block.fields ?? []).map((f: Json) => (
            <label key={f.key} className="block">
              {f.type !== "checkbox" && <div className="mb-1 text-xs text-slate-400">{f.label ?? f.key}</div>}
              {f.type === "textarea" ? (
                <textarea rows={f.rows ?? 3} className={input} value={form[f.key] ?? ""} placeholder={f.placeholder} onChange={(e) => setForm({ ...form, [f.key]: e.target.value })} />
              ) : f.type === "select" ? (
                <select className={input} value={form[f.key] ?? ""} onChange={(e) => setForm({ ...form, [f.key]: e.target.value })}>
                  {(f.options ?? []).map((o: Json) => {
                    const value = typeof o === "object" ? o.value : o;
                    return (
                      <option key={String(value)} value={value}>
                        {typeof o === "object" ? o.label ?? value : o}
                      </option>
                    );
                  })}
                </select>
              ) : f.type === "checkbox" ? (
                <span className="flex items-center gap-2 text-sm">
                  <input type="checkbox" checked={Boolean(form[f.key])} onChange={(e) => setForm({ ...form, [f.key]: e.target.checked })} />
                  {f.label ?? f.key}
                </span>
              ) : (
                <input
                  type={f.type === "number" ? "number" : f.type === "datetime" ? "datetime-local" : "text"}
                  className={input}
                  value={form[f.key] ?? ""}
                  placeholder={f.placeholder}
                  onChange={(e) => setForm({ ...form, [f.key]: f.type === "number" ? (e.target.value === "" ? "" : Number(e.target.value)) : e.target.value })}
                />
              )}
            </label>
          ))}
          <button type="submit" className={button} disabled={busy}>
            {block.submit?.label ?? "提交"}
          </button>
          {message && <div className="text-xs text-rose-400">{message}</div>}
        </form>
      );
    default:
      return <div className="text-xs text-rose-400">未知的视图块类型：{block.type}</div>;
  }
}

function RowActions({ actions, scope, run, busy }: { actions?: Array<ToolCall & { label: string }>; scope: Record<string, Json>; run: (c: ToolCall, s: Record<string, Json>) => Promise<unknown>; busy: boolean }) {
  if (!actions?.length) return null;
  return (
    <span className="inline-flex gap-2 mt-1">
      {actions.map((a, i) => (
        <button key={i} className="text-xs text-sky-300 hover:underline disabled:opacity-50" disabled={busy} onClick={() => run(a, scope)}>
          {a.label}
        </button>
      ))}
    </span>
  );
}
