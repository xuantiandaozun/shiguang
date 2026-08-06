import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ipc, onEvent } from "../lib/ipc";
import type { Todo } from "../lib/ipc";

/**
 * 待办提醒弹窗：监听调度器的 reminder-popup 事件，逐条展示到期任务。
 * - popup：仅提醒，确认/稍后即可
 * - popup_input：带输入框，填写的内容作为聊天消息发给 AI（不落本地），
 *   发送后自动展开聊天窗，由 AI 按已沉淀的 Skill 继续处理（如提交日报）
 */
export default function Reminder() {
  const [queue, setQueue] = useState<Todo[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState("");
  const inputRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    onEvent<Todo>("reminder-popup", (todo) => {
      setQueue((prev) => (prev.some((t) => t.id === todo.id) ? prev : [...prev, todo]));
    })
      .then((u) => {
        if (disposed) u();
        else unlisten = u;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const current = queue[0] as Todo | undefined;
  const isRepeat = !!current && current.repeat_rule !== "none";
  const withInput = current?.remind_mode === "popup_input";

  // 切换到下一条时重置状态；输入模式自动聚焦
  useEffect(() => {
    setInput("");
    setError("");
    setSending(false);
    if (current?.remind_mode === "popup_input") {
      const t = setTimeout(() => inputRef.current?.focus(), 80);
      return () => clearTimeout(t);
    }
  }, [current?.id, current?.remind_mode]);

  const dismiss = useCallback(() => {
    setQueue((prev) => {
      const next = prev.slice(1);
      if (next.length === 0) getCurrentWindow().hide().catch(() => {});
      return next;
    });
  }, []);

  const complete = async () => {
    if (!current) return;
    await ipc.setTodoDone(current.id, true).catch(() => {});
    dismiss();
  };

  const snooze = async () => {
    if (!current) return;
    await ipc.snoozeTodo(current.id, 10).catch(() => {});
    dismiss();
  };

  const sendToAI = async () => {
    if (!current || !input.trim() || sending) return;
    setSending(true);
    setError("");
    try {
      await ipc.sendChat(`【定时提醒「${current.title}」】\n${input.trim()}`);
      // 重复任务到期时已自动顺延，无需标记完成；一次性任务视为已响应
      if (!isRepeat) await ipc.setTodoDone(current.id, true).catch(() => {});
      dismiss();
      ipc.showChat().catch(() => {});
    } catch (e) {
      setError(String(e));
      setSending(false);
    }
  };

  if (!current) {
    return <div className="h-full rounded-2xl bg-slate-900/95 border border-slate-700/60" />;
  }

  return (
    <div className="h-full flex flex-col rounded-2xl overflow-hidden bg-slate-900/95 backdrop-blur border border-sky-500/40 shadow-2xl shadow-sky-500/10">
      <div
        data-tauri-drag-region
        className="shrink-0 h-10 flex items-center justify-between px-3 bg-slate-800/80 border-b border-slate-700/60 cursor-move"
      >
        <div data-tauri-drag-region className="flex items-center gap-2 text-sky-300">
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9" />
            <path d="M13.73 21a2 2 0 0 1-3.46 0" />
          </svg>
          <span data-tauri-drag-region className="text-sm font-medium">定时提醒</span>
          {queue.length > 1 && (
            <span className="text-[11px] text-slate-500">还有 {queue.length - 1} 条</span>
          )}
        </div>
        <button
          onClick={dismiss}
          className="w-6 h-6 rounded text-slate-400 hover:text-white hover:bg-slate-700 transition text-xs"
          title="关闭"
        >
          ✕
        </button>
      </div>

      <div className="flex-1 flex flex-col min-h-0 px-3.5 py-3 gap-2.5">
        <div className="shrink-0">
          <div className="text-[15px] font-medium text-slate-100 leading-6 break-words">{current.title}</div>
          <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-[11px] text-slate-500">
            {current.due_at && <span>截止 {current.due_at.slice(5, 16)}</span>}
            {isRepeat && <span>{current.repeat_rule === "daily" ? "每天重复" : "每周重复"}</span>}
            {current.note && <span className="break-all">{current.note}</span>}
          </div>
        </div>

        {withInput && (
          <>
            <textarea
              ref={inputRef}
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  sendToAI();
                }
              }}
              placeholder="填写内容，Enter 发送…"
              className="flex-1 min-h-0 resize-none scrollbar-thin bg-slate-800/90 text-slate-100 text-sm rounded-lg px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-500"
            />
            <div className="shrink-0 text-[11px] text-slate-500 leading-4">
              内容将作为消息发给 AI 处理，不会保存到本地
            </div>
          </>
        )}
        {!withInput && <div className="flex-1" />}

        {error && <div className="shrink-0 text-[11px] text-rose-400 leading-4">{error}</div>}

        <div className="shrink-0 flex items-center justify-end gap-2 pt-0.5">
          {!isRepeat && (
            <button
              onClick={snooze}
              className="px-2.5 py-1.5 rounded text-xs text-slate-400 hover:text-slate-200 hover:bg-slate-700/70 transition"
              title="10 分钟后再次提醒"
            >
              稍后 10 分钟
            </button>
          )}
          {withInput ? (
            <>
              {!isRepeat && (
                <button
                  onClick={complete}
                  className="px-3 py-1.5 rounded bg-slate-700 text-slate-300 text-xs hover:bg-slate-600 transition"
                >
                  标记完成
                </button>
              )}
              <button
                onClick={sendToAI}
                disabled={!input.trim() || sending}
                className="px-4 py-1.5 rounded bg-gradient-to-r from-sky-500 to-indigo-600 text-white text-xs font-medium disabled:opacity-40 hover:opacity-90 transition"
              >
                {sending ? "发送中…" : "发送给 AI"}
              </button>
            </>
          ) : isRepeat ? (
            <button
              onClick={dismiss}
              className="px-4 py-1.5 rounded bg-gradient-to-r from-sky-500 to-indigo-600 text-white text-xs font-medium hover:opacity-90 transition"
            >
              知道了
            </button>
          ) : (
            <button
              onClick={complete}
              className="px-4 py-1.5 rounded bg-gradient-to-r from-sky-500 to-indigo-600 text-white text-xs font-medium hover:opacity-90 transition"
            >
              完成
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
