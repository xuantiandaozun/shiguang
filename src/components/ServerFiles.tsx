import { useEffect, useRef, useState } from "react";
import { open, confirm } from "@tauri-apps/plugin-dialog";
import { fileSize, remoteJoin, TerminalFiles } from "../lib/useTerminalFiles";

const button = "rounded px-2 py-1.5 hover:bg-slate-700 disabled:opacity-40 disabled:cursor-not-allowed";
const date = (mtime: number | null) => mtime === null ? "—" : new Date(mtime * 1000).toLocaleString();

export default function ServerFiles({ files, connected, onLocalFiles }: { files: TerminalFiles; connected: boolean; onLocalFiles: (paths: string[]) => void }) {
  const { listing, loading, refreshing, error, transfers, refresh } = files;
  const [address, setAddress] = useState(listing.path);
  const [filter, setFilter] = useState("");
  const [uploading, setUploading] = useState(false);
  const [message, setMessage] = useState("");
  const [failure, setFailure] = useState("");
  const [selected, setSelected] = useState("");
  const [preview, setPreview] = useState<{ path: string; content: string; truncated: boolean }>();
  const [previewBusy, setPreviewBusy] = useState(false);
  const previewVersion = useRef(0);
  useEffect(() => { setAddress(listing.path); setSelected(""); setPreview(undefined); setPreviewBusy(false); previewVersion.current++; }, [listing.path]);
  useEffect(() => () => { previewVersion.current++; }, []);
  const choose = async (forAi = false) => {
    setFailure(""); setMessage("");
    try {
      const result = await open({ multiple: true, directory: false, title: forAi ? "选择供 AI 上传的本机文件" : "选择上传文件" });
      const paths = typeof result === "string" ? [result] : result;
      if (!paths?.length) return;
      onLocalFiles(paths);
      if (forAi) { setMessage(`已向 AI 提供 ${paths.length} 个本机文件路径，请在输入框说明上传位置。`); return; }
      setUploading(true);
      const destination = listing.path;
      let completed = 0;
      for (const local of paths) {
        const name = local.split(/[\\/]/).pop()!;
        const spec = await files.prepare({ local_path: local, remote_path: remoteJoin(destination, name), overwrite: false });
        if (spec.exists || spec.sensitive) {
          const accepted = await confirm(`${local}\n→ ${spec.remote_path}\n${spec.exists ? "同名文件已存在，将覆盖原文件。\n" : ""}${spec.sensitive ? "目标位于敏感位置，请核对上传影响。\n" : ""}是否继续？`, { title: spec.exists ? "确认覆盖上传" : "确认上传位置", kind: "warning", okLabel: spec.exists ? "覆盖并上传" : "继续上传", cancelLabel: "跳过" });
          if (!accepted) continue;
          spec.overwrite = !!spec.exists;
        }
        await files.upload(spec);
        completed++;
      }
      setMessage(`已完成 ${completed} / ${paths.length} 个文件上传`);
    } catch (e) { setFailure(String(e)); }
    finally { setUploading(false); }
  };
  const showPreview = async (path: string) => {
    const version = ++previewVersion.current;
    setPreviewBusy(true); setFailure(""); setPreview(undefined);
    try {
      const result = await files.call<{ content: string; truncated: boolean }>("preview", path);
      if (version === previewVersion.current) setPreview({ path, ...result });
    } catch (e) { if (version === previewVersion.current) setFailure(String(e)); }
    finally { if (version === previewVersion.current) setPreviewBusy(false); }
  };
  const entries = listing.entries.filter(e => e.name.toLowerCase().includes(filter.toLowerCase()));
  return <aside aria-label="远程文件" className="flex h-full min-h-0 flex-col border-l border-slate-700 bg-slate-900 text-sm">
    <div className="flex items-center justify-between border-b border-slate-700 px-3 py-2">
      <h2 className="font-medium">远程文件</h2>
      <div className="flex items-center gap-1">
        <button className={`${button} text-sky-300`} disabled={!connected || uploading || loading || !listing.path.startsWith("/")} title={`上传至 ${listing.path}`} onClick={() => void choose()}>上传文件</button>
        <button className={button} disabled={!connected || refreshing} onClick={() => void refresh()} aria-label="刷新远程目录">{refreshing ? "刷新中…" : "刷新"}</button>
      </div>
    </div>
    <form className="flex gap-1 px-2 py-2" onSubmit={e => { e.preventDefault(); if (address.trim() && connected) void refresh(address.trim()); }}>
      <button type="button" title="上级目录" aria-label="上级目录" className={button} disabled={!connected || loading || listing.path === "/"} onClick={() => void refresh(remoteJoin(listing.path, ".."))}>上级</button>
      <input aria-label="远程目录路径" className="min-w-0 flex-1 rounded border border-slate-600 bg-slate-950 px-2 py-1.5 font-mono text-xs focus:border-sky-400 focus:outline-none" value={address} onChange={e => setAddress(e.target.value)} />
      <button className={button} disabled={!connected || loading || !address.trim()}>前往</button>
    </form>
    <div className="flex gap-2 px-3 pb-2">
      <input aria-label="筛选文件名" placeholder="筛选当前目录" className="min-w-0 flex-1 rounded border border-slate-700 bg-slate-950 px-2 py-1 text-xs" value={filter} onChange={e => setFilter(e.target.value)} />
      <button className={`${button} text-xs text-slate-400`} disabled={uploading} onClick={() => void choose(true)}>选文件给 AI</button>
    </div>
    {address !== listing.path && <p className="px-3 pb-2 text-xs text-amber-300 break-all">当前列表及上传目录：{listing.path}（输入路径后点击“前往”）</p>}
    {(error || failure) && <div role="alert" className="max-h-24 overflow-auto px-3 pb-2 text-xs text-red-300 whitespace-pre-wrap break-all">{error || failure}</div>}
    {message && <p role="status" className="px-3 pb-2 text-xs text-sky-300">{message}</p>}
    <div className="min-h-0 flex-1 overflow-auto" aria-busy={refreshing}>
      <table className="w-full table-fixed text-left text-xs">
        <thead className="sticky top-0 bg-slate-800 text-slate-400"><tr><th className="px-3 py-2 font-normal">名称</th><th className="w-20 pr-3 text-right font-normal">大小</th><th className="w-14 pr-2 text-right font-normal">操作</th></tr></thead>
        <tbody>{entries.map(entry => <tr key={entry.name} className={`border-t border-slate-800 ${selected === entry.name ? "bg-sky-950/60" : "hover:bg-slate-800/70"}`} onClick={() => setSelected(entry.name)} onDoubleClick={() => { if (entry.stat.is_dir && connected && !loading) void refresh(remoteJoin(listing.path, entry.name)); }}>
          <td className="px-3 py-2" title={`${entry.name}\n修改时间：${date(entry.stat.mtime)}\n权限：${entry.stat.permissions === null ? "—" : (entry.stat.permissions & 0o777).toString(8)}`}>
            {entry.stat.is_dir ? <button className="w-full truncate text-left text-sky-300 disabled:opacity-40" disabled={!connected || loading} onClick={() => void refresh(remoteJoin(listing.path, entry.name))}>▸ {entry.name}</button> : <span className="block truncate">{entry.stat.is_file ? "" : "↗ "}{entry.name}</span>}
          </td>
          <td className="pr-3 text-right text-slate-400">{entry.stat.is_dir ? "目录" : fileSize(entry.stat.size)}</td>
          <td className="pr-2 text-right">{entry.stat.is_file && <button className="py-1 text-sky-300 disabled:opacity-40" disabled={!connected || previewBusy} onClick={() => void showPreview(remoteJoin(listing.path, entry.name))}>预览</button>}</td>
        </tr>)}</tbody>
      </table>
      {loading && <p role="status" className="px-3 py-4 text-slate-400">正在读取目录…</p>}
      {!loading && !entries.length && <p className="px-3 py-8 text-center text-slate-400">{!connected ? "连接服务器后显示文件" : error ? "目录读取失败，请刷新重试" : filter ? "没有匹配的文件" : "此目录为空"}</p>}
    </div>
    <div className="flex justify-between border-t border-slate-800 px-3 py-1.5 text-xs text-slate-500"><span>{listing.entries.length} 项{listing.truncated ? "（仅显示前 2000 项）" : ""}</span><span>文件目录独立于终端</span></div>
    {previewBusy && <p role="status" className="border-t border-slate-700 p-3 text-xs text-slate-400">正在读取文本…</p>}
    {preview && <section aria-label="文件内容预览" className="flex max-h-[40%] min-h-24 flex-col border-t border-slate-700">
      <div className="flex items-center justify-between gap-2 bg-slate-800 px-3 py-2 text-xs"><span className="truncate" title={preview.path}>{preview.path}</span><button className="shrink-0 text-slate-300" onClick={() => { previewVersion.current++; setPreview(undefined); }}>关闭预览</button></div>
      <pre className="min-h-0 overflow-auto whitespace-pre p-3 text-xs text-slate-300">{preview.content || "（空文件）"}</pre>
      {preview.truncated && <p className="px-3 pb-2 text-xs text-amber-300">仅预览前 128 KB</p>}
    </section>}
    {transfers.length > 0 && <section aria-label="上传任务" className="max-h-44 shrink-0 overflow-auto border-t border-slate-700">
      <h3 className="sticky top-0 bg-slate-900 px-3 py-2 text-xs font-medium">上传任务{uploading ? " · 传输中" : ""}</h3>
      {transfers.map(t => <div key={t.id} className="border-t border-slate-800 px-3 py-2 text-xs">
        <div className="flex justify-between gap-2"><span className="truncate" title={`${t.local_path}\n→ ${t.remote_path}`}>{t.remote_path.split("/").pop()}</span><span className={t.status === "done" ? "text-emerald-400" : t.status === "running" ? "text-sky-300" : "text-amber-300"}>{({ running: "上传中", done: "已完成", cancelled: "已取消", failed: "失败", unknown: "状态未知" } as Record<string, string>)[t.status] || t.status}</span></div>
        <div className="flex justify-between gap-2 pt-1 text-slate-500"><span>已传输 {fileSize(t.bytes)}</span>{(t.status === "running" || t.status === "unknown") && <button className="text-red-300" onClick={() => void files.cancel(t.id).catch(e => setFailure(String(e)))}>取消上传</button>}</div>
        {t.error && <p className="pt-1 text-red-300 break-all">{t.error}</p>}
      </div>)}
    </section>}
  </aside>;
}
