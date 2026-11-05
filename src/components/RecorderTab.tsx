import { useCallback, useEffect, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { ipc, onEvent } from "../lib/ipc";
import RecorderEditor from "./RecorderEditor";
import type { RecorderExportOpts, RecorderExportProgress, RecorderStatus, Recording } from "../lib/ipc";

const input = "w-full rounded-md border border-slate-600 bg-slate-900/60 px-2.5 py-1.5 text-xs text-slate-100 outline-none focus:border-sky-500 placeholder:text-slate-500";
const btn = "rounded-md border border-slate-600 px-2.5 py-1 text-xs text-slate-200 hover:border-sky-500 disabled:opacity-50";

const clock = (s = 0) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
const when = (iso: string) => {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" });
};

function progressText(p: RecorderExportProgress | null) {
  if (!p || p.phase === "done") return "";
  const total = p.total ?? 0;
  const done = p.done ?? 0;
  if (p.phase === "audio") return "处理配音…";
  if (p.phase === "ffmpeg") {
    return total > 0 ? `首次准备 ffmpeg：${(done / 1048576).toFixed(1)} / ${(total / 1048576).toFixed(1)} MB` : `首次准备 ffmpeg：${(done / 1048576).toFixed(1)} MB`;
  }
  return total > 0 ? `渲染中 ${Math.round((done / total) * 100)}%` : "渲染中…";
}

export default function RecorderTab() {
  const [status, setStatus] = useState<RecorderStatus>({ recording: false });
  const [items, setItems] = useState<Recording[]>([]);
  const [title, setTitle] = useState("");
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [exporting, setExporting] = useState<string | null>(null);
  const [progress, setProgress] = useState<RecorderExportProgress | null>(null);
  const [style, setStyle] = useState<"" | "plain" | "padded">("");
  const [zoom, setZoom] = useState<"" | "on" | "off">("");
  const [playing, setPlaying] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [st, list] = await Promise.all([ipc.recorderStatus(), ipc.recorderList()]);
      setStatus(st);
      setItems(list);
    } catch (e) { setError(String(e)); }
  }, []);

  useEffect(() => {
    void refresh();
    const offs = [
      onEvent("recorder-changed", () => void refresh()),
      onEvent<RecorderExportProgress>("recorder-export-progress", (p) => setProgress(p)),
    ];
    return () => { offs.forEach((o) => void o.then((f) => f())); };
  }, [refresh]);

  // 录制中每秒刷新时长和计数
  useEffect(() => {
    if (!status.recording) return;
    const timer = setInterval(() => void ipc.recorderStatus().then(setStatus).catch(() => {}), 1000);
    return () => clearInterval(timer);
  }, [status.recording]);

  const run = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    setError("");
    try { await fn(); } catch (e) { setError(String(e)); } finally { setBusy(false); void refresh(); }
  };

  const start = () => run(async () => {
    await ipc.recorderStart(title.trim(), url.trim());
    setTitle("");
  });
  const stop = () => run(() => ipc.recorderStop());

  const exportOne = async (id: string) => {
    const opts: RecorderExportOpts = {};
    if (style) opts.style = style;
    if (zoom) opts.zoom = zoom === "on";
    setExporting(id);
    setProgress(null);
    setError("");
    try {
      await ipc.recorderExport(id, opts);
      setPlaying(id);
    } catch (e) {
      setError(`导出失败：${String(e)}`);
    } finally {
      setExporting(null);
      setProgress(null);
      void refresh();
    }
  };

  const remove = (id: string) => run(async () => {
    await ipc.recorderDelete(id);
    setPendingDelete(null);
  });

  const oldExtension = /版本太旧/.test(error);

  if (editing) return <RecorderEditor id={editing} onClose={() => { setEditing(null); void refresh(); }} />;

  return <div className="space-y-6 text-sm">
    <section className="space-y-3">
      <div>
        <h2 className="text-base font-semibold text-slate-100">录屏</h2>
        <p className="mt-1 text-xs text-slate-400">录制浏览器当前标签页的操作。导出时自动放大点击处、画放大的光标和点击波纹。密码、手机号、身份证、邮箱、银行卡、密钥在页面上就被模糊，录到的画面里没有明文；更多打码规则在「插件 → 录屏」设置里加。也可以在聊天里让拾光边操作边录。</p>
      </div>

      {status.recording
        ? <div className="flex items-center justify-between gap-3 rounded-lg border border-rose-500/40 bg-rose-500/10 px-4 py-3">
          <div className="min-w-0">
            <div className="flex items-center gap-2 text-sm text-rose-100">
              <span className="inline-block h-2.5 w-2.5 animate-pulse rounded-full bg-rose-500" />
              <span className="truncate">正在录制：{status.title}</span>
            </div>
            <div className="mt-1 text-xs text-rose-200/80">
              {clock(status.elapsed_s)} · {status.frames ?? 0} 帧 · {status.clicks ?? 0} 次点击{status.marks ? ` · ${status.marks} 个标记` : ""}
              {status.ended_reason && <span className="ml-2 text-amber-300">{status.ended_reason}</span>}
            </div>
          </div>
          <button type="button" disabled={busy} onClick={() => void stop()} className="shrink-0 rounded-md bg-rose-600 px-3 py-1.5 text-xs text-white hover:bg-rose-500 disabled:opacity-50">停止录制</button>
        </div>
        : <div className="space-y-2 rounded-lg border border-slate-700/60 bg-slate-900/30 p-4">
          <div className="grid grid-cols-2 gap-2">
            <input className={input} value={title} onChange={(e) => setTitle(e.target.value)} placeholder="标题（可选，默认用网页标题）" />
            <input className={input} value={url} onChange={(e) => setUrl(e.target.value)} placeholder="先打开的网址（可选，默认录当前标签页）" />
          </div>
          <div className="flex items-center justify-between gap-3">
            <p className="text-[11px] text-slate-500">录制中不要切换标签页，否则画面会停住。</p>
            <button type="button" disabled={busy} onClick={() => void start()} className="shrink-0 rounded-md bg-sky-600 px-3 py-1.5 text-xs text-white hover:bg-sky-500 disabled:opacity-50">开始录制</button>
          </div>
        </div>}

      {error && <div className="rounded-md border border-rose-500/40 bg-rose-500/10 px-3 py-2 text-xs text-rose-200">
        {error}
        {oldExtension && <div className="mt-1 text-rose-100/80">打开浏览器的扩展管理页（chrome://extensions 或 edge://extensions），找到「拾光浏览器桥」点「重新加载」。</div>}
      </div>}
    </section>

    <section className="space-y-3">
      <div className="flex items-center justify-between gap-3">
        <h3 className="text-sm font-medium text-slate-200">录制记录</h3>
        <div className="flex items-center gap-2 text-xs text-slate-400">
          <span>导出：</span>
          <select className="rounded-md border border-slate-600 bg-slate-900/60 px-2 py-1 text-xs text-slate-200" value={style} onChange={(e) => setStyle(e.target.value as typeof style)}>
            <option value="">画面风格按设置</option>
            <option value="plain">原样</option>
            <option value="padded">带背景和圆角</option>
          </select>
          <select className="rounded-md border border-slate-600 bg-slate-900/60 px-2 py-1 text-xs text-slate-200" value={zoom} onChange={(e) => setZoom(e.target.value as typeof zoom)}>
            <option value="">自动放大按设置</option>
            <option value="on">自动放大</option>
            <option value="off">不放大</option>
          </select>
        </div>
      </div>

      {items.length === 0
        ? <div className="rounded-lg border border-slate-700/60 px-4 py-8 text-center text-xs text-slate-500">还没有录制。</div>
        : <div className="divide-y divide-slate-700/60 overflow-hidden rounded-lg border border-slate-700/60">
          {items.map((r) => {
            const isExporting = exporting === r.id;
            return <div key={r.id} className="space-y-2 px-4 py-3">
              <div className="flex items-center gap-3">
                <img src={convertFileSrc(r.thumb)} alt="" className="h-12 w-20 shrink-0 rounded border border-slate-700 bg-slate-900 object-cover" />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-sm text-slate-100">{r.title}</div>
                  <div className="mt-0.5 truncate text-[11px] text-slate-500">
                    {when(r.started_at)} · {clock(r.duration_s)} · {r.clicks} 次点击{r.marks ? ` · ${r.marks} 个标记` : ""}
                    {r.end_reason && <span className="ml-1 text-amber-400/80">（{r.end_reason}）</span>}
                  </div>
                </div>
                <div className="flex shrink-0 gap-1.5">
                  <button type="button" disabled={!!exporting || status.id === r.id} onClick={() => setEditing(r.id)} className={btn}>编辑</button>
                  {isExporting
                    ? <button type="button" onClick={() => void ipc.recorderCancelExport()} className={btn}>取消导出</button>
                    : <button type="button" disabled={!!exporting || status.id === r.id} onClick={() => void exportOne(r.id)} className={btn}>{r.exported ? "重新导出" : "导出 MP4"}</button>}
                  {r.exported && <button type="button" onClick={() => setPlaying(playing === r.id ? null : r.id)} className={btn}>{playing === r.id ? "收起" : "播放"}</button>}
                  {r.exported && <button type="button" onClick={() => void ipc.recorderReveal(r.exported!).catch((e) => setError(String(e)))} className={btn}>打开位置</button>}
                  {pendingDelete === r.id
                    ? <>
                      <button type="button" disabled={busy} onClick={() => void remove(r.id)} className="rounded-md bg-rose-600 px-2.5 py-1 text-xs text-white hover:bg-rose-500">确认删除</button>
                      <button type="button" onClick={() => setPendingDelete(null)} className={btn}>取消</button>
                    </>
                    : <button type="button" disabled={status.id === r.id} onClick={() => setPendingDelete(r.id)} className={btn}>删除</button>}
                </div>
              </div>
              {isExporting && <div className="space-y-1">
                <div className="h-1.5 overflow-hidden rounded bg-slate-800">
                  <div className="h-full bg-sky-500 transition-all" style={{ width: `${progress?.total ? Math.min(100, ((progress.done ?? 0) / progress.total) * 100) : 5}%` }} />
                </div>
                <div className="text-[11px] text-slate-400">{progressText(progress) || "准备中…"}</div>
              </div>}
              {pendingDelete === r.id && <p className="text-[11px] text-slate-500">只删除原始素材，已导出的 MP4 保留。</p>}
              {playing === r.id && r.exported && <div className="space-y-1">
                <video src={convertFileSrc(r.exported)} controls className="w-full rounded border border-slate-700 bg-black" />
                <p className="text-[11px] text-amber-300/80">对外发送前请完整看一遍，确认没有漏掉的敏感信息。</p>
              </div>}
            </div>;
          })}
        </div>}
    </section>
  </div>;
}
