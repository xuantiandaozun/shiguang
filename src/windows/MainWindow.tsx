import { useEffect, useState } from "react";
import TodosTab from "../components/TodosTab";
import RulesTab from "../components/RulesTab";
import HistoryTab from "../components/HistoryTab";
import TasksTab from "../components/TasksTab";
import SkillsTab from "../components/SkillsTab";
import UsageTab, { cacheHitRatio, formatPercent, formatTokenCount } from "../components/UsageTab";
import SettingsTab from "../components/SettingsTab";
import appIcon from "../assets/app-icon.png";
import { ipc, onEvent } from "../lib/ipc";
import type { LlmUsageTotals } from "../lib/ipc";

type Tab = "todos" | "rules" | "history" | "tasks" | "skills" | "usage" | "settings";

const TABS: Array<{ key: Tab; label: string }> = [
  { key: "todos", label: "待办事项" },
  { key: "rules", label: "整理规则" },
  { key: "history", label: "操作记录" },
  { key: "tasks", label: "后台任务" },
  { key: "skills", label: "Skills" },
  { key: "usage", label: "用量" },
  { key: "settings", label: "设置" },
];

export default function MainWindow() {
  const [tab, setTab] = useState<Tab>("todos");
  const [today, setToday] = useState<LlmUsageTotals | null>(null);

  useEffect(() => {
    const load = () => {
      ipc
        .getLlmUsageStats()
        .then((s) => setToday(s.today.totals))
        .catch(() => {});
    };
    load();
    let unlisten: (() => void) | undefined;
    onEvent("llm-usage-changed", load).then((u) => (unlisten = u));
    return () => unlisten?.();
  }, []);

  const todayTokens = today?.total_tokens ?? 0;
  const todayRequests = today?.requests ?? 0;
  const todayRatio = today ? cacheHitRatio(today) : null;

  return (
    <div className="h-full flex flex-col text-slate-200">
      <header className="shrink-0 px-5 pt-4 pb-3 border-b border-slate-800 flex items-center justify-between">
        <div className="flex items-center gap-2.5">
          <img src={appIcon} alt="拾光" className="w-7 h-7 rounded-full" draggable={false} />
          <div>
            <div className="text-sm font-semibold text-slate-100">拾光 · AI 桌面助手</div>
            <div className="text-[11px] text-slate-500">关闭此窗口不会退出程序，将最小化到系统托盘</div>
          </div>
        </div>
        {todayRequests > 0 && (
          <button
            onClick={() => setTab("usage")}
            className="text-right rounded-lg px-2.5 py-1.5 hover:bg-slate-800/80 transition"
            title="查看模型用量"
          >
            <div className="text-xs text-slate-200 tabular-nums">
              今日 {formatTokenCount(todayTokens)}
            </div>
            <div className="text-[11px] text-slate-500">
              缓存命中 {formatPercent(todayRatio)}
            </div>
          </button>
        )}
      </header>

      <nav className="shrink-0 flex gap-1 px-5 pt-3 overflow-x-auto">
        {TABS.map((t) => (
          <button
            key={t.key}
            onClick={() => setTab(t.key)}
            className={`px-4 py-2 rounded-t-lg text-sm transition whitespace-nowrap ${
              tab === t.key
                ? "bg-slate-800/80 text-sky-300 font-medium"
                : "text-slate-400 hover:text-slate-200 hover:bg-slate-800/40"
            }`}
          >
            {t.label}
          </button>
        ))}
      </nav>

      <main className="flex-1 overflow-y-auto scrollbar-thin bg-slate-800/50 mx-5 mb-5 rounded-b-lg rounded-tr-lg p-4">
        {tab === "todos" && <TodosTab />}
        {tab === "rules" && <RulesTab />}
        {tab === "history" && <HistoryTab />}
        {tab === "tasks" && <TasksTab />}
        {tab === "skills" && <SkillsTab />}
        {tab === "usage" && <UsageTab />}
        {tab === "settings" && <SettingsTab />}
      </main>
    </div>
  );
}
