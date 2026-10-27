import { MutableRefObject, useEffect, useRef, useState } from "react";
import { ipc } from "./ipc";
import { TerminalAiMemory, TerminalAiStep, TerminalFiles } from "./useTerminalFiles";
import { createTerminalCheckpoint, terminalResultExcerpt, TerminalTaskCheckpoint } from "./terminalAgentContext";

export interface TerminalChatMessage { id: string; role: "user" | "assistant"; content: string }
interface AgentOptions {
  serverId: string;
  status: string;
  output: MutableRefObject<string>;
  files: TerminalFiles;
  localFiles: MutableRefObject<string[]>;
}

export function useTerminalAgent(options: AgentOptions) {
  const latest = useRef(options); latest.current = options;
  const [messages, setMessages] = useState<TerminalChatMessage[]>([]);
  const conversation = useRef<TerminalChatMessage[]>([]);
  const [busy, setBusy] = useState(false);
  const [activity, setActivity] = useState("");
  const [pending, setPending] = useState<TerminalAiStep>();
  const pendingRef = useRef<TerminalAiStep>();
  const generation = useRef(0);
  const occupied = useRef(false);
  const commandRunning = useRef(false);
  const activeUpload = useRef<string>();
  const goal = useRef("");
  const checkpoints = useRef<TerminalTaskCheckpoint[]>([]);
  const taskMemory = useRef<TerminalAiMemory>({ summary: "", related_paths: [] });
  const lastResult = useRef("");
  const steps = useRef(0);
  const alive = useRef(true);
  const addMessage = (role: TerminalChatMessage["role"], content: string) => {
    if (!alive.current || !content.trim()) return;
    const message = { id: crypto.randomUUID(), role, content };
    conversation.current = [...conversation.current, message].slice(-60);
    setMessages(conversation.current);
  };
  const call = <T,>(action: string, data?: string) => ipc.serverTerminal<T>(action, latest.current.serverId, { data });
  // Only one unconsumed result exists. Older command output never accumulates.
  const remember = (text: string) => { lastResult.current = text.slice(0, 7000); };
  const checkpoint = (summary: string) => {
    checkpoints.current = [...checkpoints.current, createTerminalCheckpoint(goal.current, summary, taskMemory.current)].slice(-4);
    lastResult.current = ""; latest.current.output.current = "";
  };
  const interrupt = () => {
    generation.current++; occupied.current = false;
    const jobs: Promise<unknown>[] = [];
    if (commandRunning.current) jobs.push(call("write", "\u0003"));
    if (activeUpload.current) jobs.push(latest.current.files.cancel(activeUpload.current));
    commandRunning.current = false; activeUpload.current = undefined; pendingRef.current = undefined;
    return Promise.allSettled(jobs);
  };
  const stop = async (notify = true) => {
    const wasActive = occupied.current || !!pendingRef.current;
    const jobs = interrupt();
    setBusy(false); setPending(undefined); setActivity("");
    if (wasActive) checkpoint(`任务已停止。${taskMemory.current.summary || "未形成最终结论。"}`);
    if (notify && wasActive) addMessage("assistant", "已停止自动操作。已执行的操作不会撤销；需要的话可以继续告诉我要检查什么。");
    await jobs;
  };
  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; void interrupt(); };
  }, []);
  useEffect(() => {
    if (options.status === "disconnected" && occupied.current) {
      void stop(false);
      addMessage("assistant", "服务器连接已断开，自动操作已暂停。请重新连接后继续。");
    }
  }, [options.status]);

  const runCommand = async (command: string, version: number, description: string) => {
    const marker = `__SHIGUANG_${crypto.randomUUID().replace(/-/g, "")}__`;
    latest.current.output.current = "";
    commandRunning.current = true;
    setActivity("正在检查服务器，读取执行结果…");
    try {
      await call("write", `printf '\\n${marker}:BEGIN\\n'; ${command}\nprintf '\\n${marker}:%s\\n' "$?"\n`);
      const started = Date.now();
      while (alive.current && generation.current === version) {
        await new Promise(resolve => setTimeout(resolve, 120));
        if (generation.current !== version) return;
        const output = latest.current.output.current;
        const completion = new RegExp(`${marker}:(\\d+)`).exec(output);
        if (completion) {
          remember(terminalResultExcerpt(command, description, output, marker, completion[1]));
          latest.current.output.current = "";
          void latest.current.files.refresh();
          return;
        }
        if (Date.now() - started > 120000) {
          await call("write", "\u0003");
          throw new Error("此步骤超过两分钟，已发送中断并暂停。请告诉我是否继续排查；远程进程可能仍在运行。");
        }
      }
    } finally { if (generation.current === version) commandRunning.current = false; }
  };
  const perform = async (step: TerminalAiStep, version: number) => {
    if (step.upload) {
      setActivity("正在上传文件，等待传输完成…");
      const task = await latest.current.files.upload({ ...step.upload, overwrite: !!step.upload.exists }, id => {
        if (generation.current !== version) void latest.current.files.cancel(id).catch(() => {});
        else activeUpload.current = id;
      });
      if (generation.current !== version) return;
      activeUpload.current = undefined;
      taskMemory.current.related_paths = [...new Set([...taskMemory.current.related_paths, step.upload.remote_path])].slice(-20);
      remember(`上传完成：${step.upload.local_path} → ${step.upload.remote_path}，${task.bytes} 字节，任务 ${task.id}`);
    } else if (step.command) await runCommand(step.command, version, step.message);
  };
  const run = async (approved?: TerminalAiStep) => {
    const version = ++generation.current;
    occupied.current = true; setBusy(true); setPending(undefined); pendingRef.current = undefined;
    try {
      if (approved) await perform(approved, version);
      while (alive.current && generation.current === version) {
        if (latest.current.status !== "connected") throw new Error("终端已断开，请重新连接后继续。");
        if (++steps.current > 50) throw new Error("已连续处理 50 步，自动操作已暂停。你可以补充要求后继续。");
        setActivity("正在分析服务器情况…");
        const current = latest.current;
        const result = await call<TerminalAiStep>("ai_step", JSON.stringify({
          goal: goal.current,
          prior_tasks: checkpoints.current,
          task_context: taskMemory.current,
          last_result: lastResult.current,
          remote_directory: current.files.listing.path,
          available_local_files: current.localFiles.current,
        }));
        if (!alive.current || generation.current !== version) return;
        lastResult.current = "";
        if (result.memory) taskMemory.current = { summary: (result.memory.summary || taskMemory.current.summary).slice(0, 4000), related_paths: [...new Set([...taskMemory.current.related_paths, ...result.memory.related_paths])].slice(-20), working_directory: result.memory.working_directory || taskMemory.current.working_directory };
        if (!result.command && !result.upload) {
          const summary = result.message || "已完成检查。";
          checkpoint(result.memory?.summary || summary);
          addMessage("assistant", summary);
          return;
        }
        if (result.sensitive || result.upload) {
          pendingRef.current = result; setPending(result);
          return;
        }
        setActivity(result.message || "正在处理…");
        await perform(result, version);
      }
    } catch (e) {
      if (alive.current && generation.current === version) {
        checkpoint(`任务暂停：${String(e).slice(0, 600)}。${taskMemory.current.summary}`);
        addMessage("assistant", `操作已暂停：${String(e)}`);
      }
    } finally {
      if (alive.current && generation.current === version) { occupied.current = false; setBusy(false); setActivity(""); }
    }
  };
  const send = (text: string) => {
    if (occupied.current || pendingRef.current || !text.trim()) return false;
    goal.current = text.trim(); steps.current = 0;
    taskMemory.current = { summary: "", related_paths: [] }; lastResult.current = ""; latest.current.output.current = "";
    addMessage("user", goal.current);
    void run();
    return true;
  };
  const approve = () => {
    const step = pendingRef.current;
    if (step && !occupied.current) void run(step);
  };
  const reject = () => {
    if (!pendingRef.current) return;
    checkpoint(`${taskMemory.current.summary}\n用户取消操作：${pendingRef.current.message}。未经新的明确同意，不要再次提出同一操作。`);
    pendingRef.current = undefined; setPending(undefined);
    addMessage("assistant", "已取消此操作，没有执行。你可以继续提出其他要求。");
  };
  const clear = () => {
    if (occupied.current || pendingRef.current) return;
    conversation.current = []; checkpoints.current = []; taskMemory.current = { summary: "", related_paths: [] }; lastResult.current = ""; latest.current.output.current = ""; setMessages([]);
  };
  return { messages, busy, activity, pending, send, stop, approve, reject, clear };
}
