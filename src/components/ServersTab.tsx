import { useEffect, useState } from "react";
import { open, confirm } from "@tauri-apps/plugin-dialog";
import { ipc, onEvent, ServerConnection } from "../lib/ipc";

const blank = (): ServerConnection => ({ id: "", name: "", host: "", port: 22, username: "root", group: "", note: "", auth: "password", private_key: "", fingerprint: null });
const input = "w-full rounded border border-slate-600 bg-slate-900 px-2.5 py-2 text-sm text-slate-100 focus:outline-none focus:border-sky-400";
const button = "rounded px-3 py-1.5 text-sm bg-slate-700 hover:bg-slate-600 disabled:opacity-40 disabled:cursor-not-allowed";

export default function ServersTab() {
  const [servers, setServers] = useState<ServerConnection[]>([]);
  const [editor, setEditor] = useState<ServerConnection | null>(null);
  const [password, setPassword] = useState("");
  const [secretChanged, setSecretChanged] = useState(false);
  const [visible, setVisible] = useState(false);
  const [filter, setFilter] = useState("");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [connections, setConnections] = useState<Record<string, string>>({});
  const [secret, setSecret] = useState<{ name: string; value: string } | null>(null);
  const call = <T,>(name: string, args: Record<string, unknown> = {}) => ipc.serverAction<T>(name, args);
  const load = async () => setServers(await call<ServerConnection[]>("list_servers"));
  useEffect(() => {
    let dead = false;
    const refresh = async () => {
      try { const result = await call<ServerConnection[]>("list_servers"); if (!dead) setServers(result); }
      catch (e) { if (!dead) setError(String(e)); }
    };
    const status = async () => {
      try { const result = await ipc.serverTerminal<Record<string, string>>("list", ""); if (!dead) setConnections(result); }
      catch (e) { if (!dead) setError(String(e)); }
    };
    void refresh(); void status();
    const listener = onEvent("servers-changed", () => { void refresh(); });
    const timer = window.setInterval(status, 2000);
    return () => { dead = true; clearInterval(timer); void listener.then(f => f()); };
  }, []);
  async function perform(action: () => Promise<void>) {
    setError(""); setMessage(""); setBusy(true);
    try { await action(); } catch (e) { setError(String(e)); } finally { setBusy(false); }
  }
  function edit(s: ServerConnection) { setEditor({ ...s }); setPassword(""); setSecretChanged(false); setVisible(false); setError(""); }
  async function connectTerminal(s: ServerConnection) {
    const result = await call<{ status: string; fingerprint: string; previous_fingerprint?: string }>("test_server_connection", { server_id: s.id });
    if (result.status === "needs_trust") {
      if (!await confirm(`${s.host}:${s.port}\n${result.previous_fingerprint ? `原指纹：${result.previous_fingerprint}\n主机指纹已变化。\n` : ""}当前指纹：${result.fingerprint}\n请与可信渠道核对后确认。`, { title: "核对服务器身份", kind: "warning" })) return;
      await ipc.trustServerHost(s.id, result.fingerprint); await load();
    } else if (result.status !== "connected") throw new Error("连接失败，请检查服务器配置");
    await ipc.serverTerminal("open", s.id);
  }
  async function remove(s: ServerConnection) {
    if (!await confirm(`删除“${s.name}”的连接信息和已保存密码？远程文件不会删除。`, { title: "删除连接", kind: "warning" })) return;
    await call("delete_server", { server_id: s.id }); await load(); setMessage("连接已删除");
  }
  const filtered = servers.filter(s => `${s.name} ${s.host} ${s.group}`.toLowerCase().includes(filter.toLowerCase()));
  return <div className="space-y-4">
    <div className="flex items-center justify-between gap-3"><h2 className="text-base font-medium">服务器</h2><button className={`${button} bg-sky-700 hover:bg-sky-600`} disabled={busy} onClick={() => edit(blank())}>新增连接</button></div>
    {error && <div role="alert" className="rounded border border-red-800 bg-red-950/40 px-3 py-2 text-sm text-red-300 whitespace-pre-wrap break-all">{error}</div>}
    {message && <div role="status" className="text-sm text-sky-300">{message}</div>}
    {secret && <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60" onClick={() => setSecret(null)}><div role="dialog" aria-modal="true" aria-label="查看服务器密码" className="w-full max-w-md rounded-lg border border-slate-600 bg-slate-800 p-5 space-y-4" onClick={e => e.stopPropagation()} onKeyDown={e => { if (e.key === "Escape") setSecret(null); }}><h3 className="font-medium">{secret.name} · 已保存的密码 / 口令</h3><input autoFocus readOnly aria-label="已保存密码" className={`${input} font-mono`} value={secret.value || "（未保存密码或口令）"} /><div className="flex justify-end gap-2"><button className={button} onClick={() => perform(async () => { await navigator.clipboard.writeText(secret.value); setMessage("密码已复制"); })}>复制</button><button className={`${button} bg-sky-700`} onClick={() => setSecret(null)}>关闭</button></div></div></div>}
    {editor ? <form className="max-w-2xl space-y-4" onSubmit={e => { e.preventDefault(); perform(async () => {
      const { id, fingerprint: _, ...fields } = editor;
      await call<ServerConnection>(id ? "update_server" : "create_server", { ...fields, ...(id ? { server_id: id } : {}), ...(secretChanged || !id ? { secret: password } : {}) });
      setEditor(null); setPassword(""); setVisible(false); await load(); setMessage("连接已保存");
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
      {!servers.length ? <div className="py-10 text-center text-sm text-slate-400">还没有服务器连接。新增连接后，点击连接打开终端。</div> : <div className="overflow-x-auto">
        <table className="w-full text-sm text-left">
          <thead className="text-xs text-slate-400"><tr><th className="py-2">服务器</th><th>地址</th><th>终端状态</th><th className="text-right">操作</th></tr></thead>
          <tbody>{filtered.map(s => <tr key={s.id} className="border-t border-slate-700">
            <td className="py-3 pr-3"><span>{s.name}</span>{s.group && <div className="text-xs text-slate-500">{s.group}</div>}</td>
            <td className="pr-3 text-slate-400 break-all">{s.username}@{s.host}:{s.port}</td>
            <td className={`whitespace-nowrap ${connections[s.id] === "connected" ? "text-emerald-400" : "text-slate-400"}`}>{connections[s.id] === "connected" ? "已连接" : connections[s.id] === "connecting" ? "连接中" : "未连接"}</td>
            <td className="text-right whitespace-nowrap py-2">
              <button className={`${button} bg-sky-700 hover:bg-sky-600 mr-2`} disabled={busy} onClick={() => perform(() => connectTerminal(s))}>{connections[s.id] === "connected" ? "打开终端" : "连接"}</button>
              {s.auth !== "agent" && <button className={`${button} mr-2`} disabled={busy} onClick={() => perform(async () => setSecret({ name: s.name, value: await ipc.revealServerSecret(s.id) }))}>{s.auth === "key" ? "查看口令" : "查看密码"}</button>}
              <button className={`${button} mr-2`} disabled={busy} onClick={() => edit(s)}>编辑</button>
              <button className={`${button} text-red-300`} disabled={busy} onClick={() => perform(() => remove(s))}>删除</button>
            </td>
          </tr>)}</tbody>
        </table>
        {filter && !filtered.length && <p className="py-6 text-center text-sm text-slate-400">没有匹配的服务器</p>}
      </div>}
    </>}
  </div>;
}
