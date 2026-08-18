import { useEffect, useState } from "react";
import { ipc, onEvent } from "../lib/ipc";
import type { LlmUsageBySource, LlmUsageDay, LlmUsageRequest, LlmUsageSnapshot } from "../lib/ipc";

type PeriodKey = "today" | "last_7d" | "all";

const PERIODS: Array<{ key: PeriodKey; label: string }> = [
  { key: "today", label: "今日" },
  { key: "last_7d", label: "近 7 天" },
  { key: "all", label: "累计" },
];

const SOURCE_LABEL: Record<string, string> = {
  chat: "对话",
  subagent: "子代理",
  vision: "识图",
};

export function formatTokenCount(n: number): string {
  const abs = Math.abs(n);
  if (abs < 1000) return String(n);
  if (abs < 10_000) return n.toLocaleString("en-US");
  if (abs < 1_000_000) {
    const k = n / 1000;
    return `${k >= 100 ? k.toFixed(0) : k.toFixed(1).replace(/\.0$/, "")}k`;
  }
  return `${(n / 1_000_000).toFixed(2).replace(/\.00$/, "")}M`;
}

export function cacheHitRatio(t: { cache_hit_tokens: number; cache_miss_tokens: number }): number | null {
  const denom = t.cache_hit_tokens + t.cache_miss_tokens;
  if (denom <= 0) return null;
  return t.cache_hit_tokens / denom;
}

export function formatPercent(ratio: number | null): string {
  if (ratio === null) return "—";
  return `${Math.round(ratio * 1000) / 10}%`;
}

export default function UsageTab() {
  const [stats, setStats] = useState<LlmUsageSnapshot | null>(null);
  const [period, setPeriod] = useState<PeriodKey>("today");
  const [error, setError] = useState("");

  const reload = () => {
    ipc
      .getLlmUsageStats()
      .then((s) => {
        setStats(s);
        setError("");
      })
      .catch((e) => setError(String(e)));
  };

  useEffect(() => {
    reload();
    let unlisten: (() => void) | undefined;
    onEvent("llm-usage-changed", reload).then((u) => (unlisten = u));
    return () => unlisten?.();
  }, []);

  if (!stats) {
    return <div className="text-slate-500 text-sm py-8 text-center">{error || "加载中…"}</div>;
  }

  const current = stats[period];
  const totals = current.totals;
  const empty = stats.all.totals.requests === 0;
  const ratio = cacheHitRatio(totals);
  const cacheDenom = totals.cache_hit_tokens + totals.cache_miss_tokens;

  return (
    <div className="space-y-5 max-w-3xl">
      <div>
        <div className="text-sm font-medium text-slate-100">模型用量</div>
        <div className="text-xs text-slate-500 mt-1 leading-5">
          每次调用大模型都会记下输入/输出 token。今日命中率是全天加权平均，第一轮会 miss
          整份工具定义，同一条消息后面几轮才会升高；看下面「最近请求」更准。
        </div>
      </div>

      {error && <div className="text-xs text-rose-400">{error}</div>}

      {empty ? (
        <div className="text-xs text-slate-500 py-6">
          还没有用量记录。开始对话后，这里会显示 token 消耗和缓存命中情况。
        </div>
      ) : (
        <>
          <div className="flex gap-2">
            {PERIODS.map((p) => (
              <button
                key={p.key}
                onClick={() => setPeriod(p.key)}
                className={`px-3 py-1.5 rounded-full text-xs transition ${
                  period === p.key
                    ? "bg-sky-500/20 text-sky-300"
                    : "bg-slate-700/60 text-slate-300 hover:bg-slate-600"
                }`}
              >
                {p.label}
              </button>
            ))}
          </div>

          <div className="grid grid-cols-2 sm:grid-cols-4 gap-2.5">
            <StatCard label="请求次数" value={String(totals.requests)} />
            <StatCard label="输入 token" value={formatTokenCount(totals.prompt_tokens)} hint={totals.prompt_tokens.toLocaleString("en-US")} />
            <StatCard label="输出 token" value={formatTokenCount(totals.completion_tokens)} hint={totals.completion_tokens.toLocaleString("en-US")} />
            <StatCard label="合计" value={formatTokenCount(totals.total_tokens)} hint={totals.total_tokens.toLocaleString("en-US")} />
          </div>

          <section className="rounded-lg border border-slate-700/60 bg-slate-800/50 px-3 py-3 space-y-2">
            <div className="flex items-baseline justify-between gap-3">
              <div className="text-sm text-slate-100">缓存命中</div>
              <div className="text-sm font-medium text-emerald-300">{formatPercent(ratio)}</div>
            </div>
            {ratio === null ? (
              <div className="text-xs text-slate-500">这段时间的接口没有返回缓存数据</div>
            ) : (
              <>
                <div className="h-1.5 rounded-full bg-slate-900 overflow-hidden">
                  <div
                    className="h-full rounded-full bg-emerald-400/80"
                    style={{ width: `${Math.min(100, ratio * 100)}%` }}
                  />
                </div>
                <div className="text-xs text-slate-400">
                  命中 {formatTokenCount(totals.cache_hit_tokens)} · 未命中{" "}
                  {formatTokenCount(totals.cache_miss_tokens)}
                  <span className="text-slate-500">
                    {" "}
                    （{cacheDenom.toLocaleString("en-US")} 输入 token）
                  </span>
                </div>
              </>
            )}
          </section>

          {current.by_source.length > 0 && (
            <section className="space-y-2">
              <div className="text-sm font-medium text-slate-100">来源</div>
              <div className="space-y-1.5">
                {current.by_source.map((s) => (
                  <SourceRow key={s.source} item={s} />
                ))}
              </div>
            </section>
          )}

          {stats.recent.length > 0 && (
            <section className="space-y-2">
              <div className="text-sm font-medium text-slate-100">最近请求</div>
              <div className="space-y-1">
                {stats.recent.map((item) => (
                  <RecentRow key={item.id} item={item} />
                ))}
              </div>
            </section>
          )}

          <section className="space-y-2">
            <div className="text-sm font-medium text-slate-100">近两周</div>
            <div className="space-y-1">
              {stats.daily.map((d) => (
                <DayRow key={d.day} day={d} />
              ))}
            </div>
          </section>
        </>
      )}
    </div>
  );
}

function StatCard({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="rounded-lg border border-slate-700/60 bg-slate-800/50 px-3 py-2.5">
      <div className="text-[11px] text-slate-500">{label}</div>
      <div className="text-lg text-slate-100 mt-0.5 tabular-nums" title={hint}>
        {value}
      </div>
    </div>
  );
}

function SourceRow({ item }: { item: LlmUsageBySource }) {
  const ratio = cacheHitRatio(item);
  return (
    <div className="rounded-lg border border-slate-700/60 bg-slate-800/50 px-3 py-2 flex items-center gap-3">
      <span className="shrink-0 px-1.5 py-0.5 rounded text-[10px] bg-sky-500/15 text-sky-300">
        {SOURCE_LABEL[item.source] ?? item.source}
      </span>
      <span className="text-sm text-slate-100 tabular-nums">{formatTokenCount(item.total_tokens)}</span>
      <span className="text-xs text-slate-500">{item.requests} 次</span>
      <span className="ml-auto text-xs text-slate-400">
        缓存 {formatPercent(ratio)}
      </span>
    </div>
  );
}

function RecentRow({ item }: { item: LlmUsageRequest }) {
  const ratio = cacheHitRatio(item);
  const time = item.created_at.slice(11, 19) || item.created_at;
  return (
    <div className="rounded-lg px-3 py-1.5 flex items-center gap-3 text-sm text-slate-200 bg-slate-800/40">
      <span className="w-14 shrink-0 tabular-nums text-xs text-slate-500">{time}</span>
      <span className="shrink-0 px-1.5 py-0.5 rounded text-[10px] bg-sky-500/15 text-sky-300">
        {SOURCE_LABEL[item.source] ?? item.source}
      </span>
      <span className="flex-1 tabular-nums text-xs text-slate-400">
        入 {formatTokenCount(item.prompt_tokens)} · 出 {formatTokenCount(item.completion_tokens)}
      </span>
      <span className="w-16 text-right text-xs text-emerald-300/90">{formatPercent(ratio)}</span>
    </div>
  );
}

function DayRow({ day }: { day: LlmUsageDay }) {
  const empty = day.requests === 0;
  const ratio = cacheHitRatio(day);
  const label = day.day.slice(5);
  return (
    <div
      className={`rounded-lg px-3 py-1.5 flex items-center gap-3 text-sm ${
        empty ? "text-slate-600" : "text-slate-200 bg-slate-800/40"
      }`}
    >
      <span className="w-12 shrink-0 tabular-nums text-xs text-slate-500">{label}</span>
      <span className="w-14 shrink-0 tabular-nums text-xs">{empty ? "—" : `${day.requests} 次`}</span>
      <span className="flex-1 tabular-nums">{empty ? "" : formatTokenCount(day.total_tokens)}</span>
      <span className="w-20 text-right text-xs text-slate-400">
        {empty ? "" : `缓存 ${formatPercent(ratio)}`}
      </span>
    </div>
  );
}
