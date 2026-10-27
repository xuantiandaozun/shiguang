import { FormEvent, useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { useTerminalAgent } from "../lib/useTerminalAgent";
import Icon from "./Icon";
import terminalArt from "../assets/generated/terminal-empty-20261027.png";

type Agent = ReturnType<typeof useTerminalAgent>;
export default function ServerTerminalChat({ agent, connected }: { agent: Agent; connected: boolean }) {
  const [draft, setDraft] = useState("");
  const bottom = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  useEffect(() => { bottom.current?.scrollIntoView({ block: "end" }); }, [agent.messages, agent.pending, agent.activity]);
  useEffect(() => { if (!agent.busy && !agent.pending) input.current?.focus(); }, [agent.busy, agent.pending]);
  const submit = (e?: FormEvent) => {
    e?.preventDefault();
    if (connected && agent.send(draft)) setDraft("");
  };
  return <section aria-label="服务器 AI 对话" className="flex flex-1 min-h-0 flex-col bg-[#0b1120]">
    <div role="log" aria-label="对话记录" aria-live="polite" className="min-h-0 flex-1 overflow-auto px-5 py-5 space-y-5">
      {!agent.messages.length && <div className="mx-auto max-w-xl py-12">
        <img src={terminalArt} alt="" className="mb-5 h-28 w-48 rounded-xl object-cover" />
        <h2 className="text-lg font-medium">想检查或处理什么？</h2>
        <p className="mt-2 text-sm text-slate-400">直接描述目标，AI 会操作当前服务器并整理结果。</p>
        <button className="mt-5 rounded border border-slate-700 px-3 py-2 text-sm text-slate-300 hover:border-sky-600 hover:text-sky-300 disabled:opacity-40" disabled={!connected} onClick={() => { agent.send("检查这台服务器当前的运行情况，概括需要关注的问题。"); }}>检查服务器运行情况</button>
      </div>}
      {agent.messages.map(message => <article key={message.id} className={`mx-auto max-w-3xl ${message.role === "user" ? "flex justify-end" : ""}`}>
        {message.role === "user" ? <div className="max-w-[90%] rounded-lg bg-slate-800 px-4 py-3 text-sm whitespace-pre-wrap break-words">{message.content}</div> : <div className="min-w-0 text-sm">
          <div className="mb-2 text-xs font-medium text-sky-300">AI</div>
          <div className="prose prose-sm prose-invert max-w-none break-words prose-pre:overflow-auto prose-pre:bg-slate-900 prose-table:block prose-table:overflow-auto"><ReactMarkdown remarkPlugins={[remarkGfm]}>{message.content}</ReactMarkdown></div>
        </div>}
      </article>)}
      {agent.busy && <div role="status" className="mx-auto flex max-w-3xl items-center gap-2 text-sm text-slate-400"><span className="h-2 w-2 shrink-0 animate-pulse rounded-full bg-sky-400" />{agent.activity || "正在处理…"}</div>}
      {agent.pending && <div className="mx-auto max-w-3xl border-l-2 border-amber-500 bg-amber-950/20 px-4 py-3 text-sm space-y-3">
        <h3 className="font-medium text-amber-300">需要你确认</h3>
        <p className="whitespace-pre-wrap break-words">{agent.pending.message}</p>
        <p className="whitespace-pre-wrap break-all text-slate-300">{agent.pending.risk}</p>
        <details className="text-xs text-slate-400"><summary className="cursor-pointer">查看操作详情</summary><pre className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap break-all bg-slate-950 p-2">{agent.pending.upload ? `${agent.pending.upload.local_path}\n→ ${agent.pending.upload.remote_path}` : agent.pending.command}</pre></details>
        <div className="flex gap-3"><button className="rounded bg-sky-600 px-3 py-1.5 hover:bg-sky-500 disabled:opacity-40" disabled={!connected} onClick={agent.approve}>{agent.pending.upload ? agent.pending.upload.exists ? "同意覆盖并上传" : "同意上传" : "同意执行"}</button><button className="px-2 text-slate-300 hover:text-white" onClick={agent.reject}>取消此操作</button></div>
      </div>}
      <div ref={bottom} />
    </div>
    <form onSubmit={submit} className="shrink-0 border-t border-slate-700 px-4 py-3">
      <div className="flex items-end gap-2 rounded-lg border border-slate-600 bg-slate-900 p-2 focus-within:border-sky-500">
        <textarea ref={input} aria-label="告诉 AI 要完成的事" rows={2} className="min-w-0 flex-1 resize-none bg-transparent px-1 py-1 text-sm outline-none placeholder:text-slate-500 disabled:opacity-50" placeholder={agent.pending ? "请先确认或取消上方操作" : "例如：检查某个服务是否正常，最近有没有报错"} value={draft} disabled={agent.busy || !!agent.pending} onChange={e => setDraft(e.target.value)} onKeyDown={e => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); submit(); } }} />
        {agent.busy ? <button type="button" className="rounded border border-slate-600 px-4 py-2 text-sm text-slate-200 hover:bg-slate-800" onClick={() => void agent.stop()}><Icon name="stop" className="mr-1.5" />停止</button> : <button className="rounded bg-sky-600 px-4 py-2 text-sm hover:bg-sky-500 disabled:opacity-40" disabled={!connected || !draft.trim() || !!agent.pending}><Icon name="send" className="mr-1.5" />发送</button>}
      </div>
      <p className="mt-2 text-xs text-slate-500">普通操作自动执行，敏感操作才确认 · Enter 发送，Shift+Enter 换行</p>
    </form>
  </section>;
}
