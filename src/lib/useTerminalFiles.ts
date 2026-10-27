import { useCallback, useEffect, useRef, useState } from "react";
import { ipc } from "./ipc";

export interface RemoteEntry {
  name: string;
  stat: { size: number | null; mtime: number | null; permissions: number | null; is_dir: boolean; is_file: boolean };
}
export interface RemoteListing { path: string; entries: RemoteEntry[]; truncated: boolean }
export interface UploadSpec { local_path: string; remote_path: string; overwrite: boolean; exists?: boolean; sensitive?: boolean; is_dir?: boolean }
export interface TransferTask { id: string; status: string; bytes: number; error: string | null; local_path: string; remote_path: string }
export interface TerminalAiMemory { summary: string; related_paths: string[]; working_directory?: string }
export interface TerminalAiStep { command: string; message: string; sensitive: boolean; risk: string; upload?: UploadSpec; memory?: TerminalAiMemory }

export function useTerminalFiles(serverId: string, connected: boolean) {
  const [listing, setListing] = useState<RemoteListing>({ path: ".", entries: [], truncated: false });
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const initialized = useRef(false);
  const refreshAgain = useRef(false);
  const pending = useRef<{ target: string; promise: Promise<void> }>();
  const [error, setError] = useState("");
  const [transfers, setTransfers] = useState<TransferTask[]>([]);
  const path = useRef(".");
  const sequence = useRef(0);
  const alive = useRef(true);
  const call = useCallback(<T,>(action: string, data?: string) => ipc.serverTerminal<T>(action, serverId, { data }), [serverId]);
  useEffect(() => { alive.current = true; return () => { alive.current = false; sequence.current++; pending.current = undefined; refreshAgain.current = false; }; }, []);
  const refresh = useCallback(async (target?: string): Promise<void> => {
    const requested = target ?? path.current;
    // Background updates must not interrupt a user-initiated directory change.
    // Repeated updates to the same directory share the pending request.
    if (pending.current && (target === undefined || pending.current.target === requested)) {
      if (target === undefined) refreshAgain.current = true;
      return pending.current.promise;
    }
    const version = ++sequence.current;
    setLoading(!initialized.current || requested !== path.current);
    setRefreshing(true); setError("");
    const request = (async () => {
      try {
        const result = await call<RemoteListing>("files", requested);
        if (alive.current && version === sequence.current) {
          result.entries.sort((a, b) => Number(b.stat.is_dir) - Number(a.stat.is_dir) || a.name.localeCompare(b.name));
          path.current = result.path; initialized.current = true;
          setListing(result);
        }
      } catch (e) { if (alive.current && version === sequence.current) setError(String(e)); }
      finally {
        if (alive.current && version === sequence.current) {
          pending.current = undefined; setLoading(false); setRefreshing(false);
          if (refreshAgain.current) { refreshAgain.current = false; void refresh(); }
        }
      }
    })();
    pending.current = { target: requested, promise: request };
    return request;
  }, [call]);
  useEffect(() => { if (connected) void refresh(); }, [connected, refresh]);
  const prepare = (spec: UploadSpec) => call<UploadSpec>("prepare_upload", JSON.stringify(spec));
  const cancel = async (id: string) => {
    const result = await call<{ task: Omit<TransferTask, "local_path" | "remote_path"> }>("cancel_transfer", id);
    if (alive.current) setTransfers(items => items.map(t => t.id === id ? { ...t, ...result.task } : t));
  };
  const upload = async (spec: UploadSpec, onStart?: (id: string) => void) => {
    const started = await call<{ task_id: string }>("upload", JSON.stringify(spec));
    const id = started.task_id;
    onStart?.(id);
    const initial: TransferTask = { id, local_path: spec.local_path, remote_path: spec.remote_path, status: "running", bytes: 0, error: null };
    if (alive.current) setTransfers(items => [initial, ...items.filter(t => t.status === "running" || t.status === "unknown"), ...items.filter(t => t.status !== "running" && t.status !== "unknown").slice(0, 19)]);
    while (alive.current) {
      let task: TransferTask;
      try {
        const result = await call<{ task: Omit<TransferTask, "local_path" | "remote_path"> }>("transfer", id);
        task = { ...initial, ...result.task };
      } catch (e) {
        if (alive.current) setTransfers(items => items.map(t => t.id === id ? { ...t, status: "unknown", error: `无法获取状态：${String(e)}，请核实后再重试` } : t));
        throw e;
      }
      if (alive.current) setTransfers(items => items.map(t => t.id === id ? task : t));
      if (task.status !== "running") {
        void refresh();
        if (task.status !== "done") throw new Error(task.error || (task.status === "cancelled" ? "上传已取消，已传输文件可能仍然保留" : "上传未完成"));
        return task;
      }
      await new Promise(resolve => setTimeout(resolve, 700));
    }
    throw new Error("窗口已关闭，上传可能仍在继续");
  };
  return { listing, loading, refreshing, error, transfers, refresh, prepare, upload, cancel, call };
}
export type TerminalFiles = ReturnType<typeof useTerminalFiles>;
export const remoteJoin = (directory: string, name: string) => `${directory.replace(/\/$/, "")}/${name}`;
export function fileSize(bytes: number | null) {
  if (bytes === null) return "—";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1048576) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1073741824) return `${(bytes / 1048576).toFixed(1)} MB`;
  return `${(bytes / 1073741824).toFixed(1)} GB`;
}
