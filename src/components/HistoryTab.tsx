import { useCallback, useEffect, useState } from "react";
import { ipc, onEvent } from "../lib/ipc";
import type { BatchSummary } from "../lib/ipc";

export default function HistoryTab() {
  const [batches, setBatches] = useState<BatchSummary[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState<string | null>(null);

  const reload = useCallback(() => {
    ipc
      .listBatches()
      .then(setBatches)
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    reload();
    let disposed = false;
    const cleanups: Array<() => void> = [];
    (async () => {
      const u1 = await onEvent("history-changed", reload);
      const u2 = await onEvent("plan-executed", reload);
      if (disposed) {
        u1();
        u2();
      } else {
        cleanups.push(u1, u2);
      }
    })();
    return () => {
      disposed = true;
      cleanups.forEach((u) => u());
    };
  }, [reload]);

  const undo = async (b: BatchSummary) => {
    setBusy(b.batch_id);
    setError("");
    try {
      await ipc.undoBatch(b.batch_id);
      reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="space-y-3">
      <div className="text-xs text-slate-500 leading-5">
        每一次整理执行（含规则自动整理）都会记录为一个批次，撤销会将该批次的文件移动回原位置；删除项已移入系统回收站，请从回收站还原。
      </div>
      {error && <div className="text-xs text-rose-400">{error}</div>}
      {batches.length === 0 && <div className="text-center text-slate-500 text-sm py-8">暂无操作记录</div>}
      {batches.map((b) => {
        const movedCount = b.paths.filter((p) => p.op_type !== "delete").length;
        const deletedCount = b.paths.length - movedCount;
        const allDeleted = b.paths.length > 0 && movedCount === 0;
        return (
          <div key={b.batch_id} className="rounded-lg border border-slate-700/60 bg-slate-900/60 p-3">
            <div className="flex items-center justify-between">
              <div className="text-sm text-slate-100">
                {b.created_at} ·{" "}
                {deletedCount > 0
                  ? `移动 ${movedCount} 项 · 删除 ${deletedCount} 项`
                  : `移动 ${b.count} 项`}
                {b.batch_id.startsWith("auto-") && <span className="ml-2 text-[11px] text-emerald-300">自动规则</span>}
                {b.undone && <span className="ml-2 text-[11px] text-slate-500">已撤销</span>}
              </div>
              <button
                onClick={() => undo(b)}
                disabled={b.undone || busy === b.batch_id || allDeleted}
                title={allDeleted ? "删除项已移入系统回收站，请从回收站还原" : undefined}
                className="px-3 py-1.5 rounded text-xs bg-slate-700 text-slate-200 hover:bg-slate-600 disabled:opacity-40 transition"
              >
                {busy === b.batch_id ? "撤销中…" : allDeleted ? "已入回收站" : "撤销本批次"}
              </button>
            </div>
            {deletedCount > 0 && !allDeleted && (
              <div className="mt-1.5 text-[11px] text-slate-500">
                撤销仅还原移动项；删除项请从系统回收站手动恢复。
              </div>
            )}
            <details className="mt-2 text-[11px] text-slate-500">
              <summary className="cursor-pointer hover:text-slate-300 select-none">查看明细</summary>
              <div className="mt-1 space-y-0.5 max-h-40 overflow-y-auto scrollbar-thin">
                {b.paths.map((p, i) => (
                  <div key={i} className="truncate">
                    {p.op_type === "delete" ? `🗑 ${p.src} → 回收站` : `${p.src} → ${p.dst}`}
                  </div>
                ))}
              </div>
            </details>
          </div>
        );
      })}
    </div>
  );
}
