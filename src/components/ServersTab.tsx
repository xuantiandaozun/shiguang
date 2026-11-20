import { useEffect, useMemo, useRef, useState } from "react";
import { open, confirm } from "@tauri-apps/plugin-dialog";
import { ipc, onEvent, ServerConnection } from "../lib/ipc";
import Icon from "./Icon";

const blank = (): ServerConnection => ({
  id: "",
  name: "",
  host: "",
  port: 22,
  username: "root",
  group: "",
  tags: [],
  note: "",
  auth: "password",
  private_key: "",
  fingerprint: null,
  last_used_at: null,
});

const fieldInput =
  "w-full rounded-lg border border-slate-600 bg-slate-900 px-3 py-2 text-sm text-slate-100 focus:outline-none focus:border-sky-400";
const btnBase =
  "ui-button inline-flex items-center justify-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-medium disabled:opacity-40 disabled:cursor-not-allowed transition";

const UNGROUPED = "__ungrouped__";
const FILTER_ALL = "__all__";
const FILTER_RECENT = "__recent__";
const FILTER_CONNECTED = "__connected__";
const RECENT_LIMIT = 10;

const AUTH_LABELS: Record<ServerConnection["auth"], string> = {
  password: "密码",
  key: "私钥",
  agent: "Agent",
};

function parseTagInput(raw: string): string[] {
  return raw
    .split(/[,，\s]+/)
    .map((t) => t.trim().replace(/^#+/, "").trim())
    .filter(Boolean);
}

function mergeTags(existing: string[], incoming: string[]): string[] {
  const out = [...existing];
  for (const tag of incoming) {
    const clean = tag.slice(0, 32);
    if (!clean) continue;
    if (!out.some((x) => x.toLowerCase() === clean.toLowerCase())) {
      out.push(clean);
      if (out.length >= 20) break;
    }
  }
  return out;
}

function formatLastUsed(ms?: number | null): string {
  if (!ms || ms <= 0) return "";
  const diff = Date.now() - ms;
  if (diff < 60_000) return "刚刚";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export default function ServersTab() {
  const [servers, setServers] = useState<ServerConnection[]>([]);
  const [editor, setEditor] = useState<ServerConnection | null>(null);
  const [tagDraft, setTagDraft] = useState("");
  const [password, setPassword] = useState("");
  const [secretChanged, setSecretChanged] = useState(false);
  const [visible, setVisible] = useState(false);
  const [filter, setFilter] = useState("");
  const [selectedGroup, setSelectedGroup] = useState<string>(FILTER_ALL);
  const [viewMode, setViewMode] = useState<"cards" | "list">(() => {
    try {
      return localStorage.getItem("servers-view-mode") === "list" ? "list" : "cards";
    } catch {
      return "cards";
    }
  });
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [busyId, setBusyId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [connections, setConnections] = useState<Record<string, string>>({});
  const [secret, setSecret] = useState<{ name: string; value: string } | null>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  const call = <T,>(name: string, args: Record<string, unknown> = {}) =>
    ipc.serverAction<T>(name, args);
  const load = async () => setServers(await call<ServerConnection[]>("list_servers"));

  useEffect(() => {
    try {
      localStorage.setItem("servers-view-mode", viewMode);
    } catch {
      /* ignore */
    }
  }, [viewMode]);

  useEffect(() => {
    let dead = false;
    const refresh = async () => {
      try {
        const result = await call<ServerConnection[]>("list_servers");
        if (!dead) setServers(result);
      } catch (e) {
        if (!dead) setError(String(e));
      }
    };
    const status = async () => {
      try {
        const result = await ipc.serverTerminal<Record<string, string>>("list", "");
        if (!dead) setConnections(result);
      } catch (e) {
        if (!dead) setError(String(e));
      }
    };
    void refresh();
    void status();
    const listener = onEvent("servers-changed", () => {
      void refresh();
    });
    const timer = window.setInterval(status, 2000);
    return () => {
      dead = true;
      clearInterval(timer);
      void listener.then((f) => f());
    };
  }, []);

  useEffect(() => {
    if (!message) return;
    const t = window.setTimeout(() => setMessage(""), 3200);
    return () => window.clearTimeout(t);
  }, [message]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "/" || editor || secret) return;
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      e.preventDefault();
      searchRef.current?.focus();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [editor, secret]);

  async function perform(action: () => Promise<void>, id?: string) {
    setError("");
    setMessage("");
    setBusy(true);
    if (id) setBusyId(id);
    try {
      await action();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setBusyId(null);
    }
  }

  function edit(s: ServerConnection) {
    setEditor({ ...s, tags: [...(s.tags ?? [])] });
    setTagDraft("");
    setPassword("");
    setSecretChanged(false);
    setVisible(false);
    setError("");
  }

  function commitTagDraft() {
    if (!editor || !tagDraft.trim()) return;
    const next = mergeTags(editor.tags ?? [], parseTagInput(tagDraft));
    setEditor({ ...editor, tags: next });
    setTagDraft("");
  }

  async function ensureTrusted(s: ServerConnection): Promise<boolean> {
    const result = await call<{
      status: string;
      fingerprint: string;
      previous_fingerprint?: string;
    }>("test_server_connection", { server_id: s.id });
    if (result.status === "needs_trust") {
      const ok = await confirm(
        `${s.host}:${s.port}\n${
          result.previous_fingerprint ? `原指纹：${result.previous_fingerprint}\n主机指纹已变化。\n` : ""
        }当前指纹：${result.fingerprint}\n请与可信渠道核对后确认。`,
        { title: "核对服务器身份", kind: "warning" }
      );
      if (!ok) return false;
      await ipc.trustServerHost(s.id, result.fingerprint);
      await load();
      return true;
    }
    if (result.status !== "connected") {
      throw new Error("连接失败，请检查服务器地址、端口或凭据配置");
    }
    return true;
  }

  async function openTerminal(s: ServerConnection) {
    if (connections[s.id] !== "connected") {
      const ok = await ensureTrusted(s);
      if (!ok) return;
    }
    await ipc.serverTerminal("open", s.id);
    await load();
  }

  async function openSftp(s: ServerConnection) {
    if (!s.fingerprint) {
      const ok = await ensureTrusted(s);
      if (!ok) return;
    }
    await ipc.serverSftp("open", s.id);
    await load();
  }

  async function remove(s: ServerConnection) {
    if (
      !(await confirm(`删除“${s.name}”的连接信息和已保存密码？远程文件不会删除。`, {
        title: "删除连接",
        kind: "warning",
      }))
    )
      return;
    await call("delete_server", { server_id: s.id });
    await load();
    setMessage(`已删除连接「${s.name}」`);
  }

  async function copyAddress(s: ServerConnection) {
    const cmd = s.port === 22 ? `ssh ${s.username}@${s.host}` : `ssh ${s.username}@${s.host} -p ${s.port}`;
    await navigator.clipboard.writeText(cmd);
    setCopiedId(s.id);
    window.setTimeout(() => setCopiedId((cur) => (cur === s.id ? null : cur)), 1600);
  }

  const recentServers = useMemo(() => {
    return servers
      .filter((s) => typeof s.last_used_at === "number" && s.last_used_at > 0)
      .sort((a, b) => (b.last_used_at ?? 0) - (a.last_used_at ?? 0))
      .slice(0, RECENT_LIMIT);
  }, [servers]);

  const groupStats = useMemo(() => {
    const map = new Map<string, number>();
    let ungrouped = 0;
    for (const s of servers) {
      const g = s.group.trim();
      if (!g) ungrouped++;
      else map.set(g, (map.get(g) ?? 0) + 1);
    }
    const groups = [...map.entries()].sort((a, b) => a[0].localeCompare(b[0], "zh-CN"));
    return { groups, ungrouped };
  }, [servers]);

  const allTags = useMemo(() => {
    const counts = new Map<string, number>();
    for (const s of servers) {
      for (const t of s.tags ?? []) {
        const clean = t.trim();
        if (clean) counts.set(clean, (counts.get(clean) ?? 0) + 1);
      }
    }
    return [...counts.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0], "zh-CN"));
  }, [servers]);

  const connectedCount = useMemo(
    () => servers.filter((s) => connections[s.id] === "connected").length,
    [servers, connections]
  );

  const filtered = useMemo(() => {
    const rawQ = filter.trim().toLowerCase();
    const cleanQ = rawQ.replace(/^#+/, "");
    const baseList = selectedGroup === FILTER_RECENT ? recentServers : servers;

    return baseList.filter((s) => {
      if (selectedGroup === FILTER_CONNECTED && connections[s.id] !== "connected") return false;
      if (selectedGroup === UNGROUPED && s.group.trim() !== "") return false;
      if (
        selectedGroup !== FILTER_ALL &&
        selectedGroup !== FILTER_RECENT &&
        selectedGroup !== FILTER_CONNECTED &&
        selectedGroup !== UNGROUPED &&
        s.group.trim() !== selectedGroup
      ) {
        return false;
      }
      if (!cleanQ) return true;
      const tagsText = (s.tags ?? []).join(" ");
      const haystack = `${s.name} ${s.host} ${s.username} ${s.group} ${tagsText} ${s.note}`.toLowerCase();
      return haystack.includes(cleanQ);
    });
  }, [servers, recentServers, selectedGroup, filter, connections]);

  const filterByTag = (tag: string) => {
    setFilter((cur) => (cur.trim().replace(/^#+/, "").toLowerCase() === tag.toLowerCase() ? "" : `#${tag}`));
  };

  return (
    <div className="server-page h-full min-h-0 flex flex-col gap-4">
      {/* 顶部工具栏 */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="relative flex-1 min-w-[240px] max-w-md">
          <input
            ref={searchRef}
            aria-label="搜索服务器"
            className={`${fieldInput} pr-14`}
            placeholder="搜索名称、IP/域名、分组、标签或备注（按 / 聚焦）"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
          {filter && (
            <button
              type="button"
              onClick={() => setFilter("")}
              className="absolute right-2.5 top-1/2 -translate-y-1/2 rounded px-1.5 py-0.5 text-xs text-slate-400 hover:text-slate-200"
              title="清空搜索"
            >
              清空
            </button>
          )}
        </div>

        <div className="flex items-center gap-2">
          <div className="inline-flex rounded-lg border border-slate-700 p-0.5 text-xs">
            <button
              type="button"
              onClick={() => setViewMode("cards")}
              className={`rounded-md px-2.5 py-1 transition ${
                viewMode === "cards" ? "bg-sky-500/20 text-sky-300 font-medium" : "text-slate-400"
              }`}
            >
              卡片
            </button>
            <button
              type="button"
              onClick={() => setViewMode("list")}
              className={`rounded-md px-2.5 py-1 transition ${
                viewMode === "list" ? "bg-sky-500/20 text-sky-300 font-medium" : "text-slate-400"
              }`}
            >
              列表
            </button>
          </div>
          <button
            className={`${btnBase} ui-primary px-3.5 py-2 text-xs`}
            disabled={busy}
            onClick={() => edit(blank())}
          >
            <Icon name="plus" className="w-3.5 h-3.5" />
            新增服务器
          </button>
        </div>
      </div>

      {error && (
        <div
          role="alert"
          className="rounded-lg border border-red-300 bg-red-50 px-3.5 py-2.5 text-xs text-red-700 whitespace-pre-wrap break-all"
        >
          {error}
        </div>
      )}
      {message && (
        <div role="status" className="text-xs font-medium text-sky-400">
          {message}
        </div>
      )}

      {/* 主工作区：左侧分组/标签栏 + 右侧服务器列表/卡片 */}
      {!servers.length ? (
        <div className="flex flex-1 flex-col items-center justify-center rounded-2xl border border-dashed border-slate-700 py-16 px-6 text-center">
          <div className="mb-3 flex h-12 w-12 items-center justify-center rounded-2xl bg-sky-500/15 text-sky-400">
            <Icon name="server" className="w-6 h-6" />
          </div>
          <h3 className="text-base font-semibold text-slate-100">还没有保存任何服务器</h3>
          <p className="mt-1.5 max-w-sm text-xs text-slate-400 leading-relaxed">
            新增连接后，可以一键打开独立的 SSH 命令终端，或者打开 XFTP 风格的左右双栏文件管理窗口。
          </p>
          <button
            className={`${btnBase} ui-primary mt-5 px-4 py-2 text-sm`}
            onClick={() => edit(blank())}
          >
            <Icon name="plus" className="w-4 h-4" />
            新增第一台服务器
          </button>
        </div>
      ) : (
        <div className="flex flex-1 min-h-0 gap-4">
          {/* 左侧分组与标签导航 */}
          <aside className="w-48 shrink-0 flex flex-col gap-1 border-r border-slate-700/70 pr-3 overflow-y-auto">
            <div className="px-2 pb-1 text-[11px] font-medium text-slate-400 uppercase tracking-wider">
              视图筛选
            </div>
            <button
              type="button"
              onClick={() => setSelectedGroup(FILTER_ALL)}
              className={`server-group-btn flex items-center justify-between rounded-lg px-2.5 py-2 text-xs text-left ${
                selectedGroup === FILTER_ALL ? "is-active" : "text-slate-300"
              }`}
            >
              <span className="truncate">全部服务器</span>
              <span className="tabular-nums text-[11px] opacity-75">{servers.length}</span>
            </button>
            <button
              type="button"
              onClick={() => setSelectedGroup(FILTER_RECENT)}
              className={`server-group-btn flex items-center justify-between rounded-lg px-2.5 py-2 text-xs text-left ${
                selectedGroup === FILTER_RECENT ? "is-active" : "text-slate-300"
              }`}
              title="按最近操作时间排序，最多显示 10 台"
            >
              <span className="flex items-center gap-1.5 truncate">
                <Icon name="history" className="w-3.5 h-3.5 opacity-80" />
                最近操作
              </span>
              <span className="tabular-nums text-[11px] opacity-75">{recentServers.length}</span>
            </button>
            <button
              type="button"
              onClick={() => setSelectedGroup(FILTER_CONNECTED)}
              className={`server-group-btn flex items-center justify-between rounded-lg px-2.5 py-2 text-xs text-left ${
                selectedGroup === FILTER_CONNECTED ? "is-active" : "text-slate-300"
              }`}
            >
              <span className="flex items-center gap-1.5 truncate">
                <span
                  className={`inline-block h-2 w-2 rounded-full ${
                    connectedCount > 0 ? "bg-emerald-500" : "bg-slate-400/60"
                  }`}
                />
                已连接终端
              </span>
              <span className="tabular-nums text-[11px] opacity-75">{connectedCount}</span>
            </button>

            {(groupStats.groups.length > 0 || groupStats.ungrouped > 0) && (
              <>
                <div className="px-2 pt-3 pb-1 text-[11px] font-medium text-slate-400 uppercase tracking-wider">
                  分组
                </div>
                {groupStats.groups.map(([name, count]) => (
                  <button
                    key={name}
                    type="button"
                    onClick={() => setSelectedGroup(name)}
                    className={`server-group-btn flex items-center justify-between rounded-lg px-2.5 py-2 text-xs text-left ${
                      selectedGroup === name ? "is-active" : "text-slate-300"
                    }`}
                  >
                    <span className="truncate" title={name}>
                      {name}
                    </span>
                    <span className="tabular-nums text-[11px] opacity-75">{count}</span>
                  </button>
                ))}
                {groupStats.ungrouped > 0 && (
                  <button
                    type="button"
                    onClick={() => setSelectedGroup(UNGROUPED)}
                    className={`server-group-btn flex items-center justify-between rounded-lg px-2.5 py-2 text-xs text-left ${
                      selectedGroup === UNGROUPED ? "is-active" : "text-slate-300"
                    }`}
                  >
                    <span className="truncate">未分组</span>
                    <span className="tabular-nums text-[11px] opacity-75">{groupStats.ungrouped}</span>
                  </button>
                )}
              </>
            )}

            {allTags.length > 0 && (
              <>
                <div className="px-2 pt-3 pb-1 text-[11px] font-medium text-slate-400 uppercase tracking-wider">
                  标签
                </div>
                <div className="flex flex-wrap gap-1 px-1.5">
                  {allTags.map(([tag, count]) => {
                    const active = filter.trim().replace(/^#+/, "").toLowerCase() === tag.toLowerCase();
                    return (
                      <button
                        key={tag}
                        type="button"
                        onClick={() => filterByTag(tag)}
                        className={`rounded-md border px-1.5 py-0.5 text-[11px] transition ${
                          active
                            ? "border-sky-400 bg-sky-500/20 text-sky-300 font-medium"
                            : "border-slate-700/80 text-slate-400 hover:border-sky-400/60 hover:text-slate-200"
                        }`}
                        title={`按标签「${tag}」筛选（${count} 台）`}
                      >
                        #{tag}
                      </button>
                    );
                  })}
                </div>
              </>
            )}
          </aside>

          {/* 右侧服务器内容区 */}
          <div className="flex-1 min-w-0 overflow-y-auto pr-1">
            {selectedGroup === FILTER_RECENT && (
              <div className="mb-2.5 flex items-center justify-between text-xs text-slate-400">
                <span>按最近操作时间倒序展示（最多 10 台）</span>
              </div>
            )}
            {!filtered.length ? (
              <div className="py-16 text-center text-sm text-slate-400">
                {selectedGroup === FILTER_RECENT && !filter
                  ? "还没有最近操作过的服务器，连接终端或打开文件管理后会自动记录"
                  : "当前筛选下没有匹配的服务器"}
              </div>
            ) : viewMode === "cards" ? (
              <div className="grid grid-cols-1 xl:grid-cols-2 gap-3 pb-2">
                {filtered.map((s) => {
                  const st = connections[s.id];
                  const isConnected = st === "connected";
                  const isConnecting = st === "connecting";
                  const cardBusy = busy && busyId === s.id;
                  const usedLabel = formatLastUsed(s.last_used_at);
                  const tags = s.tags ?? [];
                  return (
                    <div
                      key={s.id}
                      className={`server-card rounded-xl p-4 flex flex-col justify-between gap-3 ${
                        isConnected ? "is-connected" : ""
                      }`}
                    >
                      <div className="space-y-2">
                        <div className="flex items-start justify-between gap-2">
                          <div className="min-w-0 flex items-center gap-2">
                            <span
                              className={`inline-block h-2.5 w-2.5 shrink-0 rounded-full ${
                                isConnected
                                  ? "bg-emerald-500"
                                  : isConnecting
                                    ? "bg-amber-500 animate-pulse"
                                    : "bg-slate-300"
                              }`}
                              title={isConnected ? "终端已连接" : isConnecting ? "连接中" : "未连接"}
                            />
                            <h3 className="text-sm font-semibold text-slate-100 truncate" title={s.name}>
                              {s.name}
                            </h3>
                          </div>
                          <div className="flex items-center gap-1.5 shrink-0">
                            {usedLabel && (
                              <span className="text-[11px] text-slate-400 tabular-nums" title="最近操作时间">
                                {usedLabel}
                              </span>
                            )}
                            {s.group && (
                              <span className="rounded-md bg-sky-500/15 px-2 py-0.5 text-[11px] text-sky-300">
                                {s.group}
                              </span>
                            )}
                            <span className="rounded-md border border-slate-700 px-1.5 py-0.5 text-[11px] text-slate-400">
                              {AUTH_LABELS[s.auth]}
                            </span>
                          </div>
                        </div>

                        <div className="flex items-center justify-between gap-2 rounded-lg border border-slate-700/70 bg-slate-900/50 px-2.5 py-1.5">
                          <code className="text-xs font-mono text-slate-300 truncate">
                            {s.username}@{s.host}:{s.port}
                          </code>
                          <button
                            type="button"
                            onClick={() => void copyAddress(s)}
                            className="shrink-0 text-[11px] text-slate-400 hover:text-sky-300 transition"
                            title="复制 SSH 连接命令"
                          >
                            {copiedId === s.id ? "已复制" : "复制"}
                          </button>
                        </div>

                        {tags.length > 0 && (
                          <div className="flex flex-wrap gap-1.5">
                            {tags.map((tag) => (
                              <button
                                key={tag}
                                type="button"
                                onClick={() => filterByTag(tag)}
                                className="rounded-md border border-slate-700/80 bg-[#fafbfc] px-2 py-0.5 text-[11px] text-slate-400 hover:border-sky-400 hover:text-sky-300 transition"
                                title={`点击按标签「${tag}」搜索`}
                              >
                                #{tag}
                              </button>
                            ))}
                          </div>
                        )}

                        {s.note && (
                          <p className="text-xs text-slate-400 truncate" title={s.note}>
                            {s.note}
                          </p>
                        )}
                      </div>

                      <div className="flex flex-wrap items-center justify-between gap-2 pt-2 border-t border-slate-700/60">
                        <div className="flex items-center gap-2">
                          <button
                            className={`${btnBase} ui-primary`}
                            disabled={busy}
                            onClick={() => void perform(() => openTerminal(s), s.id)}
                          >
                            <Icon name="tab" className="w-3.5 h-3.5" />
                            {cardBusy ? "连接中…" : isConnected ? "进入终端" : "SSH 终端"}
                          </button>
                          <button
                            className={btnBase}
                            disabled={busy}
                            onClick={() => void perform(() => openSftp(s), s.id)}
                            title="打开独立的 XFTP 左右双栏文件管理窗口"
                          >
                            <Icon name="files" className="w-3.5 h-3.5" />
                            文件管理
                          </button>
                        </div>
                        <div className="flex items-center gap-1">
                          {s.auth !== "agent" && (
                            <button
                              className="rounded-md px-2 py-1 text-xs text-slate-400 hover:bg-slate-800/60 hover:text-slate-100 transition"
                              disabled={busy}
                              onClick={() =>
                                void perform(async () =>
                                  setSecret({ name: s.name, value: await ipc.revealServerSecret(s.id) })
                                )
                              }
                            >
                              {s.auth === "key" ? "口令" : "密码"}
                            </button>
                          )}
                          <button
                            className="rounded-md px-2 py-1 text-xs text-slate-400 hover:bg-slate-800/60 hover:text-slate-100 transition"
                            disabled={busy}
                            onClick={() => edit(s)}
                          >
                            编辑
                          </button>
                          <button
                            className="rounded-md px-2 py-1 text-xs text-rose-500 hover:bg-rose-50 transition"
                            disabled={busy}
                            onClick={() => void perform(() => remove(s), s.id)}
                          >
                            删除
                          </button>
                        </div>
                      </div>
                    </div>
                  );
                })}
              </div>
            ) : (
              <div className="overflow-x-auto rounded-xl border border-slate-700">
                <table className="w-full text-sm text-left">
                  <thead className="bg-slate-900/60 text-xs text-slate-400">
                    <tr>
                      <th className="py-2.5 pl-3">服务器</th>
                      <th>地址</th>
                      <th>标签</th>
                      <th>状态 / 最近操作</th>
                      <th className="pr-3 text-right">操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {filtered.map((s) => {
                      const st = connections[s.id];
                      const usedLabel = formatLastUsed(s.last_used_at);
                      return (
                        <tr key={s.id} className="server-list-row border-t border-slate-700">
                          <td className="py-2.5 pr-3">
                            <div className="font-medium text-slate-100">{s.name}</div>
                            {s.group && <div className="text-[11px] text-slate-400">{s.group}</div>}
                          </td>
                          <td className="pr-3 font-mono text-xs text-slate-400 break-all">
                            {s.username}@{s.host}:{s.port}
                          </td>
                          <td className="pr-3">
                            <div className="flex flex-wrap gap-1">
                              {(s.tags ?? []).map((tag) => (
                                <button
                                  key={tag}
                                  type="button"
                                  onClick={() => filterByTag(tag)}
                                  className="rounded border border-slate-700/80 px-1.5 py-0.5 text-[11px] text-slate-400 hover:border-sky-400 hover:text-sky-300"
                                >
                                  #{tag}
                                </button>
                              ))}
                            </div>
                          </td>
                          <td className="whitespace-nowrap text-xs pr-3">
                            <div
                              className={
                                st === "connected"
                                  ? "text-emerald-600 font-medium"
                                  : st === "connecting"
                                    ? "text-amber-600"
                                    : "text-slate-400"
                              }
                            >
                              {st === "connected" ? "已连接" : st === "connecting" ? "连接中" : "未连接"}
                            </div>
                            {usedLabel && <div className="text-[11px] text-slate-400">{usedLabel}</div>}
                          </td>
                          <td className="text-right whitespace-nowrap py-2 pr-3">
                            <div className="inline-flex items-center gap-1.5">
                              <button
                                className={`${btnBase} ui-primary`}
                                disabled={busy}
                                onClick={() => void perform(() => openTerminal(s), s.id)}
                              >
                                {st === "connected" ? "进入终端" : "SSH 终端"}
                              </button>
                              <button
                                className={btnBase}
                                disabled={busy}
                                onClick={() => void perform(() => openSftp(s), s.id)}
                              >
                                文件管理
                              </button>
                              {s.auth !== "agent" && (
                                <button
                                  className={btnBase}
                                  disabled={busy}
                                  onClick={() =>
                                    void perform(async () =>
                                      setSecret({ name: s.name, value: await ipc.revealServerSecret(s.id) })
                                    )
                                  }
                                >
                                  {s.auth === "key" ? "口令" : "密码"}
                                </button>
                              )}
                              <button className={btnBase} disabled={busy} onClick={() => edit(s)}>
                                编辑
                              </button>
                              <button
                                className={`${btnBase} ui-danger`}
                                disabled={busy}
                                onClick={() => void perform(() => remove(s), s.id)}
                              >
                                删除
                              </button>
                            </div>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}
          </div>
        </div>
      )}

      {/* 查看密码弹窗 */}
      {secret && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
          onClick={() => setSecret(null)}
        >
          <div
            role="dialog"
            aria-modal="true"
            aria-label="查看服务器密码"
            className="w-full max-w-md rounded-xl border border-slate-600 bg-slate-800 p-5 space-y-4 shadow-xl"
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (e.key === "Escape") setSecret(null);
            }}
          >
            <h3 className="text-sm font-semibold">{secret.name} · 已保存的密码 / 口令</h3>
            <input
              autoFocus
              readOnly
              aria-label="已保存密码"
              className={`${fieldInput} font-mono`}
              value={secret.value || "（未保存密码或口令）"}
            />
            <div className="flex justify-end gap-2">
              <button
                className={btnBase}
                onClick={() =>
                  void perform(async () => {
                    await navigator.clipboard.writeText(secret.value);
                    setMessage("密码已复制");
                  })
                }
              >
                复制
              </button>
              <button className={`${btnBase} ui-primary`} onClick={() => setSecret(null)}>
                关闭
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 新增 / 编辑服务器弹窗 */}
      {editor && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
          onClick={() => {
            if (!busy) {
              setEditor(null);
              setPassword("");
              setTagDraft("");
            }
          }}
        >
          <form
            role="dialog"
            aria-modal="true"
            aria-label={editor.id ? "编辑服务器" : "新增服务器"}
            className="w-full max-w-xl max-h-[90vh] overflow-y-auto rounded-2xl border border-slate-600 bg-slate-800 p-5 space-y-4 shadow-2xl"
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (e.key === "Escape" && !busy) {
                setEditor(null);
                setPassword("");
                setTagDraft("");
              }
            }}
            onSubmit={(e) => {
              e.preventDefault();
              void perform(async () => {
                const { id, fingerprint: _, last_used_at: __, ...fields } = editor;
                const finalTags = mergeTags(fields.tags ?? [], parseTagInput(tagDraft));
                await call<ServerConnection>(id ? "update_server" : "create_server", {
                  ...fields,
                  tags: finalTags,
                  ...(id ? { server_id: id } : {}),
                  ...(secretChanged || !id ? { secret: password } : {}),
                });
                setEditor(null);
                setPassword("");
                setTagDraft("");
                setVisible(false);
                await load();
                setMessage("连接已保存");
              });
            }}
          >
            <div className="flex items-center justify-between border-b border-slate-700 pb-3">
              <h3 className="text-base font-semibold">{editor.id ? "编辑服务器" : "新增服务器"}</h3>
              <button
                type="button"
                disabled={busy}
                className="rounded-lg p-1 text-slate-400 hover:text-slate-100"
                onClick={() => {
                  setEditor(null);
                  setPassword("");
                  setTagDraft("");
                }}
              >
                <Icon name="close" className="w-4 h-4" />
              </button>
            </div>

            <div className="grid grid-cols-2 gap-3.5">
              <label className="space-y-1 text-xs text-slate-300">
                <span>连接名称</span>
                <input
                  autoFocus
                  required
                  className={fieldInput}
                  placeholder="例如：生产 API 主节点"
                  value={editor.name}
                  onChange={(e) => setEditor({ ...editor, name: e.target.value })}
                />
              </label>
              <label className="space-y-1 text-xs text-slate-300">
                <span>分组（选填）</span>
                <input
                  list="server-group-options"
                  className={fieldInput}
                  placeholder="例如：生产 / 测试 / 个人"
                  value={editor.group}
                  onChange={(e) => setEditor({ ...editor, group: e.target.value })}
                />
                <datalist id="server-group-options">
                  {groupStats.groups.map(([g]) => (
                    <option key={g} value={g} />
                  ))}
                </datalist>
              </label>
              <label className="space-y-1 text-xs text-slate-300">
                <span>主机地址</span>
                <input
                  required
                  className={`${fieldInput} font-mono`}
                  placeholder="192.168.1.10 或 example.com"
                  value={editor.host}
                  onChange={(e) => setEditor({ ...editor, host: e.target.value })}
                />
              </label>
              <label className="space-y-1 text-xs text-slate-300">
                <span>用户名</span>
                <input
                  required
                  className={`${fieldInput} font-mono`}
                  value={editor.username}
                  onChange={(e) => setEditor({ ...editor, username: e.target.value })}
                />
              </label>
              <label className="space-y-1 text-xs text-slate-300">
                <span>端口</span>
                <input
                  type="number"
                  min={1}
                  max={65535}
                  required
                  className={`${fieldInput} font-mono`}
                  value={editor.port}
                  onChange={(e) => setEditor({ ...editor, port: Number(e.target.value) })}
                />
              </label>
              <label className="space-y-1 text-xs text-slate-300">
                <span>认证方式</span>
                <select
                  className={fieldInput}
                  value={editor.auth}
                  onChange={(e) => {
                    setEditor({ ...editor, auth: e.target.value as ServerConnection["auth"] });
                    setPassword("");
                    setSecretChanged(true);
                  }}
                >
                  <option value="password">密码</option>
                  <option value="key">私钥文件</option>
                  <option value="agent">SSH Agent</option>
                </select>
              </label>
            </div>

            {/* 自定义标签 */}
            <div className="space-y-1.5 text-xs text-slate-300">
              <span>标签（选填，按回车或逗号添加，支持搜索）</span>
              <div className="flex flex-wrap items-center gap-1.5 rounded-lg border border-slate-600 bg-slate-900 px-2.5 py-2 focus-within:border-sky-400">
                {(editor.tags ?? []).map((tag) => (
                  <span
                    key={tag}
                    className="inline-flex items-center gap-1 rounded-md bg-sky-500/15 px-2 py-0.5 text-xs text-sky-300"
                  >
                    #{tag}
                    <button
                      type="button"
                      className="text-slate-400 hover:text-rose-500"
                      onClick={() =>
                        setEditor({
                          ...editor,
                          tags: (editor.tags ?? []).filter((t) => t !== tag),
                        })
                      }
                      aria-label={`移除标签 ${tag}`}
                    >
                      ×
                    </button>
                  </span>
                ))}
                <input
                  className="min-w-[140px] flex-1 bg-transparent text-sm text-slate-100 focus:outline-none"
                  placeholder={(editor.tags?.length ?? 0) === 0 ? "例如：GPU, Docker, 华东节点" : "继续添加标签…"}
                  value={tagDraft}
                  onChange={(e) => setTagDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === "," || e.key === "，") {
                      e.preventDefault();
                      commitTagDraft();
                    } else if (e.key === "Backspace" && !tagDraft && (editor.tags?.length ?? 0) > 0) {
                      setEditor({ ...editor, tags: (editor.tags ?? []).slice(0, -1) });
                    }
                  }}
                  onBlur={commitTagDraft}
                />
              </div>
              {allTags.length > 0 && (
                <div className="flex flex-wrap items-center gap-1 pt-0.5">
                  <span className="text-[11px] text-slate-500">常用标签：</span>
                  {allTags
                    .filter(([t]) => !(editor.tags ?? []).some((x) => x.toLowerCase() === t.toLowerCase()))
                    .slice(0, 12)
                    .map(([tag]) => (
                      <button
                        key={tag}
                        type="button"
                        onClick={() => setEditor({ ...editor, tags: mergeTags(editor.tags ?? [], [tag]) })}
                        className="rounded border border-slate-700 px-1.5 py-0.5 text-[11px] text-slate-400 hover:border-sky-400 hover:text-sky-300"
                      >
                        + {tag}
                      </button>
                    ))}
                </div>
              )}
            </div>

            {editor.auth === "key" && (
              <label className="block space-y-1 text-xs text-slate-300">
                <span>私钥绝对路径</span>
                <div className="flex gap-2">
                  <input
                    required
                    className={`${fieldInput} font-mono`}
                    value={editor.private_key}
                    onChange={(e) => setEditor({ ...editor, private_key: e.target.value })}
                  />
                  <button
                    type="button"
                    className={`${btnBase} shrink-0`}
                    onClick={() =>
                      void perform(async () => {
                        const path = await open({ multiple: false });
                        if (typeof path === "string") setEditor({ ...editor, private_key: path });
                      })
                    }
                  >
                    选择文件
                  </button>
                </div>
              </label>
            )}

            {editor.auth !== "agent" && (
              <label className="block space-y-1 text-xs text-slate-300">
                <span>{editor.auth === "key" ? "私钥口令（可留空）" : "密码"}</span>
                <div className="flex gap-2">
                  <input
                    autoComplete="off"
                    className={fieldInput}
                    type={visible ? "text" : "password"}
                    placeholder={editor.id && !secretChanged ? "留空保留已有密码，点击查看可读取" : ""}
                    value={password}
                    onChange={(e) => {
                      setPassword(e.target.value);
                      setSecretChanged(true);
                    }}
                  />
                  <button
                    type="button"
                    className={`${btnBase} shrink-0`}
                    disabled={busy}
                    onClick={() =>
                      void perform(async () => {
                        if (!visible && editor.id && !secretChanged)
                          setPassword(await ipc.revealServerSecret(editor.id));
                        setVisible(!visible);
                      })
                    }
                  >
                    {visible ? "隐藏" : "查看"}
                  </button>
                  <button
                    type="button"
                    className={`${btnBase} shrink-0`}
                    disabled={busy}
                    onClick={() =>
                      void perform(async () => {
                        const text =
                          editor.id && !secretChanged
                            ? await ipc.revealServerSecret(editor.id)
                            : password;
                        await navigator.clipboard.writeText(text);
                        setMessage("密码已复制");
                      })
                    }
                  >
                    复制
                  </button>
                </div>
                <span className="block text-[11px] text-slate-500">
                  密码保存在本机 SQLite 中，可随时查看和复制。
                </span>
              </label>
            )}

            <label className="block space-y-1 text-xs text-slate-300">
              <span>备注（选填）</span>
              <textarea
                rows={2}
                className={fieldInput}
                placeholder="用途说明、部署服务、注意事项等"
                value={editor.note}
                onChange={(e) => setEditor({ ...editor, note: e.target.value })}
              />
            </label>

            <div className="flex justify-end gap-2 pt-2 border-t border-slate-700">
              <button
                type="button"
                className={btnBase}
                disabled={busy}
                onClick={() => {
                  setEditor(null);
                  setPassword("");
                  setTagDraft("");
                }}
              >
                取消
              </button>
              <button disabled={busy} className={`${btnBase} ui-primary px-4`} type="submit">
                {busy ? "保存中…" : "保存连接"}
              </button>
            </div>
          </form>
        </div>
      )}
    </div>
  );
}
