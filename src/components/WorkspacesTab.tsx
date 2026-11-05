import { useCallback, useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { CODE_AGENT_LABELS, ipc, onEvent } from "../lib/ipc";
import type { AgentRun, AgentRunStatus, CodeAgentInfo, Workspace, WorkspaceData, WorkspaceLink } from "../lib/ipc";

const RUN_STATUS: Record<AgentRunStatus, string> = {
  running: "执行中", asking: "等待回答", idle: "本轮结束", error: "出错", cancelled: "已中断", closed: "已收尾",
};

interface Draft {
  id: number;
  name: string;
  root: string;
  aliases: string;
  summary: string;
  deploy: string;
  verify: string;
  tools: string;
  links: string;
  rules: string;
  notes: string;
  default_agent: string;
  agent_mode: string;
}

const EMPTY: Draft = { id: 0, name: "", root: "", aliases: "", summary: "", deploy: "", verify: "", tools: "", links: "", rules: "", notes: "", default_agent: "", agent_mode: "" };

const lines = (s: string) => s.split("\n").map((l) => l.trim()).filter(Boolean);
const linkText = (l: WorkspaceLink | string) => (typeof l === "string" ? l : l.label ? `${l.label} ${l.url}` : l.url);

function toDraft(w: Workspace): Draft {
  const d = w.data;
  return {
    id: w.id, name: w.name, root: w.root,
    aliases: (d.aliases ?? []).join("、"), summary: d.summary ?? "", deploy: d.deploy ?? "",
    verify: (d.verify_commands ?? []).join("\n"), tools: d.tools ?? "",
    links: (d.links ?? []).map(linkText).join("\n"), rules: (d.rules ?? []).join("\n"), notes: (d.notes ?? []).join("\n"),
    default_agent: d.default_agent ?? "", agent_mode: d.agent_mode ?? "",
  };
}

function toData(d: Draft): WorkspaceData {
  const links: WorkspaceLink[] = lines(d.links).map((l) => {
    const m = l.match(/^(.*?)[\s：:]*(https?:\/\/\S+)$/);
    return m ? { label: m[1].trim(), url: m[2] } : { label: "", url: l };
  });
  return {
    aliases: d.aliases.split(/[、,，\n]/).map((s) => s.trim()).filter(Boolean),
    summary: d.summary, deploy: d.deploy, verify_commands: lines(d.verify), tools: d.tools,
    links, rules: lines(d.rules), notes: lines(d.notes),
    default_agent: d.default_agent as WorkspaceData["default_agent"], agent_mode: d.agent_mode as WorkspaceData["agent_mode"],
  };
}

const input = "w-full rounded-md border border-slate-600 bg-slate-900/60 px-2.5 py-1.5 text-xs text-slate-100 outline-none focus:border-sky-500 placeholder:text-slate-500";

function Field({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return <label className="block">
    <span className="mb-1 block text-xs text-slate-300">{label}{hint && <span className="ml-1.5 text-slate-500">{hint}</span>}</span>
    {children}
  </label>;
}

export default function WorkspacesTab() {
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [agents, setAgents] = useState<CodeAgentInfo[] | null>(null);
  const [runs, setRuns] = useState<AgentRun[]>([]);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [pendingDelete, setPendingDelete] = useState<number | null>(null);
  const [agentPaths, setAgentPaths] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const refresh = useCallback(async () => {
    try {
      const [ws, rs] = await Promise.all([ipc.listWorkspaces(), ipc.listAgentRuns()]);
      setWorkspaces(ws);
      setRuns(rs);
      setError("");
    } catch (e) { setError(`加载失败：${String(e)}`); }
  }, []);

  const loadAgents = useCallback(async () => {
    try {
      const list = await ipc.listCodeAgents();
      setAgents(list);
      setAgentPaths(Object.fromEntries(list.map((a) => [a.kind, a.path_override])));
    } catch (e) { setError(`检测编码智能体失败：${String(e)}`); }
  }, []);

  useEffect(() => {
    void refresh();
    void loadAgents();
    let active = true;
    const stops: (() => void)[] = [];
    for (const name of ["workspaces-changed", "agent-run-changed"]) {
      onEvent(name, () => { if (active) void refresh(); }).then((stop) => { if (active) stops.push(stop); else stop(); });
    }
    return () => { active = false; stops.forEach((s) => s()); };
  }, [refresh, loadAgents]);

  async function pickFolder(detectAfter: boolean) {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir !== "string") return;
    if (!detectAfter) { setDraft((d) => (d ? { ...d, root: dir } : d)); return; }
    setBusy(true);
    try {
      const found = await ipc.detectWorkspace(dir);
      setDraft({
        ...EMPTY, name: String(found.name ?? ""), root: String(found.root ?? dir), summary: String(found.summary ?? ""),
        deploy: String(found.deploy ?? ""), verify: ((found.verify_commands as string[]) ?? []).join("\n"),
      });
      setError("");
    } catch (e) { setError(`识别失败：${String(e)}`); }
    finally { setBusy(false); }
  }

  async function save() {
    if (!draft) return;
    if (!draft.name.trim() || !draft.root.trim()) { setError("名称和根目录必填"); return; }
    setBusy(true);
    try { await ipc.saveWorkspace(draft.id, draft.name.trim(), draft.root.trim(), toData(draft)); setDraft(null); await refresh(); }
    catch (e) { setError(`保存失败：${String(e)}`); }
    finally { setBusy(false); }
  }

  async function remove(id: number) {
    setBusy(true);
    try { await ipc.deleteWorkspace(id); setPendingDelete(null); await refresh(); }
    catch (e) { setError(`删除失败：${String(e)}`); }
    finally { setBusy(false); }
  }

  async function saveAgent(a: CodeAgentInfo, enabled: boolean) {
    try {
      const list = await ipc.setCodeAgent(a.kind, enabled, agentPaths[a.kind] ?? "");
      setAgents(list);
      setError("");
    } catch (e) { setError(`保存智能体设置失败：${String(e)}`); }
  }

  const set = (k: keyof Draft) => (e: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>) =>
    setDraft((d) => (d ? { ...d, [k]: e.target.value } : d));

  return <div className="space-y-6 text-sm">
    <section className="space-y-3">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-base font-semibold text-slate-100">工作区</h2>
          <p className="mt-1 text-xs text-slate-400">登记固定项目的路径、部署方式、验证命令和规则。聊天里提到名称或别名，拾光就知道改哪里；代码修改可以外派给 Claude Code / Codex / Cursor / Pi。</p>
        </div>
        {!draft && <div className="flex shrink-0 gap-2">
          <button type="button" disabled={busy} onClick={() => void pickFolder(true)} className="rounded-md border border-slate-600 px-3 py-1.5 text-xs text-slate-200 hover:border-sky-500 disabled:opacity-50">从文件夹识别</button>
          <button type="button" onClick={() => setDraft({ ...EMPTY })} className="rounded-md bg-sky-600 px-3 py-1.5 text-xs text-white hover:bg-sky-500">新建</button>
        </div>}
      </div>

      {draft && <div className="space-y-3 rounded-lg border border-sky-500/30 bg-slate-900/40 p-4">
        <div className="grid grid-cols-2 gap-3">
          <Field label="名称"><input className={input} value={draft.name} onChange={set("name")} placeholder="博客" /></Field>
          <Field label="别名" hint="用、或逗号分隔"><input className={input} value={draft.aliases} onChange={set("aliases")} placeholder="blog、blog.example.com" /></Field>
        </div>
        <Field label="根目录">
          <div className="flex gap-2">
            <input className={input} value={draft.root} onChange={set("root")} placeholder="D:\projects\blog" />
            <button type="button" onClick={() => void pickFolder(false)} className="shrink-0 rounded-md border border-slate-600 px-2.5 text-xs text-slate-300 hover:border-sky-500">选择</button>
          </div>
        </Field>
        <Field label="一句话说明"><input className={input} value={draft.summary} onChange={set("summary")} placeholder="Nuxt 3 博客，文章从后端 API 拉取" /></Field>
        <Field label="部署方式"><input className={input} value={draft.deploy} onChange={set("deploy")} placeholder="push main 即 Vercel 自动部署" /></Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label="验证命令" hint="每行一条"><textarea rows={3} className={input} value={draft.verify} onChange={set("verify")} placeholder="npm run build" /></Field>
          <Field label="必须遵守的规则" hint="每行一条"><textarea rows={3} className={input} value={draft.rules} onChange={set("rules")} placeholder="push 前先问我" /></Field>
        </div>
        <Field label="配套工具" hint="CLI 路径、配置文件路径；不要填密钥"><textarea rows={2} className={input} value={draft.tools} onChange={set("tools")} /></Field>
        <Field label="相关链接" hint="每行：名称 网址"><textarea rows={2} className={input} value={draft.links} onChange={set("links")} placeholder="线上站点 https://blog.example.com" /></Field>
        <Field label="经验" hint="每行一条，AI 也会追加"><textarea rows={3} className={input} value={draft.notes} onChange={set("notes")} /></Field>
        <div className="grid grid-cols-2 gap-3">
          <Field label="默认外派">
            <select className={input} value={draft.default_agent} onChange={set("default_agent")}>
              <option value="">自动（Claude Code 优先）</option>
              <option value="claude_code">Claude Code</option>
              <option value="codex">Codex</option>
              <option value="cursor">Cursor</option>
              <option value="pi">Pi</option>
            </select>
          </Field>
          <Field label="外派权限">
            <select className={input} value={draft.agent_mode} onChange={set("agent_mode")}>
              <option value="">默认（可改文件、跑命令）</option>
              <option value="plan">只出方案，不改文件</option>
              <option value="edit">可改文件、跑命令</option>
              <option value="full">跳过智能体自身确认</option>
            </select>
          </Field>
        </div>
        <p className="text-[11px] text-slate-500">外派的智能体始终不能 commit / push；提交由拾光在你确认后执行。</p>
        <div className="flex justify-end gap-2">
          <button type="button" onClick={() => setDraft(null)} className="rounded-md border border-slate-600 px-3 py-1.5 text-xs text-slate-300 hover:border-slate-400">取消</button>
          <button type="button" disabled={busy} onClick={() => void save()} className="rounded-md bg-sky-600 px-3 py-1.5 text-xs text-white hover:bg-sky-500 disabled:opacity-50">保存</button>
        </div>
      </div>}

      {workspaces.length === 0 && !draft
        ? <div className="rounded-lg border border-slate-700/60 px-4 py-8 text-center text-xs text-slate-500">还没有工作区。可以在聊天里做完一个项目任务后让拾光帮你记下来，或点「从文件夹识别」。</div>
        : <div className="divide-y divide-slate-700/60 overflow-hidden rounded-lg border border-slate-700/60">
          {workspaces.map((w) => <div key={w.id} className="flex items-start justify-between gap-4 px-4 py-3">
            <div className="min-w-0">
              <div className="font-medium text-slate-100">{w.name}
                {(w.data.aliases ?? []).length > 0 && <span className="ml-2 text-xs font-normal text-slate-500">{(w.data.aliases ?? []).join("、")}</span>}
              </div>
              <div className="mt-0.5 truncate text-xs text-slate-400" title={w.root}>{w.root}</div>
              {w.data.summary && <div className="mt-1 text-xs text-slate-300">{w.data.summary}</div>}
              {w.data.deploy && <div className="mt-0.5 text-xs text-amber-300/80">部署：{w.data.deploy}</div>}
            </div>
            <div className="flex shrink-0 gap-2 text-xs">
              <button type="button" onClick={() => { setDraft(toDraft(w)); setPendingDelete(null); }} className="text-sky-400 hover:text-sky-300">编辑</button>
              {pendingDelete === w.id
                ? <button type="button" disabled={busy} onClick={() => void remove(w.id)} className="text-rose-400 hover:text-rose-300">确认删除</button>
                : <button type="button" onClick={() => setPendingDelete(w.id)} className="text-slate-500 hover:text-rose-400">删除</button>}
            </div>
          </div>)}
        </div>}
    </section>

    <section className="space-y-3">
      <div className="flex items-center justify-between">
        <div>
          <h3 className="text-sm font-semibold text-slate-100">编码智能体</h3>
          <p className="mt-1 text-xs text-slate-400">外派使用你自己的 Claude / Codex / Cursor / Pi 订阅额度。Cursor 和 Pi 不能中途回问，有疑问会写在每轮总结里。</p>
        </div>
        <button type="button" onClick={() => { setAgents(null); void loadAgents(); }} className="text-xs text-sky-400 hover:text-sky-300">重新检测</button>
      </div>
      {!agents ? <div className="text-xs text-slate-500">正在检测…</div> :
        <div className="divide-y divide-slate-700/60 overflow-hidden rounded-lg border border-slate-700/60">
          {agents.map((a) => <div key={a.kind} className="space-y-2 px-4 py-3">
            <div className="flex items-center justify-between gap-3">
              <div className="min-w-0">
                <div className="font-medium text-slate-100">{a.label}
                  <span className={`ml-2 text-xs font-normal ${a.available ? "text-emerald-400" : "text-slate-500"}`}>{a.available ? a.version : "未找到"}</span>
                </div>
                {a.path && <div className="mt-0.5 truncate text-xs text-slate-500" title={a.path}>{a.path}</div>}
              </div>
              <label className="flex shrink-0 items-center gap-1.5 text-xs text-slate-300">
                <input type="checkbox" checked={a.enabled} onChange={(e) => void saveAgent(a, e.target.checked)} />启用
              </label>
            </div>
            <div className="flex gap-2">
              <input className={input} value={agentPaths[a.kind] ?? ""} onChange={(e) => setAgentPaths((p) => ({ ...p, [a.kind]: e.target.value }))} placeholder="自定义路径（留空则从 PATH 查找）" />
              <button type="button" onClick={() => void saveAgent(a, a.enabled)} className="shrink-0 rounded-md border border-slate-600 px-2.5 text-xs text-slate-300 hover:border-sky-500">保存</button>
            </div>
          </div>)}
        </div>}
    </section>

    <section className="space-y-3">
      <h3 className="text-sm font-semibold text-slate-100">外派记录</h3>
      {runs.length === 0 ? <div className="rounded-lg border border-slate-700/60 px-4 py-6 text-center text-xs text-slate-500">还没有外派记录</div> :
        <div className="divide-y divide-slate-700/60 overflow-hidden rounded-lg border border-slate-700/60">
          {runs.map((r) => <button key={r.id} type="button" onClick={() => void ipc.showAgentRunWindow(r.id).catch((e) => setError(String(e)))} className="flex w-full items-center justify-between gap-4 px-4 py-3 text-left hover:bg-slate-700/40">
            <div className="min-w-0">
              <div className="truncate font-medium text-slate-100">{r.title}</div>
              <div className="mt-1 text-xs text-slate-500">{CODE_AGENT_LABELS[r.agent] ?? r.agent} · {r.data.workspace_name ?? r.root} · {r.created_at}</div>
            </div>
            <div className="shrink-0 text-right">
              <div className={`text-xs ${r.status === "asking" ? "text-amber-300" : r.status === "error" ? "text-rose-300" : "text-slate-300"}`}>{RUN_STATUS[r.status] ?? r.status}</div>
              <div className="mt-1 text-xs text-sky-400">打开</div>
            </div>
          </button>)}
        </div>}
    </section>

    {error && <div role="alert" className="text-xs text-rose-300">{error}</div>}
  </div>;
}
