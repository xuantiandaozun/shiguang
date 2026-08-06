import { useEffect, useState } from "react";
import { ipc, onEvent } from "../lib/ipc";
import type { BgTask } from "../lib/ipc";

const STATUS_STYLE: Record<string, { text: string; cls: string }> = {
  running: { text: "运行中", cls: "bg-sky-500/15 text-sky-300" },
  done: { text: "已完成", cls: "bg-emerald-500/15 text-emerald-300" },
  failed: { text: "失败", cls: "bg-rose-500/15 text-rose-300" },
  cancelled: { text: "已停止", cls: "bg-slate-500/20 text-slate-400" },
  timeout: { text: "超时终止", cls: "bg-amber-500/15 text-amber-300" },
};

export default function TasksTab() {
  const [tasks, setTasks] = useState<BgTask[]>([]);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [tail, setTail] = useState("");
  const [tailFor, setTailFor] = useState("");

  const refresh = () => {
    ipc.listBgTasks().then(setTasks).catch(() => {});
  };

  useEffect(() => {
    refresh();
    let unlisten: (() => void) | undefined;
    onEvent<BgTask>("task-changed", () => {
      refresh();
      // 展开中的日志面板跟随任务进度刷新
      if (expanded) loadTail(expanded);
    }).then((u) => (unlisten = u));
    return () => unlisten?.();
  }, [expanded]);

  const loadTail = (id: string) => {
    ipc
      .readBgTaskTail(id, 3000)
      .then((t) => {
        setTail(t);
        setTailFor(id);
      })
      .catch(() => {});
  };

  const toggleExpand = (id: string) => {
    if (expanded === id) {
      setExpanded(null);
      return;
    }
    setExpanded(id);
    loadTail(id);
  };

  const stop = async (id: string) => {
    try {
      await ipc.stopBgTask(id);
      refresh();
    } catch {}
  };

  return (
    <div className="space-y-3 max-w-3xl">
      <div className="text-sm font-medium text-slate-100">
        后台任务
        <span className="ml-2 text-xs font-normal text-slate-500">
          AI 在后台执行的命令；输出写入日志文件，这里只查看末尾片段
        </span>
      </div>
      {tasks.length === 0 ? (
        <div className="text-xs text-slate-500">
          暂无任务。聊天时让 AI「后台执行 xxx 命令」，任务会出现在这里。
        </div>
      ) : (
        <div className="space-y-1.5">
          {tasks.map((t) => {
            const st = STATUS_STYLE[t.status] ?? STATUS_STYLE.cancelled;
            return (
              <div
                key={t.id}
                className="rounded-lg border border-slate-700/60 bg-slate-800/50 px-3 py-2"
              >
                <div className="flex items-center gap-2">
                  {t.status === "running" && (
                    <span className="shrink-0 inline-block w-3 h-3 rounded-full border-2 border-sky-400 border-t-transparent animate-spin" />
                  )}
                  <span className={`shrink-0 px-1.5 py-0.5 rounded text-[10px] ${st.cls}`}>
                    {st.text}
                    {t.exit_code !== null && t.status !== "running" ? ` · 退出码 ${t.exit_code}` : ""}
                  </span>
                  <span className="text-sm text-slate-100 truncate" title={t.command}>
                    {t.label}
                  </span>
                  <span className="ml-auto shrink-0 text-[11px] text-slate-500">
                    {t.started_at.slice(5, 16)}
                  </span>
                  <button
                    onClick={() => toggleExpand(t.id)}
                    className="shrink-0 text-slate-500 hover:text-sky-400 text-xs px-1 transition"
                  >
                    {expanded === t.id ? "收起" : "输出"}
                  </button>
                  {t.status === "running" && (
                    <button
                      onClick={() => stop(t.id)}
                      className="shrink-0 text-slate-500 hover:text-rose-400 text-xs px-1 transition"
                    >
                      停止
                    </button>
                  )}
                </div>
                <div className="mt-1 text-[11px] text-slate-500 truncate" title={t.command}>
                  $ {t.command}
                </div>
                {expanded === t.id && (
                  <pre className="mt-2 max-h-56 overflow-y-auto whitespace-pre-wrap break-words text-[11px] leading-5 text-slate-300 bg-slate-900/70 rounded p-2">
                    {(tailFor === t.id && tail.trim()) || "（暂无输出）"}
                  </pre>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
