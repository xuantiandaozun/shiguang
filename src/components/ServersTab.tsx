import { useEffect, useRef, useState } from "react";
import { open, save, confirm } from "@tauri-apps/plugin-dialog";
import { ipc, onEvent, ServerConnection, RemoteTask, RemoteTaskView } from "../lib/ipc";

type Entry = { name: string; stat: { size: number | null; is_dir: boolean; is_file: boolean } };
const blank = (): ServerConnection => ({ id: "", name: "", host: "", port: 22, username: "root", group: "", note: "", auth: "password", private_key: "", fingerprint: null });
const input = "w-full rounded border border-slate-600 bg-slate-900 px-2.5 py-2 text-sm text-slate-100 focus:outline-none focus:border-sky-400";
const button = "rounded px-3 py-1.5 text-sm bg-slate-700 hover:bg-slate-600 disabled:opacity-40 disabled:cursor-not-allowed";
const statuses: Record<string, string> = { running: "运行中", done: "已完成", failed: "失败", cancelled: "已取消", unknown: "状态待核实" };
const kinds: Record<string, string> = { ssh_exec: "命令", sftp_upload: "上传", sftp_download: "下载" };
const parent = (path: string) => path.replace(/\/+$/, "").split("/").slice(0, -1).join("/") || "/";
const join = (path: string, name: string) => `${path.replace(/\/+$/, "")}/${name}`;
const size = (n: number) => n < 1024 ? `${n} B` : n < 1024 * 1024 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1024 / 1024).toFixed(1)} MB`;

export default function ServersTab() {
  const [servers, setServers] = useState<ServerConnection[]>([]);
  const [selected, setSelected] = useState("");
  const [editor, setEditor] = useState<ServerConnection | null>(null);
  const [password, setPassword] = useState("");
  const [secretChanged, setSecretChanged] = useState(false);
  const [visible, setVisible] = useState(false);
  const [filter, setFilter] = useState("");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [command, setCommand] = useState("");
  const [workdir, setWorkdir] = useState("");
  const [path, setPath] = useState("/");
  const [loadedPath, setLoadedPath] = useState("");
  const [entries, setEntries] = useState<Entry[]>([]);
  const [truncated, setTruncated] = useState(false);
  const [tasks, setTasks] = useState<RemoteTask[]>([]);
  const [taskId, setTaskId] = useState("");
  const [view, setView] = useState<RemoteTaskView | null>(null);
  const version = useRef(0);
  const server = servers.find(s => s.id === selected);
  const call = <T,>(name: string, args: Record<string, unknown> = {}) => ipc.serverAction<T>(name, args);
  const load = async () => {
    const [connections, jobs] = await Promise.all([call<ServerConnection[]>("list_servers"), call<RemoteTask[]>("list_remote_tasks")]);
    setServers(connections); setTasks(jobs);
    setSelected(id => connections.some(s => s.id === id) ? id : connections[0]?.id ?? "");
  };
  useEffect(() => {
    let dead = false;
    const refresh = () => { if (!dead) load().catch(e => setError(String(e))); };
    refresh();
    const listeners = [onEvent("servers-changed", refresh), onEvent("remote-tasks-changed", refresh)];
    return () => { dead = true; listeners.forEach(p => p.then(f => f())); };
  }, []);
  useEffect(() => { version.current++; setPath("/"); setLoadedPath(""); setEntries([]); setView(null); setTaskId(""); setMessage(""); setError(""); }, [selected]);
  useEffect(() => {
    let dead = false;
    const refresh = async () => {
      try { const jobs = await call<RemoteTask[]>("list_remote_tasks"); if (!dead) setTasks(jobs);
        if (taskId) { const result = await call<RemoteTaskView>("get_remote_task", { task_id: taskId }); if (!dead) setView(result); }
      } catch (e) { if (!dead) setError(String(e)); }
    };
    refresh(); const timer = window.setInterval(refresh, 2000); return () => { dead = true; clearInterval(timer); };
  }, [taskId]);
  async function perform(action: () => Promise<void>) { setError(""); setMessage(""); setBusy(true); try { await action(); } catch (e) { setError(String(e)); } finally { setBusy(false); } }
  function edit(s: ServerConnection) { setEditor({ ...s }); setPassword(""); setSecretChanged(false); setVisible(false); setError(""); }
  async function browse(target: string) {
    const current = version.current;
    const result = await call<{ entries: Entry[]; truncated: boolean }>("sftp_list", { server_id: selected, remote_path: target });
    if (version.current !== current) return;
    setEntries(result.entries.sort((a, b) => Number(b.stat.is_dir) - Number(a.stat.is_dir) || a.name.localeCompare(b.name)));
    setPath(target); setLoadedPath(target); setTruncated(result.truncated);
  }
  async function start(name: string, args: Record<string, unknown>) {
    const result = await call<{ task_id: string }>(name, { server_id: selected, ...args });
    setTaskId(result.task_id); setView(null); await load();
  }
  async function upload(directory: boolean) {
    const local = await open({ directory, multiple: false, title: directory ? "选择上传目录" : "选择上传文件" });
    if (!local || typeof local !== "string") return;
    const name = local.replace(/\\/g, "/").split("/").pop(); if (!name) return;
    const remote = join(loadedPath, name);
    let overwrite = false;
    if (entries.some(e => e.name === name)) { overwrite = await confirm(`远程 ${remote} 已存在。覆盖同名文件？目录会合并，已完成的文件不会回滚。`, { title: "覆盖远程文件", kind: "warning" }); if (!overwrite) return; }
    await start("sftp_upload", { local_path: local, remote_path: remote, overwrite });
  }
  async function download(entry: Entry) {
    const local = entry.stat.is_dir ? await open({ directory: true, title: "选择保存位置" }) : await save({ defaultPath: entry.name, title: "下载文件" });
    if (!local || typeof local !== "string") return;
    // Explicit confirmation for any existing destination is handled by this common choice.
    const overwrite = await confirm("是否允许覆盖保存位置中的同名文件？选择取消将仍然下载，但遇到同名文件时停止。", { title: "同名文件处理", okLabel: "允许覆盖", cancelLabel: "不覆盖" });
    await start("sftp_download", { local_path: entry.stat.is_dir ? `${local}\\${entry.name}` : local, remote_path: join(loadedPath, entry.name), overwrite });
  }
  return <div className="space-y-4">
    <div className="flex items-center justify-between gap-3"><h2 className="text-base font-medium">服务器</h2><button className={`${button} bg-sky-700 hover:bg-sky-600`} disabled={busy} onClick={() => edit(blank())}>新增连接</button></div>
    {error && <div role="alert" className="rounded border border-red-800 bg-red-950/40 px-3 py-2 text-sm text-red-300 whitespace-pre-wrap break-all">{error}</div>}
    {message && <div role="status" className="text-sm text-sky-300">{message}</div>}
    {editor ? <form className="max-w-2xl space-y-4" onSubmit={e => { e.preventDefault(); perform(async () => {
      const { id, fingerprint: _, ...fields } = editor;
      const saved = await call<ServerConnection>(id ? "update_server" : "create_server", { ...fields, ...(id ? { server_id: id } : {}), ...(secretChanged || !id ? { secret: password } : {}) });
      setEditor(null); setPassword(""); setVisible(false); await load(); setSelected(saved.id); setMessage("连接已保存");
    }); }}>
      <h3 className="text-sm font-medium">{editor.id ? "编辑连接" : "新增连接"}</h3>
      <div className="grid grid-cols-2 gap-4">{([ ["name", "连接名称"], ["group", "分组（选填）"], ["host", "主机地址"], ["username", "用户名"] ] as const).map(([key, label]) => <label key={key} className="space-y-1 text-sm text-slate-300"><span>{label}</span><input autoFocus={key === "name"} className={input} required={key !== "group"} value={editor[key]} onChange={e => setEditor({ ...editor, [key]: e.target.value })} /></label>)}
        <label className="space-y-1 text-sm text-slate-300">端口<input type="number" min={1} max={65535} required className={input} value={editor.port} onChange={e => setEditor({ ...editor, port: Number(e.target.value) })} /></label>
        <label className="space-y-1 text-sm text-slate-300">认证方式<select className={input} value={editor.auth} onChange={e => { setEditor({ ...editor, auth: e.target.value as ServerConnection["auth"] }); setPassword(""); setSecretChanged(true); }}><option value="password">密码</option><option value="key">私钥文件</option><option value="agent">SSH Agent</option></select></label>
      </div>
      {editor.auth === "key" && <label className="block space-y-1 text-sm">私钥绝对路径<div className="flex gap-2"><input required className={input} value={editor.private_key} onChange={e => setEditor({ ...editor, private_key: e.target.value })} /><button type="button" className={`${button} shrink-0`} onClick={() => perform(async () => { const path = await open({ multiple: false }); if (typeof path === "string") setEditor({ ...editor, private_key: path }); })}>选择文件</button></div></label>}
      {editor.auth !== "agent" && <label className="block space-y-1 text-sm"><span>{editor.auth === "key" ? "私钥口令（可留空）" : "密码"}</span><div className="flex gap-2"><input autoComplete="off" className={input} type={visible ? "text" : "password"} placeholder={editor.id && !secretChanged ? "留空保留已有密码，点击查看可读取" : ""} value={password} onChange={e => { setPassword(e.target.value); setSecretChanged(true); }} /><button type="button" className={`${button} shrink-0`} disabled={busy} onClick={() => perform(async () => { if (!visible && editor.id && !secretChanged) setPassword(await ipc.revealServerSecret(editor.id)); setVisible(!visible); })}>{visible ? "隐藏" : "查看"}</button><button type="button" className={`${button} shrink-0`} disabled={busy} onClick={() => perform(async () => { const text = editor.id && !secretChanged ? await ipc.revealServerSecret(editor.id) : password; await navigator.clipboard.writeText(text); setMessage("密码已复制"); })}>复制</button><button type="button" className={`${button} shrink-0`} onClick={() => { setPassword(""); setSecretChanged(true); }}>清除</button></div><span className="block text-xs text-slate-500">密码明文保存在本机 SQLite 中，可随时查看和复制。</span></label>}
      <label className="block space-y-1 text-sm">备注（选填）<textarea className={input} value={editor.note} onChange={e => setEditor({ ...editor, note: e.target.value })} /></label>
      <div className="flex justify-end gap-2"><button type="button" className={button} disabled={busy} onClick={() => { setEditor(null); setPassword(""); }}>取消</button><button disabled={busy} className={`${button} bg-sky-700 hover:bg-sky-600`} type="submit">{busy ? "保存中…" : "保存连接"}</button></div>
    </form> : <>
      <input aria-label="搜索服务器" className={input} placeholder="搜索名称、地址或分组" value={filter} onChange={e => setFilter(e.target.value)} />
      {!servers.length ? <div className="py-10 text-center text-sm text-slate-400">还没有服务器连接。新增连接后，可执行命令和传输文件。</div> : <div className="flex gap-4 items-start">
        <div className="w-48 shrink-0 space-y-1">{servers.filter(s => `${s.name} ${s.host} ${s.group}`.toLowerCase().includes(filter.toLowerCase())).map(s => <button key={s.id} disabled={busy} onClick={() => setSelected(s.id)} className={`w-full text-left px-3 py-2 rounded ${selected === s.id ? "bg-slate-700 text-sky-300" : "hover:bg-slate-700/40"}`}><div className="text-sm truncate">{s.name}</div><div className="text-xs text-slate-400 truncate">{s.group && `${s.group} · `}{s.host}:{s.port}</div></button>)}</div>
        {server && <div className="flex-1 min-w-0 space-y-4">
          <div className="flex items-start justify-between gap-2"><div><div className="font-medium">{server.name}</div><div className="text-xs text-slate-400 break-all">{server.username}@{server.host}:{server.port}</div>{server.note && <div className="text-xs text-slate-400 mt-1">{server.note}</div>}</div><div className="flex gap-1 shrink-0"><button className={button} disabled={busy} onClick={() => perform(async () => {
            const result = await call<{ status: string; fingerprint: string; previous_fingerprint?: string }>("test_server_connection", { server_id: server.id });
            if (result.status === "needs_trust") { const yes = await confirm(`${server.host}:${server.port}\n${result.previous_fingerprint ? `原指纹：${result.previous_fingerprint}\n主机指纹已变化。\n` : ""}当前指纹：${result.fingerprint}\n请与服务器管理员或可信渠道核对。信任此指纹？`, { title: "核对服务器身份", kind: "warning" }); if (!yes) return; await ipc.trustServerHost(server.id, result.fingerprint); await load(); const tested = await call<{ status: string }>("test_server_connection", { server_id: server.id }); if (tested.status !== "connected") throw new Error("指纹再次变化，请重新检查"); }
            setMessage("连接成功");
          })}>测试连接</button><button className={button} disabled={busy} onClick={() => edit(server)}>编辑</button><button className={`${button} text-red-300`} disabled={busy} onClick={() => perform(async () => { if (!await confirm(`删除“${server.name}”的连接信息和已保存密码？远程文件不会删除。`, { title: "删除连接", kind: "warning" })) return; await call("delete_server", { server_id: server.id }); await load(); })}>删除</button></div></div>
          <div className="text-xs text-slate-500 break-all">{server.fingerprint ? `已信任 ${server.fingerprint}` : "首次使用请测试连接并核对服务器指纹"}</div>
          <section className="space-y-2 border-t border-slate-700 pt-3"><h3 className="text-sm font-medium">执行命令</h3><input aria-label="远程工作目录" className={input} placeholder="工作目录（选填，每次命令独立执行）" value={workdir} onChange={e => setWorkdir(e.target.value)} /><textarea aria-label="远程命令" rows={3} className={`${input} font-mono`} value={command} onChange={e => setCommand(e.target.value)} placeholder="输入远程命令" /><div className="flex justify-end"><button className={`${button} bg-sky-700 hover:bg-sky-600`} disabled={busy || !command.trim() || !server.fingerprint} onClick={() => perform(() => start("ssh_exec", { command, workdir }))}>执行</button></div></section>
          <section className="space-y-2 border-t border-slate-700 pt-3"><h3 className="text-sm font-medium">远程文件</h3><form className="flex gap-2" onSubmit={e => { e.preventDefault(); perform(() => browse(path)); }}><input aria-label="远程目录" className={input} value={path} onChange={e => setPath(e.target.value)} /><button className={button} disabled={busy || !server.fingerprint}>打开</button><button type="button" className={`${button} shrink-0`} disabled={busy || !loadedPath} onClick={() => perform(() => browse(parent(loadedPath)))}>上级</button></form>
            {loadedPath && <><div className="flex items-center justify-between gap-2"><span className="text-xs text-slate-400 truncate">{loadedPath}</span><div className="flex gap-2 shrink-0"><button className={button} disabled={busy} onClick={() => perform(() => upload(false))}>上传文件</button><button className={button} disabled={busy} onClick={() => perform(() => upload(true))}>上传目录</button><button className={button} disabled={busy} onClick={() => perform(() => browse(loadedPath))}>刷新</button></div></div><div className="max-h-64 overflow-auto"><table className="w-full text-sm"><thead className="text-left text-xs text-slate-400"><tr><th className="py-2">名称</th><th className="text-right">大小</th><th className="w-16" /></tr></thead><tbody>{entries.map(entry => <tr key={entry.name} className="border-t border-slate-700/60"><td className="py-2 break-all">{entry.stat.is_dir ? <button className="text-sky-300 hover:underline" disabled={busy} onClick={() => perform(() => browse(join(loadedPath, entry.name)))}>{entry.name}/</button> : entry.name}</td><td className="text-right text-xs text-slate-400 whitespace-nowrap">{entry.stat.is_file ? size(entry.stat.size ?? 0) : "—"}</td><td className="text-right"><button className="text-xs text-sky-300 disabled:opacity-40" disabled={busy || (!entry.stat.is_dir && !entry.stat.is_file)} onClick={() => perform(() => download(entry))}>下载</button></td></tr>)}</tbody></table>{!entries.length && <p className="py-4 text-center text-xs text-slate-500">目录为空</p>}</div>{truncated && <p className="text-xs text-amber-300">仅显示前 2000 项，请进入具体子目录继续浏览。</p>}</>}
          </section>
        </div>}
      </div>}
      <section className="border-t border-slate-700 pt-3 space-y-2"><h3 className="text-sm font-medium">远程任务</h3>{tasks.length === 0 && <p className="text-xs text-slate-500">尚无命令或传输任务</p>}<div className="max-h-48 overflow-auto space-y-1">{tasks.map(t => <div key={t.id} className={`flex items-center gap-2 px-2 py-2 rounded text-xs ${taskId === t.id ? "bg-slate-700" : "hover:bg-slate-700/40"}`}><button className="flex-1 text-left" onClick={() => setTaskId(t.id)}>{t.server_name} · {kinds[t.kind]} · {statuses[t.status] ?? t.status} {t.kind !== "ssh_exec" && `· ${size(t.bytes)}`}</button><span className="text-slate-500">{new Date(t.started_at).toLocaleTimeString()}</span>{t.status === "running" && <button className="text-red-300" onClick={() => perform(async () => { await call("cancel_remote_task", { task_id: t.id }); setMessage("已请求取消；远程命令可能继续运行，请核实状态。"); })}>取消</button>}</div>)}</div>{view && <div className="space-y-1"><div className="text-xs text-slate-400">{statuses[view.task.status]}{view.task.exit_code !== null && ` · 退出码 ${view.task.exit_code}`} · 日志：{view.task.log_path}</div>{view.task.error && <p className="text-xs text-red-300 break-all">{view.task.error}</p>}<pre className="max-h-72 overflow-auto rounded bg-slate-950 p-3 text-xs whitespace-pre-wrap break-all select-text">{view.output || (view.task.kind === "ssh_exec" ? "暂无输出" : `已传输 ${size(view.task.bytes)}`)}</pre></div>}</section>
    </>}
  </div>;
}
