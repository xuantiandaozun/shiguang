import { useCallback, useEffect, useRef, useState } from "react";
import Icon from "../components/Icon";
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
import ReplyActions from "../components/ReplyActions";
import ChatSlashMenu from "../components/ChatSlashMenu";
import { startVoiceRecording, type VoiceRecorder } from "../lib/audio";
import type { DeepSeekMessageMeta, Settings } from "../lib/ipc";
import { isDeepSeekBaseUrl } from "../lib/providers";
import { startPluginRegistry, usePluginSlashCommands, usePluginToolCards, useToolLabels } from "../plugins/registry";
import PluginMount from "../plugins/PluginMount";
import appIcon from "../assets/brand-mark.svg";
import {
  buildSlashCommands,
  filterSlashCommands,
  resolveSlashLine,
  type SlashCommand,
} from "../lib/slash-commands";
import type { AutomationWorkflow } from "../lib/ipc";

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

/** 后台任务/子代理结束时系统自动发给 AI 的消息前缀（与 chat_queue.rs 一致），界面按通知显示 */
const EVENT_PREFIX = "【后台通知】";
const eventNotice = (content: string) => `🔔 ${content.slice(EVENT_PREFIX.length).split("\n")[0]}`;

const mapRow = (r: ChatMsg): UiMessage | null => {
  if (r.role === "user" && r.content.startsWith(EVENT_PREFIX)) {
    return { id: `h-${r.id}`, dbId: r.id, role: "system", content: eventNotice(r.content) };
  }
  const content = r.role === "user" ? r.content : visibleAssistantContent(r.content);
  if (r.role !== "user" && !content) return null;
  let meta: DeepSeekMessageMeta = {};
  if (r.meta_json && r.meta_json !== "{}") {
    try {
      meta = JSON.parse(r.meta_json) as DeepSeekMessageMeta;
    } catch {
      meta = {};
    }
  }
  return {
    id: `h-${r.id}`,
    dbId: r.id,
    role: r.role === "user" ? "user" : "assistant",
    content,
    reasoning: meta.reasoning_content || undefined,
    finishReason: meta.finish_reason,
    refused: meta.refused,
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
  useEffect(() => startPluginRegistry(), []);
  const {
    messages,
    streaming,
    pendingPlan,
    sessionTodos,
    setMessages,
    addMessage,
    attachDbId,
    recallLocal,
    removeMessageLocal,
    appendToken,
    appendReasoning,
    startContinueMessage,
    appendContinueToken,
    finishContinueMessage,
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
  const [chatSettings, setChatSettings] = useState<Settings | null>(null);
    const [recordingSec, setRecordingSec] = useState<number | null>(null);
  const [transcribing, setTranscribing] = useState(false);
  const recorderRef = useRef<VoiceRecorder | null>(null);
  const stoppingVoiceRef = useRef(false);
  /** AI 回复期间发出、还没被读到的追问（显示文字） */
  const [queued, setQueued] = useState<string[]>([]);
  const queuedRef = useRef<string[]>([]);
  const [showSessions, setShowSessions] = useState(false);
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [currentSessionId, setCurrentSessionId] = useState<number | null>(null);
  const workflows: AutomationWorkflow[] = [];
  const [slashIndex, setSlashIndex] = useState(0);
  const currentSessionIdRef = useRef<number | null>(null);
  currentSessionIdRef.current = currentSessionId;
  const listRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  const slashQuery = input.startsWith("/") ? input.slice(1).split(/\s/)[0] ?? "" : null;
  const slashOpen = slashQuery !== null && !streaming;
  const pluginSlashCommands = usePluginSlashCommands();
  const slashCommands = buildSlashCommands(workflows, pluginSlashCommands);
  const filteredSlash = slashOpen ? filterSlashCommands(slashCommands, slashQuery) : [];

  useEffect(() => {
    setSlashIndex(0);
  }, [slashQuery, workflows.length]);



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
    add<{ db_id: number; delta: string }>("deepseek-continue-token", (p) =>
      appendContinueToken(p.db_id, p.delta),
    );
    add<{ db_id: number; content: string; meta?: DeepSeekMessageMeta }>(
      "deepseek-continue-done",
      (p) => finishContinueMessage(p.db_id, p.content, p.meta),
    );
    add<{ content: string; db_id?: number }>("llm-message-done", (p) => {
      finishStreaming(p.content, p.db_id);
    });
    add("settings-changed", () => {
      ipc.getSettings().then(setChatSettings).catch(() => {});
    });
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
    // 一轮回复开始（含排队追问、后台通知自动触发的轮次）：补上流式占位
    add<{ session_id: number }>("chat-turn-started", () => {
      if (useChatStore.getState().streaming) return;
      addMessage({ role: "assistant", content: "", streaming: true });
      setStreaming(true);
    });
    // 排队的追问/后台通知被 AI 读到（或停止时入库）：此时才放进对话，顺序与 AI 所见一致
    add<{ session_id: number; content: string; id: number; event: boolean; answering: boolean }>(
      "chat-queued-absorbed",
      (p) => {
        if (p.session_id !== currentSessionIdRef.current) return;
        if (p.event) {
          addMessage({ role: "system", content: eventNotice(p.content), dbId: p.id });
          return;
        }
        const display = queuedRef.current.shift() ?? p.content;
        setQueued([...queuedRef.current]);
        if (p.answering) {
          const msgs = useChatStore.getState().messages;
          const last = msgs[msgs.length - 1];
          if (last && last.role === "assistant" && last.streaming && !last.content && !last.reasoning) {
            setMessages(msgs.slice(0, -1));
          }
        }
        addMessage({ role: "user", content: display, dbId: p.id });
        if (p.answering) addMessage({ role: "assistant", content: "", streaming: true });
      },
    );
    add<{ session_id: number; content: string }>("subagent-chat", (p) => {
      if (p.session_id === currentSessionIdRef.current) {
        addMessage({ role: "system", content: p.content });
      }
    });
    add<{ name: string; status: string; result?: unknown }>("tool-status", (p) => {
      if (p.name === "todo_write") return;
      if (p.status === "running") {
        addMessage({ role: "tool", toolName: p.name, content: "", streaming: true });
      } else if (p.status === "done" || p.status === "error" || p.status === "cancelled") {
        completeTool(
          p.name,
          p.status === "cancelled"
            ? "已中断"
            : p.status === "error"
              ? toolFailureSummary(p.result)
              : undefined,
          p.status === "done" ? p.result : undefined,
        );
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
    // 别的窗口（如录屏编辑）把要求填进输入框，由用户确认后发送
    add<{ text: string }>("chat-prefill", (p) => {
      setInput(p.text);
      window.setTimeout(() => inputRef.current?.focus(), 80);
    });

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
    ipc.getSettings().then(setChatSettings).catch(() => {});

    return () => {
      disposed = true;
      recorderRef.current?.cancel();
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

  const runWorkflowFromChat = async (name: string, workflowId: number) => {
    setInput("");
    setAttachments([]);
    const display = `/${name}`;
    const localId = addMessage({ role: "user", content: display });
    addMessage({ role: "assistant", content: "", streaming: true });
    setStreaming(true);
    setSessionTodos([]);
    try {
      const dbId = await ipc.runAutomationWorkflow(workflowId);
      attachDbId(localId, dbId);
    } catch (e) {
      finishStreaming();
      addMessage({ role: "system", content: `工作流启动失败：${String(e)}` });
    }
  };

  const executeSlashCommand = async (cmd: SlashCommand) => {
    setInput("");
    if (cmd.kind === "workflow" && cmd.workflowId != null) {
      await runWorkflowFromChat(cmd.label, cmd.workflowId);
      return;
    }
    if (cmd.kind === "prompt") {
      setInput(cmd.prompt ?? "");
      return;
    }
    if (cmd.id === "help") {
      const lines = slashCommands
        .filter((c) => c.kind === "workflow" || c.kind === "prompt")
        .map((c) => `· /${c.trigger}${c.description ? ` — ${c.description}` : ""}`);
      addMessage({
        role: "system",
        content:
          lines.length > 0
            ? `快捷命令（输入 / 搜索）：\n${lines.join("\n")}`
            : "暂无工作流。在主面板「工作流」页创建后，可用 /工作流名 快速运行。",
      });
    }
  };

  const send = async () => {
    const text = input.trim();
    const files = [...attachments];
    if (!text && files.length === 0) return;

    const slash = text && !streaming ? resolveSlashLine(text, slashCommands) : null;
    if (slash) {
      setInput("");
      setAttachments([]);
      await executeSlashCommand(slash);
      return;
    }

    setInput("");
    setAttachments([]);
    // 本地气泡展示：文字 + 附件文件名（后端入库会附带完整路径说明）
    let display = text;
    if (files.length) {
      const names = files.map((p) => `📎 ${fileName(p)}`).join("\n");
      display = text ? `${text}\n\n${names}` : names;
    }
    if (streaming) {
      // 追问：先显示在输入框上方排队，AI 下一步读到时再放进对话（见 chat-queued-absorbed）
      queuedRef.current.push(display);
      setQueued([...queuedRef.current]);
      try {
        const id = await ipc.sendChat(text, files.length ? files : undefined);
        // 恰好赶上空闲（上一轮刚结束）：直接作为新一轮发出
        if (id > 0) {
          const i = queuedRef.current.indexOf(display);
          if (i >= 0) queuedRef.current.splice(i, 1);
          setQueued([...queuedRef.current]);
          const msgs = useChatStore.getState().messages;
          const last = msgs[msgs.length - 1];
          const placeholder = last && last.role === "assistant" && last.streaming && !last.content && !last.reasoning;
          if (placeholder) setMessages(msgs.slice(0, -1));
          addMessage({ role: "user", content: display, dbId: id });
          if (placeholder) addMessage({ role: "assistant", content: "", streaming: true });
        }
      } catch (e) {
        const i = queuedRef.current.indexOf(display);
        if (i >= 0) queuedRef.current.splice(i, 1);
        setQueued([...queuedRef.current]);
        addMessage({ role: "system", content: `发送失败：${String(e)}` });
      }
      return;
    }
    const localId = addMessage({ role: "user", content: display });
    addMessage({ role: "assistant", content: "", streaming: true });
    setStreaming(true);
    setSessionTodos([]);
    try {
      const dbId = await ipc.sendChat(text, files.length ? files : undefined);
      if (dbId > 0) attachDbId(localId, dbId);
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

  const patchChatOptions = async (patch: Parameters<typeof ipc.setDeepSeekChatOptions>[0]) => {
    try {
      const next = await ipc.setDeepSeekChatOptions(patch);
      setChatSettings(next);
    } catch (e) {
      addMessage({ role: "system", content: `切换设置失败：${String(e)}` });
    }
  };

  const stopAndTranscribeVoice = async () => {
    if (stoppingVoiceRef.current) return;
    const rec = recorderRef.current;
    if (!rec) return;
    stoppingVoiceRef.current = true;
    recorderRef.current = null;
    setRecordingSec(null);
    setTranscribing(true);
    try {
      const { base64 } = await rec.stop();
      const res = await ipc.deepseekAsr(base64, "voice.wav");
      const text = res.text?.trim();
      if (text) {
        setInput((prev) => (prev ? `${prev} ${text}` : text));
        window.setTimeout(() => inputRef.current?.focus(), 40);
      } else {
        addMessage({ role: "system", content: "未识别到有效语音内容。" });
      }
    } catch (e) {
      addMessage({ role: "system", content: `语音识别失败：${String(e)}` });
    } finally {
      setTranscribing(false);
      stoppingVoiceRef.current = false;
    }
  };

  const toggleVoiceInput = async () => {
    if (transcribing) return;
    if (recorderRef.current) {
      await stopAndTranscribeVoice();
      return;
    }
    try {
      const rec = await startVoiceRecording(
        (sec) => setRecordingSec(sec),
        () => { void stopAndTranscribeVoice(); },
      );
      recorderRef.current = rec;
      setRecordingSec(0);
    } catch (e) {
      addMessage({ role: "system", content: `无法启动录音：${String(e)}` });
    }
  };

  const handleContinue = async (dbId: number) => {
    if (streaming) return;
    startContinueMessage(dbId);
    try {
      await ipc.continueChatMessage(dbId);
    } catch (e) {
      finishStreaming();
      addMessage({ role: "system", content: `继续生成失败：${String(e)}` });
    }
  };

  const handleRegenerate = async (m: UiMessage) => {
    if (streaming || m.dbId == null) return;
    removeMessageLocal(m.id);
    addMessage({ role: "assistant", content: "", streaming: true });
    setStreaming(true);
    setSessionTodos([]);
    try {
      await ipc.regenerateChatMessage(m.dbId);
    } catch (e) {
      finishStreaming();
      addMessage({ role: "system", content: `重新生成失败：${String(e)}` });
    }
  };

  const isDeepSeekMain = isDeepSeekBaseUrl(chatSettings?.base_url);
  const lastAssistantId = [...messages].reverse().find((x) => x.role === "assistant" && x.dbId != null)?.id;
  const toolLabels = useToolLabels();
  const runningToolName = [...messages]
    .reverse()
    .find((x) => x.role === "tool" && x.streaming)?.toolName;
  const runningToolLabel = runningToolName ? (toolLabels[runningToolName] ?? runningToolName) : undefined;

  return (
    <div className="relative h-full flex flex-col rounded-2xl overflow-hidden bg-slate-900/95 backdrop-blur border border-slate-700/60 shadow-2xl">
      <div
        data-tauri-drag-region
        className="h-11 shrink-0 flex items-center justify-between px-3 bg-slate-800/80 border-b border-slate-700/60 cursor-move"
      >
        <div data-tauri-drag-region className="flex items-center gap-2 min-w-0">
          <img src={appIcon} alt="拾光" className="w-5 h-5 rounded-full shrink-0" draggable={false} />
          <span data-tauri-drag-region className="text-[#1f2329] text-sm font-semibold tracking-tight">
            拾光
          </span>
          {streaming && (
            <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded-full bg-[#fff4e2] border border-[#e8a23a]/60 text-[10px] font-medium text-[#875318]">
              <span className="w-1.5 h-1.5 rounded-full bg-[#c98428] animate-pulse" />
              运行中
            </span>
          )}
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={createSession}
            className="w-7 h-7 rounded-md flex items-center justify-center text-[#646a73] hover:text-[#1f2329] hover:bg-[#f3f4f6] transition"
            title="新会话"
          >
            <Icon name="plus" />
          </button>
          <button
            onClick={() => {
              setShowSessions((v) => !v);
              refreshSessions();
            }}
            title="历史会话"
            className={`w-7 h-7 rounded-md flex items-center justify-center transition ${
              showSessions
                ? "bg-[#fff4e2] text-[#a36313]"
                : "text-[#646a73] hover:text-[#1f2329] hover:bg-[#f3f4f6]"
            }`}
          >
            <Icon name="history" /><span className="sr-only">历史会话</span>
          </button>
          <button
            title="主面板"
            onClick={() => ipc.openMain().catch(() => {})}
            className="w-7 h-7 rounded-md flex items-center justify-center text-[#646a73] hover:text-[#1f2329] hover:bg-[#f3f4f6] transition"
          >
            <Icon name="settings" /><span className="sr-only">主面板</span>
          </button>
          <button
            onClick={() => ipc.hideChat().catch(() => {})}
            className="w-7 h-7 rounded-md flex items-center justify-center text-[#646a73] hover:text-[#1f2329] hover:bg-[#f3f4f6] transition text-xs"
            title="收起"
          >
            —
          </button>
        </div>
      </div>

      <SessionProgressBar items={sessionTodos} />

      <div ref={listRef} className="flex-1 overflow-y-auto scrollbar-thin px-3 py-3 space-y-3">
        {messages.length === 0 && (
          <div className="text-[#646a73] text-xs text-center mt-14 leading-7 px-4">
            <div className="text-sm font-medium text-[#1f2329] mb-1">有什么我可以帮你的？</div>
            试试对我说：
            <br />
            「帮我整理一下桌面」 · 「明天下午三点提醒我交周报」
            <br />
            <span className="text-[#8c939d]">输入 / 可快速运行工作流，支持拖入或粘贴文件</span>
          </div>
        )}
        {messages.map((m) => (
          <MessageBubble
            key={m.id}
            m={m}
            deepSeekEnabled={isDeepSeekMain}
            isLastAssistant={m.id === lastAssistantId}
            streaming={streaming}
            activePlanId={pendingPlan?.id ?? null}
            onConfirmPlan={confirmPlan}
            onCancelPlan={cancelPlan}
            onAnswerAsk={answerAsk}
            onSkipAsk={skipAsk}
            onRecall={recall}
            onContinue={handleContinue}
            onRegenerate={handleRegenerate}
          />
        ))}
        <div className="h-1" />
      </div>

      {(streaming || transcribing) && (
        <div className="shrink-0 mx-2.5 mb-1.5 px-3 py-2 rounded-xl border border-[#e8a23a]/50 bg-[#fffaf0] flex items-center justify-between gap-2 shadow-sm">
          <div className="flex items-center gap-2 min-w-0">
            <span className="relative flex h-2.5 w-2.5 shrink-0">
              <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-[#e8a23a] opacity-75" />
              <span className="relative inline-flex rounded-full h-2.5 w-2.5 bg-[#c98428]" />
            </span>
            <span className="text-xs font-medium text-[#875318] truncate">
              {transcribing
                  ? "正在识别语音输入…"
                  : cancelling
                    ? "正在中断当前回复…"
                    : runningToolLabel
                      ? `正在执行：${runningToolLabel}`
                      : "拾光正在思考与处理…"}
            </span>
          </div>
          {streaming && (
            <button
              type="button"
              onClick={stop}
              disabled={cancelling}
              className="shrink-0 inline-flex items-center gap-1.5 px-2.5 py-1 rounded-lg border border-rose-200 bg-white text-rose-600 hover:bg-rose-50 hover:border-rose-300 text-xs font-medium disabled:opacity-50 transition"
              title="停止当前生成与任务"
            >
              <span className="inline-block w-2 h-2 rounded-[2px] bg-rose-500" />
              {cancelling ? "中断中" : "停止"}
            </button>
          )}
        </div>
      )}

      <div className="shrink-0 px-2.5 pb-2.5 pt-1 bg-white relative z-20 overflow-visible">
        {slashOpen && (
          <div className="absolute bottom-full left-2 right-2 z-30 mb-1">
            <ChatSlashMenu
              commands={filteredSlash}
              activeIndex={Math.min(slashIndex, Math.max(0, filteredSlash.length - 1))}
              onPick={(cmd) => void executeSlashCommand(cmd)}
            />
          </div>
        )}
        {dragOver && (
          <div className="absolute inset-0 z-10 flex items-center justify-center rounded-b-2xl bg-[#fff4e2]/90 border-2 border-dashed border-[#e8a23a] pointer-events-none">
            <span className="text-sm text-[#875318] font-medium">松开以添加文件</span>
          </div>
        )}
        {queued.length > 0 && (
          <div className="mb-2 flex flex-col gap-1">
            {queued.map((q, i) => (
              <div
                key={`${i}-${q}`}
                title="AI 做完当前这一步就会看到"
                className="flex items-center gap-1.5 px-2.5 py-1 rounded-lg bg-[#fff8ec] border border-[#e8a23a]/50 text-[11px] text-[#875318]"
              >
                <span className="shrink-0 font-medium">排队中</span>
                <span className="truncate">{q}</span>
              </div>
            ))}
          </div>
        )}
        {attachments.length > 0 && (
          <div className="flex flex-wrap items-center gap-1.5 mb-2">
            {attachments.map((p) => (
              <span
                key={p}
                className="inline-flex items-center gap-1 max-w-full px-2 py-1 rounded-lg bg-[#f8f9fb] border border-[#e2e5e9] text-[11px] text-[#1f2329]"
                title={p}
              >
                <span className="truncate">📎 {fileName(p)}</span>
                <button
                  onClick={() => setAttachments((prev) => prev.filter((x) => x !== p))}
                  className="shrink-0 text-[#8c939d] hover:text-rose-600 leading-none"
                  title="移除"
                >
                  ×
                </button>
              </span>
            ))}
          </div>
        )}
        <div className="rounded-xl border border-[#dfe3e8] bg-[#f8f9fb] focus-within:border-[#c98428] focus-within:bg-white transition shadow-sm">
          <textarea
            ref={inputRef}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (slashOpen) {
                if (e.key === "ArrowDown" && filteredSlash.length > 0) {
                  e.preventDefault();
                  setSlashIndex((i) => Math.min(i + 1, filteredSlash.length - 1));
                  return;
                }
                if (e.key === "ArrowUp" && filteredSlash.length > 0) {
                  e.preventDefault();
                  setSlashIndex((i) => Math.max(i - 1, 0));
                  return;
                }
                if (e.key === "Escape") {
                  e.preventDefault();
                  setInput("");
                  return;
                }
                if (e.key === "Tab" && filteredSlash.length > 0) {
                  e.preventDefault();
                  const cmd = filteredSlash[Math.min(slashIndex, filteredSlash.length - 1)];
                  if (cmd) setInput(`/${cmd.trigger}`);
                  return;
                }
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  if (filteredSlash.length > 0) {
                    const cmd = filteredSlash[Math.min(slashIndex, filteredSlash.length - 1)];
                    void executeSlashCommand(cmd);
                  }
                  return;
                }
              }
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                send();
              }
            }}
            rows={1}
            placeholder={streaming ? "可以继续补充，AI 下一步会看到…" : "和拾光说点什么… 输入 / 运行工作流"}
            className="chat-input w-full resize-none scrollbar-thin bg-transparent text-[#1f2329] text-sm px-3 pt-2.5 pb-1.5 outline-none border-0 placeholder:text-[#8c939d] min-h-[38px] max-h-28 block"
          />
          <div className="flex items-center justify-between gap-2 px-2 pb-2 pt-1 border-t border-[#edf0f3]">
            <div className="flex items-center gap-1 flex-wrap min-w-0">
              <button
                type="button"
                onClick={pickFiles}
                title="添加文件（也可粘贴或拖入）"
                className="shrink-0 h-7 w-7 rounded-md text-[#646a73] hover:text-[#1f2329] hover:bg-[#edf0f3] transition flex items-center justify-center"
              >
                <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
                </svg>
              </button>
              {isDeepSeekMain && (
                <button
                  type="button"
                  onClick={() => void toggleVoiceInput()}
                  disabled={transcribing}
                  title={
                    recordingSec !== null
                      ? "点击停止录音并转写文字（最长 120 秒）"
                      : transcribing
                        ? "正在识别语音…"
                        : "DeepSeek 语音输入"
                  }
                  className={`shrink-0 h-7 rounded-md border transition flex items-center justify-center ${
                    recordingSec !== null
                      ? "px-2 gap-1 bg-rose-50 border-rose-300 text-rose-700"
                      : "w-7 border-transparent text-[#646a73] hover:text-[#1f2329] hover:bg-[#edf0f3]"
                  } disabled:opacity-40`}
                >
                  {transcribing ? (
                    <span className="inline-block w-3.5 h-3.5 rounded-full border-2 border-[#c98428] border-t-transparent animate-spin" />
                  ) : (
                    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
                      <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
                      <line x1="12" y1="19" x2="12" y2="23" />
                      <line x1="8" y1="23" x2="16" y2="23" />
                    </svg>
                  )}
                  {recordingSec !== null && (
                    <span className="text-[11px] tabular-nums">
                      {Math.floor(recordingSec / 60)}:{String(recordingSec % 60).padStart(2, "0")}
                    </span>
                  )}
                </button>
              )}
              {isDeepSeekMain && chatSettings && (
                <>
                  <button
                    type="button"
                    disabled={streaming}
                    onClick={() =>
                      void patchChatOptions({ thinking_enabled: !chatSettings.thinking_enabled })
                    }
                    className={`h-7 px-2 rounded-md border text-[11px] transition ${
                      chatSettings.thinking_enabled
                        ? "border-[#e8a23a] bg-[#fff4e2] text-[#875318] font-medium"
                        : "border-transparent text-[#646a73] hover:bg-[#edf0f3] hover:text-[#1f2329]"
                    } disabled:opacity-50`}
                    title="开启/关闭 DeepSeek 深度思考"
                  >
                    深度思考
                  </button>
                </>
              )}
            </div>
            <button
              type="button"
              onClick={send}
              disabled={!input.trim() && attachments.length === 0}
              title={streaming ? "AI 会在下一步看到这条消息，先回应你再继续" : "发送 (Enter)"}
              className="shrink-0 h-7 px-3 rounded-lg bg-[#b57520] hover:bg-[#9f651a] text-white text-xs font-medium disabled:opacity-35 transition"
            >
              发送
            </button>
          </div>
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

// 工具调用提示的友好文案来自插件（tools.json 的 label 与 ui.tool_labels）。

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
  deepSeekEnabled,
  isLastAssistant,
  streaming,
  activePlanId,
  onConfirmPlan,
  onCancelPlan,
  onAnswerAsk,
  onSkipAsk,
  onRecall,
  onContinue,
  onRegenerate,
}: {
  m: UiMessage;
  deepSeekEnabled: boolean;
  isLastAssistant: boolean;
  streaming: boolean;
  activePlanId: number | null;
  onConfirmPlan: (p: Plan) => void;
  onCancelPlan: (p: Plan) => void;
  onAnswerAsk: (answers: AskAnswer[]) => void;
  onSkipAsk: () => void;
  onRecall: (m: UiMessage) => void;
  onContinue: (dbId: number) => void;
  onRegenerate: (m: UiMessage) => void;
}) {
  const toolLabels = useToolLabels();
  const toolCards = usePluginToolCards();
  if (m.role === "system") {
    return <div className="text-center text-[11px] text-slate-500 px-4 leading-5">{m.content}</div>;
  }
  if (m.role === "tool") {
    const label = toolLabels[m.toolName ?? ""] ?? m.toolName;
    const status = (
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
    // 插件声明了该工具的结果卡片（ui.tool_cards）时，在状态行下方渲染。
    const card = !m.streaming && !m.toolFailed && m.toolResult !== undefined ? toolCards.find((c) => c.tool === m.toolName) : undefined;
    if (!card) return status;
    return (
      <div className="space-y-1.5">
        {status}
        <div className="rounded-lg border border-slate-700 overflow-hidden">
          <PluginMount item={card} slot="tool_card" params={{ tool: m.toolName, result: m.toolResult }} />
        </div>
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
        {m.refused ? (
          <div className="rounded-lg border border-amber-300 bg-amber-50 px-2.5 py-2 text-xs text-amber-900 leading-5">
            {m.content}
          </div>
        ) : (
          m.content && <Markdown content={m.content} />
        )}
        {m.content && !m.streaming && (
          <ReplyActions
            content={m.content}
            deepSeekEnabled={deepSeekEnabled}
            disabled={streaming}
            canContinue={Boolean(isLastAssistant && m.dbId != null && !m.refused)}
            onContinue={m.dbId != null ? () => onContinue(m.dbId!) : undefined}
            onRegenerate={isLastAssistant && m.dbId != null ? () => onRegenerate(m) : undefined}
          />
        )}
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
            {q.details && (
              <pre className="mb-1.5 max-h-48 overflow-y-auto scrollbar-thin whitespace-pre-wrap break-words rounded-md bg-slate-800/80 px-2.5 py-1.5 text-[11px] leading-4 text-slate-300 font-sans">
                {q.details}
              </pre>
            )}
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
  const [open, setOpen] = useState(false);

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
    <div className="session-progress shrink-0 border-b border-slate-700/60 bg-slate-800/70 px-3 py-1.5">
      <button type="button" onClick={() => setOpen((v) => !v)} className="w-full text-left">
        <div className="flex items-center gap-2">
          <div className="w-14 h-[3px] rounded-full bg-slate-700 overflow-hidden shrink-0">
            <div
              className="h-full bg-gradient-to-r from-sky-500 to-indigo-500 transition-all"
              style={{ width: `${pct}%` }}
            />
          </div>
          <span className="flex-1 text-[11px] text-slate-300 truncate">{headline}</span>
          <span className="text-[11px] text-slate-400 tabular-nums shrink-0">
            {done}/{items.length}
          </span>
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
