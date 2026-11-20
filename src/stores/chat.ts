import { create } from "zustand";
import type {
  AskUserPrompt,
  DeepSeekMessageMeta,
  Plan,
  SessionTodo,
} from "../lib/ipc";

export interface UiMessage {
  id: string;
  dbId?: number;
  role: "user" | "assistant" | "tool" | "system";
  content: string;
  /** 思考模式下的思维链 */
  reasoning?: string;
  toolName?: string;
  toolFailed?: boolean;
  /** 工具结果（仅本次会话内存中，用于插件声明的结果卡片） */
  toolResult?: unknown;
  plan?: Plan;
  ask?: AskUserPrompt;
  askSettled?: boolean;
  streaming?: boolean;
  finishReason?: string;
  refused?: boolean;
}

let seq = 0;
const nid = () => `m${++seq}-${Date.now()}`;

interface ChatState {
  messages: UiMessage[];
  streaming: boolean;
  pendingPlan: Plan | null;
  pendingAsk: AskUserPrompt | null;
  sessionTodos: SessionTodo[];
  setMessages: (m: UiMessage[]) => void;
  addMessage: (m: Omit<UiMessage, "id">) => string;
  attachDbId: (localId: string, dbId: number) => void;
  recallLocal: (localId: string) => void;
  removeMessageLocal: (localId: string) => void;
  appendToken: (d: string) => void;
  appendReasoning: (d: string) => void;
  startContinueMessage: (dbId: number) => void;
  appendContinueToken: (dbId: number, delta: string) => void;
  finishContinueMessage: (dbId: number, content: string, meta?: DeepSeekMessageMeta) => void;
  finishStreaming: (content?: string, dbId?: number) => void;
  completeTool: (toolName: string, error?: string, result?: unknown) => void;
  setStreaming: (b: boolean) => void;
  setPendingPlan: (p: Plan | null) => void;
  setPendingAsk: (p: AskUserPrompt | null) => void;
  setSessionTodos: (todos: SessionTodo[]) => void;
  settleAsk: () => void;
}

export const useChatStore = create<ChatState>()((set) => ({
  messages: [],
  streaming: false,
  pendingPlan: null,
  pendingAsk: null,
  sessionTodos: [],
  setMessages: (messages) => set({ messages }),
  addMessage: (m) => {
    const id = nid();
    set((s) => ({ messages: [...s.messages, { ...m, id }] }));
    return id;
  },
  attachDbId: (localId, dbId) =>
    set((s) => ({
      messages: s.messages.map((m) => (m.id === localId ? { ...m, dbId } : m)),
    })),
  // 撤回：移除该条消息及紧随其后、下一条用户消息之前的所有消息（与后端语义一致）
  recallLocal: (localId) =>
    set((s) => {
      const idx = s.messages.findIndex((m) => m.id === localId);
      if (idx < 0) return {};
      const msgs = [...s.messages];
      let end = idx + 1;
      while (end < msgs.length && msgs[end].role !== "user") end++;
      msgs.splice(idx, end - idx);
      return { messages: msgs };
    }),
  removeMessageLocal: (localId) =>
    set((s) => ({ messages: s.messages.filter((m) => m.id !== localId) })),
  appendToken: (d) =>
    set((s) => {
      const msgs = [...s.messages];
      const last = msgs[msgs.length - 1];
      if (last && last.role === "assistant" && last.streaming) {
        msgs[msgs.length - 1] = { ...last, content: last.content + d };
      } else {
        msgs.push({ id: nid(), role: "assistant", content: d, streaming: true });
      }
      return { messages: msgs };
    }),
  appendReasoning: (d) =>
    set((s) => {
      const msgs = [...s.messages];
      const last = msgs[msgs.length - 1];
      if (last && last.role === "assistant" && last.streaming) {
        msgs[msgs.length - 1] = { ...last, reasoning: (last.reasoning ?? "") + d };
      } else {
        msgs.push({ id: nid(), role: "assistant", content: "", reasoning: d, streaming: true });
      }
      return { messages: msgs };
    }),
  startContinueMessage: (dbId) =>
    set((s) => ({
      streaming: true,
      messages: s.messages.map((m) => (m.dbId === dbId ? { ...m, streaming: true } : m)),
    })),
  appendContinueToken: (dbId, delta) =>
    set((s) => ({
      messages: s.messages.map((m) => (m.dbId === dbId ? { ...m, content: m.content + delta } : m)),
    })),
  finishContinueMessage: (dbId, content, meta) =>
    set((s) => ({
      streaming: false,
      messages: s.messages.map((m) =>
        m.dbId === dbId
          ? {
              ...m,
              content,
              streaming: false,
              finishReason: meta?.finish_reason ?? m.finishReason,
              refused: meta?.refused ?? m.refused,
            }
          : m,
      ),
    })),
  finishStreaming: (content, dbId) =>
    set((s) => {
      const msgs = s.messages.map((m) => ({ ...m }));
      // 多轮工具调用时可能存在多个流式气泡，最终内容只落到最近一个 assistant 气泡，
      // 其余仅结束流式状态，避免同一段内容被写到多个气泡里造成重复
      let applied = false;
      for (let i = msgs.length - 1; i >= 0; i--) {
        if (!msgs[i].streaming) continue;
        if (!applied && msgs[i].role === "assistant") {
          msgs[i].content = content ?? msgs[i].content;
          if (dbId != null) msgs[i].dbId = dbId;
          applied = true;
        }
        msgs[i].streaming = false;
      }
      return {
        messages: msgs.filter(
          (m) =>
            !(
              m.role === "assistant" &&
              m.content === "" &&
              !m.plan &&
              !m.ask &&
              !m.reasoning
            )
        ),
        streaming: false,
      };
    }),
  completeTool: (toolName, error, result) =>
    set((s) => {
      const msgs = s.messages.map((m) => ({ ...m }));
      for (let i = msgs.length - 1; i >= 0; i--) {
        if (msgs[i].role === "tool" && msgs[i].toolName === toolName && msgs[i].streaming) {
          msgs[i].streaming = false;
          msgs[i].toolFailed = Boolean(error);
          msgs[i].content = error ?? "";
          if (!error && result !== undefined) msgs[i].toolResult = result;
          break;
        }
      }
      return { messages: msgs };
    }),
  setStreaming: (streaming) => set({ streaming }),
  setPendingPlan: (pendingPlan) => set({ pendingPlan }),
  setPendingAsk: (pendingAsk) => set({ pendingAsk }),
  setSessionTodos: (sessionTodos) => set({ sessionTodos }),
  settleAsk: () =>
    set((s) => ({
      pendingAsk: null,
      messages: s.messages.map((m) => (m.ask && !m.askSettled ? { ...m, askSettled: true } : m)),
    })),
}));
