import { useEffect, useRef, useState } from "react";
import { ipc } from "../lib/ipc";
import { getCurrentWindow } from "@tauri-apps/api/window";
import ServerFiles from "../components/ServerFiles";
import ServerTerminalChat from "../components/ServerTerminalChat";
import { useTerminalFiles } from "../lib/useTerminalFiles";
import { useTerminalAgent } from "../lib/useTerminalAgent";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

export default function ServerTerminal() {
  const serverId = new URLSearchParams(window.location.hash.split("?")[1]).get("server") || "";
  const container = useRef<HTMLDivElement>(null);
  const terminal = useRef<Terminal>();
  const fitAddon = useRef<FitAddon>();
  const [status, setStatus] = useState("connecting");
  const [error, setError] = useState("");
  const [aiMode, setAiMode] = useState(false);
  const aiModeRef = useRef(false);
  const [switching, setSwitching] = useState(false);
  const switchingRef = useRef(false);
  const [showFiles, setShowFiles] = useState(() => {
    try { return localStorage.getItem(`terminal-files-visible:${serverId}`) !== "false"; } catch { return true; }
  });
  const [maximized, setMaximized] = useState(false);
  const [windowBusy, setWindowBusy] = useState(false);
  const [filesWidth, setFilesWidth] = useState(380);
  const resizing = useRef(false);
  const localFiles = useRef<string[]>([]);
  const output = useRef("");
  const files = useTerminalFiles(serverId, status === "connected");
  const agent = useTerminalAgent({ serverId, status, output, files, localFiles });
  const call = <T,>(action: string, extra = {}) => ipc.serverTerminal<T>(action, serverId, extra);
  useEffect(() => {
    try { localStorage.setItem(`terminal-files-visible:${serverId}`, String(showFiles)); } catch { /* Preference storage is optional. */ }
  }, [showFiles, serverId]);
  useEffect(() => {
    let dead = false;
    let sequence = 0;
    const win = getCurrentWindow();
    const update = async () => {
      const version = ++sequence;
      try { const value = await win.isMaximized(); if (!dead && version === sequence) setMaximized(value); }
      catch (e) { if (!dead) setError(String(e)); }
    };
    void update();
    const listener = win.onResized(() => { void update(); });
    return () => { dead = true; void listener.then(unlisten => unlisten()).catch(() => {}); };
  }, []);
  const toggleMaximize = async () => {
    if (windowBusy) return;
    setWindowBusy(true);
    try { const win = getCurrentWindow(); await win.toggleMaximize(); setMaximized(await win.isMaximized()); }
    catch (e) { setError(String(e)); }
    finally { setWindowBusy(false); }
  };
  const connect = async () => {
    setError(""); setStatus("connecting");
    try { await call("connect", { cols: terminal.current?.cols, rows: terminal.current?.rows }); }
    catch (e) { setError(String(e)); setStatus("disconnected"); }
  };
  const switchMode = async () => {
    if (switchingRef.current) return;
    switchingRef.current = true; setSwitching(true); setError("");
    try {
      if (!aiModeRef.current) {
        // Do not let a half-entered manual command become a prefix for AI input.
        if (status === "connected") await call("write", { data: "\u0003\r" });
        aiModeRef.current = true; setAiMode(true); output.current = "";
      } else {
        // Keep output hidden while interruption and its trailing output are drained.
        await agent.stop(false);
        if (status === "connected") await call("write", { data: "\u0003" }).catch(e => setError(String(e)));
        await new Promise(resolve => setTimeout(resolve, 250));
        terminal.current?.reset(); output.current = "";
        aiModeRef.current = false; setAiMode(false);
      }
    } catch (e) { setError(String(e)); }
    finally {
      switchingRef.current = false; setSwitching(false);
      if (!aiModeRef.current && status === "connected") void call("write", { data: "\r" }).catch(e => setError(String(e)));
    }
  };

  useEffect(() => {
    let dead = false; let polling = false;
    const term = new Terminal({ cursorBlink: true, fontSize: 14, fontFamily: 'Consolas, "Courier New", monospace', scrollback: 10000, theme: { background: "#0b1120", foreground: "#e2e8f0" } });
    const fit = new FitAddon(); term.loadAddon(fit); term.open(container.current!); terminal.current = term; fitAddon.current = fit;
    const report = (e: unknown) => { if (!dead) setError(String(e)); };
    const resize = () => {
      if (aiModeRef.current || !container.current?.clientWidth || !container.current?.clientHeight) return;
      fit.fit(); void call("resize", { cols: term.cols, rows: term.rows }).catch(() => {});
    };
    resize();
    const observer = new ResizeObserver(resize); observer.observe(container.current!);
    const decoder = new TextDecoder();
    const input = term.onData(data => { if (!aiModeRef.current && !switchingRef.current) void call("write", { data }).catch(report); });
    void connect(); term.focus();
    const timer = window.setInterval(async () => {
      if (polling) return; polling = true;
      try {
        const result = await call<{ status: string; error: string | null; output: number[] }>("poll");
        if (!dead) {
          setStatus(result.status); if (result.error) setError(result.error);
          if (result.output.length) {
            const bytes = new Uint8Array(result.output);
            output.current = (output.current + decoder.decode(bytes, { stream: true })).slice(-48000);
            // AI reads the same SSH output, but no command echo or raw output reaches the UI.
            if (!aiModeRef.current && !switchingRef.current) term.write(bytes);
          }
        }
      } catch (e) { report(e); } finally { polling = false; }
    }, 80);
    return () => { dead = true; clearInterval(timer); observer.disconnect(); input.dispose(); term.dispose(); terminal.current = undefined; fitAddon.current = undefined; };
  }, [serverId]);
  useEffect(() => {
    if (aiMode) return;
    const frame = requestAnimationFrame(() => {
      if (container.current?.clientWidth && container.current?.clientHeight) fitAddon.current?.fit();
      terminal.current?.focus();
    });
    return () => cancelAnimationFrame(frame);
  }, [aiMode, showFiles]);

  return <div className="h-screen flex flex-col bg-[#0b1120] text-slate-200">
    <header className="flex flex-wrap items-center justify-between gap-3 border-b border-slate-700 px-4 py-3 text-sm">
      <span className="shrink-0"><span className={`inline-block mr-2 h-2 w-2 rounded-full ${status === "connected" ? "bg-emerald-400" : status === "connecting" ? "bg-amber-400" : "bg-slate-500"}`} />{status === "connected" ? "已连接" : status === "connecting" ? "连接中…" : "已断开"}<span className="ml-3 text-slate-500">{aiMode ? "AI 对话" : "命令终端"}</span></span>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <button disabled={windowBusy} className="disabled:opacity-40" aria-label={maximized ? "还原窗口" : "最大化窗口"} onClick={() => void toggleMaximize()}>{maximized ? "还原" : "最大化"}</button>
        <button className={showFiles ? "text-sky-300" : ""} aria-expanded={showFiles} aria-controls="remote-file-panel" onClick={() => setShowFiles(!showFiles)}>{showFiles ? "隐藏文件列表" : "显示文件列表"}</button>
        <button disabled={switching} className={`${aiMode ? "text-sky-300" : ""} disabled:opacity-40`} onClick={() => void switchMode()}>{switching ? "切换中…" : aiMode ? "退出 AI 模式" : "AI 模式"}</button>
        {aiMode ? <button disabled={agent.busy || !!agent.pending} className="disabled:opacity-40" onClick={agent.clear}>清空对话</button> : <button onClick={() => terminal.current?.clear()}>清屏</button>}
        {status === "disconnected" ? <button className="text-sky-300" disabled={switching} onClick={connect}>重新连接</button> : <button disabled={switching} onClick={() => { void agent.stop(false).then(() => call("disconnect")).catch(e => setError(String(e))); }}>断开连接</button>}
      </div>
    </header>
    {error && <div role="alert" className="max-h-24 overflow-auto px-4 py-2 text-sm text-red-300 whitespace-pre-wrap break-all">{error}</div>}
    <div className="flex flex-1 min-h-0 overflow-hidden">
      <div className="flex flex-1 min-w-0 flex-col">
        <div ref={container} aria-hidden={aiMode} className={aiMode ? "hidden" : "flex-1 min-h-0 p-2"} />
        {aiMode && <ServerTerminalChat agent={agent} connected={status === "connected" && !switching} />}
      </div>
      {showFiles && <div role="separator" aria-label="调整文件面板宽度" aria-orientation="vertical" aria-valuemin={280} aria-valuemax={620} aria-valuenow={filesWidth} tabIndex={0}
          className="w-1.5 shrink-0 cursor-col-resize bg-slate-800 hover:bg-sky-700 focus:bg-sky-700 touch-none"
          onPointerDown={e => { resizing.current = true; e.currentTarget.setPointerCapture(e.pointerId); }}
          onPointerMove={e => { if (resizing.current) setFilesWidth(Math.max(280, Math.min(620, window.innerWidth - e.clientX))); }}
          onPointerUp={() => { resizing.current = false; }} onPointerCancel={() => { resizing.current = false; }}
          onKeyDown={e => { if (e.key === "ArrowLeft" || e.key === "ArrowRight") { e.preventDefault(); setFilesWidth(w => Math.max(280, Math.min(620, w + (e.key === "ArrowLeft" ? 20 : -20)))); } }} />}
        <div id="remote-file-panel" className={showFiles ? "min-w-[280px] max-w-[50%] shrink-0" : "hidden"} style={{ width: filesWidth }}><ServerFiles files={files} connected={status === "connected"} onLocalFiles={paths => { localFiles.current = [...new Set([...localFiles.current, ...paths])].slice(-50); }} /></div>
    </div>
    <footer className="border-t border-slate-800 px-4 py-1 text-xs text-slate-500">{aiMode ? "AI 通过当前 SSH 会话执行操作；操作目标、执行结果及提供的本机路径会发送给已配置的模型。" : "Ctrl+C 中断命令 · Ctrl+V 粘贴 · 关闭窗口将断开终端；上传任务可能继续，请先取消不需要的传输"}</footer>
  </div>;
}
