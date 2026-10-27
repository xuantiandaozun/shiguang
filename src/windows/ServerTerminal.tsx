import { useEffect, useRef, useState } from "react";
import { ipc } from "../lib/ipc";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

export default function ServerTerminal() {
  const serverId = new URLSearchParams(window.location.hash.split("?")[1]).get("server") || "";
  const container = useRef<HTMLDivElement>(null);
  const terminal = useRef<Terminal>();
  const [status, setStatus] = useState("connecting");
  const [error, setError] = useState("");
  const call = <T,>(action: string, extra = {}) => ipc.serverTerminal<T>(action, serverId, extra);
  const connect = async () => {
    setError(""); setStatus("connecting");
    try { await call("connect", { cols: terminal.current?.cols, rows: terminal.current?.rows }); }
    catch (e) { setError(String(e)); setStatus("disconnected"); }
  };
  useEffect(() => {
    let dead = false; let polling = false;
    const term = new Terminal({ cursorBlink: true, fontSize: 14, fontFamily: 'Consolas, "Courier New", monospace', scrollback: 10000, theme: { background: "#0b1120", foreground: "#e2e8f0" } });
    const fit = new FitAddon(); term.loadAddon(fit); term.open(container.current!); terminal.current = term;
    const report = (e: unknown) => { if (!dead) setError(String(e)); };
    const resize = () => { fit.fit(); void call("resize", { cols: term.cols, rows: term.rows }).catch(() => {}); };
    fit.fit();
    const observer = new ResizeObserver(resize); observer.observe(container.current!);
    const input = term.onData(data => { void call("write", { data }).catch(report); });
    void connect(); term.focus();
    const timer = window.setInterval(async () => {
      if (polling) return; polling = true;
      try {
        const result = await call<{ status: string; error: string | null; output: number[] }>("poll");
        if (!dead) { setStatus(result.status); if (result.error) setError(result.error); if (result.output.length) term.write(new Uint8Array(result.output)); }
      } catch (e) { report(e); } finally { polling = false; }
    }, 80);
    return () => { dead = true; clearInterval(timer); observer.disconnect(); input.dispose(); term.dispose(); terminal.current = undefined; };
  }, [serverId]);
  return <div className="h-screen flex flex-col bg-[#0b1120] text-slate-200">
    <header className="flex items-center justify-between gap-3 border-b border-slate-700 px-4 py-3 text-sm">
      <span><span className={`inline-block mr-2 h-2 w-2 rounded-full ${status === "connected" ? "bg-emerald-400" : status === "connecting" ? "bg-amber-400" : "bg-slate-500"}`} />{status === "connected" ? "已连接" : status === "connecting" ? "连接中…" : "已断开"}</span>
      <div className="flex gap-4"><button onClick={() => terminal.current?.clear()}>清屏</button>{status === "disconnected" ? <button className="text-sky-300" onClick={connect}>重新连接</button> : <button onClick={() => void call("disconnect").catch(e => setError(String(e)))}>断开连接</button>}</div>
    </header>
    {error && <div role="alert" className="px-4 py-2 text-sm text-red-300 whitespace-pre-wrap break-all">{error}</div>}
    <div ref={container} className="flex-1 min-h-0 p-2" />
    <footer className="border-t border-slate-800 px-4 py-1 text-xs text-slate-500">Ctrl+C 中断命令 · Ctrl+V 粘贴 · 关闭窗口将断开此终端，远程进程不保证停止</footer>
  </div>;
}
