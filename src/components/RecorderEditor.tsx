// 录屏时间线编辑器：拖动时间轴看画面，框选打码，编辑放大段、剪切、解说（配音文件由拾光用 CLI 生成后填入）。
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type React from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { ipc, onEvent } from "../lib/ipc";
import type { RecorderExportProgress, RecorderProject, TimelineDoc } from "../lib/ipc";

type Tab = "narration" | "zooms" | "masks" | "cuts";

const btn = "rounded-md border border-slate-600 px-2.5 py-1 text-xs text-slate-200 hover:border-sky-500 disabled:opacity-50";
const num = "w-16 rounded border border-slate-600 bg-slate-900/60 px-1.5 py-0.5 text-xs text-slate-100 outline-none focus:border-sky-500";
const r1 = (v: number) => Math.round(v * 10) / 10;
const fmt = (t: number) => `${Math.floor(t / 60)}:${(t % 60).toFixed(1).padStart(4, "0")}`;
/** 只有文字时的朗读时长估计（与内核一致：约每秒 4 个字） */
const estimate = (text: string) => Math.min(30, Math.max(1.5, text.replace(/\s/g, "").length / 4));

function Num({ value, onChange, step = 0.1, min, max }: { value: number; onChange: (v: number) => void; step?: number; min?: number; max?: number }) {
  return <input type="number" className={num} value={r1(value)} step={step} min={min} max={max}
    onChange={(e) => { const v = parseFloat(e.target.value); if (Number.isFinite(v)) onChange(v); }} />;
}

export default function RecorderEditor({ id, onClose }: { id: string; onClose: () => void }) {
  const [proj, setProj] = useState<RecorderProject | null>(null);
  const [doc, setDoc] = useState<TimelineDoc | null>(null);
  const [t, setT] = useState(0);
  const [tab, setTab] = useState<Tab>("narration");
  const [dirty, setDirty] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState<"" | "save" | "preview" | "export">("");
  const [progress, setProgress] = useState<RecorderExportProgress | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [focusPick, setFocusPick] = useState<number | null>(null);
  const [cutStart, setCutStart] = useState<number | null>(null);
  const [drag, setDrag] = useState<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  const stage = useRef<HTMLDivElement>(null);

  const load = useCallback(async () => {
    try {
      const p = await ipc.recorderProject(id);
      setProj(p);
      setDoc(p.timeline);
      setPreview(p.preview ? `${convertFileSrc(p.preview)}?v=${Date.now()}` : null);
      setDirty(false);
    } catch (e) { setError(String(e)); }
  }, [id]);

  useEffect(() => { void load(); }, [load]);
  useEffect(() => {
    const off = onEvent<RecorderExportProgress>("recorder-export-progress", (p) => { if (p.id === id) setProgress(p); });
    return () => { void off.then((f) => f()); };
  }, [id]);

  const duration = proj?.duration ?? 0;
  const frameUrl = useMemo(() => {
    if (!proj || proj.frames.length === 0) return "";
    let lo = 0, hi = proj.frames.length - 1;
    while (lo < hi) { const mid = (lo + hi + 1) >> 1; if (proj.frames[mid][0] <= t) lo = mid; else hi = mid - 1; }
    return convertFileSrc(`${proj.dir}\\frames\\${proj.frames[lo][1]}`);
  }, [proj, t]);

  const update = (fn: (d: TimelineDoc) => TimelineDoc) => {
    setDoc((d) => (d ? fn(d) : d));
    setDirty(true);
    setNotice("");
  };
  const editZooms = (fn: (z: TimelineDoc["zooms"]) => TimelineDoc["zooms"]) => update((d) => ({ ...d, zooms_edited: true, zooms: fn(d.zooms) }));

  const save = async (): Promise<boolean> => {
    if (!doc) return false;
    setBusy("save");
    setError("");
    try {
      const saved = await ipc.recorderSaveTimeline(id, doc);
      setDoc(saved);
      setDirty(false);
      return true;
    } catch (e) { setError(`保存失败：${String(e)}`); return false; } finally { setBusy(""); }
  };

  const exportVideo = async (previewOnly: boolean) => {
    if (dirty && !(await save())) return;
    setBusy(previewOnly ? "preview" : "export");
    setProgress(null);
    setError("");
    try {
      const r = await ipc.recorderExport(id, { preview: previewOnly });
      if (previewOnly) setPreview(`${convertFileSrc(r.path)}?v=${Date.now()}`);
      else setNotice(`已导出：${r.path}。对外发送前请完整看一遍。`);
    } catch (e) { setError(`导出失败：${String(e)}`); } finally { setBusy(""); setProgress(null); }
  };

  // 画面上：拖动框选打码；点选放大焦点
  const norm = (e: React.PointerEvent) => {
    const r = stage.current!.getBoundingClientRect();
    return { x: Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)), y: Math.min(1, Math.max(0, (e.clientY - r.top) / r.height)) };
  };
  const onDown = (e: React.PointerEvent) => {
    const p = norm(e);
    if (focusPick !== null) {
      editZooms((zs) => zs.map((z, i) => (i === focusPick ? { ...z, focus: [r1(p.x * 100) / 100, r1(p.y * 100) / 100] as [number, number] } : z)));
      setFocusPick(null);
      return;
    }
    (e.target as Element).setPointerCapture(e.pointerId);
    setDrag({ x0: p.x, y0: p.y, x1: p.x, y1: p.y });
  };
  const onMove = (e: React.PointerEvent) => { if (drag) { const p = norm(e); setDrag({ ...drag, x1: p.x, y1: p.y }); } };
  const onUp = () => {
    if (!drag) return;
    const x = Math.min(drag.x0, drag.x1), y = Math.min(drag.y0, drag.y1);
    const w = Math.abs(drag.x1 - drag.x0), h = Math.abs(drag.y1 - drag.y0);
    setDrag(null);
    if (w < 0.01 || h < 0.01) return;
    const round = (v: number) => Math.round(v * 1000) / 1000;
    // 默认从当前时刻一直打到结尾，宁可多遮
    update((d) => ({ ...d, masks: [...d.masks, { start: r1(t), end: duration, rect: [round(x), round(y), round(w), round(h)] }] }));
    setTab("masks");
  };

  if (!proj || !doc) {
    return <div className="space-y-3 text-sm">
      <button type="button" onClick={onClose} className={btn}>返回</button>
      <div className="text-xs text-slate-400">{error || "加载中…"}</div>
    </div>;
  }

  const pct = (v: number) => `${(Math.min(Math.max(v, 0), duration) / duration) * 100}%`;
  const activeZoom = doc.zooms.find((z) => t >= z.start && t <= z.end);
  const burnValue = doc.burn_subtitles == null ? "" : doc.burn_subtitles ? "on" : "off";
  const progressLabel = progress?.phase === "ffmpeg" ? "准备 ffmpeg…" : progress?.phase === "audio" ? "处理配音…"
    : progress?.phase === "render" && progress.total ? `渲染 ${Math.round(((progress.done ?? 0) / progress.total) * 100)}%` : "准备中…";

  const track = (items: { start: number; end: number; key: string; title: string; onClick: () => void }[], color: string) =>
    <div className="relative h-4">
      {items.map((it) => <button key={it.key} type="button" title={it.title} onClick={it.onClick}
        className={`absolute top-0.5 h-3 rounded-sm ${color}`} style={{ left: pct(it.start), width: `max(3px, calc(${pct(it.end)} - ${pct(it.start)}))` }} />)}
    </div>;

  return <div className="space-y-4 text-sm">
    <div className="flex items-center justify-between gap-3">
      <div className="flex min-w-0 items-center gap-2">
        <button type="button" onClick={() => { if (!dirty || confirm("有未保存的修改，确定返回？")) onClose(); }} className={btn}>返回</button>
        <h2 className="truncate text-base font-semibold text-slate-100">编辑：{proj.meta.title}</h2>
        {dirty && <span className="text-[11px] text-amber-300">未保存</span>}
      </div>
      <div className="flex shrink-0 gap-2">
        <button type="button" disabled={!!busy || !dirty} onClick={() => void save()} className={btn}>保存</button>
        <button type="button" disabled={!!busy} onClick={() => void exportVideo(true)} className={btn}>{busy === "preview" ? progressLabel : "快速预览"}</button>
        <button type="button" disabled={!!busy} onClick={() => void exportVideo(false)} className="rounded-md bg-sky-600 px-3 py-1 text-xs text-white hover:bg-sky-500 disabled:opacity-50">{busy === "export" ? progressLabel : "导出 MP4"}</button>
      </div>
    </div>

    {error && <div className="rounded-md border border-rose-500/40 bg-rose-500/10 px-3 py-2 text-xs text-rose-200">{error}</div>}
    {notice && <div className="rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-200">{notice}</div>}

    <div className="grid gap-3" style={{ gridTemplateColumns: preview ? "1fr 1fr" : "1fr" }}>
      <div className="space-y-1">
        <div ref={stage} className={`relative select-none overflow-hidden rounded border border-slate-700 bg-black ${focusPick !== null ? "cursor-crosshair" : "cursor-cell"}`}
          onPointerDown={onDown} onPointerMove={onMove} onPointerUp={onUp}>
          {frameUrl && <img src={frameUrl} alt="" draggable={false} className="block w-full" />}
          {doc.masks.filter((m) => t >= m.start && t <= m.end).map((m, i) =>
            <div key={i} className="pointer-events-none absolute border-2 border-rose-500 bg-rose-500/40 backdrop-blur-sm"
              style={{ left: `${m.rect[0] * 100}%`, top: `${m.rect[1] * 100}%`, width: `${m.rect[2] * 100}%`, height: `${m.rect[3] * 100}%` }} />)}
          {activeZoom?.focus && (() => {
            const s = 1 / activeZoom.scale;
            const cx = Math.min(Math.max(activeZoom.focus[0], s / 2), 1 - s / 2), cy = Math.min(Math.max(activeZoom.focus[1], s / 2), 1 - s / 2);
            return <div className="pointer-events-none absolute border-2 border-dashed border-sky-400"
              style={{ left: `${(cx - s / 2) * 100}%`, top: `${(cy - s / 2) * 100}%`, width: `${s * 100}%`, height: `${s * 100}%` }} />;
          })()}
          {drag && <div className="pointer-events-none absolute border-2 border-rose-400 bg-rose-400/20"
            style={{ left: `${Math.min(drag.x0, drag.x1) * 100}%`, top: `${Math.min(drag.y0, drag.y1) * 100}%`, width: `${Math.abs(drag.x1 - drag.x0) * 100}%`, height: `${Math.abs(drag.y1 - drag.y0) * 100}%` }} />}
          {activeZoom && <div className="pointer-events-none absolute left-2 top-2 rounded bg-sky-600/80 px-1.5 py-0.5 text-[10px] text-white">放大 {activeZoom.scale.toFixed(1)}×{activeZoom.focus ? "" : " 跟随光标"}</div>}
        </div>
        <p className="text-[11px] text-slate-500">{focusPick !== null ? "在画面上点一下，设为放大焦点。" : "在画面上拖动可框选打码区域（默认从当前时刻打到结尾）。这里显示的是原始画面，放大和光标效果看「快速预览」。"}</p>
      </div>
      {preview && <div className="space-y-1">
        <video src={preview} controls className="w-full rounded border border-slate-700 bg-black" />
        <p className="text-[11px] text-slate-500">快速预览（低分辨率）。修改后需重新生成。</p>
      </div>}
    </div>

    <div className="space-y-1 rounded-lg border border-slate-700/60 bg-slate-900/30 p-3">
      <div className="flex items-center gap-3">
        <span className="w-24 shrink-0 font-mono text-xs text-slate-300">{fmt(t)} / {fmt(duration)}</span>
        <input type="range" className="flex-1" min={0} max={duration} step={0.05} value={t} onChange={(e) => setT(parseFloat(e.target.value))} />
      </div>
      <div className="relative ml-[6.75rem] space-y-0.5" onClick={(e) => {
        if ((e.target as HTMLElement).closest("button")) return;
        const r = e.currentTarget.getBoundingClientRect();
        setT(Math.max(0, Math.min(duration, ((e.clientX - r.left) / r.width) * duration)));
      }}>
        <div className="pointer-events-none absolute inset-y-0 z-10 w-px bg-amber-300" style={{ left: pct(t) }} />
        <div className="relative h-3">
          {proj.marks.map((m, i) => <button key={`m${i}`} type="button" title={`标记：${m.label}`} onClick={() => setT(m.t)} className="absolute top-0 h-3 w-1 -translate-x-1/2 rounded-sm bg-amber-400" style={{ left: pct(m.t) }} />)}
          {proj.clicks.map((c, i) => <button key={`c${i}`} type="button" title={`点击：${c.label}`} onClick={() => setT(c.t)} className="absolute top-1 h-1.5 w-1.5 -translate-x-1/2 rounded-full bg-slate-300" style={{ left: pct(c.t) }} />)}
        </div>
        {track(doc.narration.map((n, i) => ({ start: n.at, end: n.at + estimate(n.text), key: `n${i}`, title: `解说：${n.text}`, onClick: () => { setT(n.at); setTab("narration"); } })), "bg-emerald-500/80")}
        {track(doc.zooms.map((z, i) => ({ start: z.start, end: z.end, key: `z${i}`, title: `放大 ${z.scale}×`, onClick: () => { setT(z.start); setTab("zooms"); } })), "bg-sky-500/80")}
        {track(doc.masks.map((m, i) => ({ start: m.start, end: m.end, key: `k${i}`, title: "打码", onClick: () => { setT(m.start); setTab("masks"); } })), "bg-rose-500/80")}
        {track(doc.cuts.map((c, i) => ({ start: c.start, end: c.end, key: `x${i}`, title: "剪掉", onClick: () => { setT(c.start); setTab("cuts"); } })), "bg-slate-500")}
      </div>
      <div className="ml-[6.75rem] flex gap-3 text-[10px] text-slate-500">
        <span><i className="mr-1 inline-block h-2 w-2 bg-amber-400" />标记</span><span><i className="mr-1 inline-block h-2 w-2 rounded-full bg-slate-300" />点击</span>
        <span><i className="mr-1 inline-block h-2 w-2 bg-emerald-500" />解说</span><span><i className="mr-1 inline-block h-2 w-2 bg-sky-500" />放大</span>
        <span><i className="mr-1 inline-block h-2 w-2 bg-rose-500" />打码</span><span><i className="mr-1 inline-block h-2 w-2 bg-slate-500" />剪掉</span>
      </div>
    </div>

    <div className="rounded-lg border border-slate-700/60">
      <div className="flex gap-1 border-b border-slate-700/60 px-2 pt-2">
        {([["narration", `解说 ${doc.narration.length}`], ["zooms", `放大 ${doc.zooms.length}`], ["masks", `打码 ${doc.masks.length}`], ["cuts", `剪切 ${doc.cuts.length}`]] as [Tab, string][]).map(([k, label]) =>
          <button key={k} type="button" onClick={() => setTab(k)} className={`rounded-t px-3 py-1.5 text-xs ${tab === k ? "bg-slate-700/60 text-slate-100" : "text-slate-400 hover:text-slate-200"}`}>{label}</button>)}
      </div>
      <div className="space-y-2 p-3">
        {tab === "narration" && <>
          <div className="flex flex-wrap items-center gap-2">
            <button type="button" className={btn} onClick={() => update((d) => ({ ...d, narration: [...d.narration, { at: r1(t), text: "" }] }))}>在 {fmt(t)} 添加解说</button>
            {proj.marks.some((m) => m.note) && doc.narration.length === 0 && <button type="button" className={btn}
              onClick={() => update((d) => ({ ...d, narration: proj.marks.filter((m) => m.note).map((m) => ({ at: m.t, text: m.note })) }))}>从步骤标记生成</button>}
            <label className="ml-auto flex items-center gap-1.5 text-xs text-slate-400">字幕
              <select className="rounded border border-slate-600 bg-slate-900/60 px-1.5 py-0.5 text-xs text-slate-200" value={burnValue}
                onChange={(e) => update((d) => ({ ...d, burn_subtitles: e.target.value === "" ? null : e.target.value === "on" }))}>
                <option value="">按设置（{proj.burn_default ? "画进画面" : "不画"}）</option>
                <option value="on">画进画面</option>
                <option value="off">只生成 .srt</option>
              </select>
            </label>
          </div>
          <p className="text-[11px] text-slate-500">配音可以在聊天里让拾光生成，比如「给这个录屏写解说并用火山 CLI 配音」，生成后会自动填到这里。某段配音比画面长时，画面会自动定格等它说完。</p>
          {doc.narration.map((n, i) => <div key={i} className="space-y-1.5 rounded border border-slate-700/60 p-2">
            <div className="flex items-center gap-2 text-xs text-slate-400">
              <span>第</span><Num value={n.at} min={0} max={duration} onChange={(v) => update((d) => ({ ...d, narration: d.narration.map((x, j) => (j === i ? { ...x, at: v } : x)) }))} /><span>秒</span>
              <button type="button" className={btn} onClick={() => update((d) => ({ ...d, narration: d.narration.map((x, j) => (j === i ? { ...x, at: r1(t) } : x)) }))}>设为当前</button>
              <button type="button" className={btn} onClick={() => setT(n.at)}>跳到这里</button>
              <button type="button" className={`${btn} ml-auto`} onClick={() => update((d) => ({ ...d, narration: d.narration.filter((_, j) => j !== i) }))}>删除</button>
            </div>
            <textarea rows={2} className="w-full rounded border border-slate-600 bg-slate-900/60 px-2 py-1 text-xs text-slate-100 outline-none focus:border-sky-500" value={n.text} placeholder="解说词（也用作字幕）"
              onChange={(e) => update((d) => ({ ...d, narration: d.narration.map((x, j) => (j === i ? { ...x, text: e.target.value } : x)) }))} />
            <input className="w-full rounded border border-slate-600 bg-slate-900/60 px-2 py-1 text-xs text-slate-100 outline-none focus:border-sky-500" value={n.audio ?? ""} placeholder="配音文件（可选）：audio/01.mp3 或完整路径"
              onChange={(e) => update((d) => ({ ...d, narration: d.narration.map((x, j) => (j === i ? { ...x, audio: e.target.value || undefined } : x)) }))} />
          </div>)}
        </>}

        {tab === "zooms" && <>
          <div className="flex flex-wrap items-center gap-2">
            <button type="button" className={btn} onClick={() => editZooms((zs) => [...zs, { start: r1(t), end: r1(Math.min(duration, t + 2)), scale: 1.8 }].sort((a, b) => a.start - b.start))}>在 {fmt(t)} 添加放大</button>
            {doc.zooms_edited && <button type="button" className={btn} onClick={() => update((d) => ({ ...d, zooms_edited: false }))}>恢复自动</button>}
            <span className="text-[11px] text-slate-500">{doc.zooms_edited ? "手动放大段：导出时原样使用。" : "当前是按点击自动生成的，改任意一项后变为手动。"}</span>
          </div>
          {doc.zooms.map((z, i) => <div key={i} className="flex flex-wrap items-center gap-2 text-xs text-slate-400">
            <Num value={z.start} min={0} max={duration} onChange={(v) => editZooms((zs) => zs.map((x, j) => (j === i ? { ...x, start: v } : x)))} />–
            <Num value={z.end} min={0} max={duration} onChange={(v) => editZooms((zs) => zs.map((x, j) => (j === i ? { ...x, end: v } : x)))} />秒
            <span className="ml-2">倍数</span><Num value={z.scale} min={1} max={3} onChange={(v) => editZooms((zs) => zs.map((x, j) => (j === i ? { ...x, scale: v } : x)))} />
            <span className="ml-2">{z.focus ? `焦点 (${z.focus[0].toFixed(2)}, ${z.focus[1].toFixed(2)})` : "跟随光标"}</span>
            <button type="button" className={btn} onClick={() => { setT(z.start + (z.end - z.start) / 2); setFocusPick(i); }}>{focusPick === i ? "请在画面上点选…" : "画面上点选焦点"}</button>
            {z.focus && <button type="button" className={btn} onClick={() => editZooms((zs) => zs.map((x, j) => (j === i ? { start: x.start, end: x.end, scale: x.scale } : x)))}>改为跟随光标</button>}
            <button type="button" className={`${btn} ml-auto`} onClick={() => editZooms((zs) => zs.filter((_, j) => j !== i))}>删除</button>
          </div>)}
        </>}

        {tab === "masks" && <>
          <p className="text-[11px] text-slate-500">在上面的画面上拖动框选即可新增。网页上的密码、手机号等在录制时已经模糊，这里用于补漏。</p>
          {doc.masks.map((m, i) => <div key={i} className="flex flex-wrap items-center gap-2 text-xs text-slate-400">
            <Num value={m.start} min={0} max={duration} onChange={(v) => update((d) => ({ ...d, masks: d.masks.map((x, j) => (j === i ? { ...x, start: v } : x)) }))} />–
            <Num value={m.end} min={0} max={duration} onChange={(v) => update((d) => ({ ...d, masks: d.masks.map((x, j) => (j === i ? { ...x, end: v } : x)) }))} />秒
            <button type="button" className={btn} onClick={() => update((d) => ({ ...d, masks: d.masks.map((x, j) => (j === i ? { ...x, start: r1(t) } : x)) }))}>从当前开始</button>
            <button type="button" className={btn} onClick={() => update((d) => ({ ...d, masks: d.masks.map((x, j) => (j === i ? { ...x, end: r1(t) } : x)) }))}>到当前结束</button>
            <button type="button" className={btn} onClick={() => setT(m.start)}>跳到这里</button>
            <button type="button" className={`${btn} ml-auto`} onClick={() => update((d) => ({ ...d, masks: d.masks.filter((_, j) => j !== i) }))}>删除</button>
          </div>)}
        </>}

        {tab === "cuts" && <>
          <div className="flex flex-wrap items-center gap-2">
            {cutStart === null
              ? <button type="button" className={btn} onClick={() => setCutStart(r1(t))}>从 {fmt(t)} 开始剪</button>
              : <>
                <button type="button" className={btn} disabled={t <= cutStart} onClick={() => {
                  const start = cutStart;
                  update((d) => ({ ...d, cuts: [...d.cuts, { start, end: r1(t) }].sort((a, b) => a.start - b.start) }));
                  setCutStart(null);
                }}>剪到 {fmt(t)}</button>
                <button type="button" className={btn} onClick={() => setCutStart(null)}>取消</button>
                <span className="text-[11px] text-amber-300">起点 {fmt(cutStart)}，拖动时间轴到终点</span>
              </>}
            <span className="text-[11px] text-slate-500">剪掉等待加载、操作失误的片段。挂在被剪片段里的解说会移到剪切点之后。</span>
          </div>
          {doc.cuts.map((c, i) => <div key={i} className="flex flex-wrap items-center gap-2 text-xs text-slate-400">
            <Num value={c.start} min={0} max={duration} onChange={(v) => update((d) => ({ ...d, cuts: d.cuts.map((x, j) => (j === i ? { ...x, start: v } : x)) }))} />–
            <Num value={c.end} min={0} max={duration} onChange={(v) => update((d) => ({ ...d, cuts: d.cuts.map((x, j) => (j === i ? { ...x, end: v } : x)) }))} />秒
            <button type="button" className={btn} onClick={() => setT(c.start)}>跳到这里</button>
            <button type="button" className={`${btn} ml-auto`} onClick={() => update((d) => ({ ...d, cuts: d.cuts.filter((_, j) => j !== i) }))}>删除</button>
          </div>)}
        </>}
      </div>
    </div>
  </div>;
}
