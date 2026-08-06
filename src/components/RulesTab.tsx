import { useCallback, useEffect, useState } from "react";
import { ipc, onEvent } from "../lib/ipc";
import type { Rule } from "../lib/ipc";

const MATCH_TYPES: Array<{ key: string; label: string; hint: string }> = [
  { key: "ext", label: "扩展名", hint: "如 pdf, docx, png（逗号分隔多个）" },
  { key: "keyword", label: "关键词", hint: "文件名包含即命中，逗号分隔多个" },
  { key: "regex", label: "正则", hint: "如 ^report-\\d+\\.xlsx$" },
];

export default function RulesTab() {
  const [rules, setRules] = useState<Rule[]>([]);
  const [name, setName] = useState("");
  const [matchType, setMatchType] = useState("ext");
  const [pattern, setPattern] = useState("");
  const [target, setTarget] = useState("");
  const [error, setError] = useState("");

  const reload = useCallback(() => {
    ipc
      .listRules()
      .then(setRules)
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    reload();
    let disposed = false;
    const cleanups: Array<() => void> = [];
    (async () => {
      const u = await onEvent("rules-changed", reload);
      if (disposed) u();
      else cleanups.push(u);
    })();
    return () => {
      disposed = true;
      cleanups.forEach((u) => u());
    };
  }, [reload]);

  const submit = async () => {
    if (!name.trim() || !pattern.trim() || !target.trim()) {
      setError("请填写规则名称、匹配模式和目标文件夹");
      return;
    }
    setError("");
    try {
      await ipc.upsertRule({
        name: name.trim(),
        matchType,
        pattern: pattern.trim(),
        targetFolder: target.trim(),
      });
      setName("");
      setPattern("");
      setTarget("");
      reload();
    } catch (e) {
      setError(String(e));
    }
  };

  const hint = MATCH_TYPES.find((t) => t.key === matchType)?.hint ?? "";

  return (
    <div className="space-y-4">
      <div className="text-xs text-slate-500 leading-5">
        规则命中桌面新文件时会自动移动到目标文件夹。AI 整理方案确认后也可以对 AI 说「以后都按这个规则来」自动沉淀规则。目标文件夹填名称则位于整理根目录下，填绝对路径则直接使用。
      </div>

      <div className="rounded-lg border border-slate-700/60 bg-slate-900/60 p-3 space-y-2.5">
        <div className="text-xs text-slate-400 font-medium">新建规则</div>
        <div className="flex gap-2">
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="规则名，如：图片归集"
            className="flex-1 bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-500"
          />
          <select
            value={matchType}
            onChange={(e) => setMatchType(e.target.value)}
            className="bg-slate-800 text-sm rounded px-2 py-2 border border-slate-700 text-slate-300"
          >
            {MATCH_TYPES.map((t) => (
              <option key={t.key} value={t.key}>
                {t.label}
              </option>
            ))}
          </select>
          <input
            value={pattern}
            onChange={(e) => setPattern(e.target.value)}
            placeholder={hint}
            className="flex-1 bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-500"
          />
          <input
            value={target}
            onChange={(e) => setTarget(e.target.value)}
            placeholder="目标文件夹，如：图片"
            className="flex-1 bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-500"
          />
          <button
            onClick={submit}
            className="px-4 py-2 rounded bg-gradient-to-r from-sky-500 to-indigo-600 text-white text-sm font-medium hover:opacity-90 transition"
          >
            添加
          </button>
        </div>
        {error && <div className="text-xs text-rose-400">{error}</div>}
      </div>

      <div className="space-y-2">
        {rules.length === 0 && <div className="text-center text-slate-500 text-sm py-8">暂无规则</div>}
        {rules.map((r) => (
          <div
            key={r.id}
            className={`flex items-center gap-3 rounded-lg border px-3 py-2.5 ${
              r.enabled ? "border-slate-700/60 bg-slate-900/60" : "border-slate-800 bg-slate-900/40 opacity-60"
            }`}
          >
            <label className="relative inline-flex cursor-pointer items-center">
              <input
                type="checkbox"
                className="peer sr-only"
                checked={r.enabled}
                onChange={(e) => ipc.toggleRule(r.id, e.target.checked).then(reload).catch(() => {})}
              />
              <div className="h-5 w-9 rounded-full bg-slate-700 peer-checked:bg-sky-500 after:absolute after:left-0.5 after:top-0.5 after:h-4 after:w-4 after:rounded-full after:bg-white after:transition peer-checked:after:translate-x-4" />
            </label>
            <div className="flex-1 min-w-0">
              <div className="text-sm text-slate-100">{r.name}</div>
              <div className="text-[11px] text-slate-500 truncate">
                {MATCH_TYPES.find((t) => t.key === r.match_type)?.label ?? r.match_type}：{r.pattern} → {r.target_folder}
              </div>
            </div>
            <span className="text-[11px] px-2 py-0.5 rounded-full bg-emerald-500/15 text-emerald-300">
              {r.approved ? "已审核" : "待审核"}
            </span>
            <button
              onClick={() => ipc.deleteRule(r.id).then(reload).catch(() => {})}
              className="text-slate-500 hover:text-rose-400 text-sm px-1 transition"
              title="删除"
            >
              ✕
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}
