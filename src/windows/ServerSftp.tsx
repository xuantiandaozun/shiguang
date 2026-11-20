// XFTP 风格独立文件管理窗口：左侧本机文件 + 中间快捷传输栏 + 右侧远程服务器 + 底部传输队列。
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { confirm } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ipc, type ServerConnection } from "../lib/ipc";
import { fileSize, remoteJoin, type RemoteEntry, type RemoteListing } from "../lib/useTerminalFiles";
import Icon from "../components/Icon";
import FileTypeIcon from "../components/FileTypeIcon";

interface LocalEntry {
  name: string;
  path: string;
  is_dir: boolean;
  is_file: boolean;
  size: number | null;
  mtime: number | null;
}

interface LocalListing {
  path: string;
  parent: string | null;
  entries: LocalEntry[];
  truncated: boolean;
}

interface LocalRoots {
  quick: Array<{ label: string; path: string }>;
  drives: string[];
  default_path: string;
}

interface SftpTransfer {
  id: string;
  direction: "upload" | "download";
  name: string;
  local_path: string;
  remote_path: string;
  status: string;
  bytes: number;
  error: string | null;
}

function localJoin(dir: string, name: string): string {
  const sep = dir.includes("\\") ? "\\" : "/";
  const clean = dir.endsWith("\\") || dir.endsWith("/") ? dir.slice(0, -1) : dir;
  return `${clean}${sep}${name}`;
}

function formatTime(mtime: number | null): string {
  if (mtime === null) return "—";
  const d = new Date(mtime * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

const smallBtn =
  "inline-flex items-center justify-center gap-1 rounded-md border border-slate-700 bg-white px-2 py-1 text-xs text-slate-200 hover:bg-slate-800/40 disabled:opacity-40 disabled:cursor-not-allowed transition";
const primaryBtn =
  "inline-flex items-center justify-center gap-1 rounded-md px-2.5 py-1 text-xs font-medium text-white bg-[#c97a16] hover:bg-[#b36a10] disabled:opacity-40 disabled:cursor-not-allowed transition";

export default function ServerSftp() {
  const serverId = new URLSearchParams(window.location.hash.split("?")[1]).get("server") || "";
  const [server, setServer] = useState<ServerConnection | null>(null);
  const [maximized, setMaximized] = useState(false);

  // 左侧：本机状态
  const [roots, setRoots] = useState<LocalRoots>({ quick: [], drives: [], default_path: "" });
  const [localListing, setLocalListing] = useState<LocalListing>({ path: "", parent: null, entries: [], truncated: false });
  const [localAddress, setLocalAddress] = useState("");
  const [localFilter, setLocalFilter] = useState("");
  const [localLoading, setLocalLoading] = useState(false);
  const [localError, setLocalError] = useState("");
  const [localSelected, setLocalSelected] = useState<string[]>([]);

  // 右侧：远程服务器状态
  const [remoteListing, setRemoteListing] = useState<RemoteListing>({ path: ".", entries: [], truncated: false });
  const [remoteAddress, setRemoteAddress] = useState(".");
  const [remoteFilter, setRemoteFilter] = useState("");
  const [remoteLoading, setRemoteLoading] = useState(false);
  const [remoteError, setRemoteError] = useState("");
  const [remoteSelected, setRemoteSelected] = useState<string[]>([]);
  const [remoteConnected, setRemoteConnected] = useState(false);

  // 中间分割比例（百分比）
  const [splitPct, setSplitPct] = useState(48);
  const dragging = useRef(false);
  const splitContainerRef = useRef<HTMLDivElement>(null);

  // 底部传输队列 + 文本预览 + 输入弹框（新建目录/重命名）
  const [transfers, setTransfers] = useState<SftpTransfer[]>([]);
  const [queueOpen, setQueueOpen] = useState(true);
  const [preview, setPreview] = useState<{ path: string; content: string; truncated: boolean } | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);
  const [promptModal, setPromptModal] = useState<{
    title: string;
    label: string;
    initial: string;
    onConfirm: (value: string) => Promise<void>;
  } | null>(null);
  const [promptValue, setPromptValue] = useState("");
  const [promptBusy, setPromptBusy] = useState(false);
  const [banner, setBanner] = useState<{ text: string; error?: boolean } | null>(null);

  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  useEffect(() => {
    if (!banner) return;
    const t = window.setTimeout(() => setBanner(null), 3500);
    return () => window.clearTimeout(t);
  }, [banner]);

  const sftp = useCallback(
    <T,>(action: string, payload?: Record<string, unknown>) => ipc.serverSftp<T>(action, serverId, payload),
    [serverId]
  );

  // 窗口最大化监听
  useEffect(() => {
    let dead = false;
    const win = getCurrentWindow();
    const sync = () => {
      win.isMaximized().then((v) => !dead && setMaximized(v)).catch(() => {});
    };
    sync();
    const listener = win.onResized(sync);
    return () => {
      dead = true;
      void listener.then((u) => u()).catch(() => {});
    };
  }, []);

  const loadLocal = useCallback(
    async (targetPath?: string) => {
      const path = targetPath ?? localListing.path;
      setLocalLoading(true);
      setLocalError("");
      try {
        const res = await sftp<LocalListing>("local_list", path ? { path } : {});
        if (!alive.current) return;
        setLocalListing(res);
        setLocalAddress(res.path);
        setLocalSelected([]);
        try {
          localStorage.setItem(`sftp-local-path:${serverId}`, res.path);
        } catch {
          /* ignore */
        }
      } catch (e) {
        if (alive.current) setLocalError(String(e));
      } finally {
        if (alive.current) setLocalLoading(false);
      }
    },
    [sftp, localListing.path, serverId]
  );

  const loadRemote = useCallback(
    async (targetPath?: string) => {
      const path = targetPath ?? (remoteListing.path || ".");
      setRemoteLoading(true);
      setRemoteError("");
      try {
        const res = await sftp<RemoteListing>("remote_list", { path });
        if (!alive.current) return;
        res.entries.sort((a, b) => Number(b.stat.is_dir) - Number(a.stat.is_dir) || a.name.localeCompare(b.name));
        setRemoteListing(res);
        setRemoteAddress(res.path);
        setRemoteSelected([]);
        setRemoteConnected(true);
      } catch (e) {
        if (alive.current) {
          setRemoteError(String(e));
          setRemoteConnected(false);
        }
      } finally {
        if (alive.current) setRemoteLoading(false);
      }
    },
    [sftp, remoteListing.path]
  );

  // 初始化：服务器信息 + 本机盘符/快捷目录 + 两侧文件列表
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [info, rootInfo] = await Promise.all([
          sftp<ServerConnection>("server_info"),
          sftp<LocalRoots>("local_roots"),
        ]);
        if (cancelled) return;
        setServer(info);
        setRoots(rootInfo);
        let initialLocal = rootInfo.default_path;
        try {
          const saved = localStorage.getItem(`sftp-local-path:${serverId}`);
          if (saved) initialLocal = saved;
        } catch {
          /* ignore */
        }
        await Promise.all([loadLocal(initialLocal), loadRemote(".")]);
      } catch (e) {
        if (!cancelled) setRemoteError(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [serverId]);

  const pollTransfer = useCallback(
    async (item: SftpTransfer) => {
      setTransfers((prev) => [item, ...prev.slice(0, 49)]);
      setQueueOpen(true);
      while (alive.current) {
        await new Promise((r) => setTimeout(r, 500));
        if (!alive.current) return;
        try {
          const res = await sftp<{ task: { status: string; bytes: number; error: string | null } }>("transfer", {
            task_id: item.id,
          });
          const updated: SftpTransfer = { ...item, ...res.task };
          setTransfers((prev) => prev.map((t) => (t.id === item.id ? updated : t)));
          if (updated.status !== "running") {
            if (updated.status === "done") {
              if (item.direction === "upload") void loadRemote();
              else void loadLocal();
            }
            return;
          }
        } catch (e) {
          setTransfers((prev) =>
            prev.map((t) =>
              t.id === item.id ? { ...t, status: "unknown", error: `状态查询失败：${String(e)}` } : t
            )
          );
          return;
        }
      }
    },
    [sftp, loadLocal, loadRemote]
  );

  // 上传（左 → 右，支持多选文件或文件夹）
  const uploadEntries = async (entries: LocalEntry[]) => {
    if (!entries.length || !remoteListing.path.startsWith("/")) return;
    let started = 0;
    for (const entry of entries) {
      const remoteTarget = remoteJoin(remoteListing.path, entry.name);
      try {
        const spec = await sftp<{
          local_path: string;
          remote_path: string;
          exists?: boolean;
          sensitive?: boolean;
        }>("prepare_upload", {
          local_path: entry.path,
          remote_path: remoteTarget,
          overwrite: false,
        });
        let overwrite = false;
        if (spec.exists || spec.sensitive) {
          const ok = await confirm(
            `本机：${entry.path}\n→ 远程：${spec.remote_path}\n${
              spec.exists ? "远程已存在同名文件/目录，将覆盖。\n" : ""
            }${spec.sensitive ? "目标位于系统敏感目录，请确认影响。\n" : ""}是否继续上传？`,
            {
              title: spec.exists ? "确认覆盖上传" : "确认上传位置",
              kind: "warning",
              okLabel: spec.exists ? "覆盖上传" : "继续上传",
              cancelLabel: "跳过",
            }
          );
          if (!ok) continue;
          overwrite = !!spec.exists;
        }
        const res = await sftp<{ task_id: string }>("upload", {
          local_path: entry.path,
          remote_path: spec.remote_path,
          overwrite,
        });
        started++;
        void pollTransfer({
          id: res.task_id,
          direction: "upload",
          name: entry.name,
          local_path: entry.path,
          remote_path: spec.remote_path,
          status: "running",
          bytes: 0,
          error: null,
        });
      } catch (e) {
        setBanner({ text: `上传 ${entry.name} 失败：${String(e)}`, error: true });
      }
    }
    if (started > 0) setBanner({ text: `已加入 ${started} 个上传任务` });
  };

  // 下载（右 → 左，支持多选文件或文件夹）
  const downloadEntries = async (entries: RemoteEntry[]) => {
    if (!entries.length || !localListing.path) return;
    let started = 0;
    for (const entry of entries) {
      const remotePath = remoteJoin(remoteListing.path, entry.name);
      const localTarget = localJoin(localListing.path, entry.name);
      try {
        const st = await sftp<{ exists: boolean; is_dir: boolean }>("local_exists", { path: localTarget });
        let overwrite = false;
        if (st.exists) {
          const ok = await confirm(
            `远程：${remotePath}\n→ 本机：${localTarget}\n本机已存在同名文件/目录，是否覆盖下载？`,
            { title: "确认覆盖下载", kind: "warning", okLabel: "覆盖下载", cancelLabel: "跳过" }
          );
          if (!ok) continue;
          overwrite = true;
        }
        const res = await sftp<{ task_id: string }>("download", {
          remote_path: remotePath,
          local_path: localTarget,
          overwrite,
        });
        started++;
        void pollTransfer({
          id: res.task_id,
          direction: "download",
          name: entry.name,
          local_path: localTarget,
          remote_path: remotePath,
          status: "running",
          bytes: 0,
          error: null,
        });
      } catch (e) {
        setBanner({ text: `下载 ${entry.name} 失败：${String(e)}`, error: true });
      }
    }
    if (started > 0) setBanner({ text: `已加入 ${started} 个下载任务` });
  };

  const openPrompt = (title: string, label: string, initial: string, onConfirm: (v: string) => Promise<void>) => {
    setPromptValue(initial);
    setPromptModal({ title, label, initial, onConfirm });
  };

  // 本机操作
  const handleLocalMkdir = () => {
    if (!localListing.path) return;
    openPrompt("新建本机文件夹", "文件夹名称", "", async (name) => {
      await sftp("local_mkdir", { parent: localListing.path, name });
      await loadLocal();
      setBanner({ text: `已创建本机文件夹「${name}」` });
    });
  };

  const handleLocalRename = (entry: LocalEntry) => {
    openPrompt("重命名本机文件", "新名称", entry.name, async (newName) => {
      if (newName === entry.name) return;
      await sftp("local_rename", { path: entry.path, new_name: newName });
      await loadLocal();
      setBanner({ text: `已重命名为「${newName}」` });
    });
  };

  const handleLocalDelete = async (entries: LocalEntry[]) => {
    if (!entries.length) return;
    const names = entries.slice(0, 5).map((e) => e.name).join("、");
    const ok = await confirm(
      `将选中的 ${entries.length} 个本机项目移到回收站？\n${names}${entries.length > 5 ? " 等" : ""}`,
      { title: "移到回收站", kind: "warning", okLabel: "移到回收站", cancelLabel: "取消" }
    );
    if (!ok) return;
    for (const entry of entries) {
      await sftp("local_delete", { path: entry.path });
    }
    await loadLocal();
    setBanner({ text: `已将 ${entries.length} 项移到回收站` });
  };

  // 远程操作
  const handleRemoteMkdir = () => {
    if (!remoteListing.path.startsWith("/")) return;
    openPrompt("新建远程文件夹", "文件夹名称", "", async (name) => {
      const target = remoteJoin(remoteListing.path, name.trim());
      await sftp("remote_mkdir", { path: target });
      await loadRemote();
      setBanner({ text: `已创建远程目录「${name}」` });
    });
  };

  const handleRemoteRename = (entry: RemoteEntry) => {
    openPrompt("重命名远程文件", "新名称", entry.name, async (newName) => {
      if (newName === entry.name) return;
      const oldPath = remoteJoin(remoteListing.path, entry.name);
      const newPath = remoteJoin(remoteListing.path, newName.trim());
      await sftp("remote_rename", { old_path: oldPath, new_path: newPath });
      await loadRemote();
      setBanner({ text: `已重命名为「${newName}」` });
    });
  };

  const handleRemoteDelete = async (entries: RemoteEntry[]) => {
    if (!entries.length) return;
    const names = entries.slice(0, 5).map((e) => e.name).join("、");
    const ok = await confirm(
      `永久删除服务器上的 ${entries.length} 个项目（含子目录）？此操作不可撤销。\n${names}${entries.length > 5 ? " 等" : ""}`,
      { title: "确认删除远程文件", kind: "warning", okLabel: "确认删除", cancelLabel: "取消" }
    );
    if (!ok) return;
    for (const entry of entries) {
      await sftp("remote_delete", { path: remoteJoin(remoteListing.path, entry.name) });
    }
    await loadRemote();
    setBanner({ text: `已删除 ${entries.length} 个远程项目` });
  };

  const handleRemotePreview = async (entry: RemoteEntry) => {
    const path = remoteJoin(remoteListing.path, entry.name);
    setPreviewBusy(true);
    try {
      const res = await sftp<{ content: string; truncated: boolean }>("preview", { path });
      setPreview({ path, ...res });
    } catch (e) {
      setBanner({ text: String(e), error: true });
    } finally {
      setPreviewBusy(false);
    }
  };

  const toggleSelect = (list: string[], name: string, multi: boolean): string[] => {
    if (!multi) return list.length === 1 && list[0] === name ? [] : [name];
    return list.includes(name) ? list.filter((x) => x !== name) : [...list, name];
  };

  const visibleLocal = useMemo(
    () => localListing.entries.filter((e) => e.name.toLowerCase().includes(localFilter.toLowerCase())),
    [localListing.entries, localFilter]
  );
  const visibleRemote = useMemo(
    () => remoteListing.entries.filter((e) => e.name.toLowerCase().includes(remoteFilter.toLowerCase())),
    [remoteListing.entries, remoteFilter]
  );

  const selectedLocalEntries = useMemo(
    () => localListing.entries.filter((e) => localSelected.includes(e.name)),
    [localListing.entries, localSelected]
  );
  const selectedRemoteEntries = useMemo(
    () => remoteListing.entries.filter((e) => remoteSelected.includes(e.name)),
    [remoteListing.entries, remoteSelected]
  );

  const runningCount = transfers.filter((t) => t.status === "running").length;

  return (
    <div className="h-screen flex flex-col bg-[#f6f7f9] text-slate-200 select-none">
      {/* 顶栏 */}
      <header className="shrink-0 flex flex-wrap items-center justify-between gap-3 border-b border-slate-700 bg-white px-4 py-2.5">
        <div className="flex items-center gap-2.5 min-w-0">
          <span
            className={`inline-block h-2.5 w-2.5 rounded-full shrink-0 ${
              remoteConnected ? "bg-emerald-500" : remoteLoading ? "bg-amber-400 animate-pulse" : "bg-rose-400"
            }`}
          />
          <span className="text-sm font-semibold text-slate-100 truncate">
            {server?.name || "文件管理"}
          </span>
          {server && (
            <code className="hidden sm:inline rounded bg-[#f6f7f9] border border-slate-700 px-2 py-0.5 font-mono text-xs text-slate-400 truncate">
              {server.username}@{server.host}:{server.port}
            </code>
          )}
          <span className="rounded bg-sky-500/15 px-1.5 py-0.5 text-[11px] font-medium text-sky-300">
            SFTP 双栏
          </span>
        </div>

        <div className="flex items-center gap-2">
          {banner && (
            <span className={`text-xs ${banner.error ? "text-rose-500" : "text-sky-400"}`}>
              {banner.text}
            </span>
          )}
          <button
            className={smallBtn}
            onClick={() =>
              void ipc.serverTerminal("open", serverId).catch((e) => setBanner({ text: String(e), error: true }))
            }
            title="同时打开该服务器的 SSH 命令终端窗口"
          >
            <Icon name="tab" className="w-3.5 h-3.5" />
            打开 SSH 终端
          </button>
          <button
            className={smallBtn}
            onClick={() => {
              void loadLocal();
              void loadRemote();
            }}
            title="同时刷新本机与远程目录"
          >
            <Icon name="refresh" className={`w-3.5 h-3.5 ${localLoading || remoteLoading ? "animate-spin" : ""}`} />
            刷新两侧
          </button>
          <button
            className={smallBtn}
            onClick={() => void getCurrentWindow().toggleMaximize().catch(() => {})}
            title={maximized ? "还原窗口" : "最大化窗口"}
          >
            <Icon name="maximize" className="w-3.5 h-3.5" />
          </button>
        </div>
      </header>

      {/* 主区域：左本机 + 中间传输条 + 右远程 */}
      <div ref={splitContainerRef} className="flex flex-1 min-h-0 overflow-hidden p-2 gap-0">
        {/* 左侧：本机文件 */}
        <section
          aria-label="本机文件"
          className="flex min-w-[300px] min-h-0 flex-col rounded-xl border border-slate-700 bg-white overflow-hidden"
          style={{ width: `${splitPct}%` }}
        >
          {/* 面板标题 + 快捷目录/盘符 */}
          <div className="shrink-0 flex flex-wrap items-center justify-between gap-2 border-b border-slate-700/80 bg-[#fafbfc] px-3 py-2">
            <span className="text-xs font-semibold text-slate-100">本机文件 (Windows)</span>
            <div className="flex flex-wrap items-center gap-1">
              {roots.quick.map((q) => (
                <button
                  key={q.label}
                  type="button"
                  onClick={() => void loadLocal(q.path)}
                  className="rounded px-1.5 py-0.5 text-[11px] text-slate-400 hover:bg-sky-500/15 hover:text-sky-300 transition"
                >
                  {q.label}
                </button>
              ))}
              {roots.drives.length > 0 && (
                <select
                  aria-label="切换磁盘"
                  className="ml-1 rounded border border-slate-700 bg-white px-1.5 py-0.5 font-mono text-[11px] text-slate-200"
                  value={roots.drives.find((d) => localListing.path.toUpperCase().startsWith(d.toUpperCase())) ?? ""}
                  onChange={(e) => {
                    if (e.target.value) void loadLocal(e.target.value);
                  }}
                >
                  <option value="" disabled>
                    磁盘
                  </option>
                  {roots.drives.map((d) => (
                    <option key={d} value={d}>
                      {d}
                    </option>
                  ))}
                </select>
              )}
            </div>
          </div>

          {/* 地址栏 */}
          <form
            className="shrink-0 flex items-center gap-1.5 border-b border-slate-700/60 px-2.5 py-2"
            onSubmit={(e) => {
              e.preventDefault();
              if (localAddress.trim()) void loadLocal(localAddress.trim());
            }}
          >
            <button
              type="button"
              className={smallBtn}
              disabled={localLoading || !localListing.parent}
              onClick={() => localListing.parent && void loadLocal(localListing.parent)}
              title="上级目录"
            >
              <Icon name="up" className="w-3.5 h-3.5" />
            </button>
            <input
              aria-label="本机目录路径"
              className="min-w-0 flex-1 rounded-md border border-slate-700 bg-white px-2.5 py-1 font-mono text-xs text-slate-100 focus:border-sky-400 focus:outline-none"
              value={localAddress}
              onChange={(e) => setLocalAddress(e.target.value)}
            />
            <button className={smallBtn} disabled={localLoading || !localAddress.trim()}>
              前往
            </button>
            <button type="button" className={smallBtn} onClick={handleLocalMkdir} title="新建本机文件夹">
              <Icon name="plus" className="w-3.5 h-3.5" />
            </button>
            <button type="button" className={smallBtn} disabled={localLoading} onClick={() => void loadLocal()} title="刷新">
              <Icon name="refresh" className={`w-3.5 h-3.5 ${localLoading ? "animate-spin" : ""}`} />
            </button>
          </form>

          {/* 过滤 & 选中操作栏 */}
          <div className="shrink-0 flex flex-wrap items-center justify-between gap-2 border-b border-slate-700/60 bg-[#fcfcfd] px-2.5 py-1.5">
            <input
              aria-label="筛选本机文件"
              placeholder="筛选当前目录…"
              className="w-40 min-w-0 flex-1 rounded-md border border-slate-700/80 bg-white px-2 py-1 text-xs focus:border-sky-400 focus:outline-none"
              value={localFilter}
              onChange={(e) => setLocalFilter(e.target.value)}
            />
            <div className="flex items-center gap-1">
              <button
                type="button"
                className={primaryBtn}
                disabled={!selectedLocalEntries.length || !remoteConnected}
                onClick={() => void uploadEntries(selectedLocalEntries)}
                title="上传选中文件/文件夹到右侧远程目录"
              >
                上传 →
              </button>
              <button
                type="button"
                className={smallBtn}
                disabled={selectedLocalEntries.length !== 1}
                onClick={() => selectedLocalEntries[0] && handleLocalRename(selectedLocalEntries[0])}
              >
                重命名
              </button>
              <button
                type="button"
                className={smallBtn}
                disabled={!selectedLocalEntries.length}
                onClick={() => void handleLocalDelete(selectedLocalEntries)}
              >
                回收站
              </button>
              <button
                type="button"
                className={smallBtn}
                disabled={!localListing.path}
                onClick={() =>
                  void ipc
                    .revealInFolder(selectedLocalEntries[0]?.path || localListing.path)
                    .catch((e) => setBanner({ text: String(e), error: true }))
                }
                title="在资源管理器中打开"
              >
                定位
              </button>
            </div>
          </div>

          {localError && (
            <div className="shrink-0 bg-rose-50 px-3 py-1.5 text-xs text-rose-600 break-all">{localError}</div>
          )}

          {/* 本机文件列表 */}
          <div className="min-h-0 flex-1 overflow-auto">
            <table className="w-full table-fixed text-left text-xs">
              <thead className="sticky top-0 z-10 bg-[#f6f7f9] text-slate-400 border-b border-slate-700">
                <tr>
                  <th className="w-7 pl-2.5 py-2">
                    <input
                      type="checkbox"
                      aria-label="全选本机文件"
                      checked={visibleLocal.length > 0 && visibleLocal.every((e) => localSelected.includes(e.name))}
                      onChange={(e) =>
                        setLocalSelected(e.target.checked ? visibleLocal.map((x) => x.name) : [])
                      }
                    />
                  </th>
                  <th className="py-2 font-medium">名称</th>
                  <th className="w-20 pr-2 text-right font-medium">大小</th>
                  <th className="w-32 pr-3 text-right font-medium">修改时间</th>
                </tr>
              </thead>
              <tbody>
                {visibleLocal.map((entry) => {
                  const selected = localSelected.includes(entry.name);
                  return (
                    <tr
                      key={entry.name}
                      className={`sftp-row border-t border-slate-700/40 cursor-pointer ${selected ? "is-selected" : ""}`}
                      onClick={(e) => setLocalSelected((cur) => toggleSelect(cur, entry.name, e.ctrlKey || e.metaKey))}
                      onDoubleClick={() => {
                        if (entry.is_dir) void loadLocal(entry.path);
                        else if (remoteConnected) void uploadEntries([entry]);
                      }}
                    >
                      <td
                        className="pl-2.5 py-1.5"
                        onClick={(e) => {
                          e.stopPropagation();
                          setLocalSelected((cur) => toggleSelect(cur, entry.name, true));
                        }}
                      >
                        <input type="checkbox" readOnly checked={selected} />
                      </td>
                      <td className="py-1.5 pr-2" title={entry.path}>
                        <div className="flex min-w-0 items-center gap-2">
                          <FileTypeIcon name={entry.name} directory={entry.is_dir} regular={entry.is_file} />
                          <span className={`min-w-0 truncate ${entry.is_dir ? "font-medium text-sky-400" : "text-slate-100"}`}>
                            {entry.name}
                          </span>
                        </div>
                      </td>
                      <td className="pr-2 text-right tabular-nums text-slate-400">
                        {entry.is_dir ? "文件夹" : fileSize(entry.size)}
                      </td>
                      <td className="pr-3 text-right tabular-nums text-[11px] text-slate-400">
                        {formatTime(entry.mtime)}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            {localLoading && <p className="px-3 py-6 text-center text-xs text-slate-400">正在读取本机目录…</p>}
            {!localLoading && !visibleLocal.length && (
              <p className="px-3 py-10 text-center text-xs text-slate-400">
                {localFilter ? "没有匹配的文件" : "此目录为空"}
              </p>
            )}
          </div>

          <div className="shrink-0 flex items-center justify-between border-t border-slate-700/70 bg-[#fafbfc] px-3 py-1 text-[11px] text-slate-400">
            <span>
              共 {localListing.entries.length} 项{localListing.truncated ? "（仅前 2000 项）" : ""} · 已选 {localSelected.length} 项
            </span>
            <span>双击文件夹进入 · 双击文件直接上传</span>
          </div>
        </section>

        {/* 中间拖拽分割条 + XFTP 双向传输快捷按钮 */}
        <div
          role="separator"
          aria-orientation="vertical"
          aria-label="调整左右面板宽度"
          className="w-10 shrink-0 flex flex-col items-center justify-center gap-2 cursor-col-resize select-none"
          onPointerDown={(e) => {
            if ((e.target as HTMLElement).closest("button")) return;
            dragging.current = true;
            e.currentTarget.setPointerCapture(e.pointerId);
          }}
          onPointerMove={(e) => {
            if (!dragging.current || !splitContainerRef.current) return;
            const rect = splitContainerRef.current.getBoundingClientRect();
            const pct = ((e.clientX - rect.left) / rect.width) * 100;
            setSplitPct(Math.max(25, Math.min(75, pct)));
          }}
          onPointerUp={() => {
            dragging.current = false;
          }}
          onPointerCancel={() => {
            dragging.current = false;
          }}
        >
          <button
            type="button"
            disabled={!selectedLocalEntries.length || !remoteConnected}
            onClick={() => void uploadEntries(selectedLocalEntries)}
            className="flex h-8 w-8 items-center justify-center rounded-lg border border-slate-700 bg-white text-xs font-bold text-[#c97a16] shadow-sm hover:bg-[#fff3df] disabled:opacity-35 disabled:cursor-not-allowed transition"
            title="上传选中的本机文件到远程服务器 (→)"
          >
            →
          </button>
          <div className="h-10 w-1 rounded-full bg-slate-300/80" />
          <button
            type="button"
            disabled={!selectedRemoteEntries.length || !localListing.path}
            onClick={() => void downloadEntries(selectedRemoteEntries)}
            className="flex h-8 w-8 items-center justify-center rounded-lg border border-slate-700 bg-white text-xs font-bold text-emerald-600 shadow-sm hover:bg-emerald-50 disabled:opacity-35 disabled:cursor-not-allowed transition"
            title="下载选中的远程文件到本机目录 (←)"
          >
            ←
          </button>
        </div>

        {/* 右侧：远程服务器文件 */}
        <section
          aria-label="远程服务器文件"
          className="flex flex-1 min-w-[300px] min-h-0 flex-col rounded-xl border border-slate-700 bg-white overflow-hidden"
        >
          {/* 面板标题 + 远程快捷目录 */}
          <div className="shrink-0 flex flex-wrap items-center justify-between gap-2 border-b border-slate-700/80 bg-[#fafbfc] px-3 py-2">
            <span className="text-xs font-semibold text-slate-100">
              远程服务器 ({server?.host || "SFTP"})
            </span>
            <div className="flex items-center gap-1">
              <button
                type="button"
                onClick={() => void loadRemote(".")}
                className="rounded px-1.5 py-0.5 text-[11px] text-slate-400 hover:bg-sky-500/15 hover:text-sky-300 transition"
              >
                主目录 (~)
              </button>
              <button
                type="button"
                onClick={() => void loadRemote("/")}
                className="rounded px-1.5 py-0.5 text-[11px] text-slate-400 hover:bg-sky-500/15 hover:text-sky-300 transition"
              >
                根目录 (/)
              </button>
              <button
                type="button"
                onClick={() => void loadRemote("/tmp")}
                className="rounded px-1.5 py-0.5 text-[11px] text-slate-400 hover:bg-sky-500/15 hover:text-sky-300 transition"
              >
                /tmp
              </button>
              <button
                type="button"
                onClick={() => void loadRemote("/var/log")}
                className="rounded px-1.5 py-0.5 text-[11px] text-slate-400 hover:bg-sky-500/15 hover:text-sky-300 transition"
              >
                /var/log
              </button>
            </div>
          </div>

          {/* 远程地址栏 */}
          <form
            className="shrink-0 flex items-center gap-1.5 border-b border-slate-700/60 px-2.5 py-2"
            onSubmit={(e) => {
              e.preventDefault();
              if (remoteAddress.trim()) void loadRemote(remoteAddress.trim());
            }}
          >
            <button
              type="button"
              className={smallBtn}
              disabled={remoteLoading || remoteListing.path === "/"}
              onClick={() => void loadRemote(remoteJoin(remoteListing.path, ".."))}
              title="上级目录"
            >
              <Icon name="up" className="w-3.5 h-3.5" />
            </button>
            <input
              aria-label="远程目录路径"
              className="min-w-0 flex-1 rounded-md border border-slate-700 bg-white px-2.5 py-1 font-mono text-xs text-slate-100 focus:border-sky-400 focus:outline-none"
              value={remoteAddress}
              onChange={(e) => setRemoteAddress(e.target.value)}
            />
            <button className={smallBtn} disabled={remoteLoading || !remoteAddress.trim()}>
              前往
            </button>
            <button
              type="button"
              className={smallBtn}
              disabled={!remoteConnected}
              onClick={handleRemoteMkdir}
              title="新建远程文件夹"
            >
              <Icon name="plus" className="w-3.5 h-3.5" />
            </button>
            <button type="button" className={smallBtn} disabled={remoteLoading} onClick={() => void loadRemote()} title="刷新">
              <Icon name="refresh" className={`w-3.5 h-3.5 ${remoteLoading ? "animate-spin" : ""}`} />
            </button>
          </form>

          {/* 远程过滤 & 操作栏 */}
          <div className="shrink-0 flex flex-wrap items-center justify-between gap-2 border-b border-slate-700/60 bg-[#fcfcfd] px-2.5 py-1.5">
            <input
              aria-label="筛选远程文件"
              placeholder="筛选当前目录…"
              className="w-40 min-w-0 flex-1 rounded-md border border-slate-700/80 bg-white px-2 py-1 text-xs focus:border-sky-400 focus:outline-none"
              value={remoteFilter}
              onChange={(e) => setRemoteFilter(e.target.value)}
            />
            <div className="flex items-center gap-1">
              <button
                type="button"
                className="inline-flex items-center justify-center gap-1 rounded-md px-2.5 py-1 text-xs font-medium text-white bg-emerald-600 hover:bg-emerald-700 disabled:opacity-40 disabled:cursor-not-allowed transition"
                disabled={!selectedRemoteEntries.length || !localListing.path}
                onClick={() => void downloadEntries(selectedRemoteEntries)}
                title="下载选中的远程文件/文件夹到左侧本机目录"
              >
                ← 下载
              </button>
              <button
                type="button"
                className={smallBtn}
                disabled={selectedRemoteEntries.length !== 1 || !selectedRemoteEntries[0]?.stat.is_file || previewBusy}
                onClick={() => selectedRemoteEntries[0] && void handleRemotePreview(selectedRemoteEntries[0])}
              >
                {previewBusy ? "读取中…" : "预览"}
              </button>
              <button
                type="button"
                className={smallBtn}
                disabled={selectedRemoteEntries.length !== 1}
                onClick={() => selectedRemoteEntries[0] && handleRemoteRename(selectedRemoteEntries[0])}
              >
                重命名
              </button>
              <button
                type="button"
                className={`${smallBtn} text-rose-600 hover:bg-rose-50`}
                disabled={!selectedRemoteEntries.length}
                onClick={() => void handleRemoteDelete(selectedRemoteEntries)}
              >
                删除
              </button>
            </div>
          </div>

          {remoteError && (
            <div className="shrink-0 bg-rose-50 px-3 py-1.5 text-xs text-rose-600 break-all">{remoteError}</div>
          )}

          {/* 远程文件列表 */}
          <div className="min-h-0 flex-1 overflow-auto">
            <table className="w-full table-fixed text-left text-xs">
              <thead className="sticky top-0 z-10 bg-[#f6f7f9] text-slate-400 border-b border-slate-700">
                <tr>
                  <th className="w-7 pl-2.5 py-2">
                    <input
                      type="checkbox"
                      aria-label="全选远程文件"
                      checked={visibleRemote.length > 0 && visibleRemote.every((e) => remoteSelected.includes(e.name))}
                      onChange={(e) =>
                        setRemoteSelected(e.target.checked ? visibleRemote.map((x) => x.name) : [])
                      }
                    />
                  </th>
                  <th className="py-2 font-medium">名称</th>
                  <th className="w-20 pr-2 text-right font-medium">大小</th>
                  <th className="w-14 pr-2 text-right font-medium">权限</th>
                  <th className="w-32 pr-3 text-right font-medium">修改时间</th>
                </tr>
              </thead>
              <tbody>
                {visibleRemote.map((entry) => {
                  const selected = remoteSelected.includes(entry.name);
                  const perm =
                    entry.stat.permissions === null
                      ? "—"
                      : (entry.stat.permissions & 0o777).toString(8).padStart(3, "0");
                  return (
                    <tr
                      key={entry.name}
                      className={`sftp-row border-t border-slate-700/40 cursor-pointer ${selected ? "is-selected" : ""}`}
                      onClick={(e) =>
                        setRemoteSelected((cur) => toggleSelect(cur, entry.name, e.ctrlKey || e.metaKey))
                      }
                      onDoubleClick={() => {
                        if (entry.stat.is_dir) void loadRemote(remoteJoin(remoteListing.path, entry.name));
                        else if (localListing.path) void downloadEntries([entry]);
                      }}
                    >
                      <td
                        className="pl-2.5 py-1.5"
                        onClick={(e) => {
                          e.stopPropagation();
                          setRemoteSelected((cur) => toggleSelect(cur, entry.name, true));
                        }}
                      >
                        <input type="checkbox" readOnly checked={selected} />
                      </td>
                      <td className="py-1.5 pr-2" title={remoteJoin(remoteListing.path, entry.name)}>
                        <div className="flex min-w-0 items-center gap-2">
                          <FileTypeIcon
                            name={entry.name}
                            directory={entry.stat.is_dir}
                            regular={entry.stat.is_file}
                          />
                          <span
                            className={`min-w-0 truncate ${
                              entry.stat.is_dir ? "font-medium text-sky-400" : "text-slate-100"
                            }`}
                          >
                            {entry.name}
                          </span>
                        </div>
                      </td>
                      <td className="pr-2 text-right tabular-nums text-slate-400">
                        {entry.stat.is_dir ? "目录" : fileSize(entry.stat.size)}
                      </td>
                      <td className="pr-2 text-right font-mono text-[11px] text-slate-400">{perm}</td>
                      <td className="pr-3 text-right tabular-nums text-[11px] text-slate-400">
                        {formatTime(entry.stat.mtime)}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            {remoteLoading && <p className="px-3 py-6 text-center text-xs text-slate-400">正在读取服务器目录…</p>}
            {!remoteLoading && !visibleRemote.length && (
              <p className="px-3 py-10 text-center text-xs text-slate-400">
                {remoteError ? "读取失败，请点击刷新重试" : remoteFilter ? "没有匹配的文件" : "此目录为空"}
              </p>
            )}
          </div>

          <div className="shrink-0 flex items-center justify-between border-t border-slate-700/70 bg-[#fafbfc] px-3 py-1 text-[11px] text-slate-400">
            <span>
              共 {remoteListing.entries.length} 项{remoteListing.truncated ? "（仅前 2000 项）" : ""} · 已选 {remoteSelected.length} 项
            </span>
            <span>双击目录进入 · 双击文件直接下载</span>
          </div>
        </section>
      </div>

      {/* 底部 XFTP 传输队列 */}
      <footer className="shrink-0 border-t border-slate-700 bg-white">
        <div className="flex items-center justify-between px-4 py-1.5 bg-[#fafbfc] border-b border-slate-700/60">
          <button
            type="button"
            onClick={() => setQueueOpen(!queueOpen)}
            className="flex items-center gap-2 text-xs font-semibold text-slate-100"
          >
            <span>{queueOpen ? "▾" : "▸"} 传输队列</span>
            <span className="font-normal text-slate-400">
              {runningCount > 0 ? `进行中 ${runningCount} · ` : ""}共 {transfers.length} 条
            </span>
          </button>
          {transfers.some((t) => t.status !== "running") && (
            <button
              type="button"
              className="text-xs text-slate-400 hover:text-slate-100"
              onClick={() => setTransfers((prev) => prev.filter((t) => t.status === "running"))}
            >
              清空已完成
            </button>
          )}
        </div>
        {queueOpen && (
          <div className="max-h-36 overflow-auto">
            {!transfers.length ? (
              <div className="px-4 py-3 text-xs text-slate-400">
                暂无传输任务。选中本机或远程文件后，点击中间「→ / ←」按钮或双击文件即可双向传输（支持整个文件夹）。
              </div>
            ) : (
              <table className="w-full table-fixed text-left text-xs">
                <tbody>
                  {transfers.map((t) => (
                    <tr key={t.id} className="border-t border-slate-700/40">
                      <td className="w-16 pl-4 py-1.5">
                        <span
                          className={`inline-block rounded px-1.5 py-0.5 text-[11px] font-medium ${
                            t.direction === "upload"
                              ? "bg-amber-50 text-amber-700"
                              : "bg-emerald-50 text-emerald-700"
                          }`}
                        >
                          {t.direction === "upload" ? "↑ 上传" : "↓ 下载"}
                        </span>
                      </td>
                      <td className="w-44 truncate pr-2 font-medium text-slate-100" title={t.name}>
                        {t.name}
                      </td>
                      <td
                        className="truncate pr-2 font-mono text-[11px] text-slate-400"
                        title={`${t.local_path} ${t.direction === "upload" ? "→" : "←"} ${t.remote_path}`}
                      >
                        {t.local_path} {t.direction === "upload" ? "→" : "←"} {t.remote_path}
                      </td>
                      <td className="w-24 pr-2 text-right tabular-nums text-slate-400">
                        {fileSize(t.bytes)}
                      </td>
                      <td className="w-24 pr-2 text-right">
                        <span
                          className={
                            t.status === "done"
                              ? "text-emerald-600 font-medium"
                              : t.status === "running"
                                ? "text-amber-600 font-medium"
                                : "text-rose-600"
                          }
                          title={t.error ?? ""}
                        >
                          {({ running: "传输中…", done: "已完成", cancelled: "已取消", failed: "失败", unknown: "状态未知" } as Record<string, string>)[t.status] || t.status}
                        </span>
                      </td>
                      <td className="w-16 pr-4 text-right">
                        {t.status === "running" && (
                          <button
                            type="button"
                            className="text-rose-600 hover:underline"
                            onClick={() => void sftp("cancel_transfer", { task_id: t.id }).catch(() => {})}
                          >
                            取消
                          </button>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        )}
      </footer>

      {/* 文本预览弹窗 */}
      {preview && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-6"
          onClick={() => setPreview(null)}
        >
          <div
            role="dialog"
            aria-modal="true"
            className="flex flex-col w-full max-w-3xl max-h-[82vh] rounded-2xl border border-slate-700 bg-white shadow-2xl overflow-hidden"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-center justify-between gap-3 border-b border-slate-700 bg-[#fafbfc] px-4 py-3">
              <div className="min-w-0">
                <div className="text-xs font-mono text-slate-100 truncate" title={preview.path}>
                  {preview.path}
                </div>
                {preview.truncated && (
                  <div className="text-[11px] text-amber-600">文件较大，仅展示前 128 KB</div>
                )}
              </div>
              <div className="flex items-center gap-2 shrink-0">
                <button
                  type="button"
                  className={smallBtn}
                  onClick={() => {
                    void navigator.clipboard.writeText(preview.content);
                    setBanner({ text: "已复制预览内容" });
                  }}
                >
                  复制全文
                </button>
                <button type="button" className={primaryBtn} onClick={() => setPreview(null)}>
                  关闭
                </button>
              </div>
            </div>
            <pre className="flex-1 min-h-0 overflow-auto p-4 font-mono text-xs leading-relaxed text-slate-100 whitespace-pre select-text">
              {preview.content || "（空文件）"}
            </pre>
          </div>
        </div>
      )}

      {/* 新建文件夹 / 重命名弹窗 */}
      {promptModal && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
          onClick={() => !promptBusy && setPromptModal(null)}
        >
          <form
            role="dialog"
            aria-modal="true"
            className="w-full max-w-sm rounded-xl border border-slate-700 bg-white p-5 space-y-4 shadow-xl"
            onClick={(e) => e.stopPropagation()}
            onSubmit={(e) => {
              e.preventDefault();
              if (!promptValue.trim()) return;
              setPromptBusy(true);
              void promptModal
                .onConfirm(promptValue.trim())
                .then(() => setPromptModal(null))
                .catch((err) => setBanner({ text: String(err), error: true }))
                .finally(() => setPromptBusy(false));
            }}
          >
            <h3 className="text-sm font-semibold text-slate-100">{promptModal.title}</h3>
            <label className="block space-y-1 text-xs text-slate-400">
              <span>{promptModal.label}</span>
              <input
                autoFocus
                required
                className="w-full rounded-lg border border-slate-700 bg-white px-3 py-2 text-sm text-slate-100 focus:border-sky-400 focus:outline-none"
                value={promptValue}
                onChange={(e) => setPromptValue(e.target.value)}
              />
            </label>
            <div className="flex justify-end gap-2">
              <button type="button" className={smallBtn} disabled={promptBusy} onClick={() => setPromptModal(null)}>
                取消
              </button>
              <button type="submit" className={primaryBtn} disabled={promptBusy || !promptValue.trim()}>
                {promptBusy ? "处理中…" : "确定"}
              </button>
            </div>
          </form>
        </div>
      )}
    </div>
  );
}
