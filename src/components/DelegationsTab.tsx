import { useCallback, useEffect, useState } from "react";
import { ipc, onEvent } from "../lib/ipc";
import type { DelegationTask } from "../lib/ipc";

const STATUS: Record<DelegationTask["status"], string> = {
  running: "进行中", waiting_peer: "等待委托方", waiting_user: "等待你回答", complete: "已完成",
  blocked: "已暂停，可继续", error: "出错，可继续", cancelled: "已中断",
};

export default function DelegationsTab() {
  const [tasks, setTasks] = useState<DelegationTask[]>([]);
  const [error, setError] = useState("");
  const refresh = useCallback(async () => {
    try { setTasks(await ipc.listDelegations()); setError(""); }
    catch (e) { setError(`加载委托记录失败：${String(e)}`); }
  }, []);

  useEffect(() => {
    void refresh();
    let active = true;
    let unlisten: (() => void) | undefined;
    onEvent<string>("delegation-changed", () => { if (active) void refresh(); })
      .then((stop) => { if (active) unlisten = stop; else stop(); });
    return () => { active = false; unlisten?.(); };
  }, [refresh]);

  async function open(id: string) {
    try { await ipc.showDelegationWindow(id); setError(""); }
    catch (e) { setError(`打开委托窗口失败：${String(e)}`); }
  }

  return <div className="space-y-4 text-sm">
    <div>
      <h2 className="text-base font-semibold text-slate-100">委托记录</h2>
      <p className="mt-1 text-xs text-slate-400">点击记录可打开独立对话窗口，查看过程或继续补充意见。</p>
    </div>
    {tasks.length === 0 ? <div className="rounded-lg border border-slate-700/60 px-4 py-10 text-center text-slate-500">还没有委托记录</div> :
      <div className="divide-y divide-slate-700/60 overflow-hidden rounded-lg border border-slate-700/60">
        {tasks.map((task) => <button key={task.id} type="button" onClick={() => void open(task.id)} className="flex w-full items-center justify-between gap-4 px-4 py-3 text-left hover:bg-slate-700/40">
          <div className="min-w-0">
            <div className="truncate font-medium text-slate-100">{task.title}</div>
            <div className="mt-1 text-xs text-slate-500">{task.origin} · {task.created_at}</div>
          </div>
          <div className="shrink-0 text-right">
            <div className="text-xs text-slate-300">{STATUS[task.status]}</div>
            <div className="mt-1 text-xs text-sky-400">打开对话</div>
          </div>
        </button>)}
      </div>}
    {error && <div role="alert" className="text-xs text-rose-300">{error}</div>}
  </div>;
}
