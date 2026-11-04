import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ipc, onEvent } from "../lib/ipc";
import type { AgentRun, AgentRunEvent, AgentRunStatus } from "../lib/ipc";
import Markdown from "../components/Markdown";
import appIcon from "../assets/app-icon.png";

const STATUS: Record<AgentRunStatus, string> = {
  running: "正在执行", asking: "在等回答", idle: "本轮结束，可追问", error: "出错，可补充后继续", cancelled: "已中断", closed: "已收尾",
};

const runId = new URLSearchParams(window.location.hash.split("?")[1] ?? "").get("id") ?? "";

function parseTool(content: string): { name: string; detail: string } {
  try {
    const v = JSON.parse(content);
    return { name: String(v.name ?? "工具"), detail: String(v.detail ?? "") };
  } catch {
    return { name: "工具", detail: content };
  }
}

function Bubble({ who, tone, children, time }: { who: string; tone: "agent" | "main" | "user" | "ask"; children: React.ReactNode; time: string }) {
  const left = tone === "agent" || tone === "ask";
  const box = {
    agent: "rounded-tl-sm border border-[#e2e5e9] bg-white",
    ask: "rounded-tl-sm border border-[#ead4ad] bg-[#fff8ec]",
    main: "rounded-tr-sm border border-[#e2e5e9] bg-[#f3f4f6]",
    user: "rounded-tr-sm border border-[#ead4ad] bg-[#fff3df]",
  }[tone];
  return <div className={`flex ${left ? "justify-start" : "justify-end"}`}>
    <div className="max-w-[88%] min-w-0">
      <div className={`mb-1 flex gap-1.5 text-[11px] text-[#646a73] ${left ? "justify-start" : "justify-end"}`}>
        <span className="text-[#1f2329]">{who}</span><span>{time.slice(11, 16)}</span>
      </div>
      <div className={`rounded-2xl px-3 py-2 text-sm leading-6 break-words text-[#1f2329] ${box}`}>{children}</div>
    </div>
  </div>;
}

function EventRow({ e, agent }: { e: AgentRunEvent; agent: string }) {
  switch (e.kind) {
    case "text":
      return <Bubble who={agent} tone="agent" time={e.created_at}><Markdown content={e.content} /></Bubble>;
    case "ask":
      return <Bubble who={`${agent} · 提问`} tone="ask" time={e.created_at}><div className="whitespace-pre-wrap">{e.content}</div></Bubble>;
    case "checkpoint":
      return <Bubble who={`${agent} · 关键节点`} tone="ask" time={e.created_at}><div className="whitespace-pre-wrap">{e.content}</div></Bubble>;
    case "main":
      return <Bubble who="拾光" tone="main" time={e.created_at}><div className="whitespace-pre-wrap">{e.content}</div></Bubble>;
    case "user":
      return <Bubble who="你" tone="user" time={e.created_at}><div className="whitespace-pre-wrap">{e.content}</div></Bubble>;
    case "tool": {
      const t = parseTool(e.content);
      return <div className="flex gap-1.5 px-1 text-[11px] text-[#646a73]"><span className="shrink-0 text-[#945b12]">▸ {t.name}</span><span className="truncate" title={t.detail}>{t.detail}</span></div>;
    }
    case "tool_error":
      return <div className="whitespace-pre-wrap break-words px-1 text-[11px] text-[#bd3038]">✕ {e.content}</div>;
    case "error":
      return <div className="whitespace-pre-wrap break-words rounded-lg border border-[#f0c4c6] bg-[#fdf0f0] px-3 py-2 text-xs text-[#bd3038]">{e.content}</div>;
    default:
      return <div className="text-center text-xs text-[#646a73]">{e.content}</div>;
  }
}

export default function AgentRunWindow() {
  const [run, setRun] = useState<AgentRun | null>(null);
  const [agentLabel, setAgentLabel] = useState("智能体");
  const [events, setEvents] = useState<AgentRunEvent[]>([]);
  const [reply, setReply] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const lastId = useRef(0);
  const endRef = useRef<HTMLDivElement>(null);

  const refresh = useCallback(async () => {
    if (!runId) return;
    try {
      const view = await ipc.getAgentRun(runId, lastId.current);
      setRun(view.run);
      setAgentLabel(view.agent_label);
      if (view.events.length) {
        lastId.current = view.events[view.events.length - 1].id;
        setEvents((prev) => [...prev, ...view.events]);
      }
      setError("");
    } catch (e) { setError(`加载失败：${String(e)}`); }
  }, []);

  useEffect(() => {
    void refresh();
    let active = true;
    let unlisten: (() => void) | undefined;
    onEvent<string>("agent-run-changed", (id) => { if (active && id === runId) void refresh(); })
      .then((stop) => { if (active) unlisten = stop; else stop(); });
    return () => { active = false; unlisten?.(); };
  }, [refresh]);

  useEffect(() => { endRef.current?.scrollIntoView({ behavior: "smooth" }); }, [events.length]);

  async function act(fn: () => Promise<unknown>, label: string) {
    if (busy) return;
    setBusy(true);
    try { await fn(); await refresh(); }
    catch (e) { setError(`${label}失败：${String(e)}`); }
    finally { setBusy(false); }
  }

  const send = () => {
    const text = reply.trim();
    if (!text) return;
    void act(async () => { await ipc.replyAgentRun(runId, text); setReply(""); }, "发送");
  };

  const status = run?.status;
  const result = run?.data.result as { changed_total?: number; diff_stat?: string; head_changed?: boolean } | undefined;
  const worktreeActive = run?.data.worktree?.state === "active";
  const canSend = status && status !== "closed" && !(status === "running" && run?.agent !== "claude_code");

  return <div className="relative flex h-full flex-col overflow-hidden rounded-2xl border border-[#e2e5e9] bg-white text-[#1f2329] shadow-2xl">
    <header data-tauri-drag-region className="flex h-11 shrink-0 cursor-move items-center justify-between border-b border-[#e2e5e9] px-3">
      <div data-tauri-drag-region className="flex min-w-0 items-center gap-2">
        <img src={appIcon} alt="" className="h-5 w-5 rounded-full" draggable={false} />
        <span data-tauri-drag-region className="truncate text-sm font-medium">外派 · {agentLabel}</span>
      </div>
      <div className="flex shrink-0 items-center gap-1">
        <button type="button" onClick={() => void getCurrentWindow().minimize()} title="最小化，任务继续执行" aria-label="最小化" className="grid h-8 w-8 place-items-center rounded-md text-slate-400 hover:bg-[#f3f4f6] hover:text-[#1f2329]">−</button>
        <button type="button" onClick={() => void getCurrentWindow().close()} title="关闭窗口，任务继续执行" aria-label="关闭" className="grid h-8 w-8 place-items-center rounded-md text-xl leading-none text-slate-400 hover:bg-[#f3f4f6] hover:text-[#1f2329]">×</button>
      </div>
    </header>

    <div className="shrink-0 border-b border-[#e2e5e9] px-3 py-2">
      <div className="truncate text-sm font-medium" title={run?.title}>{run?.title ?? "正在加载…"}</div>
      {run && <div className="mt-1 flex items-center justify-between gap-2 text-xs text-[#646a73]">
        <span className="flex min-w-0 items-center gap-1.5">
          <span className={`h-1.5 w-1.5 shrink-0 rounded-full ${status === "running" ? "animate-pulse bg-[#b57520]" : status === "asking" ? "bg-[#a36313]" : status === "error" ? "bg-[#bd3038]" : status === "idle" ? "bg-[#23834b]" : "bg-slate-400"}`} />
          <span className="shrink-0">{STATUS[run.status]}</span>
          <span className="truncate" title={run.root}>· {run.data.workspace_name ?? run.root}</span>
        </span>
        {(status === "running" || status === "asking") && <button type="button" disabled={busy} onClick={() => void act(() => ipc.stopAgentRun(runId), "中断")} className="shrink-0 rounded border border-[#d5d9df] px-2 py-1 hover:border-[#bd3038] hover:text-[#bd3038] disabled:opacity-50">中断</button>}
      </div>}
    </div>

    <div className="min-h-0 flex-1 space-y-2.5 overflow-y-auto px-3 py-3 scrollbar-thin">
      {events.map((e) => <EventRow key={e.id} e={e} agent={agentLabel} />)}
      {status === "running" && <div className="text-center text-xs text-[#646a73]">执行中…</div>}
      {status === "idle" && result && <details className="rounded-lg border border-[#e2e5e9] bg-[#f7f8fa] px-3 py-2 text-xs">
        <summary className="cursor-pointer">拾光核对：{result.changed_total ?? 0} 个文件有改动{result.head_changed ? "，智能体自己提交了 commit" : ""}</summary>
        {result.diff_stat && <pre className="mt-2 whitespace-pre-wrap break-words font-mono text-[11px] text-[#454b54]">{result.diff_stat}</pre>}
      </details>}
      {worktreeActive && status !== "running" && status !== "asking" && <div className="flex items-center justify-between gap-2 rounded-lg border border-[#ead4ad] bg-[#fff8ec] px-3 py-2 text-xs text-[#754b18]">
        <span>改动在隔离的 worktree 里</span>
        <span className="flex gap-2">
          <button type="button" disabled={busy} onClick={() => void act(() => ipc.finishAgentRunWorktree(runId, "apply"), "合回")} className="rounded bg-[#945b12] px-2 py-1 text-white hover:bg-[#75470e] disabled:opacity-50">合回项目</button>
          <button type="button" disabled={busy} onClick={() => void act(() => ipc.finishAgentRunWorktree(runId, "discard"), "丢弃")} className="rounded border border-[#d5d9df] px-2 py-1 hover:border-[#bd3038] hover:text-[#bd3038] disabled:opacity-50">丢弃</button>
        </span>
      </div>}
      <div ref={endRef} />
    </div>

    {error && <div role="alert" className="shrink-0 px-3 py-2 text-xs text-[#bd3038]">{error}</div>}
    {canSend && <div className="shrink-0 border-t border-[#e2e5e9] p-2.5">
      <div className="flex items-end gap-2">
        <textarea value={reply} onChange={(e) => setReply(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); } }} rows={2}
          placeholder={status === "asking" ? "直接回答它的问题…" : status === "running" ? "插话（Claude Code 会在下一步看到）…" : "追问或纠偏，在同一会话里继续…"}
          className="max-h-28 min-h-10 flex-1 resize-none rounded-lg border border-[#d5d9df] px-3 py-2 text-sm placeholder:text-[#646a73] outline-none focus:border-[#b57520]" />
        <button type="button" disabled={busy || !reply.trim()} onClick={send} className="rounded-lg bg-[#945b12] px-3 py-2 text-sm text-white hover:bg-[#75470e] disabled:cursor-not-allowed disabled:opacity-40">发送</button>
      </div>
    </div>}
  </div>;
}
