import { useEffect, useState } from "react";
import { ipc, onEvent } from "../lib/ipc";
import type { ExternalSkill, SkillInfo } from "../lib/ipc";

const SOURCE_LABEL: Record<string, string> = {
  claude: "Claude",
  codex: "Codex",
  cursor: "Cursor",
  "cursor-builtin": "Cursor 内置",
  local: "本地",
  synced: "已同步",
  migrated: "由工作流迁移",
  builtin: "内置",
};

export default function SkillsTab() {
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [external, setExternal] = useState<ExternalSkill[] | null>(null);
  const [scanning, setScanning] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [preview, setPreview] = useState<{ name: string; content: string } | null>(null);
  const [msg, setMsg] = useState("");
  const [err, setErr] = useState("");
  const [creating, setCreating] = useState(false);
  const [form, setForm] = useState({ name: "", description: "", body: "" });

  const refresh = () => {
    ipc.listSkills().then(setSkills).catch((e) => setErr(String(e)));
  };

  useEffect(() => {
    refresh();
    let unlisten: (() => void) | undefined;
    onEvent("skills-changed", refresh).then((u) => (unlisten = u));
    return () => unlisten?.();
  }, []);

  const scan = async () => {
    setScanning(true);
    setErr("");
    setMsg("");
    try {
      const list = await ipc.scanExternalSkills();
      setExternal(list);
      setSelected(new Set(list.filter((e) => !e.already_synced).map((e) => `${e.source}::${e.name}`)));
      setMsg(list.length === 0 ? "未在本机发现可同步的外部技能" : `发现 ${list.length} 个外部技能`);
    } catch (e) {
      setErr(String(e));
    } finally {
      setScanning(false);
    }
  };

  const syncSelected = async (overwrite: boolean) => {
    if (!external || selected.size === 0) return;
    setSyncing(true);
    setErr("");
    setMsg("");
    try {
      // 按来源分组同步
      const bySource = new Map<string, string[]>();
      for (const key of selected) {
        const [source, ...rest] = key.split("::");
        const name = rest.join("::");
        if (!bySource.has(source)) bySource.set(source, []);
        bySource.get(source)!.push(name);
      }
      let imported = 0;
      let updated = 0;
      let skipped = 0;
      for (const [source, names] of bySource) {
        const r = await ipc.syncSkills(source, names, overwrite);
        imported += r.imported;
        updated += r.updated;
        skipped += r.skipped;
      }
      setMsg(`同步完成：导入 ${imported}，更新 ${updated}，跳过 ${skipped}`);
      refresh();
      const list = await ipc.scanExternalSkills();
      setExternal(list);
      setSelected(new Set());
    } catch (e) {
      setErr(String(e));
    } finally {
      setSyncing(false);
    }
  };

  const toggleEnabled = async (s: SkillInfo) => {
    try {
      await ipc.setSkillEnabled(s.name, !s.enabled);
      refresh();
    } catch (e) {
      setErr(String(e));
    }
  };

  const remove = async (name: string) => {
    if (!confirm(`确认删除技能「${name}」？此操作不可恢复。`)) return;
    try {
      await ipc.deleteSkill(name);
      if (preview?.name === name) setPreview(null);
      refresh();
    } catch (e) {
      setErr(String(e));
    }
  };

  const showPreview = async (name: string) => {
    try {
      const content = await ipc.readSkillContent(name);
      setPreview({ name, content });
    } catch (e) {
      setErr(String(e));
    }
  };

  const create = async () => {
    if (!form.name.trim() || !form.description.trim() || !form.body.trim()) return;
    try {
      await ipc.createSkill(form.name.trim(), form.description.trim(), form.body.trim());
      setForm({ name: "", description: "", body: "" });
      setCreating(false);
      setMsg(`已创建技能「${form.name.trim()}」`);
      refresh();
    } catch (e) {
      setErr(String(e));
    }
  };

  const toggleSelect = (key: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  return (
    <div className="space-y-5 max-w-3xl">
      <section className="space-y-2.5">
        <div className="flex items-center gap-2">
          <div className="text-sm font-medium text-slate-100">已安装 Skills</div>
          <span className="text-xs text-slate-500">
            {skills.length} 个 · 内部只读（改代码打包更新）· 外部可编辑 / 同步 / AI 沉淀
          </span>
          <button
            onClick={() => setCreating((v) => !v)}
            className="ml-auto px-3 py-1 rounded text-xs bg-slate-700 text-slate-200 hover:bg-slate-600 transition"
          >
            {creating ? "取消新建" : "新建外部技能"}
          </button>
        </div>

        {creating && (
          <div className="rounded-lg border border-slate-700/60 bg-slate-800/40 p-3 space-y-2">
            <input
              value={form.name}
              onChange={(e) => setForm({ ...form, name: e.target.value })}
              placeholder="技能名，如 git-auto-commit-zh"
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-600"
            />
            <input
              value={form.description}
              onChange={(e) => setForm({ ...form, description: e.target.value })}
              placeholder="触发场景描述（会出现在 AI 目录摘要里）"
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-600"
            />
            <textarea
              value={form.body}
              onChange={(e) => setForm({ ...form, body: e.target.value })}
              placeholder="技能正文 Markdown：步骤、约束、示例…"
              rows={8}
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-600 font-mono"
            />
            <button
              onClick={create}
              disabled={!form.name.trim() || !form.description.trim() || !form.body.trim()}
              className="px-3.5 py-1.5 rounded bg-sky-600 text-white text-sm hover:bg-sky-500 disabled:opacity-40 transition"
            >
              创建
            </button>
          </div>
        )}

        {skills.length === 0 ? (
          <div className="text-xs text-slate-500">
            暂无技能。可从下方同步 Claude / Codex / Cursor，或让 AI 在任务完成后沉淀为外部 Skill。
          </div>
        ) : (
          <div className="space-y-1.5">
            {skills.map((s) => {
              const isInternal = s.scope === "internal";
              return (
              <div
                key={s.name}
                className={`rounded-lg border px-3 py-2 ${
                  s.enabled
                    ? "border-slate-700/60 bg-slate-800/50"
                    : "border-slate-800 bg-slate-900/40 opacity-60"
                }`}
              >
                <div className="flex items-center gap-2">
                  <span className="text-sm text-slate-100 font-medium truncate">{s.name}</span>
                  <span
                    className={`shrink-0 px-1.5 py-0.5 rounded text-[10px] ${
                      isInternal
                        ? "bg-violet-500/15 text-violet-300"
                        : "bg-indigo-500/15 text-indigo-300"
                    }`}
                  >
                    {isInternal ? "内部" : SOURCE_LABEL[s.synced_from] || SOURCE_LABEL[s.source] || "外部"}
                  </span>
                  {!s.enabled && (
                    <span className="shrink-0 px-1.5 py-0.5 rounded text-[10px] bg-slate-700 text-slate-400">
                      已禁用
                    </span>
                  )}
                  <span className="ml-auto shrink-0 text-[11px] text-slate-500">
                    {s.updated_at.slice(0, 10)}
                  </span>
                  <button
                    onClick={() => showPreview(s.name)}
                    className="shrink-0 text-slate-500 hover:text-sky-400 text-xs px-1 transition"
                  >
                    查看
                  </button>
                  <button
                    onClick={() => toggleEnabled(s)}
                    className="shrink-0 text-slate-500 hover:text-amber-400 text-xs px-1 transition"
                  >
                    {s.enabled ? "禁用" : "启用"}
                  </button>
                  {!isInternal && (
                    <button
                      onClick={() => remove(s.name)}
                      className="shrink-0 text-slate-500 hover:text-rose-400 text-xs px-1 transition"
                    >
                      删除
                    </button>
                  )}
                </div>
                <div className="mt-1 text-[11px] text-slate-400 line-clamp-2 leading-4">
                  {s.description}
                </div>
              </div>
            );
            })}
          </div>
        )}
      </section>

      {preview && (
        <section className="space-y-2">
          <div className="flex items-center gap-2">
            <div className="text-sm font-medium text-slate-100">预览 · {preview.name}</div>
            <button
              onClick={() => setPreview(null)}
              className="ml-auto text-xs text-slate-500 hover:text-slate-300"
            >
              关闭
            </button>
          </div>
          <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words text-[11px] leading-5 text-slate-300 bg-slate-900/70 rounded-lg p-3 border border-slate-700/50">
            {preview.content}
          </pre>
        </section>
      )}

      <section className="space-y-2.5">
        <div className="flex items-center gap-2 flex-wrap">
          <div className="text-sm font-medium text-slate-100">从外部同步</div>
          <span className="text-xs text-slate-500">
            扫描 ~/.claude/skills · ~/.codex/skills · ~/.cursor/skills
          </span>
          <button
            onClick={scan}
            disabled={scanning}
            className="ml-auto px-3 py-1 rounded text-xs bg-slate-700 text-slate-200 hover:bg-slate-600 disabled:opacity-40 transition"
          >
            {scanning ? "扫描中…" : "扫描外部技能"}
          </button>
          {external && external.length > 0 && (
            <>
              <button
                onClick={() => syncSelected(false)}
                disabled={syncing || selected.size === 0}
                className="px-3 py-1 rounded text-xs bg-sky-600 text-white hover:bg-sky-500 disabled:opacity-40 transition"
              >
                {syncing ? "同步中…" : `导入选中（${selected.size}）`}
              </button>
              <button
                onClick={() => syncSelected(true)}
                disabled={syncing || selected.size === 0}
                className="px-3 py-1 rounded text-xs bg-amber-600/80 text-white hover:bg-amber-500 disabled:opacity-40 transition"
                title="覆盖本地同名技能"
              >
                导入并覆盖
              </button>
            </>
          )}
        </div>

        {external && external.length > 0 && (
          <div className="space-y-1.5 max-h-72 overflow-y-auto scrollbar-thin">
            {external.map((e) => {
              const key = `${e.source}::${e.name}`;
              return (
                <label
                  key={key}
                  className="flex items-start gap-2.5 rounded-lg border border-slate-700/60 bg-slate-800/40 px-3 py-2 cursor-pointer hover:bg-slate-800/70"
                >
                  <input
                    type="checkbox"
                    checked={selected.has(key)}
                    onChange={() => toggleSelect(key)}
                    className="mt-0.5 w-3.5 h-3.5 accent-sky-500"
                  />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="text-sm text-slate-100 truncate">{e.name}</span>
                      <span className="shrink-0 px-1.5 py-0.5 rounded text-[10px] bg-emerald-500/15 text-emerald-300">
                        {SOURCE_LABEL[e.source] ?? e.source}
                      </span>
                      {e.already_synced && (
                        <span className="shrink-0 text-[10px] text-slate-500">本地已有</span>
                      )}
                    </div>
                    <div className="mt-0.5 text-[11px] text-slate-400 line-clamp-2">
                      {e.description}
                    </div>
                  </div>
                </label>
              );
            })}
          </div>
        )}
      </section>

      {(msg || err) && (
        <div className={`text-xs ${err ? "text-rose-400" : "text-emerald-400"}`}>
          {err || msg}
        </div>
      )}
    </div>
  );
}
