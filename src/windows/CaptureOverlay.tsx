// 框选截图（也用于桌面录屏选范围，purpose=record）：全屏显示冻结的屏幕画面。拖动框选区域；鼠标停在窗口上会高亮，单击选中该窗口（截完整窗口，被遮挡也完整）。
// Enter/双击完成，Esc/右键重选或退出。坐标：后端给的是物理像素，按图片显示比例换算。
import { useCallback, useEffect, useRef, useState } from "react";
import type React from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { ipc } from "../lib/ipc";

interface Win { hwnd: number; title: string; process: string; rect: [number, number, number, number] }
interface Init { image: string; width: number; height: number; windows: Win[]; copy: boolean; purpose?: "screenshot" | "record" }
type Sel = { kind: "region"; rect: [number, number, number, number] } | { kind: "window"; win: Win };

const id = new URLSearchParams(window.location.hash.split("?")[1] ?? "").get("id") ?? "";

export default function CaptureOverlay() {
  const [init, setInit] = useState<Init | null>(null);
  const [error, setError] = useState("");
  const [hover, setHover] = useState<Win | null>(null);
  const [drag, setDrag] = useState<{ x0: number; y0: number; x1: number; y1: number; moved: boolean } | null>(null);
  const [sel, setSel] = useState<Sel | null>(null);
  const [busy, setBusy] = useState(false);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    document.body.style.margin = "0";
    document.body.style.overflow = "hidden";
    document.body.style.background = "#000";
    ipc.captureOverlayInit(id).then(setInit).catch((e) => setError(String(e)));
  }, []);

  /** 屏幕物理像素 ↔ CSS 像素 */
  const k = () => (root.current && init ? root.current.clientWidth / init.width : 1);
  const toPhys = (e: React.MouseEvent) => ({ x: Math.round(e.clientX / k()), y: Math.round(e.clientY / k()) });

  const cancel = useCallback(() => { void ipc.captureCancel(id); }, []);
  const finish = useCallback(async (copy?: boolean) => {
    if (!sel || busy) return;
    setBusy(true);
    try {
      if (sel.kind === "window") await ipc.captureFinish(id, "window", null, sel.win.hwnd, copy ?? null);
      else await ipc.captureFinish(id, "region", sel.rect, null, copy ?? null);
    } catch (e) { setError(String(e)); setBusy(false); }
  }, [sel, busy]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { if (sel) setSel(null); else cancel(); }
      if (e.key === "Enter") void finish();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [sel, cancel, finish]);

  const windowAt = (x: number, y: number) =>
    init?.windows.find((w) => x >= w.rect[0] && y >= w.rect[1] && x < w.rect[0] + w.rect[2] && y < w.rect[1] + w.rect[3]) ?? null;

  const onDown = (e: React.MouseEvent) => {
    if (e.button === 2) return;
    if (sel) return; // 已选中：只能用工具栏或 Esc
    const p = toPhys(e);
    setDrag({ x0: p.x, y0: p.y, x1: p.x, y1: p.y, moved: false });
  };
  const onMove = (e: React.MouseEvent) => {
    const p = toPhys(e);
    if (drag) {
      const moved = drag.moved || Math.abs(p.x - drag.x0) > 4 || Math.abs(p.y - drag.y0) > 4;
      setDrag({ ...drag, x1: p.x, y1: p.y, moved });
    } else if (!sel) {
      setHover(windowAt(p.x, p.y));
    }
  };
  const onUp = () => {
    if (!drag) return;
    if (drag.moved) {
      const x = Math.min(drag.x0, drag.x1), y = Math.min(drag.y0, drag.y1);
      const w = Math.abs(drag.x1 - drag.x0), h = Math.abs(drag.y1 - drag.y0);
      if (w > 3 && h > 3) setSel({ kind: "region", rect: [x, y, w, h] });
    } else if (hover) {
      setSel({ kind: "window", win: hover });
    }
    setDrag(null);
  };
  const onContext = (e: React.MouseEvent) => {
    e.preventDefault();
    if (sel) setSel(null); else cancel();
  };

  if (error) {
    return <div className="fixed inset-0 flex items-center justify-center bg-black/80 text-sm text-rose-200" onClick={cancel}>
      {error}（点击关闭）
    </div>;
  }
  if (!init) return null;

  // 当前框：选中 > 拖动中 > 悬停窗口
  let box: [number, number, number, number] | null = null;
  let label = "";
  if (sel?.kind === "region") { box = sel.rect; label = `${sel.rect[2]} × ${sel.rect[3]}`; }
  else if (sel?.kind === "window") { box = sel.win.rect; label = `${sel.win.title} · ${sel.win.rect[2]} × ${sel.win.rect[3]}`; }
  else if (drag?.moved) {
    box = [Math.min(drag.x0, drag.x1), Math.min(drag.y0, drag.y1), Math.abs(drag.x1 - drag.x0), Math.abs(drag.y1 - drag.y0)];
    label = `${box[2]} × ${box[3]}`;
  } else if (hover) { box = hover.rect; label = `${hover.title}（单击${init.purpose === "record" ? "录制这个窗口" : "截取整个窗口"}）`; }
  const record = init.purpose === "record";

  const s = k();
  const css = box && {
    left: Math.max(0, box[0]) * s, top: Math.max(0, box[1]) * s,
    width: (Math.min(init.width, box[0] + box[2]) - Math.max(0, box[0])) * s,
    height: (Math.min(init.height, box[1] + box[3]) - Math.max(0, box[1])) * s,
  };
  const toolbarTop = css ? (css.top + css.height + 44 < window.innerHeight ? css.top + css.height + 8 : Math.max(8, css.top - 44)) : 0;
  const btn = "rounded px-3 py-1.5 text-xs text-white hover:bg-white/15 disabled:opacity-50";

  return <div ref={root} className="fixed inset-0 cursor-crosshair select-none overflow-hidden"
    onMouseDown={onDown} onMouseMove={onMove} onMouseUp={onUp} onContextMenu={onContext}
    onDoubleClick={() => void finish()}>
    <img src={convertFileSrc(init.image)} alt="" draggable={false} className="pointer-events-none absolute inset-0 h-full w-full"
      onLoad={() => void ipc.captureOverlayShow()} />
    {css
      ? <div className="pointer-events-none absolute border-2 border-sky-400" style={{ ...css, boxShadow: "0 0 0 99999px rgba(0,0,0,0.45)" }}>
        <div className="absolute -top-6 left-0 max-w-[60vw] truncate whitespace-nowrap rounded bg-black/75 px-1.5 py-0.5 text-[11px] text-white">{label}</div>
      </div>
      : <div className="pointer-events-none absolute inset-0 bg-black/30" />}
    {!sel && !drag && <div className="pointer-events-none absolute left-1/2 top-4 -translate-x-1/2 rounded-full bg-black/70 px-4 py-1.5 text-xs text-white">
      {record ? "拖动框选要录制的区域，或单击要录制的窗口 · Esc 取消" : "拖动框选区域，或单击一个窗口 · Esc 取消"}
    </div>}
    {sel && css && <div className="absolute flex gap-1 rounded-lg bg-slate-900/95 p-1 shadow-xl"
      style={{ top: toolbarTop, left: Math.min(Math.max(8, css.left + css.width - 300), window.innerWidth - 308) }}
      onMouseDown={(e) => e.stopPropagation()} onMouseUp={(e) => e.stopPropagation()} onDoubleClick={(e) => e.stopPropagation()}>
      <button type="button" disabled={busy} className={btn} onClick={() => setSel(null)}>重选</button>
      <button type="button" disabled={busy} className={btn} onClick={cancel}>取消</button>
      {!record && <button type="button" disabled={busy} className={btn} onClick={() => void finish(true)}>保存并复制</button>}
      <button type="button" disabled={busy} className={`rounded px-3 py-1.5 text-xs text-white disabled:opacity-50 ${record ? "bg-rose-600 hover:bg-rose-500" : "bg-sky-600 hover:bg-sky-500"}`} onClick={() => void finish()}>
        {record ? "开始录制（Enter）" : busy ? "保存中…" : init.copy ? "完成（Enter）" : "保存（Enter）"}
      </button>
    </div>}
  </div>;
}
