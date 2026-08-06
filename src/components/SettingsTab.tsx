import { useEffect, useState } from "react";
import { ipc, onEvent } from "../lib/ipc";
import type { ProfileEntry, Settings } from "../lib/ipc";

const PRESETS: Array<{ name: string; baseUrl: string; model: string }> = [
  { name: "DeepSeek", baseUrl: "https://api.deepseek.com/v1", model: "deepseek-v4-flash" },
  { name: "通义千问", baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "qwen-plus" },
  { name: "OpenAI", baseUrl: "https://api.openai.com/v1", model: "gpt-4o-mini" },
];

export default function SettingsTab() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    ipc
      .getSettings()
      .then(setSettings)
      .catch((e) => setError(String(e)));
  }, []);

  if (!settings) {
    return <div className="text-slate-500 text-sm py-8 text-center">{error || "加载中…"}</div>;
  }

  const update = (patch: Partial<Settings>) => {
    setSettings((s) => (s ? { ...s, ...patch } : s));
    setSaved(false);
  };

  const save = async () => {
    setError("");
    try {
      await ipc.saveSettings(settings);
      setSaved(true);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="space-y-5 max-w-2xl">
      <section className="space-y-2.5">
        <div className="text-sm font-medium text-slate-100">大模型 API</div>
        <div className="flex gap-2">
          {PRESETS.map((p) => (
            <button
              key={p.name}
              onClick={() => update({ base_url: p.baseUrl, model: p.model })}
              className="px-3 py-1.5 rounded-full text-xs bg-slate-700/60 text-slate-300 hover:bg-slate-600 transition"
            >
              {p.name}
            </button>
          ))}
        </div>
        <label className="block">
          <div className="text-xs text-slate-400 mb-1">Base URL（OpenAI 兼容接口）</div>
          <input
            value={settings.base_url}
            onChange={(e) => update({ base_url: e.target.value })}
            className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
          />
        </label>
        <label className="block">
          <div className="text-xs text-slate-400 mb-1">API Key（仅保存在本机 SQLite，不会上传）</div>
          <input
            type="password"
            value={settings.api_key}
            onChange={(e) => update({ api_key: e.target.value })}
            placeholder="sk-..."
            className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-600"
          />
        </label>
        <label className="block">
          <div className="text-xs text-slate-400 mb-1">模型</div>
          <input
            value={settings.model}
            onChange={(e) => update({ model: e.target.value })}
            className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
          />
        </label>
        <label className="flex items-center gap-2.5 text-sm text-slate-300 cursor-pointer">
          <input
            type="checkbox"
            checked={settings.thinking_enabled}
            onChange={(e) => update({ thinking_enabled: e.target.checked })}
            className="w-4 h-4 accent-sky-500"
          />
          启用思考模式
          <span className="text-xs text-slate-500">先输出思维链再作答，复杂任务更准确；仅 DeepSeek 接口生效</span>
        </label>
        {settings.thinking_enabled && (
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">思考强度</div>
            <select
              value={settings.reasoning_effort}
              onChange={(e) => update({ reasoning_effort: e.target.value })}
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
            >
              <option value="low">low — 最快，适合简单问答</option>
              <option value="high">high — 默认，兼顾速度与质量</option>
              <option value="max">max — 最深思考，慢但更准</option>
            </select>
          </label>
        )}
      </section>

      <section className="space-y-2.5">
        <div className="text-sm font-medium text-slate-100">
          图像识别（视觉模型）
          <span className="ml-2 text-xs font-normal text-slate-500">
            独立配置，与上方聊天模型互不影响；用于看图、读截图、提取图中文字
          </span>
        </div>
        <label className="block">
          <div className="text-xs text-slate-400 mb-1">Base URL（OpenAI 兼容接口）</div>
          <input
            value={settings.vision_base_url}
            onChange={(e) => update({ vision_base_url: e.target.value })}
            placeholder="https://dashscope.aliyuncs.com/compatible-mode/v1"
            className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-600"
          />
        </label>
        <label className="block">
          <div className="text-xs text-slate-400 mb-1">视觉模型 API Key（与聊天 Key 分开，仅保存在本机）</div>
          <input
            type="password"
            value={settings.vision_api_key}
            onChange={(e) => update({ vision_api_key: e.target.value })}
            placeholder="sk-..."
            className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-600"
          />
        </label>
        <label className="block">
          <div className="text-xs text-slate-400 mb-1">视觉模型</div>
          <input
            value={settings.vision_model}
            onChange={(e) => update({ vision_model: e.target.value })}
            placeholder="qwen-vl-max"
            className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-600"
          />
        </label>
      </section>

      <section className="space-y-2.5">
        <div className="text-sm font-medium text-slate-100">
          子代理
          <span className="ml-2 text-xs font-normal text-slate-500">
            主代理可把「读一堆材料、做多步分析」的子任务委托给子代理；它独立工作，只把最终结论带回对话
          </span>
        </div>
        <label className="flex items-center gap-2.5 text-sm text-slate-300 cursor-pointer">
          <input
            type="checkbox"
            checked={settings.subagent_thinking_enabled}
            onChange={(e) => update({ subagent_thinking_enabled: e.target.checked })}
            className="w-4 h-4 accent-sky-500"
          />
          子代理启用思考模式
          <span className="text-xs text-slate-500">默认关闭：子任务通常较简单，关掉更快更省；仅 DeepSeek 接口生效</span>
        </label>
        {settings.subagent_thinking_enabled && (
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">子代理思考强度</div>
            <select
              value={settings.subagent_reasoning_effort}
              onChange={(e) => update({ subagent_reasoning_effort: e.target.value })}
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
            >
              <option value="low">low — 最快，子代理推荐</option>
              <option value="high">high — 兼顾速度与质量</option>
              <option value="max">max — 最深思考</option>
            </select>
          </label>
        )}
        <label className="block">
          <div className="text-xs text-slate-400 mb-1">子代理模型（留空则跟随主模型）</div>
          <input
            value={settings.subagent_model}
            onChange={(e) => update({ subagent_model: e.target.value })}
            placeholder="如 deepseek-v4-flash；可填更便宜的模型专门跑子任务"
            className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-600"
          />
        </label>
      </section>

      <section className="space-y-2.5">
        <div className="text-sm font-medium text-slate-100">
          后台任务与命令执行
          <span className="ml-2 text-xs font-normal text-slate-500">
            允许 AI 执行 Windows 命令：耗时任务在后台运行，输出写入日志文件，只按需取片段进对话
          </span>
        </div>
        <label className="flex items-center gap-2.5 text-sm text-slate-300 cursor-pointer">
          <input
            type="checkbox"
            checked={settings.command_tools_enabled}
            onChange={(e) => update({ command_tools_enabled: e.target.checked })}
            className="w-4 h-4 accent-sky-500"
          />
          允许 AI 执行命令行
          <span className="text-xs text-slate-500">关闭后 run_command / 后台任务类工具不可用</span>
        </label>
      </section>

      <section className="space-y-2.5">
        <div className="text-sm font-medium text-slate-100">
          个人信息 · 基本资料
          <span className="ml-2 text-xs font-normal text-slate-500">
            固定字段；仅求职/发帖等需要本人信息的场景才会提供给 AI，平时不加载
          </span>
        </div>
        <div className="grid grid-cols-2 gap-2.5">
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">真实姓名（仅招聘等实名场景使用）</div>
            <input
              value={settings.profile_name}
              onChange={(e) => update({ profile_name: e.target.value })}
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
            />
          </label>
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">自媒体号名称（对外默认使用）</div>
            <input
              value={settings.profile_alias}
              onChange={(e) => update({ profile_alias: e.target.value })}
              placeholder="发帖/署名/网站资料用"
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-600"
            />
          </label>
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">性别</div>
            <select
              value={settings.profile_gender}
              onChange={(e) => update({ profile_gender: e.target.value })}
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
            >
              <option value="">未填写</option>
              <option value="男">男</option>
              <option value="女">女</option>
              <option value="其他">其他</option>
            </select>
          </label>
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">出生年月</div>
            <input
              value={settings.profile_birth}
              onChange={(e) => update({ profile_birth: e.target.value })}
              placeholder="如 1995-06"
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-600"
            />
          </label>
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">所在城市</div>
            <input
              value={settings.profile_city}
              onChange={(e) => update({ profile_city: e.target.value })}
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
            />
          </label>
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">手机</div>
            <input
              value={settings.profile_phone}
              onChange={(e) => update({ profile_phone: e.target.value })}
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
            />
          </label>
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">邮箱</div>
            <input
              value={settings.profile_email}
              onChange={(e) => update({ profile_email: e.target.value })}
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
            />
          </label>
        </div>
      </section>

      <ProfileSection />

      <section className="space-y-2.5">
        <div className="text-sm font-medium text-slate-100">桌面整理</div>
        <div className="text-xs text-slate-500">桌面路径：{settings.desktop_path}</div>
        <label className="block">
          <div className="text-xs text-slate-400 mb-1">整理根目录（分类文件夹直接建在这里，默认桌面顶层）</div>
          <input
            value={settings.organize_root}
            onChange={(e) => update({ organize_root: e.target.value })}
            className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
          />
        </label>
        <label className="flex items-center gap-2.5 text-sm text-slate-300 cursor-pointer">
          <input
            type="checkbox"
            checked={settings.auto_organize}
            onChange={(e) => update({ auto_organize: e.target.checked })}
            className="w-4 h-4 accent-sky-500"
          />
          启用自动整理（命中已审核规则的桌面新文件自动归类）
        </label>
      </section>

      <section className="space-y-2.5">
        <div className="text-sm font-medium text-slate-100">系统</div>
        <label className="flex items-center gap-2.5 text-sm text-slate-300 cursor-pointer">
          <input
            type="checkbox"
            checked={settings.autostart}
            onChange={(e) => update({ autostart: e.target.checked })}
            className="w-4 h-4 accent-sky-500"
          />
          开机自动启动
        </label>
      </section>

      <TempCleanupSection tempPath={settings.temp_path} />

      <div className="flex items-center gap-3 pt-1">
        <button
          onClick={save}
          className="px-5 py-2 rounded bg-gradient-to-r from-sky-500 to-indigo-600 text-white text-sm font-medium hover:opacity-90 transition"
        >
          保存设置
        </button>
        {saved && <span className="text-xs text-emerald-400">已保存</span>}
        {error && <span className="text-xs text-rose-400">{error}</span>}
      </div>
    </div>
  );
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function TempCleanupSection({ tempPath }: { tempPath: string }) {
  const [info, setInfo] = useState<{
    file_count: number;
    dir_count: number;
    total_bytes: number;
  } | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState("");
  const [err, setErr] = useState("");

  const refresh = () => {
    ipc
      .getTempInfo()
      .then((t) => {
        setInfo({
          file_count: t.file_count,
          dir_count: t.dir_count,
          total_bytes: t.total_bytes,
        });
        setErr("");
      })
      .catch((e) => setErr(String(e)));
  };

  useEffect(() => {
    refresh();
  }, []);

  const clear = async () => {
    if (busy) return;
    const total = (info?.file_count ?? 0) + (info?.dir_count ?? 0);
    if (total === 0) {
      setMsg("临时目录已是空的");
      return;
    }
    if (!window.confirm(`确定删除临时目录中的全部 ${info?.file_count ?? 0} 个文件？此操作不可恢复。`)) {
      return;
    }
    setBusy(true);
    setMsg("");
    setErr("");
    try {
      const before = await ipc.clearTempDir();
      setMsg(
        `已清理 ${before.file_count} 个文件` +
          (before.dir_count ? `、${before.dir_count} 个文件夹` : "") +
          `，释放 ${formatBytes(before.total_bytes)}`
      );
      refresh();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="space-y-2.5">
      <div className="text-sm font-medium text-slate-100">
        临时文件
        <span className="ml-2 text-xs font-normal text-slate-500">
          AI 草稿与中间产物只写在这里，不会堆到桌面；任务结束后也会询问是否清理
        </span>
      </div>
      <div className="text-xs text-slate-500 break-all">目录：{tempPath}</div>
      <div className="text-xs text-slate-400">
        {info
          ? `占用 ${formatBytes(info.total_bytes)} · ${info.file_count} 个文件` +
            (info.dir_count ? ` · ${info.dir_count} 个文件夹` : "")
          : "统计加载中…"}
      </div>
      <div className="flex items-center gap-3">
        <button
          onClick={clear}
          disabled={busy || ((info?.file_count ?? 0) + (info?.dir_count ?? 0) === 0)}
          className="px-3.5 py-1.5 rounded bg-slate-700 text-slate-200 text-sm hover:bg-slate-600 disabled:opacity-40 transition"
        >
          {busy ? "清理中…" : "清理临时目录"}
        </button>
        <button
          onClick={refresh}
          disabled={busy}
          className="px-3 py-1.5 rounded text-slate-400 text-sm hover:text-slate-200 transition"
        >
          刷新
        </button>
        {msg && <span className="text-xs text-emerald-400">{msg}</span>}
        {err && <span className="text-xs text-rose-400">{err}</span>}
      </div>
    </section>
  );
}

function ProfileSection() {
  const [entries, setEntries] = useState<ProfileEntry[]>([]);
  const [label, setLabel] = useState("");
  const [content, setContent] = useState("");
  const [editId, setEditId] = useState<number | null>(null);

  const refresh = () => {
    ipc.listProfileEntries().then(setEntries).catch(() => {});
  };

  useEffect(() => {
    refresh();
    let unlisten: (() => void) | undefined;
    onEvent("profile-changed", refresh).then((u) => (unlisten = u));
    return () => unlisten?.();
  }, []);

  const reset = () => {
    setLabel("");
    setContent("");
    setEditId(null);
  };

  const save = async () => {
    if (!label.trim() || !content.trim()) return;
    try {
      await ipc.saveProfileEntry(editId, label.trim(), content.trim());
      reset();
      refresh();
    } catch {}
  };

  const startEdit = (e: ProfileEntry) => {
    setEditId(e.id);
    setLabel(e.label);
    setContent(e.content);
  };

  const remove = async (id: number) => {
    try {
      await ipc.deleteProfileEntry(id);
      if (editId === id) reset();
      refresh();
    } catch {}
  };

  return (
    <section className="space-y-2.5">
      <div className="text-sm font-medium text-slate-100">
        个人信息 · 补充资料
        <span className="ml-2 text-xs font-normal text-slate-500">
          自由条目，AI 聊天时会自动保存维护（工作经历、自媒体号、项目描述等），也可手动管理
        </span>
      </div>
      {entries.length === 0 ? (
        <div className="text-xs text-slate-500">
          暂无条目。和 AI 聊天时透露的经历、账号等信息会自动沉淀到这里。
        </div>
      ) : (
        <div className="space-y-1.5">
          {entries.map((e) => (
            <div
              key={e.id}
              className="rounded-lg border border-slate-700/60 bg-slate-800/50 px-3 py-2"
            >
              <div className="flex items-center gap-2">
                <span className="shrink-0 px-1.5 py-0.5 rounded text-[10px] bg-indigo-500/15 text-indigo-300">
                  {e.label}
                </span>
                <span className="text-sm text-slate-100 truncate">{e.content}</span>
                <span className="ml-auto shrink-0 text-[11px] text-slate-500">
                  {e.updated_at.slice(0, 10)}
                </span>
                <button
                  onClick={() => startEdit(e)}
                  className="shrink-0 text-slate-500 hover:text-sky-400 text-xs px-1 transition"
                  title="编辑该条目"
                >
                  编辑
                </button>
                <button
                  onClick={() => remove(e.id)}
                  className="shrink-0 text-slate-500 hover:text-rose-400 text-xs px-1 transition"
                  title="删除该条目"
                >
                  删除
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
      <div className="rounded-lg border border-slate-700/60 bg-slate-800/30 p-2.5 space-y-2">
        <div className="text-xs text-slate-400">{editId ? "编辑条目" : "手动新增条目"}</div>
        <div className="flex gap-2">
          <input
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            placeholder="条目名，如 工作经历"
            className="w-44 shrink-0 bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-600"
          />
          <input
            value={content}
            onChange={(e) => setContent(e.target.value)}
            placeholder="具体内容"
            className="flex-1 bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-600"
          />
          <button
            onClick={save}
            disabled={!label.trim() || !content.trim()}
            className="shrink-0 px-3.5 py-2 rounded bg-slate-700 text-slate-200 text-sm hover:bg-slate-600 disabled:opacity-40 transition"
          >
            {editId ? "保存" : "添加"}
          </button>
          {editId !== null && (
            <button
              onClick={reset}
              className="shrink-0 px-3 py-2 rounded text-slate-400 text-sm hover:text-slate-200 transition"
            >
              取消
            </button>
          )}
        </div>
      </div>
    </section>
  );
}
