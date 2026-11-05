// 录屏编辑窗口（独立窗口，#/recorder-editor?id=...）。主要靠拾光改：写一句要求交给聊天，拾光用 record_edit 改完这里自动刷新。
// 编辑台只留轻量功能：看画面、框选打码、改解说文字、删掉不要的编辑、选配乐和导出选项。改动自动保存。
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type React from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { ipc, onEvent } from "../lib/ipc";
import type { RecorderExportProgress, RecorderProject, TimelineDoc } from "../lib/ipc";

const btn = "rounded-md border border-slate-600 px-2.5 py-1 text-xs text-slate-200 hover:border-sky-500 disabled:opacity-50";
const sel = "rounded border border-slate-600 bg-slate-900/60 px-1.5 py-0.5 text-xs text-slate-200";
const r1 = (v: number) => Math.round(v * 10) / 10;
const fmt = (t: number) => `${Math.floor(t / 60)}:${(t % 60).toFixed(1).padStart(4, "0")}`;
/** 只有文字时的朗读时长估计（与内核一致：约每秒 4 个字） */
const estimate = (text: string) => Math.min(30, Math.max(1.5, text.replace(/\s/g, "").length / 4));
const fileName = (p: string) => p.split(/[\\/]/).pop() ?? p;

const IDEAS = ["写解说词并配音", "加个片头和片尾", "字幕换成综艺花字，重点词标出来", "在关键操作处加贴纸或标注", "剪掉等待加载和操作失误的片段", "配上背景音乐，节奏紧凑一些"];

const hashId = () => new URLSearchParams(window.location.hash.split("?")[1] ?? "").get("id") ?? "";

export default function RecorderEditor({ id: propId }: { id?: string } = {}) {
  const id = propId ?? hashId();
  const [proj, setProj] = useState<RecorderProject | null>(null);
  const [doc, setDoc] = useState<TimelineDoc | null>(null);
  const [t, setT] = useState(0);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState<"" | "preview" | "export">("");
  const [progress, setProgress] = useState<RecorderExportProgress | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [exported, setExported] = useState<string | null>(null);
  const [cutStart, setCutStart] = useState<number | null>(null);
  const [drag, setDrag] = useState<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  const [ask, setAsk] = useState("");
  const stage = useRef<HTMLDivElement>(null);
  const version = useRef(0);
  const dirtyRef = useRef(false);
  const savingRef = useRef(false);
  dirtyRef.current = dirty;

  const load = useCallback(async () => {
    try {
      const p = await ipc.recorderProject(id);
      setProj(p);
      setDoc(p.timeline);
      setPreview(p.preview ? `${convertFileSrc(p.preview)}?v=${Date.now()}` : null);
      setDirty(false);
      document.title = `拾光 · 录屏编辑：${p.meta.title}`;
    } catch (e) { setError(String(e)); }
  }, [id]);

  useEffect(() => { void load(); }, [load]);
  useEffect(() => {
    const offs = [
      onEvent<RecorderExportProgress>("recorder-export-progress", (p) => { if (p.id === id) setProgress(p); }),
      // 拾光（或别的窗口）改了这份时间线：本地没有待保存的修改就重新加载
      onEvent<{ id: string }>("recorder-timeline-changed", (p) => {
        if (p.id !== id || savingRef.current) return;
        if (dirtyRef.current) setNotice("拾光修改了这份录屏，你这边的改动保存后会覆盖它。");
        else void load().then(() => setNotice("已载入拾光的修改。"));
      }),
    ];
    return () => { offs.forEach((o) => void o.then((f) => f())); };
  }, [id, load]);

  const save = useCallback(async (d: TimelineDoc): Promise<boolean> => {
    const v = version.current;
    savingRef.current = true;
    setSaving(true);
    try {
      const saved = await ipc.recorderSaveTimeline(id, d);
      if (version.current === v) { setDoc(saved); setDirty(false); }
      return true;
    } catch (e) { setError(`保存失败：${String(e)}`); return false; } finally { savingRef.current = false; setSaving(false); }
  }, [id]);

  // 自动保存
  useEffect(() => {
    if (!dirty || !doc) return;
    const timer = window.setTimeout(() => void save(doc), 700);
    return () => window.clearTimeout(timer);
  }, [dirty, doc, save]);

  const duration = proj?.duration ?? 0;
  const frameUrl = useMemo(() => {
    if (!proj || proj.frames.length === 0) return "";
    let lo = 0, hi = proj.frames.length - 1;
    while (lo < hi) { const mid = (lo + hi + 1) >> 1; if (proj.frames[mid][0] <= t) lo = mid; else hi = mid - 1; }
    return convertFileSrc(`${proj.dir}\\frames\\${proj.frames[lo][1]}`);
  }, [proj, t]);

  const update = (fn: (d: TimelineDoc) => TimelineDoc) => {
    version.current += 1;
    setDoc((d) => (d ? fn(d) : d));
    setDirty(true);
    setNotice("");
    setError("");
  };

  const exportVideo = async (previewOnly: boolean) => {
    if (dirty && doc && !(await save(doc))) return;
    setBusy(previewOnly ? "preview" : "export");
    setProgress(null);
    setError("");
    try {
      const r = await ipc.recorderExport(id, { preview: previewOnly });
      if (previewOnly) setPreview(`${convertFileSrc(r.path)}?v=${Date.now()}`);
      else { setExported(r.path); setNotice("已导出 MP4。对外发送前请完整看一遍，确认没有漏掉的敏感信息。"); }
    } catch (e) { setError(`导出失败：${String(e)}`); } finally { setBusy(""); setProgress(null); }
  };

  const handToAI = async (text: string) => {
    const want = text.trim();
    if (!want || !proj) return;
    const prompt = `请帮我编辑录屏「${proj.meta.title}」（录屏 id：${id}）：${want}`;
    try {
      await emit("chat-prefill", { text: prompt });
      await ipc.showChat();
      setAsk("");
      setNotice("已填到聊天框，确认后发送。拾光改完这里会自动刷新。");
    } catch (e) { setError(String(e)); }
  };

  // 画面上拖动框选打码
  const norm = (e: React.PointerEvent) => {
    const r = stage.current!.getBoundingClientRect();
    return { x: Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)), y: Math.min(1, Math.max(0, (e.clientY - r.top) / r.height)) };
  };
  const onDown = (e: React.PointerEvent) => {
    const p = norm(e);
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
  };

  if (!proj || !doc) {
    return <div className="p-4 text-xs text-slate-400">{error || "加载中…"}</div>;
  }

  const pct = (v: number) => `${(Math.min(Math.max(v, 0), duration) / duration) * 100}%`;
  const burnValue = doc.burn_subtitles == null ? "" : doc.burn_subtitles ? "on" : "off";
  const tightenValue = doc.tighten == null ? "" : doc.tighten ? "on" : "off";
  const setMusic = (path: string | null) => update((d) => ({
    ...d,
    music: path ? { path, volume: d.music?.volume ?? 0.25, duck: d.music?.duck ?? 0.35, fade_in: d.music?.fade_in ?? 1, fade_out: d.music?.fade_out ?? 2.5 } : null,
  }));
  const pctOf = (p: RecorderExportProgress) => (p.total ? Math.round(((p.done ?? 0) / p.total) * 100) : 0);
  const progressLabel = progress?.phase === "ffmpeg" ? "准备 ffmpeg…" : progress?.phase === "audio" ? "处理配音…"
    : progress?.phase === "font" ? `下载字体 ${pctOf(progress)}%`
    : progress?.phase === "scene" ? `渲染片段 ${progress.scene ?? ""}/${progress.scenes ?? ""} · ${pctOf(progress)}%`
    : progress?.phase === "render" && progress.total ? `渲染 ${pctOf(progress)}%` : "准备中…";
  const scenes = doc.scenes ?? [];
  const style = doc.subtitle_style;
  const customStyle = !!style && Object.keys(style).some((k) => k !== "preset");
  const presetValue = style?.preset || (style ? "classic" : "");

  const track = (items: { start: number; end: number; key: string; title: string }[], color: string) =>
    <div className="relative h-3">
      {items.map((it) => <button key={it.key} type="button" title={it.title} onClick={() => setT(it.start)}
        className={`absolute top-0.5 h-2 rounded-sm ${color}`} style={{ left: pct(it.start), width: `max(3px, calc(${pct(it.end)} - ${pct(it.start)}))` }} />)}
    </div>;

  const row = (key: string, at: number, body: React.ReactNode, onDelete: () => void) =>
    <div key={key} className="flex items-center gap-2 text-xs text-slate-400">
      <button type="button" className="w-24 shrink-0 text-left font-mono hover:text-sky-300" title="跳到这里" onClick={() => setT(at)}>{body}</button>
      <button type="button" className="ml-auto shrink-0 text-slate-500 hover:text-rose-300" onClick={onDelete}>删除</button>
    </div>;

  return <div className="h-full space-y-3 overflow-y-auto pr-1 text-sm">
    <div className="flex items-center justify-between gap-3">
      <div className="flex min-w-0 items-center gap-2">
        <h2 className="truncate text-base font-semibold text-slate-100">{proj.meta.title}</h2>
        <span className="shrink-0 text-[11px] text-slate-500">{fmt(duration)} · {saving ? "保存中…" : dirty ? "待保存" : "已保存"}</span>
      </div>
      <div className="flex shrink-0 gap-2">
        <button type="button" disabled={!!busy} onClick={() => void exportVideo(true)} className={btn}>{busy === "preview" ? progressLabel : "快速预览"}</button>
        <button type="button" disabled={!!busy} onClick={() => void exportVideo(false)} className="rounded-md bg-sky-600 px-3 py-1 text-xs text-white hover:bg-sky-500 disabled:opacity-50">{busy === "export" ? progressLabel : "导出 MP4"}</button>
        {exported && <button type="button" onClick={() => void ipc.recorderReveal(exported).catch((e) => setError(String(e)))} className={btn}>打开位置</button>}
      </div>
    </div>

    {/* 主要的编辑方式：交给拾光 */}
    <div className="space-y-2 rounded-lg border border-sky-500/40 bg-sky-500/5 p-3">
      <div className="flex gap-2">
        <input className="flex-1 rounded-md border border-slate-600 bg-slate-900/60 px-2.5 py-1.5 text-xs text-slate-100 outline-none placeholder:text-slate-500 focus:border-sky-500"
          value={ask} onChange={(e) => setAsk(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing) void handToAI(ask); }}
          placeholder="想怎么改？比如：写一段解说并配音，剪掉开头等待的部分" />
        <button type="button" disabled={!ask.trim()} onClick={() => void handToAI(ask)} className="shrink-0 rounded-md bg-sky-600 px-3 py-1.5 text-xs text-white hover:bg-sky-500 disabled:opacity-50">交给拾光</button>
      </div>
      <div className="flex flex-wrap gap-1.5">
        {IDEAS.map((s) => <button key={s} type="button" onClick={() => void handToAI(s)}
          className="rounded-full border border-slate-600 px-2 py-0.5 text-[11px] text-slate-300 hover:border-sky-500 hover:text-sky-200">{s}</button>)}
      </div>
    </div>

    {error && <div className="rounded-md border border-rose-500/40 bg-rose-500/10 px-3 py-2 text-xs text-rose-200">{error}</div>}
    {notice && <div className="rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-200">{notice}</div>}

    <div className="grid gap-3" style={{ gridTemplateColumns: preview ? "1fr 1fr" : "1fr" }}>
      <div className="space-y-1">
        <div ref={stage} className="relative cursor-cell select-none overflow-hidden rounded border border-slate-700 bg-black"
          onPointerDown={onDown} onPointerMove={onMove} onPointerUp={onUp}>
          {frameUrl && <img src={frameUrl} alt="" draggable={false} className="block w-full" />}
          {doc.masks.filter((m) => t >= m.start && t <= m.end).map((m, i) =>
            <div key={i} className="pointer-events-none absolute border-2 border-rose-500 bg-rose-500/40 backdrop-blur-sm"
              style={{ left: `${m.rect[0] * 100}%`, top: `${m.rect[1] * 100}%`, width: `${m.rect[2] * 100}%`, height: `${m.rect[3] * 100}%` }} />)}
          {drag && <div className="pointer-events-none absolute border-2 border-rose-400 bg-rose-400/20"
            style={{ left: `${Math.min(drag.x0, drag.x1) * 100}%`, top: `${Math.min(drag.y0, drag.y1) * 100}%`, width: `${Math.abs(drag.x1 - drag.x0) * 100}%`, height: `${Math.abs(drag.y1 - drag.y0) * 100}%` }} />}
        </div>
        <p className="text-[11px] text-slate-500">原始画面。拖动框选可打码（从当前时刻打到结尾）；放大、光标效果看「快速预览」。</p>
      </div>
      {preview && <div className="space-y-1">
        <video src={preview} controls className="w-full rounded border border-slate-700 bg-black" />
        <p className="text-[11px] text-slate-500">快速预览（低分辨率），修改后需重新生成。</p>
      </div>}
    </div>

    <div className="space-y-1 rounded-lg border border-slate-700/60 bg-slate-900/30 p-3">
      <div className="flex items-center gap-3">
        <span className="w-24 shrink-0 font-mono text-xs text-slate-300">{fmt(t)}</span>
        <input type="range" className="flex-1" min={0} max={duration} step={0.05} value={t} onChange={(e) => setT(parseFloat(e.target.value))} />
      </div>
      <div className="relative ml-[6.75rem]" onClick={(e) => {
        if ((e.target as HTMLElement).closest("button")) return;
        const r = e.currentTarget.getBoundingClientRect();
        setT(Math.max(0, Math.min(duration, ((e.clientX - r.left) / r.width) * duration)));
      }}>
        <div className="pointer-events-none absolute inset-y-0 z-10 w-px bg-amber-300" style={{ left: pct(t) }} />
        <div className="relative h-3">
          {proj.marks.map((m, i) => <button key={`m${i}`} type="button" title={`标记：${m.label}`} onClick={() => setT(m.t)} className="absolute top-0 h-3 w-1 -translate-x-1/2 rounded-sm bg-amber-400" style={{ left: pct(m.t) }} />)}
          {proj.clicks.map((c, i) => <button key={`c${i}`} type="button" title={`点击：${c.label}`} onClick={() => setT(c.t)} className="absolute top-1 h-1.5 w-1.5 -translate-x-1/2 rounded-full bg-slate-300" style={{ left: pct(c.t) }} />)}
        </div>
        {track(doc.narration.map((n, i) => ({ start: n.at, end: n.at + estimate(n.text), key: `n${i}`, title: `解说：${n.text}` })), "bg-emerald-500/80")}
        {track(doc.zooms.map((z, i) => ({ start: z.start, end: z.end, key: `z${i}`, title: `放大 ${z.scale}×` })), "bg-sky-500/80")}
        {track(doc.masks.map((m, i) => ({ start: m.start, end: m.end, key: `k${i}`, title: "打码" })), "bg-rose-500/80")}
        {track(doc.cuts.map((c, i) => ({ start: c.start, end: c.end, key: `x${i}`, title: "剪掉" })), "bg-slate-500")}
        {scenes.length > 0 && track(scenes.map((sc) => ({ start: sc.at, end: sc.at + (sc.kind === "overlay" ? sc.duration : 0.4), key: `s${sc.id}`, title: `${sc.kind === "overlay" ? "叠加" : "插入"}片段：${sc.title || sc.template || sc.id}（${sc.duration} 秒）` })), "bg-violet-500/80")}
      </div>
      <div className="ml-[6.75rem] flex flex-wrap gap-3 text-[10px] text-slate-500">
        <span><i className="mr-1 inline-block h-2 w-2 bg-amber-400" />标记</span><span><i className="mr-1 inline-block h-2 w-2 rounded-full bg-slate-300" />点击</span>
        <span><i className="mr-1 inline-block h-2 w-2 bg-emerald-500" />解说</span><span><i className="mr-1 inline-block h-2 w-2 bg-sky-500" />放大</span>
        <span><i className="mr-1 inline-block h-2 w-2 bg-rose-500" />打码</span><span><i className="mr-1 inline-block h-2 w-2 bg-slate-500" />剪掉</span>
        {scenes.length > 0 && <span><i className="mr-1 inline-block h-2 w-2 bg-violet-500" />代码片段</span>}
      </div>
    </div>

    {/* 轻量的手动调整 */}
    <details className="rounded-lg border border-slate-700/60 bg-slate-900/20" open={doc.narration.length + doc.masks.length + doc.cuts.length > 0}>
      <summary className="cursor-pointer select-none px-3 py-2 text-xs text-slate-300">
        手动调整 · 解说 {doc.narration.length} · 打码 {doc.masks.length} · 剪掉 {doc.cuts.length} · 片段 {scenes.length} · 放大 {doc.zooms_edited ? `${doc.zooms.length} 段（手动）` : "自动"}
      </summary>
      <div className="space-y-3 border-t border-slate-700/60 p-3">
        <div className="space-y-1.5">
          <div className="flex items-center gap-2 text-xs text-slate-300">
            <span className="font-medium">解说</span>
            <button type="button" className={btn} onClick={() => update((d) => ({ ...d, narration: [...d.narration, { at: r1(t), text: "" }] }))}>在 {fmt(t)} 添加</button>
          </div>
          {doc.narration.map((n, i) => <div key={i} className="flex items-center gap-2 text-xs text-slate-400">
            <button type="button" className="w-14 shrink-0 text-left font-mono hover:text-sky-300" title="跳到这里" onClick={() => setT(n.at)}>{fmt(n.at)}</button>
            <input className="min-w-0 flex-1 rounded border border-slate-700 bg-slate-900/60 px-2 py-0.5 text-xs text-slate-100 outline-none focus:border-sky-500" value={n.text} placeholder="解说词（也用作字幕）"
              onChange={(e) => update((d) => ({ ...d, narration: d.narration.map((x, j) => (j === i ? { ...x, text: e.target.value } : x)) }))} />
            <span className="w-8 shrink-0 text-[10px] text-slate-500" title={n.audio ?? ""}>{n.audio ? "有配音" : ""}</span>
            <button type="button" className="shrink-0 text-slate-500 hover:text-rose-300" onClick={() => update((d) => ({ ...d, narration: d.narration.filter((_, j) => j !== i) }))}>删除</button>
          </div>)}
        </div>

        {scenes.length > 0 && <div className="space-y-1">
          <div className="text-xs font-medium text-slate-300">代码片段 <span className="font-normal text-slate-500">（片头、章节卡、贴纸等，内容让拾光改）</span></div>
          {scenes.map((sc, i) => <div key={sc.id} className="flex items-center gap-2 text-xs text-slate-400">
            <button type="button" className="w-14 shrink-0 text-left font-mono hover:text-sky-300" title="跳到这里" onClick={() => setT(sc.at)}>{fmt(sc.at)}</button>
            <span className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] ${sc.kind === "overlay" ? "bg-violet-500/20 text-violet-200" : "bg-sky-500/20 text-sky-200"}`}>{sc.kind === "overlay" ? "叠加" : "插入"}</span>
            <span className="min-w-0 flex-1 truncate text-slate-200">{sc.title || (proj.scene_templates?.find((t) => t.id === sc.template)?.label.split("：")[0]) || sc.id}</span>
            <span className="shrink-0">{sc.duration} 秒</span>
            <button type="button" className="shrink-0 text-slate-500 hover:text-rose-300" onClick={() => update((d) => ({ ...d, scenes: (d.scenes ?? []).filter((_, j) => j !== i) }))}>删除</button>
          </div>)}
        </div>}

        <div className="grid gap-3 sm:grid-cols-3">
          <div className="space-y-1">
            <div className="text-xs font-medium text-slate-300">打码 <span className="font-normal text-slate-500">（在画面上拖动添加）</span></div>
            {doc.masks.map((m, i) => row(`k${i}`, m.start, `${fmt(m.start)}–${fmt(m.end)}`, () => update((d) => ({ ...d, masks: d.masks.filter((_, j) => j !== i) }))))}
          </div>
          <div className="space-y-1">
            <div className="flex items-center gap-2 text-xs font-medium text-slate-300">剪掉
              {cutStart === null
                ? <button type="button" className={btn} onClick={() => setCutStart(r1(t))}>从 {fmt(t)} 开始</button>
                : <>
                  <button type="button" className={btn} disabled={t <= cutStart} onClick={() => {
                    const start = cutStart;
                    update((d) => ({ ...d, cuts: [...d.cuts, { start, end: r1(t) }].sort((a, b) => a.start - b.start) }));
                    setCutStart(null);
                  }}>剪到 {fmt(t)}</button>
                  <button type="button" className={btn} onClick={() => setCutStart(null)}>取消</button>
                </>}
            </div>
            {doc.cuts.map((c, i) => row(`x${i}`, c.start, `${fmt(c.start)}–${fmt(c.end)}`, () => update((d) => ({ ...d, cuts: d.cuts.filter((_, j) => j !== i) }))))}
          </div>
          <div className="space-y-1">
            <div className="flex items-center gap-2 text-xs font-medium text-slate-300">放大
              {doc.zooms_edited
                ? <button type="button" className={btn} onClick={() => update((d) => ({ ...d, zooms_edited: false }))}>恢复自动</button>
                : <span className="font-normal text-slate-500">按点击自动生成</span>}
            </div>
            {doc.zooms_edited && doc.zooms.map((z, i) => row(`z${i}`, z.start, `${fmt(z.start)} ${z.scale.toFixed(1)}×`,
              () => update((d) => ({ ...d, zooms_edited: true, zooms: d.zooms.filter((_, j) => j !== i) }))))}
          </div>
        </div>
      </div>
    </details>

    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 rounded-lg border border-slate-700/60 bg-slate-900/20 px-3 py-2 text-xs text-slate-400">
      <label className="flex items-center gap-1.5">配乐
        <select className={`${sel} max-w-[14rem]`} value={doc.music?.path ?? ""} onChange={(e) => setMusic(e.target.value || null)}>
          <option value="">{proj.music_library.length ? "不加" : "不加（配乐目录是空的）"}</option>
          {doc.music && !proj.music_library.includes(doc.music.path) && <option value={doc.music.path}>{fileName(doc.music.path)}</option>}
          {proj.music_library.map((m) => <option key={m} value={m}>{fileName(m)}</option>)}
        </select>
      </label>
      <label className="flex items-center gap-1.5">字幕样式
        <select className={sel} value={presetValue} onChange={(e) => update((d) => ({ ...d, subtitle_style: e.target.value ? { preset: e.target.value } : null }))}>
          <option value="">经典（默认）</option>
          {(proj.subtitle_presets ?? []).filter((p) => p.id !== "classic").map((p) => <option key={p.id} value={p.id}>{p.label.split("：")[0]}{customStyle && presetValue === p.id ? "（拾光调过）" : ""}</option>)}
        </select>
      </label>
      <label className="flex items-center gap-1.5">字幕
        <select className={sel} value={burnValue} onChange={(e) => update((d) => ({ ...d, burn_subtitles: e.target.value === "" ? null : e.target.value === "on" }))}>
          <option value="">按设置（{proj.burn_default ? "画进画面" : "不画"}）</option>
          <option value="on">画进画面</option>
          <option value="off">只生成 .srt</option>
        </select>
      </label>
      <label className="flex items-center gap-1.5">节奏
        <select className={sel} value={tightenValue} onChange={(e) => update((d) => ({ ...d, tighten: e.target.value === "" ? null : e.target.value === "on" }))}>
          <option value="">按设置（{proj.tighten_default ? "紧凑" : "保留原样"}）</option>
          <option value="on">紧凑：剪掉静止空档</option>
          <option value="off">保留原样</option>
        </select>
      </label>
      <span className="text-[11px] text-slate-500">配乐目录：{proj.music_dir}</span>
    </div>
  </div>;
}
