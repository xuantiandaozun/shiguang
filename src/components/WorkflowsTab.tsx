import { useCallback, useEffect, useState } from "react";
import { ipc, onEvent } from "../lib/ipc";
import type { AutomationWorkflow, AutomationWorkflowInput } from "../lib/ipc";

const SCHEDULE_LABEL: Record<string, string> = { manual: "仅手动", once: "执行一次", daily: "每天", weekly: "每周" };
const emptyForm = (): AutomationWorkflowInput => ({ name: "", description: "", prompt: "", schedule_rule: "manual", next_run_at: null, enabled: true });

function toInputTime(value: string | null) { return value ? value.replace(" ", "T").slice(0, 16) : ""; }
function toDbTime(value: string) { return value ? `${value.replace("T", " ")}:00` : null; }

export default function WorkflowsTab() {
  const [items, setItems] = useState<AutomationWorkflow[]>([]);
  const [form, setForm] = useState<AutomationWorkflowInput>(emptyForm());
  const [when, setWhen] = useState("");
  const [editing, setEditing] = useState<number | null>(null);
  const [error, setError] = useState("");
  const reload = useCallback(() => ipc.listAutomationWorkflows().then(setItems).catch((e) => setError(String(e))), []);

  useEffect(() => { reload(); let off: (() => void) | undefined; onEvent("automation-workflows-changed", reload).then((u) => (off = u)); return () => off?.(); }, [reload]);
  const reset = () => { setForm(emptyForm()); setWhen(""); setEditing(null); setError(""); };
  const save = async () => {
    try {
      const next = { ...form, next_run_at: form.schedule_rule === "manual" ? null : toDbTime(when) };
      await ipc.saveAutomationWorkflow(next); reset(); reload();
    } catch (e) { setError(String(e)); }
  };
  const edit = (w: AutomationWorkflow) => { setEditing(w.id); setForm({ id: w.id, name: w.name, description: w.description, prompt: w.prompt, schedule_rule: w.schedule_rule, next_run_at: w.next_run_at, enabled: w.enabled }); setWhen(toInputTime(w.next_run_at)); setError(""); };

  return <div className="space-y-4 max-w-3xl">
    <div>
      <div className="text-sm font-medium text-slate-100">工作流</div>
      <p className="mt-1 text-xs text-slate-500">把固定目标和约束交给 AI 自主完成，可在这里一键运行或按计划触发。运行时可使用与聊天相同的工具；Skill 只是 AI 的通用方法说明，不会自动执行。</p>
    </div>
    <div className="rounded-lg border border-slate-700/60 bg-slate-900/55 p-3 space-y-2.5">
      <div className="flex items-center justify-between"><span className="text-xs font-medium text-slate-300">{editing ? "编辑工作流" : "新建工作流"}</span>{editing && <button onClick={reset} className="text-xs text-slate-500 hover:text-slate-200">取消编辑</button>}</div>
      <div className="grid grid-cols-[minmax(0,1fr)_150px] gap-2">
        <input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="例如：每日整理并汇总" className="bg-slate-800 rounded px-3 py-2 text-sm border border-slate-700 outline-none focus:border-sky-500" />
        <select value={form.schedule_rule} onChange={(e) => setForm({ ...form, schedule_rule: e.target.value })} className="bg-slate-800 rounded px-2 py-2 text-sm border border-slate-700">
          {Object.entries(SCHEDULE_LABEL).map(([k, v]) => <option key={k} value={k}>{v}</option>)}
        </select>
      </div>
      <input value={form.description} onChange={(e) => setForm({ ...form, description: e.target.value })} placeholder="说明：这个流程会产出什么（可选）" className="w-full bg-slate-800 rounded px-3 py-2 text-sm border border-slate-700 outline-none focus:border-sky-500" />
      <textarea value={form.prompt} onChange={(e) => setForm({ ...form, prompt: e.target.value })} placeholder="说明目标、范围、产出与约束；AI 会自行规划并调用工具完成…" rows={4} className="w-full resize-y bg-slate-800 rounded px-3 py-2 text-sm leading-6 border border-slate-700 outline-none focus:border-sky-500" />
      {form.schedule_rule !== "manual" && <div className="flex items-center gap-2"><span className="text-xs text-slate-400">下次执行</span><input type="datetime-local" value={when} onChange={(e) => setWhen(e.target.value)} className="bg-slate-800 rounded px-2 py-1.5 text-sm border border-slate-700 [color-scheme:dark]" /><label className="ml-auto text-xs text-slate-400"><input type="checkbox" checked={form.enabled} onChange={(e) => setForm({ ...form, enabled: e.target.checked })} className="mr-1 accent-sky-500" />启用计划</label></div>}
      {error && <div className="text-xs text-rose-400">{error}</div>}
      <div className="flex justify-end"><button onClick={save} className="rounded bg-gradient-to-r from-sky-500 to-indigo-600 px-4 py-2 text-sm text-white hover:opacity-90">{editing ? "保存修改" : "创建工作流"}</button></div>
    </div>
    {items.length === 0 ? <div className="py-7 text-center text-sm text-slate-500">还没有工作流。也可以在聊天里说“把这个流程保存成工作流”。</div> : <div className="space-y-2">
      {items.map((w) => <div key={w.id} className="rounded-lg border border-slate-700/60 bg-slate-900/45 px-3 py-3">
        <div className="flex items-center gap-2"><div className="min-w-0 flex-1"><div className="truncate text-sm text-slate-100">{w.name}</div><div className="mt-0.5 text-[11px] text-slate-500">{SCHEDULE_LABEL[w.schedule_rule] ?? w.schedule_rule}{w.next_run_at ? ` · 下次 ${w.next_run_at}` : ""}{!w.enabled ? " · 已停用" : ""}</div></div><button onClick={() => ipc.runAutomationWorkflow(w.id).then(reload).catch((e) => setError(String(e)))} className="rounded bg-sky-500/15 px-2.5 py-1.5 text-xs text-sky-300 hover:bg-sky-500/25">运行</button><button onClick={() => edit(w)} className="px-1 text-xs text-slate-500 hover:text-slate-200">编辑</button><button onClick={() => { if (confirm(`删除工作流「${w.name}」？`)) ipc.deleteAutomationWorkflow(w.id).then(reload).catch((e) => setError(String(e))); }} className="px-1 text-xs text-slate-500 hover:text-rose-400">删除</button></div>
        {w.description && <div className="mt-2 text-xs text-slate-400">{w.description}</div>}<details className="mt-2"><summary className="cursor-pointer text-[11px] text-slate-500 hover:text-slate-300">查看执行内容 · 已运行 {w.run_count} 次</summary><pre className="mt-2 whitespace-pre-wrap rounded bg-slate-950/60 p-2 text-[11px] leading-5 text-slate-300">{w.prompt}</pre></details>
      </div>)}
    </div>}
  </div>;
}
