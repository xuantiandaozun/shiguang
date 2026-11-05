// 「录屏截图」页的截图区：框选（可点选窗口）、全屏、指定窗口；最近截图。
import { useCallback, useEffect, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { ipc, onEvent } from "../lib/ipc";
import type { ScreenWindow } from "../lib/ipc";

const btn = "rounded-md border border-slate-600 px-2.5 py-1 text-xs text-slate-200 hover:border-sky-500 disabled:opacity-50";

export default function ScreenshotPanel() {
  const [windows, setWindows] = useState<ScreenWindow[]>([]);
  const [picked, setPicked] = useState("");
  const [includeSelf, setIncludeSelf] = useState(false);
  const [copy, setCopy] = useState(true);
  const [hotkey, setHotkey] = useState("");
  const [recent, setRecent] = useState<{ path: string; name: string }[]>([]);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);

  const loadRecent = useCallback(() => { void ipc.screenshotRecent(12).then(setRecent).catch(() => {}); }, []);
  const loadWindows = useCallback(() => {
    void ipc.screenshotWindows(includeSelf).then((ws) => {
      setWindows(ws);
      setPicked((p) => (ws.some((w) => String(w.hwnd) === p) ? p : ""));
    }).catch(() => {});
  }, [includeSelf]);

  useEffect(() => {
    loadRecent();
    void ipc.getPluginSettings("recorder").then((s) => {
      setHotkey(String(s.screenshot_hotkey ?? ""));
      setCopy(s.screenshot_copy !== false);
      setIncludeSelf(s.screenshot_include_self === true);
    }).catch(() => {});
    const off = onEvent("screenshot-saved", () => loadRecent());
    return () => { void off.then((f) => f()); };
  }, [loadRecent]);
  useEffect(() => { loadWindows(); }, [loadWindows]);

  const shoot = async (mode: "interactive" | "screen" | "window") => {
    setBusy(true);
    setMsg(null);
    try {
      const win = windows.find((w) => String(w.hwnd) === picked);
      // 按标题找窗口；同名窗口时后端取最上层
      const r = await ipc.screenshotCapture(mode, { window: win?.title, includeSelf, copy });
      setMsg({ ok: true, text: `已保存 ${r.width}×${r.height}${r.copied ? "，已复制到剪贴板" : ""}` });
      loadRecent();
    } catch (e) {
      const text = String(e);
      if (!/取消/.test(text)) setMsg({ ok: false, text });
    } finally { setBusy(false); }
  };

  return <section className="space-y-3">
    <div className="flex items-start justify-between gap-3">
      <div>
        <h2 className="text-base font-semibold text-slate-100">截图</h2>
        <p className="mt-1 text-xs text-slate-400">
          框选时可以拖动选区域，也可以把鼠标停在某个软件窗口上单击截整个窗口。按窗口截图时，被别的窗口挡住也能截完整。
          {hotkey ? <> 全局快捷键 <kbd className="rounded border border-slate-600 px-1 text-[11px] text-slate-200">{hotkey}</kbd>（在「插件 → 录屏」设置里改）。</> : " 可在「插件 → 录屏」设置里配置全局快捷键。"}
        </p>
      </div>
    </div>
    <div className="space-y-2 rounded-lg border border-slate-700/60 bg-slate-900/30 p-4">
      <div className="flex flex-wrap items-center gap-2">
        <button type="button" disabled={busy} onClick={() => void shoot("interactive")} className="rounded-md bg-sky-600 px-3 py-1.5 text-xs text-white hover:bg-sky-500 disabled:opacity-50">框选截图</button>
        <button type="button" disabled={busy} onClick={() => void shoot("screen")} className={btn}>全屏</button>
        <span className="mx-1 h-4 w-px bg-slate-700" />
        <select className="max-w-[18rem] rounded-md border border-slate-600 bg-slate-900/60 px-2 py-1 text-xs text-slate-200" value={picked}
          onFocus={loadWindows} onChange={(e) => setPicked(e.target.value)}>
          <option value="">选择要截的窗口…</option>
          {windows.map((w) => <option key={w.hwnd} value={String(w.hwnd)}>{w.title}（{w.process.replace(/\.exe$/i, "")}）</option>)}
        </select>
        <button type="button" disabled={busy || !picked} onClick={() => void shoot("window")} className={btn}>截取窗口</button>
      </div>
      <div className="flex flex-wrap items-center gap-4 text-xs text-slate-400">
        <label className="flex items-center gap-1.5"><input type="checkbox" checked={copy} onChange={(e) => setCopy(e.target.checked)} />同时复制到剪贴板</label>
        <label className="flex items-center gap-1.5"><input type="checkbox" checked={includeSelf} onChange={(e) => setIncludeSelf(e.target.checked)} />包含拾光自己的窗口（做宣传素材时打开）</label>
      </div>
      {msg && <div className={`text-xs ${msg.ok ? "text-emerald-300" : "text-rose-300"}`}>{msg.text}</div>}
    </div>
    {recent.length > 0 && <div className="grid grid-cols-6 gap-2">
      {recent.map((r) => <div key={r.path} className="group relative overflow-hidden rounded border border-slate-700 bg-slate-900" title={r.name}>
        <img src={convertFileSrc(r.path)} alt={r.name} className="h-16 w-full object-cover" />
        <div className="absolute inset-0 hidden items-center justify-center gap-1 bg-black/60 group-hover:flex">
          <button type="button" className="rounded bg-white/15 px-1.5 py-0.5 text-[10px] text-white hover:bg-white/25"
            onClick={() => void ipc.screenshotCopy(r.path).then(() => setMsg({ ok: true, text: "已复制到剪贴板" })).catch((e) => setMsg({ ok: false, text: String(e) }))}>复制</button>
          <button type="button" className="rounded bg-white/15 px-1.5 py-0.5 text-[10px] text-white hover:bg-white/25"
            onClick={() => void ipc.recorderReveal(r.path).catch((e) => setMsg({ ok: false, text: String(e) }))}>位置</button>
        </div>
      </div>)}
    </div>}
  </section>;
}
