import { useCallback, useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ipc, onEvent } from "../lib/ipc";
import type {
  Plan,
  ExecResult,
  ChatMsg,
  SessionInfo,
  AskUserPrompt,
  AskAnswer,
  AskQuestion,
  SessionTodo,
  SessionTodoSnapshot,
} from "../lib/ipc";
import { useChatStore } from "../stores/chat";
import type { UiMessage } from "../stores/chat";
import Markdown from "../components/Markdown";
import appIcon from "../assets/app-icon.png";

const TOOL_DIGEST_MARKERS = [
  "【中断前已完成的工具调用与结果】",
  "【工具预算耗尽前已完成的工具调用与结果】",
];

function visibleAssistantContent(content: string): string {
  let cut = content.length;
  for (const marker of TOOL_DIGEST_MARKERS) {
    const i = content.indexOf(marker);
    if (i >= 0) cut = Math.min(cut, i);
  }
  let text = content.slice(0, cut).trimEnd();
  for (const footer of [
    "（回复被用户中断；以上资料已收集完毕，继续时请直接基于它们推进，不要重复收集）",
    "（以上资料已收集完毕，用户说「继续」时请直接基于它们推进，不要重复收集）",
  ]) {
    const i = text.indexOf(footer);
    if (i >= 0) text = text.slice(0, i).trimEnd();
  }
  if (
    !text &&
    (content.includes("【中断前已完成的工具调用与结果】") || content.includes("回复被用户中断"))
  ) {
    return "（已中断）";
  }
  return text;
}

const mapRow = (r: ChatMsg): UiMessage | null => {
  const content = r.role === "user" ? r.content : visibleAssistantContent(r.content);
  if (r.role !== "user" && !content) return null;
  return {
    id: `h-${r.id}`,
    dbId: r.id,
    role: r.role === "user" ? "user" : "assistant",
    content,
  };
};

const mapHistory = (rows: ChatMsg[]): UiMessage[] =>
  rows.flatMap((r) => {
    const m = mapRow(r);
    return m ? [m] : [];
  });

function overlayPending(msgs: UiMessage[], plan: Plan | null, ask: AskUserPrompt | null): UiMessage[] {
  const next = [...msgs];
  if (plan && !next.some((m) => m.plan)) {
    next.push({
      id: "pending-plan",
      role: "assistant",
      content: "你还有一个待确认的整理方案：",
      plan,
    });
  }
  if (ask && !next.some((m) => m.ask && !m.askSettled)) {
    next.push({
      id: "pending-ask",
      role: "assistant",
      content: "",
      ask,
    });
  }
  return next;
}

async function loadSessionChrome(sessionId: number) {
  const [plan, ask, todos] = await Promise.all([
    ipc.getPendingPlan().catch(() => null),
    ipc.getPendingAsk().catch(() => null),
    ipc.getSessionTodos(sessionId).catch(() => [] as SessionTodo[]),
  ]);
  return { plan, ask, todos };
}

const fileName = (p: string) => p.replace(/\\/g, "/").split("/").pop() || p;

const MAX_PASTE_BYTES = 32 * 1024 * 1024;

function pasteHasFiles(dt: DataTransfer | null): boolean {
  if (!dt) return false;
  if (dt.files && dt.files.length > 0) return true;
  const types = Array.from(dt.types ?? []);
  if (types.some((t) => t === "Files" || t === "files" || t.startsWith("image/"))) return true;
  return Array.from(dt.items ?? []).some((it) => it.kind === "file" || it.type.startsWith("image/"));
}

function filesFromClipboard(dt: DataTransfer): File[] {
  const fromList = Array.from(dt.files ?? []);
  if (fromList.length) return fromList;
  const fromItems: File[] = [];
  for (const it of Array.from(dt.items ?? [])) {
    if (it.kind !== "file") continue;
    const f = it.getAsFile();
    if (f) fromItems.push(f);
  }
  return fromItems;
}

function uint8ToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

export default function ChatPanel() {
  const {
    messages,
    streaming,
    pendingPlan,
    sessionTodos,
    setMessages,
    addMessage,
    attachDbId,
    recallLocal,
    appendToken,
    appendReasoning,
    finishStreaming,
    completeTool,
    setStreaming,
    setPendingPlan,
    setPendingAsk,
    setSessionTodos,
    settleAsk,
  } = useChatStore();
  const [input, setInput] = useState("");
  const [attachments, setAttachments] = useState<string[]>([]);
  const [dragOver, setDragOver] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [showSessions, setShowSessions] = useState(false);
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [currentSessionId, setCurrentSessionId] = useState<number | null>(null);
  const currentSessionIdRef = useRef<number | null>(null);
  currentSessionIdRef.current = currentSessionId;
  const listRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  // 输入框随内容自动增高（上限 max-h-28），未超高时不出现滚动条
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    const max = 112;
    el.style.height = `${Math.min(el.scrollHeight, max)}px`;
    el.style.overflowY = el.scrollHeight > max ? "auto" : "hidden";
  }, [input]);

  const addAttachments = useCallback((paths: string[]) => {
    if (!paths.length) return;
    setAttachments((prev) => {
      const set = new Set(prev);
      for (const p of paths) {
        const n = p.trim();
        if (n) set.add(n);
      }
      return Array.from(set);
    });
  }, []);

  const ingestPastedFiles = useCallback(
    async (files: File[], includeImage: boolean) => {
      try {
        const imported = await ipc.importClipboardAttachments(includeImage);
        if (imported.paths.length) addAttachments(imported.paths);
        if (imported.skipped_dirs > 0) {
          addMessage({
            role: "system",
            content: imported.paths.length
              ? `已跳过 ${imported.skipped_dirs} 个文件夹，目前只支持粘贴文件`
              : "暂不支持直接粘贴文件夹，请选择其中的文件",
          });
        }
        if (imported.paths.length || imported.skipped_dirs > 0) return;
        if (!includeImage || !files.length) return;

        const paths: string[] = [];
        for (const file of files) {
          if (file.size > MAX_PASTE_BYTES) {
            addMessage({
              role: "system",
              content: `「${file.name || "未命名"}」较大，请改用拖入或点击左侧按钮添加`,
            });
            continue;
          }
          const buf = new Uint8Array(await file.arrayBuffer());
          const name =
            file.name && file.name !== "image.png" ? file.name : `截图-${Date.now()}.png`;
          paths.push(await ipc.savePastedFile(name, uint8ToBase64(buf)));
        }
        addAttachments(paths);
      } catch (e) {
        if (includeImage || files.length) {
          addMessage({ role: "system", content: `粘贴文件失败：${String(e)}` });
        }
      }
    },
    [addAttachments, addMessage],
  );

  useEffect(() => {
    const onPaste = (e: ClipboardEvent) => {
      const hasFiles = pasteHasFiles(e.clipboardData);
      if (hasFiles) {
        e.preventDefault();
        const files = e.clipboardData ? filesFromClipboard(e.clipboardData) : [];
        void ingestPastedFiles(files, true);
        return;
      }
      // 资源管理器复制的文件在部分 WebView 上不会出现在 paste 事件里，补查 CF_HDROP。
      // 不读剪贴板图片，避免从 Word 粘贴文字时把选区位图当成附件。
      void ingestPastedFiles([], false);
    };
    window.addEventListener("paste", onPaste);
    return () => window.removeEventListener("paste", onPaste);
  }, [ingestPastedFiles]);

  const pickFiles = async () => {
    try {
      const selected = await open({
        multiple: true,
        title: "选择要发送的文件",
        filters: [
          {
            name: "常用文档与图片",
            extensions: [
              "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
              "txt", "md", "csv", "json", "log",
              "png", "jpg", "jpeg", "gif", "webp", "bmp",
            ],
          },
          { name: "所有文件", extensions: ["*"] },
        ],
      });
      if (!selected) return;
      addAttachments(Array.isArray(selected) ? selected : [selected]);
    } catch (e) {
      addMessage({ role: "system", content: `选择文件失败：${String(e)}` });
    }
  };

  // 拖拽文件到聊天窗口
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onDragDropEvent((event) => {
        if (event.payload.type === "enter" || event.payload.type === "over") {
          setDragOver(true);
        } else if (event.payload.type === "leave") {
          setDragOver(false);
        } else if (event.payload.type === "drop") {
          setDragOver(false);
          addAttachments(event.payload.paths);
        }
      })
      .then((u) => (unlisten = u))
      .catch(() => {});
    return () => unlisten?.();
  }, [addAttachments]);

  const refreshSessions = useCallback(() => {
    ipc.listSessions().then(setSessions).catch(() => {});
  }, []);

  useEffect(() => {
    let disposed = false;
    const cleanups: Array<() => void> = [];
    const add = async <T,>(name: string, cb: (p: T) => void) => {
      const u = await onEvent<T>(name, cb);
      if (disposed) u();
      else cleanups.push(u);
    };

    add<{ delta: string }>("llm-token", (p) => appendToken(p.delta));
    add<{ delta: string }>("llm-reasoning", (p) => appendReasoning(p.delta));
    add<{ content: string }>("llm-message-done", (p) => finishStreaming(p.content));
    add<{ content: string }>("llm-cancelled", (p) => {
      finishStreaming(p.content.trim() ? `${p.content}\n\n（已中断）` : undefined);
      if (!p.content.trim()) {
        addMessage({ role: "system", content: "已中断。" });
      }
    });
    add<{ message: string }>("llm-error", (p) => {
      finishStreaming();
      addMessage({ role: "system", content: `出错了：${p.message}` });
    });
    add<SessionTodoSnapshot>("session-todos", (p) => {
      if (p.session_id === currentSessionIdRef.current) setSessionTodos(p.todos);
    });
    add<{ session_id: number; content: string }>("subagent-chat", (p) => {
      if (p.session_id === currentSessionIdRef.current) {
        addMessage({ role: "system", content: p.content });
      }
    });
    add<{ name: string; status: string; result?: unknown }>("tool-status", (p) => {
      if (p.name === "todo_write") return;
      if (p.status === "running") {
        addMessage({ role: "tool", toolName: p.name, content: "", streaming: true });
      } else if (p.status === "done" || p.status === "error") {
        completeTool(p.name, p.status === "error" ? toolFailureSummary(p.result) : undefined);
      }
    });
    add<Plan>("plan-proposed", (p) => {
      setPendingPlan(p);
      addMessage({ role: "assistant", content: "我拟了一份整理方案，请确认：", plan: p });
    });
    add<AskUserPrompt>("ask-user", (p) => {
      setPendingAsk(p);
      addMessage({ role: "assistant", content: "", ask: p });
    });
    add("ask-user-settled", () => settleAsk());
    add<ExecResult>("plan-executed", (p) => {
      setPendingPlan(null);
      const parts = [`移动 ${p.moved} 项`];
      if (p.deleted) parts.push(`删除 ${p.deleted} 项（已放入回收站，可从系统回收站恢复）`);
      if (p.skipped) parts.push(`跳过 ${p.skipped} 项`);
      addMessage({
        role: "system",
        content: `整理完成：${parts.join("，")}。移动项如需还原，可在主窗口「操作记录」一键撤销。`,
      });
    });
    add<{ plan_id: number }>("plan-cancelled", () => {
      setPendingPlan(null);
      addMessage({ role: "system", content: "已取消该整理方案。" });
    });
    add("sessions-changed", refreshSessions);

    const hydrate = (attempt = 0) => {
      ipc
        .getCurrentSession()
        .then(async (view) => {
          const chrome = await loadSessionChrome(view.session_id);
          if (disposed) return;
          setCurrentSessionId(view.session_id);
          setPendingPlan(chrome.plan);
          setPendingAsk(chrome.ask);
          setSessionTodos(chrome.todos);
          setMessages(overlayPending(mapHistory(view.messages), chrome.plan, chrome.ask));
        })
        .catch(() => {
          if (disposed || attempt >= 20) return;
          window.setTimeout(() => hydrate(attempt + 1), 100);
        });
    };
    hydrate();
    refreshSessions();

    return () => {
      disposed = true;
      cleanups.forEach((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    listRef.current?.scrollTo({ top: listRef.current.scrollHeight });
  }, [messages, showSessions]);

  // 回复结束（完成/出错/已中断）后复位中断按钮状态
  useEffect(() => {
    if (!streaming) setCancelling(false);
  }, [streaming]);

  const send = async () => {
    const text = input.trim();
    const files = [...attachments];
    if ((!text && files.length === 0) || streaming) return;
    setInput("");
    setAttachments([]);
    // 本地气泡展示：文字 + 附件文件名（后端入库会附带完整路径说明）
    let display = text;
    if (files.length) {
      const names = files.map((p) => `📎 ${fileName(p)}`).join("\n");
      display = text ? `${text}\n\n${names}` : names;
    }
    const localId = addMessage({ role: "user", content: display });
    addMessage({ role: "assistant", content: "", streaming: true });
    setStreaming(true);
    setSessionTodos([]);
    try {
      const dbId = await ipc.sendChat(text, files.length ? files : undefined);
      attachDbId(localId, dbId);
    } catch (e) {
      finishStreaming();
      addMessage({ role: "system", content: `发送失败：${String(e)}` });
    }
  };

  const stop = async () => {
    setCancelling(true);
    try {
      await ipc.stopChat();
    } catch {
      setCancelling(false);
    }
  };

  const recall = async (m: UiMessage) => {
    if (m.dbId == null) return;
    recallLocal(m.id);
    try {
      await ipc.recallMessage(m.dbId);
    } catch (e) {
      addMessage({ role: "system", content: `撤回失败：${String(e)}` });
    }
  };

  const adoptSession = async (view: { session_id: number; messages: ChatMsg[] }) => {
    const chrome = await loadSessionChrome(view.session_id);
    setCurrentSessionId(view.session_id);
    setPendingPlan(chrome.plan);
    setPendingAsk(chrome.ask);
    setSessionTodos(chrome.todos);
    setMessages(overlayPending(mapHistory(view.messages), chrome.plan, chrome.ask));
  };

  const createSession = async () => {
    try {
      const view = await ipc.newSession();
      await adoptSession(view);
      refreshSessions();
    } catch (e) {
      addMessage({ role: "system", content: `新建会话失败：${String(e)}` });
    }
  };

  const switchTo = async (id: number) => {
    try {
      const view = await ipc.switchSession(id);
      await adoptSession(view);
      setShowSessions(false);
    } catch (e) {
      addMessage({ role: "system", content: `切换会话失败：${String(e)}` });
    }
  };

  const removeSession = async (id: number) => {
    try {
      const view = await ipc.deleteSession(id);
      if (id === currentSessionId) {
        await adoptSession(view);
      }
      refreshSessions();
    } catch (e) {
      addMessage({ role: "system", content: `删除会话失败：${String(e)}` });
    }
  };

  const confirmPlan = async (plan: Plan) => {
    try {
      await ipc.executePlan(plan.id);
    } catch (e) {
      addMessage({ role: "system", content: `执行失败：${String(e)}` });
      setPendingPlan(null);
    }
  };

  const cancelPlan = async (plan: Plan) => {
    try {
      await ipc.cancelPlan(plan.id);
    } catch {
      setPendingPlan(null);
    }
  };

  const answerAsk = async (answers: AskAnswer[]) => {
    try {
      await ipc.answerAskUser(answers);
    } catch (e) {
      addMessage({ role: "system", content: `没能记下你的选择：${String(e)}` });
    }
  };

  const skipAsk = async () => {
    try {
      await ipc.dismissAskUser();
    } catch {
      settleAsk();
    }
  };

  return (
    <div className="relative h-full flex flex-col rounded-2xl overflow-hidden bg-slate-900/95 backdrop-blur border border-slate-700/60 shadow-2xl">
      <div
        data-tauri-drag-region
        className="h-11 shrink-0 flex items-center justify-between px-3 bg-slate-800/80 border-b border-slate-700/60 cursor-move"
      >
        <div data-tauri-drag-region className="flex items-center gap-2">
          <img src={appIcon} alt="拾光" className="w-5 h-5 rounded-full" draggable={false} />
          <span data-tauri-drag-region className="text-slate-200 text-sm font-medium">
            拾光
          </span>
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={createSession}
            className="w-7 h-7 rounded text-slate-300 hover:text-white hover:bg-slate-700 transition text-base leading-none"
            title="新会话"
          >
            +
          </button>
          <button
            onClick={() => {
              setShowSessions((v) => !v);
              refreshSessions();
            }}
            className={`px-2 py-1 text-xs rounded transition ${
              showSessions ? "bg-sky-500/20 text-sky-300" : "text-slate-300 hover:text-white hover:bg-slate-700"
            }`}
          >
            历史
          </button>
          <button
            onClick={() => ipc.openMain().catch(() => {})}
            className="px-2 py-1 text-xs rounded text-slate-300 hover:text-white hover:bg-slate-700 transition"
          >
            主面板
          </button>
          <button
            onClick={() => ipc.hideChat().catch(() => {})}
            className="w-7 h-7 rounded text-slate-400 hover:text-white hover:bg-slate-700 transition"
            title="收起"
          >
            —
          </button>
        </div>
      </div>

      <SessionProgressBar items={sessionTodos} />

      <div ref={listRef} className="flex-1 overflow-y-auto scrollbar-thin px-3 py-3 space-y-2.5">
        {messages.length === 0 && (
          <div className="text-slate-500 text-xs text-center mt-10 leading-6">
            试试对我说：
            <br />
            「帮我整理一下桌面」
            <br />
            「明天下午三点提醒我交周报」
          </div>
        )}
        {messages.map((m) => (
          <MessageBubble
            key={m.id}
            m={m}
            activePlanId={pendingPlan?.id ?? null}
            onConfirmPlan={confirmPlan}
            onCancelPlan={cancelPlan}
            onAnswerAsk={answerAsk}
            onSkipAsk={skipAsk}
            onRecall={recall}
          />
        ))}
        <div className="h-1" />
      </div>

      <div className="shrink-0 p-2.5 border-t border-slate-700/60 bg-slate-800/50 relative">
        {dragOver && (
          <div className="absolute inset-0 z-10 flex items-center justify-center rounded-b-2xl bg-sky-500/15 border-2 border-dashed border-sky-400 pointer-events-none">
            <span className="text-sm text-sky-300 font-medium">松开以添加文件</span>
          </div>
        )}
        {attachments.length > 0 && (
          <div className="flex flex-wrap gap-1.5 mb-2">
            {attachments.map((p) => (
              <span
                key={p}
                className="inline-flex items-center gap-1 max-w-full px-2 py-1 rounded-md bg-slate-900/80 border border-slate-700 text-[11px] text-slate-300"
                title={p}
              >
                <span className="truncate">📎 {fileName(p)}</span>
                <button
                  onClick={() => setAttachments((prev) => prev.filter((x) => x !== p))}
                  className="shrink-0 text-slate-500 hover:text-rose-400 leading-none"
                  title="移除"
                >
                  ×
                </button>
              </span>
            ))}
          </div>
        )}
        <div className="flex items-end gap-2">
          <button
            onClick={pickFiles}
            disabled={streaming}
            title="添加文件（也可粘贴或拖入）"
            className="shrink-0 w-9 h-9 rounded-lg bg-slate-900/80 border border-slate-700 text-slate-400 hover:text-sky-300 hover:border-sky-500/50 disabled:opacity-40 transition flex items-center justify-center"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
            </svg>
          </button>
          <textarea
            ref={inputRef}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                send();
              }
            }}
            rows={1}
            placeholder="和 AI 说点什么… 可粘贴、拖入或点左侧附文件"
            className="flex-1 resize-none scrollbar-none bg-slate-900/80 text-slate-100 text-sm rounded-lg px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-500 max-h-28"
          />
          {streaming ? (
            <button
              onClick={stop}
              disabled={cancelling}
              title="中断当前回复"
              className="px-3.5 py-2 rounded-lg bg-rose-500/90 text-white text-sm font-medium disabled:opacity-50 hover:bg-rose-500 transition shrink-0 flex items-center gap-1.5"
            >
              <span className="inline-block w-2.5 h-2.5 rounded-[2px] bg-white" />
              {cancelling ? "中断中…" : "停止"}
            </button>
          ) : (
            <button
              onClick={send}
              disabled={!input.trim() && attachments.length === 0}
              className="px-3.5 py-2 rounded-lg bg-gradient-to-r from-sky-500 to-indigo-600 text-white text-sm font-medium disabled:opacity-40 hover:opacity-90 transition shrink-0"
            >
              发送
            </button>
          )}
        </div>
      </div>

      {showSessions && (
        <div className="absolute inset-x-0 top-11 bottom-0 z-20 flex flex-col bg-slate-900/95 backdrop-blur border-t border-slate-700/60">
          <div className="shrink-0 flex items-center justify-between px-3 py-2.5 border-b border-slate-800">
            <span className="text-sm text-slate-200 font-medium">历史会话</span>
            <button
              onClick={() => setShowSessions(false)}
              className="w-6 h-6 rounded text-slate-400 hover:text-white hover:bg-slate-700 transition text-xs"
            >
              ✕
            </button>
          </div>
          <div className="flex-1 overflow-y-auto scrollbar-thin p-2 space-y-1.5">
            {sessions.length === 0 && (
              <div className="text-center text-slate-500 text-xs py-8">暂无历史会话</div>
            )}
            {sessions.map((s) => (
              <div
                key={s.id}
                onClick={() => switchTo(s.id)}
                className={`group flex items-center gap-2 rounded-lg px-3 py-2.5 cursor-pointer transition ${
                  s.id === currentSessionId
                    ? "bg-sky-500/15 border border-sky-500/40"
                    : "bg-slate-800/60 border border-transparent hover:bg-slate-800"
                }`}
              >
                <div className="flex-1 min-w-0">
                  <div className="text-sm text-slate-100 truncate">{s.title || "新会话"}</div>
                  <div className="text-[11px] text-slate-500 mt-0.5">
                    {s.updated_at.slice(0, 16)} · {s.msg_count} 条
                  </div>
                </div>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    removeSession(s.id);
                  }}
                  className="opacity-0 group-hover:opacity-100 text-slate-500 hover:text-rose-400 text-sm px-1 transition"
                  title="删除会话"
                >
                  ✕
                </button>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

/** 工具调用提示的友好文案（原始工具名 → 用户可读的进行式描述） */
const TOOL_LABELS: Record<string, string> = {
  discover_capabilities: "发现所需能力",
  get_tool_call_history: "查询工具调用历史",
  scan_desktop: "扫描目录",
  search_files: "搜索本机文件",
  read_file: "读取文件",
  get_file_info: "查询文件属性",
  create_file: "创建文件",
  clear_temp_files: "清理临时文件",
  edit_file: "编辑文件",
  read_image: "看图理解",
  ocr_image: "OCR 提取文字",
  propose_organization: "生成整理方案",
  add_todo: "新建待办",
  list_todos: "查询待办",
  complete_todo: "完成待办",
  snooze_todo: "延后提醒",
  get_operation_history: "查询整理历史",
  undo_batch: "撤销整理批次",
  create_rule: "创建整理规则",
  toggle_rule: "切换规则状态",
  list_profile: "查询个人信息",
  save_profile_entry: "保存个人信息",
  delete_profile_entry: "删除个人信息",
  browser_navigate: "打开网页",
  browser_snapshot: "读取页面快照",
  browser_find: "查找页面控件",
  browser_network_observe: "观察网页接口",
  browser_request: "调用网页接口",
  browser_read: "提取页面正文",
  browser_click: "点击页面元素",
  browser_type: "输入文字",
  browser_scroll: "滚动页面",
  browser_tabs: "查询标签页",
  browser_activate_tab: "切换标签页",
  browser_screenshot: "页面截图",
  browser_evaluate: "执行页面脚本",
  browser_status: "诊断浏览器连接",
  find_browser_recipes: "查找浏览器经验",
  save_browser_recipe: "保存浏览器配方",
  run_browser_recipe: "运行浏览器配方",
  run_subagent: "子代理处理子任务",
  await_subagent: "等待子代理",
  web_search: "搜索网页",
  web_fetch: "抓取网页",
  run_command: "执行命令",
  run_command_background: "后台执行命令",
  await_task: "等待任务结束",
  check_task: "查询后台任务",
  list_tasks: "列出后台任务",
  stop_task: "停止后台任务",
  lookup_cache: "读写参考缓存",
  get_system_info: "查询本机信息",
  list_skills: "列出 Skills",
  load_skill: "加载 Skill",
  create_skill: "创建 Skill",
  delete_skill: "删除 Skill",
  manage_skill: "管理 Skill",
  list_workflows: "查看工作流",
  create_workflow: "创建工作流",
  manage_workflow: "管理工作流",
  ask_user: "等你确认",
  todo_write: "更新进度",
};

function toolFailureSummary(result: unknown): string {
  if (!result || typeof result !== "object") return "执行遇到问题，AI 正在根据结果尝试修正";
  const value = result as {
    error?: unknown;
    status?: unknown;
    exit_code?: unknown;
    guidance?: unknown;
  };
  if (typeof value.error === "string" && value.error.trim()) return value.error.trim();
  if (Array.isArray(value.guidance)) {
    const first = value.guidance.find((item): item is string => typeof item === "string" && !!item.trim());
    if (first) return first;
  }
  if (value.status === "timeout") return "命令执行超时，AI 正在调整执行方式";
  if (value.status === "failed") {
    return typeof value.exit_code === "number"
      ? `命令执行失败（退出码 ${value.exit_code}），AI 正在检查输出`
      : "命令执行失败，AI 正在检查输出";
  }
  return "执行遇到问题，AI 正在根据结果尝试修正";
}

function MessageBubble({
  m,
  activePlanId,
  onConfirmPlan,
  onCancelPlan,
  onAnswerAsk,
  onSkipAsk,
  onRecall,
}: {
  m: UiMessage;
  activePlanId: number | null;
  onConfirmPlan: (p: Plan) => void;
  onCancelPlan: (p: Plan) => void;
  onAnswerAsk: (answers: AskAnswer[]) => void;
  onSkipAsk: () => void;
  onRecall: (m: UiMessage) => void;
}) {
  if (m.role === "system") {
    return <div className="text-center text-[11px] text-slate-500 px-4 leading-5">{m.content}</div>;
  }
  if (m.role === "tool") {
    const label = TOOL_LABELS[m.toolName ?? ""] ?? m.toolName;
    return (
      <div
        className={`flex items-start gap-2 text-[11px] ${m.toolFailed ? "text-rose-400" : "text-slate-400"}`}
      >
        {m.streaming ? (
          <>
            <span className="inline-block w-3 h-3 rounded-full border-2 border-sky-400 border-t-transparent animate-spin" />
            {label}
          </>
        ) : m.toolFailed ? (
          <>
            <span className="shrink-0 text-rose-400">!</span>
            <span>
              <span>{label}未成功</span>
              {m.content && <span className="block text-rose-300/80">{m.content}</span>}
            </span>
          </>
        ) : (
          <>
            <span className="text-emerald-400">✓</span>
            {label}
          </>
        )}
      </div>
    );
  }
  if (m.role === "user") {
    return (
      <div className="group flex flex-col items-end">
        <div className="max-w-[85%] rounded-2xl rounded-tr-sm bg-gradient-to-r from-sky-500 to-indigo-600 text-white text-sm px-3 py-2 whitespace-pre-wrap break-words leading-6">
          {m.content}
        </div>
        {m.dbId != null && (
          <button
            onClick={() => onRecall(m)}
            className="mt-0.5 mr-1 text-[10px] text-slate-500 opacity-0 group-hover:opacity-100 hover:text-rose-400 transition"
          >
            撤回
          </button>
        )}
      </div>
    );
  }
  return (
    <div className="flex justify-start">
      <div className="max-w-[92%] rounded-2xl rounded-tl-sm bg-slate-800 text-slate-100 text-sm px-3 py-2 break-words leading-6 border border-slate-700/50">
        {m.reasoning &&
          (m.streaming && !m.content ? (
            <div className="mb-1.5 text-xs leading-5">
              <div className="flex items-center gap-1.5 text-slate-500 mb-1">
                <span className="inline-block w-2 h-2 rounded-full bg-sky-400 animate-pulse" />
                正在思考…
              </div>
              <div className="whitespace-pre-wrap break-words text-slate-400 border-l-2 border-sky-500/30 pl-2 max-h-36 overflow-y-auto scrollbar-thin">
                {m.reasoning}
              </div>
            </div>
          ) : (
            <details className="mb-1.5 text-xs">
              <summary className="cursor-pointer text-slate-500 hover:text-slate-300 select-none">
                思考过程
              </summary>
              <div className="mt-1 whitespace-pre-wrap break-words leading-5 text-slate-400 border-l-2 border-slate-600/60 pl-2 max-h-36 overflow-y-auto scrollbar-thin">
                {m.reasoning}
              </div>
            </details>
          ))}
        {m.content && <Markdown content={m.content} />}
        {m.streaming && <span className="inline-block w-1.5 h-3.5 bg-sky-400 ml-0.5 animate-pulse" />}
        {m.plan && (
          <PlanCard
            plan={m.plan}
            active={m.plan.id === activePlanId}
            onConfirm={() => onConfirmPlan(m.plan!)}
            onCancel={() => onCancelPlan(m.plan!)}
          />
        )}
        {m.ask && (
          <AskUserCard prompt={m.ask} active={!m.askSettled} onAnswer={onAnswerAsk} onSkip={onSkipAsk} />
        )}
      </div>
    </div>
  );
}

function PlanCard({
  plan,
  active,
  onConfirm,
  onCancel,
}: {
  plan: Plan;
  active: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const moveTotal = plan.categories
    .filter((c) => c.action !== "delete")
    .reduce((n, c) => n + c.files.length, 0);
  const delTotal = plan.categories
    .filter((c) => c.action === "delete")
    .reduce((n, c) => n + c.files.length, 0);
  const total = moveTotal + delTotal;
  const confirmLabel =
    delTotal > 0
      ? `确认执行（移动 ${moveTotal} · 删除 ${delTotal}）`
      : `确认执行（${total} 项）`;
  return (
    <div className="mt-2 rounded-lg border border-sky-500/30 bg-slate-900/70 p-2.5">
      {plan.summary && <div className="text-xs text-slate-300 mb-2">{plan.summary}</div>}
      <div className="space-y-1.5 max-h-52 overflow-y-auto scrollbar-thin">
        {plan.categories.map((c, i) =>
          c.action === "delete" ? (
            <details key={i} className="text-xs">
              <summary className="cursor-pointer text-rose-300 hover:text-rose-200 select-none">
                🗑 {c.name}（{c.files.length} 项）
              </summary>
              <div className="mt-1 ml-3 text-slate-400 space-y-0.5">
                <div className="text-rose-400/70">→ 回收站（可恢复）</div>
                {c.files.slice(0, 20).map((f, j) => (
                  <div key={j} className="truncate">
                    {f}
                  </div>
                ))}
                {c.files.length > 20 && <div className="text-slate-500">… 等 {c.files.length} 项</div>}
              </div>
            </details>
          ) : (
            <details key={i} className="text-xs">
              <summary className="cursor-pointer text-sky-300 hover:text-sky-200 select-none">
                {c.name}（{c.files.length} 项）
              </summary>
              <div className="mt-1 ml-3 text-slate-400 space-y-0.5">
                <div className="text-slate-500">→ {c.target_folder}</div>
                {c.files.slice(0, 20).map((f, j) => (
                  <div key={j} className="truncate">
                    {f}
                  </div>
                ))}
                {c.files.length > 20 && <div className="text-slate-500">… 等 {c.files.length} 项</div>}
              </div>
            </details>
          )
        )}
      </div>
      {delTotal > 0 && (
        <div className="mt-2 text-[11px] text-rose-300/90 leading-4">
          含 {delTotal} 项删除，执行后移入系统回收站（可手动恢复），请确认无误。
        </div>
      )}
      {active ? (
        <div className="flex gap-2 mt-2.5">
          <button
            onClick={onConfirm}
            className={`flex-1 py-1.5 rounded text-white text-xs font-medium hover:opacity-90 transition ${
              delTotal > 0
                ? "bg-gradient-to-r from-rose-500 to-red-600"
                : "bg-gradient-to-r from-sky-500 to-indigo-600"
            }`}
          >
            {confirmLabel}
          </button>
          <button
            onClick={onCancel}
            className="px-3 py-1.5 rounded bg-slate-700 text-slate-300 text-xs hover:bg-slate-600 transition"
          >
            取消
          </button>
        </div>
      ) : (
        <div className="mt-2 text-[11px] text-slate-500">该方案已处理（{plan.status}）</div>
      )}
    </div>
  );
}

function AskUserCard({
  prompt,
  active,
  onAnswer,
  onSkip,
}: {
  prompt: AskUserPrompt;
  active: boolean;
  onAnswer: (answers: AskAnswer[]) => void;
  onSkip: () => void;
}) {
  const [picked, setPicked] = useState<Record<string, string[]>>({});
  const [custom, setCustom] = useState<Record<string, string>>({});

  const toggle = (question: AskQuestion, label: string) => {
    setPicked((prev) => {
      const cur = prev[question.id] ?? [];
      if (question.multi_select) {
        const next = cur.includes(label) ? cur.filter((x) => x !== label) : [...cur, label];
        return { ...prev, [question.id]: next };
      }
      return { ...prev, [question.id]: [label] };
    });
  };

  const ready = prompt.questions.every((q) => {
    const selected = picked[q.id] ?? [];
    const note = (custom[q.id] ?? "").trim();
    if (q.options.length === 0) return note.length > 0;
    return selected.length > 0 || note.length > 0;
  });

  const submit = () => {
    if (!ready) return;
    onAnswer(
      prompt.questions.map((q) => {
        const note = (custom[q.id] ?? "").trim();
        return {
          id: q.id,
          selected: picked[q.id] ?? [],
          custom: note || null,
        };
      })
    );
  };

  const onlySingleChoices =
    prompt.questions.length === 1 &&
    prompt.questions[0].options.length > 0 &&
    !prompt.questions[0].multi_select;

  return (
    <div className="mt-2 rounded-lg border border-sky-500/30 bg-slate-900/70 p-2.5">
      <div className="space-y-3">
        {prompt.questions.map((q) => (
          <div key={q.id}>
            {q.header && <div className="text-[11px] text-sky-300 mb-0.5">{q.header}</div>}
            <div className="text-xs text-slate-200 mb-1.5 leading-5">{q.question}</div>
            {q.options.length > 0 && (
              <div className="flex flex-col gap-1">
                {q.options.map((opt) => {
                  const on = (picked[q.id] ?? []).includes(opt.label);
                  return (
                    <button
                      key={opt.label}
                      type="button"
                      disabled={!active}
                      onClick={() => {
                        if (onlySingleChoices) {
                          onAnswer([{ id: q.id, selected: [opt.label], custom: null }]);
                          return;
                        }
                        toggle(q, opt.label);
                      }}
                      className={`text-left rounded-md px-2.5 py-1.5 text-xs border transition ${
                        on
                          ? "border-sky-400 bg-sky-500/20 text-sky-100"
                          : "border-slate-600 bg-slate-800/80 text-slate-200 hover:border-sky-500/50"
                      } disabled:opacity-50`}
                    >
                      <span className="font-medium">{opt.label}</span>
                      {opt.description && (
                        <span className="block text-[11px] text-slate-400 mt-0.5 leading-4">
                          {opt.description}
                        </span>
                      )}
                    </button>
                  );
                })}
              </div>
            )}
            {active && (
              <input
                value={custom[q.id] ?? ""}
                onChange={(e) => setCustom((p) => ({ ...p, [q.id]: e.target.value }))}
                placeholder={q.options.length ? "或写一句" : "用一句话说明"}
                className="mt-1.5 w-full bg-slate-800/80 text-slate-100 text-xs rounded-md px-2.5 py-1.5 outline-none border border-slate-600 focus:border-sky-500 placeholder:text-slate-500"
              />
            )}
          </div>
        ))}
      </div>
      {active ? (
        <div className="flex gap-2 mt-2.5">
          {(!onlySingleChoices || prompt.questions.some((q) => (custom[q.id] ?? "").trim())) && (
            <button
              type="button"
              disabled={!ready}
              onClick={submit}
              className="flex-1 py-1.5 rounded text-white text-xs font-medium bg-gradient-to-r from-sky-500 to-indigo-600 hover:opacity-90 disabled:opacity-40 transition"
            >
              确定
            </button>
          )}
          <button
            type="button"
            onClick={onSkip}
            className="px-3 py-1.5 rounded bg-slate-700 text-slate-300 text-xs hover:bg-slate-600 transition"
          >
            先不答
          </button>
        </div>
      ) : (
        <div className="mt-2 text-[11px] text-slate-500">已记下你的选择</div>
      )}
    </div>
  );
}

function SessionProgressBar({ items }: { items: SessionTodo[] }) {
  const [open, setOpen] = useState(() => items.some((i) => i.status === "in_progress"));
  const hasActive = items.some((i) => i.status === "in_progress");

  useEffect(() => {
    if (hasActive) setOpen(true);
  }, [hasActive]);

  if (items.length === 0) return null;

  const done = items.filter((i) => i.status === "completed").length;
  const current = items.find((i) => i.status === "in_progress");
  const pct = Math.round((done / items.length) * 100);
  const headline = current
    ? current.content
    : done === items.length
      ? "这一步都做完了"
      : "下一步还没开始";

  return (
    <div className="shrink-0 border-b border-slate-700/60 bg-slate-800/70 px-3 py-2">
      <button type="button" onClick={() => setOpen((v) => !v)} className="w-full text-left">
        <div className="flex items-center gap-2">
          <div className="flex-1 h-1 rounded-full bg-slate-700 overflow-hidden">
            <div
              className="h-full bg-gradient-to-r from-sky-500 to-indigo-500 transition-all"
              style={{ width: `${pct}%` }}
            />
          </div>
          <span className="text-[11px] text-slate-400 tabular-nums shrink-0">
            {done}/{items.length}
          </span>
        </div>
        <div className="mt-1 flex items-center justify-between gap-2">
          <span className="text-[11px] text-slate-300 truncate">{headline}</span>
          <span className="text-slate-500 text-[10px] shrink-0">{open ? "▴" : "▾"}</span>
        </div>
      </button>
      {open && (
        <ul className="mt-1.5 space-y-1">
          {items.map((item) => {
            const mark =
              item.status === "completed" ? "✓" : item.status === "in_progress" ? "●" : "○";
            const color =
              item.status === "completed"
                ? "text-slate-500 line-through"
                : item.status === "in_progress"
                  ? "text-sky-200"
                  : "text-slate-300";
            const markColor =
              item.status === "completed"
                ? "text-emerald-400"
                : item.status === "in_progress"
                  ? "text-sky-400"
                  : "text-slate-500";
            return (
              <li key={item.content} className="flex items-start gap-1.5 text-[11px] leading-4">
                <span className={`${markColor} shrink-0`}>{mark}</span>
                <span className={color}>{item.content}</span>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
