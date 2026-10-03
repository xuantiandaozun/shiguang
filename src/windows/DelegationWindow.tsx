import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ipc, onEvent } from "../lib/ipc";
import type { DelegationMessage, DelegationTask, DelegationView } from "../lib/ipc";
import Markdown from "../components/Markdown";
import appIcon from "../assets/app-icon.png";

const STATUS: Record<DelegationTask["status"], string> = {
  running: "正在执行", waiting_peer: "等待委托方回复", waiting_user: "等待你的回答", complete: "已完成",
  blocked: "已暂停，可继续", error: "执行出错，可补充后继续", cancelled: "已中断",
};

const taskId = new URLSearchParams(window.location.hash.split("?")[1] ?? "").get("id") ?? "";

function Message({ message, origin }: { message: DelegationMessage; origin: string }) {
  if (message.role === "tool") {
    const tool = message.content.split("：", 1)[0];
    return <details className="mx-8 text-xs text-slate-500"><summary className="cursor-pointer py-1 text-center hover:text-slate-300">拾光调用工具 · {tool}</summary><div className="mt-1 whitespace-pre-wrap break-words rounded-lg bg-slate-950/60 px-3 py-2">{message.content}</div></details>;
  }
  if (message.role === "system") {
    return <div className="mx-auto max-w-[92%] text-center text-xs text-slate-500">{message.content}</div>;
  }
  const fromShiguang = message.role === "shiguang";
  const fromUser = message.role === "user";
  const label = fromShiguang ? "拾光" : fromUser ? "你" : origin;
  const avatar = fromShiguang ? <img src={appIcon} alt="" className="h-7 w-7 rounded-full" draggable={false} /> : fromUser ? "我" : origin.slice(0, 1).toUpperCase();
  const time = message.created_at.slice(11, 16) || message.created_at;
  return <div className={`flex items-end gap-2 ${fromShiguang ? "justify-start" : "justify-end"}`}>
    {fromShiguang && <div className="mb-1 grid h-7 w-7 shrink-0 place-items-center">{avatar}</div>}
    <div className="max-w-[80%] min-w-0">
      <div className={`mb-1 flex items-center gap-1.5 text-[11px] ${fromShiguang ? "justify-start" : "justify-end"}`} title={message.created_at}>
        <span className={fromShiguang ? "text-slate-300" : fromUser ? "text-amber-300" : "text-sky-300"}>{label}</span>
        <span className="text-slate-500">{fromShiguang ? "助手" : fromUser ? "本人" : "委托方"} · {time}</span>
      </div>
      <div className={`rounded-2xl px-3 py-2 text-sm leading-6 break-words ${fromShiguang
        ? "rounded-tl-sm border border-slate-700/50 bg-slate-800 text-slate-100"
        : fromUser
          ? "rounded-tr-sm border border-amber-600/40 bg-amber-950/60 text-amber-50"
          : "rounded-tr-sm border border-sky-500/40 bg-sky-900/70 text-sky-50"}`}>
        {fromShiguang ? <Markdown content={message.content} /> : <div className="whitespace-pre-wrap">{message.content}</div>}
      </div>
    </div>
    {!fromShiguang && <div className={`mb-1 grid h-7 w-7 shrink-0 place-items-center rounded-full text-xs font-semibold ${fromUser ? "bg-amber-800/70 text-amber-100" : "bg-sky-700/70 text-sky-100"}`}>{avatar}</div>}
  </div>;
}

export default function DelegationWindow() {
  const [view, setView] = useState<DelegationView | null>(null);
  const [reply, setReply] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const endRef = useRef<HTMLDivElement>(null);

  const refresh = useCallback(async () => {
    if (!taskId) return;
    try { setView(await ipc.getDelegation(taskId)); setError(""); }
    catch (e) { setError(`加载委托失败：${String(e)}`); }
  }, []);

  useEffect(() => {
    void refresh();
    let active = true;
    let unlisten: (() => void) | undefined;
    onEvent<string>("delegation-changed", (changedId) => {
      if (active && changedId === taskId) void refresh();
    }).then((stop) => { if (active) unlisten = stop; else stop(); });
    return () => { active = false; unlisten?.(); };
  }, [refresh]);

  useEffect(() => { endRef.current?.scrollIntoView({ behavior: "smooth" }); }, [view?.messages.length]);

  async function send() {
    if (!reply.trim() || busy || !taskId) return;
    setBusy(true);
    try { await ipc.replyDelegation(taskId, reply.trim()); setReply(""); await refresh(); }
    catch (e) { setError(`发送失败：${String(e)}`); }
    finally { setBusy(false); }
  }

  async function stop() {
    if (busy || !taskId) return;
    setBusy(true);
    try { await ipc.cancelDelegation(taskId); await refresh(); }
    catch (e) { setError(`中断失败：${String(e)}`); }
    finally { setBusy(false); }
  }

  async function closeWindow() {
    try { await getCurrentWindow().close(); }
    catch (e) { setError(`关闭窗口失败：${String(e)}`); }
  }

  async function minimizeWindow() {
    try { await getCurrentWindow().minimize(); }
    catch (e) { setError(`最小化窗口失败：${String(e)}`); }
  }

  const task = view?.task;
  const active = task && ["running", "waiting_peer", "waiting_user", "blocked"].includes(task.status);
  return <div className="relative flex h-full flex-col overflow-hidden rounded-2xl border border-slate-700/60 bg-slate-900/95 text-slate-200 shadow-2xl">
    <header data-tauri-drag-region className="flex h-11 shrink-0 cursor-move items-center justify-between border-b border-slate-700/60 bg-slate-800/80 px-3">
      <div data-tauri-drag-region className="flex min-w-0 items-center gap-2">
        <img src={appIcon} alt="拾光" className="h-5 w-5 rounded-full" draggable={false} />
        <span data-tauri-drag-region className="truncate text-sm font-medium">{task?.origin ?? "委托方"} 与拾光</span>
      </div>
      <div className="flex shrink-0 cursor-default items-center gap-1">
        <button type="button" onClick={() => void minimizeWindow()} title="最小化，任务继续执行" aria-label="最小化委托窗口" className="grid h-8 w-8 place-items-center rounded-md text-slate-400 hover:bg-slate-700 hover:text-white focus-visible:outline focus-visible:outline-2 focus-visible:outline-sky-400">−</button>
        <button type="button" onClick={() => void closeWindow()} title="关闭窗口，任务继续执行" aria-label="关闭委托窗口" className="grid h-8 w-8 place-items-center rounded-md text-xl leading-none text-slate-400 hover:bg-slate-700 hover:text-white focus-visible:outline focus-visible:outline-2 focus-visible:outline-sky-400">×</button>
      </div>
    </header>

    <div className="shrink-0 border-b border-slate-700/50 px-3 py-2">
      <div className="truncate text-sm font-medium text-slate-100" title={task?.title}>{task?.title ?? "正在加载委托…"}</div>
      {task && <div className="mt-1 flex items-center justify-between gap-2 text-xs text-slate-400">
        <span className="flex items-center gap-1.5"><span className={`h-1.5 w-1.5 rounded-full ${task.status === "running" ? "animate-pulse bg-sky-400" : task.status === "complete" ? "bg-emerald-400" : "bg-amber-400"}`} />{STATUS[task.status]}</span>
        {active && <button type="button" disabled={busy} onClick={() => void stop()} className="rounded border border-slate-600 px-2 py-1 text-slate-300 hover:border-rose-400 hover:text-rose-300 disabled:opacity-50">中断任务</button>}
      </div>}
    </div>

    <div className="min-h-0 flex-1 space-y-3 overflow-y-auto px-3 py-3 scrollbar-thin">
      {view?.messages.map((message) => <Message key={message.id} message={message} origin={task?.origin ?? "委托方"} />)}
      {task?.question && <div className="whitespace-pre-wrap rounded-lg border border-amber-700/50 bg-amber-950/30 px-3 py-2 text-xs text-amber-100">{task.status === "waiting_user" ? "拾光需要你确认：" : "拾光正在等待委托方："}{task.question}</div>}
      {task && ["blocked", "error"].includes(task.status) && task.result && <div className="whitespace-pre-wrap break-words rounded-lg border border-amber-700/50 bg-amber-950/30 px-3 py-2 text-xs text-amber-100">{task.result}<p className="mt-2">补充意见后可沿用当前对话继续。</p></div>}
      {!view && !error && <div className="pt-10 text-center text-xs text-slate-500">正在加载对话…</div>}
      <div ref={endRef} />
    </div>

    {error && <div role="alert" className="shrink-0 px-3 py-2 text-xs text-rose-300">{error}</div>}
    {task?.status !== "cancelled" && <div className="shrink-0 border-t border-slate-700/60 bg-slate-800/50 p-2.5">
      <div className="flex items-end gap-2">
        <textarea value={reply} onChange={(e) => setReply(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void send(); } }} rows={2} placeholder={task?.status === "waiting_user" ? "输入你的选择或决定…" : "给拾光补充意见…"} className="max-h-28 min-h-10 flex-1 resize-none rounded-lg border border-slate-700 bg-slate-900/80 px-3 py-2 text-sm text-slate-100 outline-none focus:border-sky-500" />
        <button type="button" disabled={busy || !reply.trim()} onClick={() => void send()} className="rounded-lg bg-sky-600 px-3 py-2 text-sm text-white hover:bg-sky-500 disabled:opacity-40">发送</button>
      </div>
    </div>}
  </div>;
}
