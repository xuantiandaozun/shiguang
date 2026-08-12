import { create } from "zustand";
import type { Plan } from "../lib/ipc";

export interface UiMessage {
  id: string;
  dbId?: number;
  role: "user" | "assistant" | "tool" | "system";
  content: string;
  /** 思考模式下的思维链（仅流式期间展示，不入库） */
  reasoning?: string;
  toolName?: string;
  toolFailed?: boolean;
  plan?: Plan;
  streaming?: boolean;
}

let seq = 0;
const nid = () => `m${++seq}-${Date.now()}`;

interface ChatState {
  messages: UiMessage[];
  streaming: boolean;
  pendingPlan: Plan | null;
  setMessages: (m: UiMessage[]) => void;
  addMessage: (m: Omit<UiMessage, "id">) => string;
  attachDbId: (localId: string, dbId: number) => void;
  recallLocal: (localId: string) => void;
  appendToken: (d: string) => void;
  appendReasoning: (d: string) => void;
  finishStreaming: (content?: string) => void;
  completeTool: (toolName: string, error?: string) => void;
  setStreaming: (b: boolean) => void;
  setPendingPlan: (p: Plan | null) => void;
}

export const useChatStore = create<ChatState>()((set) => ({
  messages: [],
  streaming: false,
  pendingPlan: null,
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
  finishStreaming: (content) =>
    set((s) => {
      const msgs = s.messages.map((m) => ({ ...m }));
      // 多轮工具调用时可能存在多个流式气泡，最终内容只落到最近一个 assistant 气泡，
      // 其余仅结束流式状态，避免同一段内容被写到多个气泡里造成重复
      let applied = false;
      for (let i = msgs.length - 1; i >= 0; i--) {
        if (!msgs[i].streaming) continue;
        if (!applied && msgs[i].role === "assistant") {
          msgs[i].content = content ?? msgs[i].content;
          applied = true;
        }
        msgs[i].streaming = false;
      }
      return {
        messages: msgs.filter(
          (m) => !(m.role === "assistant" && m.content === "" && !m.plan && !m.reasoning)
        ),
        streaming: false,
      };
    }),
  completeTool: (toolName, error) =>
    set((s) => {
      const msgs = s.messages.map((m) => ({ ...m }));
      for (let i = msgs.length - 1; i >= 0; i--) {
        if (msgs[i].role === "tool" && msgs[i].toolName === toolName && msgs[i].streaming) {
          msgs[i].streaming = false;
          msgs[i].toolFailed = Boolean(error);
          msgs[i].content = error ?? "";
          break;
        }
      }
      return { messages: msgs };
    }),
  setStreaming: (streaming) => set({ streaming }),
  setPendingPlan: (pendingPlan) => set({ pendingPlan }),
}));
