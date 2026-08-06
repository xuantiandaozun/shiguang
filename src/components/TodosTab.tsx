import { useCallback, useEffect, useState } from "react";
import { ipc, onEvent } from "../lib/ipc";
import type { Todo } from "../lib/ipc";

const PRIORITY_LABEL = ["低", "中", "高"];
const REPEAT_LABEL: Record<string, string> = { none: "不重复", daily: "每天", weekly: "每周" };
const REMIND_MODE_LABEL: Record<string, string> = {
  notify: "仅通知",
  popup: "弹窗",
  popup_input: "弹窗+输入",
};

export default function TodosTab() {
  const [todos, setTodos] = useState<Todo[]>([]);
  const [filter, setFilter] = useState<"pending" | "done" | "all">("pending");
  const [title, setTitle] = useState("");
  const [note, setNote] = useState("");
  const [dueAt, setDueAt] = useState("");
  const [repeat, setRepeat] = useState("none");
  const [priority, setPriority] = useState(1);
  const [remindMode, setRemindMode] = useState("notify");
  const [error, setError] = useState("");

  const reload = useCallback(() => {
    ipc
      .listTodos(filter)
      .then(setTodos)
      .catch((e) => setError(String(e)));
  }, [filter]);

  useEffect(() => {
    reload();
    let disposed = false;
    const cleanups: Array<() => void> = [];
    (async () => {
      const u1 = await onEvent("todos-changed", reload);
      const u2 = await onEvent("reminder-fired", reload);
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

  const submit = async () => {
    if (!title.trim()) return;
    setError("");
    try {
      const due = dueAt ? dueAt.replace("T", " ") + (dueAt.length === 16 ? ":00" : "") : null;
      await ipc.addTodo(title.trim(), note.trim(), due, repeat, priority, remindMode);
      setTitle("");
      setNote("");
      setDueAt("");
      setRepeat("none");
      setPriority(1);
      setRemindMode("notify");
      if (filter === "done") setFilter("pending");
      else reload();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="space-y-4">
      <div className="rounded-lg border border-slate-700/60 bg-slate-900/60 p-3 space-y-2.5">
        <div className="text-xs text-slate-400 font-medium">新建待办</div>
        <div className="flex gap-2">
          <input
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && submit()}
            placeholder="要做什么？"
            className="flex-1 bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-500"
          />
          <input
            type="datetime-local"
            value={dueAt}
            onChange={(e) => setDueAt(e.target.value)}
            className="bg-slate-800 text-sm rounded px-2 py-2 outline-none border border-slate-700 focus:border-sky-500 text-slate-300 [color-scheme:dark]"
          />
          <select
            value={repeat}
            onChange={(e) => setRepeat(e.target.value)}
            className="bg-slate-800 text-sm rounded px-2 py-2 border border-slate-700 text-slate-300"
          >
            {Object.entries(REPEAT_LABEL).map(([k, v]) => (
              <option key={k} value={k}>
                {v}
              </option>
            ))}
          </select>
          <select
            value={priority}
            onChange={(e) => setPriority(Number(e.target.value))}
            className="bg-slate-800 text-sm rounded px-2 py-2 border border-slate-700 text-slate-300"
          >
            {PRIORITY_LABEL.map((v, i) => (
              <option key={i} value={i}>
                优先级:{v}
              </option>
            ))}
          </select>
          <button
            onClick={submit}
            className="px-4 py-2 rounded bg-gradient-to-r from-sky-500 to-indigo-600 text-white text-sm font-medium hover:opacity-90 transition"
          >
            添加
          </button>
        </div>
        <input
          value={note}
          onChange={(e) => setNote(e.target.value)}
          placeholder="备注（可选）"
          className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-500"
        />
        <div className="flex items-center gap-2 text-xs text-slate-400">
          <span className="shrink-0">提醒方式</span>
          <div className="flex gap-1">
            {Object.entries(REMIND_MODE_LABEL).map(([k, v]) => (
              <button
                key={k}
                onClick={() => setRemindMode(k)}
                className={`px-2.5 py-1 rounded-full transition ${
                  remindMode === k ? "bg-sky-500/20 text-sky-300" : "text-slate-500 hover:bg-slate-700/50"
                }`}
              >
                {v}
              </button>
            ))}
          </div>
          {remindMode === "popup_input" && (
            <span className="text-slate-500">填写的内容会作为消息发给 AI 处理</span>
          )}
        </div>
        {error && <div className="text-xs text-rose-400">{error}</div>}
      </div>

      <div className="flex gap-2 text-xs">
        {(["pending", "done", "all"] as const).map((f) => (
          <button
            key={f}
            onClick={() => setFilter(f)}
            className={`px-3 py-1.5 rounded-full transition ${
              filter === f ? "bg-sky-500/20 text-sky-300" : "text-slate-400 hover:bg-slate-700/50"
            }`}
          >
            {f === "pending" ? "进行中" : f === "done" ? "已完成" : "全部"}
          </button>
        ))}
      </div>

      <div className="space-y-2">
        {todos.length === 0 && <div className="text-center text-slate-500 text-sm py-8">暂无待办，也可以在聊天窗对 AI 说「提醒我…」</div>}
        {todos.map((t) => (
          <div
            key={t.id}
            className={`flex items-center gap-3 rounded-lg border px-3 py-2.5 transition ${
              t.status === "done" ? "border-slate-800 bg-slate-900/40 opacity-60" : "border-slate-700/60 bg-slate-900/60"
            }`}
          >
            <input
              type="checkbox"
              checked={t.status === "done"}
              onChange={(e) => ipc.setTodoDone(t.id, e.target.checked).then(reload).catch(() => {})}
              className="w-4 h-4 accent-sky-500 cursor-pointer"
            />
            <div className="flex-1 min-w-0">
              <div className={`text-sm ${t.status === "done" ? "line-through text-slate-500" : "text-slate-100"}`}>
                {t.title}
              </div>
              <div className="text-[11px] text-slate-500 flex gap-3 mt-0.5">
                {t.due_at && <span>截止 {t.due_at}</span>}
                {t.repeat_rule !== "none" && <span>{REPEAT_LABEL[t.repeat_rule] ?? t.repeat_rule}</span>}
                {t.remind_mode && t.remind_mode !== "notify" && (
                  <span className="text-sky-400/80">{REMIND_MODE_LABEL[t.remind_mode] ?? t.remind_mode}</span>
                )}
                {t.note && <span className="truncate">{t.note}</span>}
              </div>
            </div>
            <span
              className={`text-[11px] px-2 py-0.5 rounded-full ${
                t.priority === 2
                  ? "bg-rose-500/15 text-rose-300"
                  : t.priority === 1
                    ? "bg-amber-500/15 text-amber-300"
                    : "bg-slate-600/30 text-slate-400"
              }`}
            >
              {PRIORITY_LABEL[t.priority] ?? "中"}
            </span>
            <button
              onClick={() => ipc.deleteTodo(t.id).then(reload).catch(() => {})}
              className="text-slate-500 hover:text-rose-400 text-sm px-1 transition"
              title="删除"
            >
              ✕
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}
