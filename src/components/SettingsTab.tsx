import { ProfileModelSettings, SubagentSettings } from "./ModelProfileSettings";
import { useEffect, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { ipc, onEvent } from "../lib/ipc";
import type { LlmProfile, ProfileEntry, Settings } from "../lib/ipc";
import {
  CUSTOM_CATALOG_MODEL,
  PROVIDER_GROUPS,
  PROVIDER_PRESETS,
  listedProviderPresets,
  applyProviderPreset,
  bindingSummary,
  catalogByGroup,
  catalogForProvider,
  catalogOptionLabel,
  catalogSupportsVision,
  catalogContextWindow,
  catalogVisionModels,
  inferCatalogModel,
  llmProfileStatus,
  llmProfileStatusLabel,
  migrateSettingsProfiles,
  reconcileSubagentRoles,
  patchProfileCatalog,
  presetByBaseUrl,
  profileCatalogLabel,
  profileCatalogModel,
  providerByBaseUrl,
  syncProfileModel,
  type CatalogModel,
  type ProviderPreset,
} from "../lib/models";

const SELECT_CLASS =
  "w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500";
const INPUT_CLASS =
  "w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-600";

function uniqueProfileName(base: string, profiles: LlmProfile[]): string {
  if (!profiles.some((p) => p.name === base)) return base;
  let i = 2;
  while (profiles.some((p) => p.name === `${base} ${i}`)) i += 1;
  return `${base} ${i}`;
}

function newProfileId(): string {
  return typeof crypto !== "undefined" && crypto.randomUUID
    ? crypto.randomUUID()
    : `lp-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

function emptyProfile(name = "自定义"): LlmProfile {
  return {
    id: newProfileId(),
    name,
    base_url: "",
    api_key: "",
    catalog_model: "",
    model: "",
    thinking_enabled: false,
    reasoning_effort: "high",
    supports_vision: null,
  };
}

function profileSupportsVision(profile: LlmProfile): boolean {
  return profileCatalogModel(profile) === CUSTOM_CATALOG_MODEL
    ? profile.supports_vision ?? catalogSupportsVision(CUSTOM_CATALOG_MODEL, profile.model)
    : catalogSupportsVision(profileCatalogModel(profile), profile.model);
}

function formatContextWindow(value: number): string {
  if (value >= 1_000_000 && value % 1_000_000 === 0) return `${value / 1_000_000}m`;
  if (value >= 1_000 && value % 1_000 === 0) return `${value / 1_000}k`;
  return String(value);
}

function parseContextWindow(value: string): number | null {
  const match = value.trim().toLowerCase().match(/^(\d+(?:\.\d+)?)\s*([km]?)$/);
  if (!match) return null;
  const multiplier = match[2] === "m" ? 1_000_000 : match[2] === "k" ? 1_000 : 1;
  const parsed = Math.round(Number(match[1]) * multiplier);
  return Number.isFinite(parsed) && parsed >= 16_000 && parsed <= 20_000_000 ? parsed : null;
}

function ContextWindowInput({ value, disabled, onChange }: { value: number; disabled?: boolean; onChange: (value: number) => void }) {
  const [text, setText] = useState(() => formatContextWindow(value));
  const [invalid, setInvalid] = useState(false);
  useEffect(() => setText(formatContextWindow(value)), [value]);
  const commit = () => {
    const parsed = parseContextWindow(text);
    if (parsed === null) { setInvalid(true); return; }
    setInvalid(false);
    setText(formatContextWindow(parsed));
    onChange(parsed);
  };
  return <>
    <input type="text" inputMode="decimal" value={text} disabled={disabled}
      onChange={(e) => { setText(e.target.value); setInvalid(false); }}
      onBlur={commit} onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }}
      placeholder="例如 256k、1m" aria-invalid={invalid}
      className={`${INPUT_CLASS} disabled:opacity-60 disabled:cursor-not-allowed ${invalid ? "border-rose-500" : ""}`} />
    {invalid && <div className="text-[11px] mt-1 text-rose-400">请输入 16k～20m，例如 128k、256k 或 1m</div>}
  </>;
}

function CatalogModelSelect({
  catalogId,
  baseUrl,
  apiModel,
  onChange,
  allowEmpty,
  emptyLabel = "跟随主模型",
  visionOnly,
}: {
  catalogId: string;
  baseUrl: string;
  apiModel: string;
  onChange: (catalogId: string, apiModel: string) => void;
  allowEmpty?: boolean;
  emptyLabel?: string;
  visionOnly?: boolean;
}) {
  const provider = providerByBaseUrl(baseUrl);
  const pool = visionOnly ? catalogVisionModels() : catalogForProvider(provider?.id);
  const knownIds = new Set(pool.map((m) => m.id));
  const effectiveId =
    allowEmpty && !catalogId
      ? ""
      : catalogId === CUSTOM_CATALOG_MODEL || !knownIds.has(catalogId)
        ? CUSTOM_CATALOG_MODEL
        : catalogId;
  const groups = catalogByGroup();

  return (
    <div className="space-y-1.5">
      <select
        value={effectiveId}
        onChange={(e) => {
          const next = e.target.value;
          if (next === "") {
            onChange("", "");
            return;
          }
          if (next === CUSTOM_CATALOG_MODEL) {
            onChange(CUSTOM_CATALOG_MODEL, apiModel);
            return;
          }
          const resolved = patchProfileCatalog(
            { id: "", name: "", base_url: baseUrl, api_key: "", catalog_model: next, model: "", thinking_enabled: false, reasoning_effort: "high" },
            next
          );
          onChange(next, resolved.model);
        }}
        className={SELECT_CLASS}
      >
        {allowEmpty && <option value="">{emptyLabel}</option>}
        {(visionOnly ? [["视觉", pool] as const] : (Object.entries(groups) as [string, CatalogModel[]][])).map(
          ([group, items]) => {
            const list = visionOnly ? pool : items.filter((m) => knownIds.has(m.id));
            if (!list.length) return null;
            return (
              <optgroup key={group} label={group}>
                {list.map((m) => (
                  <option key={m.id} value={m.id}>
                    {catalogOptionLabel(m, provider?.id)}
                  </option>
                ))}
              </optgroup>
            );
          }
        )}
        <option value={CUSTOM_CATALOG_MODEL}>自定义 API 模型名…</option>
      </select>
      {effectiveId === CUSTOM_CATALOG_MODEL && (
        <input
          value={apiModel}
          onChange={(e) => onChange(CUSTOM_CATALOG_MODEL, e.target.value)}
          placeholder="输入该供应商要求的 API 模型名"
          className={INPUT_CLASS}
        />
      )}
      {effectiveId && effectiveId !== CUSTOM_CATALOG_MODEL && provider && (
        <div className="text-[11px] text-slate-500">
          当前供应商 API 名：
          <span className="text-slate-400 font-mono ml-1">
            {patchProfileCatalog(
              { id: "", name: "", base_url: baseUrl, api_key: "", catalog_model: effectiveId, model: "", thinking_enabled: false, reasoning_effort: "high" },
              effectiveId
            ).model}
          </span>
        </div>
      )}
    </div>
  );
}

function ModelCatalogPanel() {
  const [open, setOpen] = useState(false);
  const groups = catalogByGroup();

  return (
    <div className="rounded-lg border border-slate-700/50 overflow-hidden">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="w-full flex items-center justify-between px-3 py-2 text-xs text-slate-400 hover:text-slate-200 hover:bg-slate-800/50 transition"
      >
        <span>统一模型目录（各供应商 API 名对照）</span>
        <span>{open ? "收起" : "展开"}</span>
      </button>
      {open && (
        <div className="border-t border-slate-700/50 max-h-64 overflow-y-auto text-[11px]">
          {(Object.entries(groups) as [string, CatalogModel[]][]).map(([group, items]) => (
            <div key={group}>
              <div className="sticky top-0 px-3 py-1 bg-slate-900/95 text-slate-500">{group}</div>
              {items.map((m) => (
                <div
                  key={m.id}
                  className="px-3 py-1.5 border-b border-slate-800/80 hover:bg-slate-800/30"
                >
                  <div className="text-slate-200">{m.label}</div>
                  <div className="text-slate-500 font-mono mt-0.5">{m.id}</div>
                  <div className="text-slate-600 mt-0.5 leading-4">{bindingSummary(m)}</div>
                </div>
              ))}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export default function SettingsTab() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    ipc
      .getSettings()
      .then((s) => {
        const profiles = migrateSettingsProfiles(s.llm_profiles ?? []);
        const active = profiles.find((p) => p.id === s.active_llm_profile_id);
        const automaticWindow = active
          ? active.models?.find((m) => m.model === active.model)?.context_window ?? catalogContextWindow(profileCatalogModel(active), active.model)
          : undefined;
        setSettings({
          ...s,
          llm_profiles: profiles,
          active_llm_profile_id: s.active_llm_profile_id ?? "",
          context_window: automaticWindow ?? s.context_window,
          vision_catalog_model:
            s.vision_catalog_model ||
            (s.vision_model
              ? inferCatalogModel(s.vision_model, s.vision_base_url)
              : ""),
          subagent_catalog_model:
            s.subagent_catalog_model ||
            (s.subagent_model ? inferCatalogModel(s.subagent_model, s.base_url) : ""),
        });
      })
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

  const chatCatalogId =
    settings.llm_profiles?.find((p) => p.id === settings.active_llm_profile_id)?.catalog_model ||
    profileCatalogModel({
      id: "",
      name: "",
      base_url: settings.base_url,
      api_key: settings.api_key,
      catalog_model: "",
      model: settings.model,
      thinking_enabled: false,
      reasoning_effort: "high",
    });
  const activeChatProfile = settings.llm_profiles?.find((p) => p.id === settings.active_llm_profile_id);
  const chatSeesImages = activeChatProfile
    ? profileSupportsVision(activeChatProfile)
    : catalogSupportsVision(chatCatalogId, settings.model);

  return (
    <div className="space-y-5 max-w-2xl">
      <LlmProfilesSection
        settings={settings}
        update={update}
        saved={saved}
        onSave={save}
        onActivate={async (next) => {
          setError("");
          try {
            await ipc.saveSettings(next);
            setSettings(next);
            setSaved(true);
          } catch (e) {
            setError(String(e));
          }
        }}
      />

      <section className="space-y-2.5 rounded-xl border border-sky-500/20 bg-sky-500/[0.04] p-4">
        <div className="text-sm font-medium text-slate-100">
          智能加速
          <span className="ml-2 text-xs font-normal text-slate-500">
            用高速结构化判断辅助浏览器和上下文管理；复杂页面仍由主模型补充候选，再交给加速模型复核
          </span>
        </div>
        <label className="flex items-center gap-2.5 text-sm text-slate-300 cursor-pointer">
          <input
            type="checkbox"
            checked={settings.system_one_enabled}
            onChange={(e) => update({ system_one_enabled: e.target.checked })}
            className="w-4 h-4 accent-sky-500"
          />
          启用 TypeSafe System One
        </label>
        {settings.system_one_enabled && (
          <div className="space-y-2.5 border-l border-slate-700 pl-3">
            <label className="block">
              <div className="text-xs text-slate-400 mb-1">API 地址</div>
              <input value={settings.system_one_base_url} onChange={(e) => update({ system_one_base_url: e.target.value })} className={INPUT_CLASS} />
            </label>
            <label className="block">
              <div className="text-xs text-slate-400 mb-1">API Key（仅保存在本机）</div>
              <input type="password" value={settings.system_one_api_key} onChange={(e) => update({ system_one_api_key: e.target.value })} placeholder="输入 TypeSafe API Key" className={INPUT_CLASS} />
            </label>
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-2.5">
              <label className="block">
                <div className="text-xs text-slate-400 mb-1">模型</div>
                <input value={settings.system_one_model} onChange={(e) => update({ system_one_model: e.target.value })} placeholder="jev-latest" className={INPUT_CLASS} />
              </label>
              <label className="block">
                <div className="text-xs text-slate-400 mb-1">自动采纳最低置信度</div>
                <input type="number" min="0.5" max="1" step="0.05" value={settings.system_one_min_confidence} onChange={(e) => update({ system_one_min_confidence: Number(e.target.value) })} className={INPUT_CLASS} />
              </label>
            </div>
            <label className="flex items-center gap-2 text-sm text-slate-300 cursor-pointer">
              <input type="checkbox" checked={settings.system_one_browser_assist} onChange={(e) => update({ system_one_browser_assist: e.target.checked })} className="w-4 h-4 accent-sky-500" />
              浏览器动作持续复核
            </label>
            <label className="flex items-center gap-2 text-sm text-slate-300 cursor-pointer">
              <input type="checkbox" checked={settings.system_one_context_assist} onChange={(e) => update({ system_one_context_assist: e.target.checked })} className="w-4 h-4 accent-sky-500" />
              上下文智能保留（按内容决定近期原文保留量）
            </label>
          </div>
        )}
      </section>

      <section className="space-y-2.5">
        <div className="text-sm font-medium text-slate-100">
          上下文与压缩
          <span className="ml-2 text-xs font-normal text-slate-500">按模型窗口的 80% 动态触发，不再使用固定 64KB 阈值</span>
        </div>
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-2.5">
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">主模型上下文窗口（Token）</div>
            {(() => {
              const current = settings.llm_profiles.find((p) => p.id === settings.active_llm_profile_id);
              const automatic = current?.models?.find((m) => m.model === current.model)?.context_window;
              return <>
                <ContextWindowInput value={automatic ?? settings.context_window} disabled={Boolean(automatic)} onChange={(context_window) => update({ context_window })} />
                <div className="text-[11px] mt-1 text-slate-500">
                  {automatic ? `已由统一模型目录自动维护：${formatContextWindow(automatic)}` : "支持 128k、256k、1m 等写法，保存时自动换算为 Token"}
                </div>
              </>;
            })()}
          </label>
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">摘要模型（留空跟随主模型）</div>
            <input value={settings.compact_model} onChange={(e) => update({ compact_model: e.target.value })} placeholder="留空跟随主模型" className={INPUT_CLASS} />
          </label>
        </div>
      </section>

      <section className="space-y-2.5">
        <div className="text-sm font-medium text-slate-100">
          图像识别（视觉模型）
          <span className="ml-2 text-xs font-normal text-slate-500">
            {chatSeesImages
              ? "当前聊天模型已能直接看图，附件和截图会发给它；OCR 仍可用于只要文字、省 token。下方视觉接口仅在主模型不能看图时使用"
              : "主模型不能看图时，read_image 会调用下面这套视觉接口；OCR 始终可用"}
          </span>
        </div>
        <label className="block">
          <div className="text-xs text-slate-400 mb-1">Base URL（OpenAI 兼容接口）</div>
          <input
            value={settings.vision_base_url}
            onChange={(e) => update({ vision_base_url: e.target.value })}
            placeholder="https://dashscope.aliyuncs.com/compatible-mode/v1"
            className={INPUT_CLASS}
          />
        </label>
        <label className="block">
          <div className="text-xs text-slate-400 mb-1">视觉模型 API Key（与聊天 Key 分开，仅保存在本机）</div>
          <input
            type="password"
            value={settings.vision_api_key}
            onChange={(e) => update({ vision_api_key: e.target.value })}
            placeholder="sk-..."
            className={INPUT_CLASS}
          />
        </label>
        <label className="block">
          <div className="text-xs text-slate-400 mb-1">视觉模型</div>
          <CatalogModelSelect
            catalogId={settings.vision_catalog_model || ""}
            baseUrl={settings.vision_base_url}
            apiModel={settings.vision_model}
            onChange={(catalogId, apiModel) =>
              update({
                vision_catalog_model: catalogId,
                vision_model: apiModel,
              })
            }
            visionOnly
          />
        </label>
      </section>

      <SubagentSettings settings={settings} update={update} />

      <section className="space-y-2.5">
        <div className="text-sm font-medium text-slate-100">AI 操作权限</div>
        <div className="grid grid-cols-1 gap-2">
          {[
            ["confirmation", "谨慎", "每次执行有副作用的操作前都先询问。"],
            ["balanced", "平衡", "普通操作直接完成；发送、删除、覆盖、提权等敏感操作才确认。"],
            ["autopilot", "高效", "非危险操作不再打断；危险、不可逆和提权操作仍会确认。"],
          ].map(([value, label, hint]) => (
            <button
              key={value}
              onClick={() => update({ permission_level: value })}
              className={`text-left rounded-lg border px-3 py-2.5 transition ${
                settings.permission_level === value
                  ? "border-sky-500/70 bg-sky-500/10"
                  : "border-slate-700/60 bg-slate-900/40 hover:border-slate-600"
              }`}
            >
              <div className="text-sm text-slate-200">{label}</div>
              <div className="mt-0.5 text-xs text-slate-500">{hint}</div>
            </button>
          ))}
        </div>
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

      <ManualUpdateSection />

      <BrowserExtensionSection />

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

function ManualUpdateSection() {
  const [currentVersion, setCurrentVersion] = useState("…");
  const [available, setAvailable] = useState<Update | null>(null);
  const [status, setStatus] = useState<"idle" | "checking" | "downloading">("idle");
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [progress, setProgress] = useState<number | null>(null);

  useEffect(() => {
    getVersion().then(setCurrentVersion).catch(() => setCurrentVersion("未知"));
  }, []);

  const checkNow = async () => {
    setStatus("checking");
    setError("");
    setMessage("");
    if (available) await available.close().catch(() => undefined);
    setAvailable(null);
    try {
      const update = await check({ timeout: 20_000 });
      if (update) {
        setAvailable(update);
        setMessage(`发现新版本 ${update.version}`);
      } else {
        setMessage("当前已是最新版本");
      }
    } catch (e) {
      setError(`检查更新失败：${String(e)}`);
    } finally {
      setStatus("idle");
    }
  };

  const install = async () => {
    if (!available) return;
    setStatus("downloading");
    setError("");
    setMessage("正在下载更新…");
    setProgress(0);
    let downloaded = 0;
    let total: number | undefined;
    try {
      await available.downloadAndInstall((event) => {
        if (event.event === "Started") total = event.data.contentLength;
        if (event.event === "Progress") downloaded += event.data.chunkLength;
        if (event.event === "Finished") setProgress(100);
        else if (total) setProgress(Math.min(99, Math.round((downloaded / total) * 100)));
      }, { timeout: 120_000, restartAfterInstall: true });
    } catch (e) {
      setError(`更新失败：${String(e)}`);
      setMessage("");
      setStatus("idle");
      setProgress(null);
    }
  };

  const dismiss = async () => {
    await available?.close().catch(() => undefined);
    setAvailable(null);
    setMessage("");
  };

  return (
    <section className="space-y-2.5">
      <div className="text-sm font-medium text-slate-100">软件更新</div>
      <div className="flex flex-wrap items-center gap-2.5">
        <span className="text-xs text-slate-500">当前版本：v{currentVersion}</span>
        <button type="button" onClick={checkNow} disabled={status !== "idle"}
          className="px-3 py-1.5 rounded text-xs bg-slate-700 text-slate-200 hover:bg-slate-600 disabled:opacity-50 transition">
          {status === "checking" ? "检查中…" : "检查更新"}
        </button>
      </div>
      <div className="text-[11px] text-slate-500">仅在点击后检查，不会自动更新或强制升级。</div>
      {available && status !== "downloading" && (
        <div className="rounded-lg border border-sky-500/30 bg-sky-500/[0.06] px-3 py-2.5 space-y-2">
          <div className="text-sm text-slate-200">{message}</div>
          {available.body && <div className="text-xs text-slate-400 whitespace-pre-wrap max-h-28 overflow-y-auto">{available.body}</div>}
          <div className="flex items-center gap-2">
            <button type="button" onClick={install} className="px-3 py-1.5 rounded text-xs font-medium bg-sky-500 text-white hover:bg-sky-400 transition">下载并安装</button>
            <button type="button" onClick={dismiss} className="px-3 py-1.5 rounded text-xs text-slate-400 hover:text-slate-200 transition">稍后再说</button>
          </div>
        </div>
      )}
      {status === "downloading" && (
        <div className="space-y-1.5">
          <div className="flex justify-between text-xs text-slate-400"><span>{message}</span><span>{progress === null ? "" : `${progress}%`}</span></div>
          <div className="h-1.5 rounded-full bg-slate-800 overflow-hidden"><div className="h-full bg-sky-500 transition-all" style={{ width: `${progress ?? 8}%` }} /></div>
          <div className="text-[11px] text-slate-500">下载完成后将退出拾光并启动安装程序。</div>
        </div>
      )}
      {!available && message && <div className="text-xs text-emerald-400">{message}</div>}
      {error && <div className="text-xs text-rose-400 break-all">{error}</div>}
    </section>
  );
}

function LlmProfilesSection({
  settings,
  update,
  saved,
  onSave,
  onActivate,
}: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  saved: boolean;
  onSave: () => Promise<void>;
  onActivate: (next: Settings) => Promise<void>;
}) {
  const profiles = settings.llm_profiles ?? [];
  const [expandedId, setExpandedId] = useState<string | null>(
    settings.active_llm_profile_id || profiles[0]?.id || null
  );
  const [localError, setLocalError] = useState("");
  const [activatingId, setActivatingId] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const keyInputRef = useRef<HTMLInputElement>(null);
  const pendingKeyFocus = useRef(false);

  const editing = profiles.find((p) => p.id === expandedId) ?? null;
  const active = profiles.find((p) => p.id === settings.active_llm_profile_id) ?? null;
  const editingPreset = editing ? presetByBaseUrl(editing.base_url) : undefined;
  const editingHint = editingPreset?.hint;

  useEffect(() => {
    if (pendingKeyFocus.current && keyInputRef.current) {
      keyInputRef.current.focus();
      pendingKeyFocus.current = false;
    }
  }, [expandedId, editing?.id]);

  const replaceProfiles = (next: LlmProfile[], extra: Partial<Settings> = {}) => {
    const activeId = extra.active_llm_profile_id ?? settings.active_llm_profile_id;
    const current = next.find((p) => p.id === activeId);
    const automaticWindow = current?.models?.find((m) => m.model === current.model)?.context_window;
    update({
      llm_profiles: next,
      subagent_roles: reconcileSubagentRoles(next, activeId, settings.subagent_roles ?? []),
      ...extra,
      ...(current
        ? {
            base_url: current.base_url,
            api_key: current.api_key,
            model: current.model,
            thinking_enabled: current.thinking_enabled,
            reasoning_effort: current.reasoning_effort,
            model_supports_vision: profileSupportsVision(current),
            ...(automaticWindow ? { context_window: automaticWindow } : {}),
          }
        : {}),
    });
  };

  const patchProfile = (id: string, patch: Partial<LlmProfile>) => {
    replaceProfiles(
      profiles.map((p) => (p.id === id ? syncProfileModel({ ...p, ...patch }) : p))
    );
  };

  const addFromPreset = (preset: ProviderPreset) => {
    const existing = profiles.find(
      (p) => p.base_url.replace(/\/+$/, "") === preset.baseUrl.replace(/\/+$/, "")
    );
    if (existing) {
      setExpandedId(existing.id);
      setLocalError(`已有「${existing.name}」配置，请在下方编辑 Key 或切换使用`);
      pendingKeyFocus.current = !existing.api_key.trim();
      return;
    }
    const profile = migrateSettingsProfiles([syncProfileModel({
      ...emptyProfile(uniqueProfileName(preset.name, profiles)),
      base_url: preset.baseUrl,
      catalog_model: preset.defaultCatalogModel,
      thinking_enabled: Boolean(preset.thinking),
      reasoning_effort: "high",
    })])[0];
    const next = [...profiles, profile];
    replaceProfiles(next, profiles.length === 0 ? { active_llm_profile_id: profile.id } : {});
    setExpandedId(profile.id);
    pendingKeyFocus.current = true;
    setLocalError("");
  };

  const applyPreset = (preset: ProviderPreset) => {
    if (!editing) return;
    patchProfile(editing.id, applyProviderPreset(editing, preset));
    setLocalError("");
  };

  const addCustom = () => {
    const profile = emptyProfile(uniqueProfileName("自定义", profiles));
    const next = [...profiles, profile];
    replaceProfiles(next, profiles.length === 0 ? { active_llm_profile_id: profile.id } : {});
    setExpandedId(profile.id);
    pendingKeyFocus.current = true;
  };

  const activate = async (id: string) => {
    const p = profiles.find((x) => x.id === id);
    if (!p) return;
    const status = llmProfileStatus(p);
    if (status === "expired") { setLocalError("此套餐已到期，请续期或更改到期处理"); setExpandedId(id); return; }
    if (status === "no-key") {
      setLocalError("先填写 API Key，再切换使用");
      setExpandedId(id);
      pendingKeyFocus.current = true;
      return;
    }
    if (status === "incomplete") {
      setLocalError("Base URL 和模型都不能为空");
      setExpandedId(id);
      return;
    }
    setLocalError("");
    setActivatingId(id);
    try {
      const synced = syncProfileModel(p);
      const next: Settings = {
        ...settings,
        llm_profiles: profiles.map((x) => (x.id === id ? synced : x)),
        active_llm_profile_id: id,
        subagent_roles: reconcileSubagentRoles(profiles, id, settings.subagent_roles ?? []),
        base_url: synced.base_url,
        api_key: synced.api_key,
        model: synced.model,
        thinking_enabled: synced.thinking_enabled,
        reasoning_effort: synced.reasoning_effort,
        model_supports_vision: profileSupportsVision(synced),
        context_window:
          synced.models?.find((m) => m.model === synced.model)?.context_window ?? settings.context_window,
      };
      await onActivate(next);
    } finally {
      setActivatingId(null);
    }
  };

  const saveSection = async () => {
    setSaving(true);
    setLocalError("");
    try {
      await onSave();
    } catch (e) {
      setLocalError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const remove = (id: string) => {
    if (profiles.length <= 1) {
      setLocalError("至少保留一套配置");
      return;
    }
    const next = profiles.filter((p) => p.id !== id);
    const extra: Partial<Settings> = {};
    if (id === settings.active_llm_profile_id) {
      extra.active_llm_profile_id = next[0].id;
    }
    replaceProfiles(next, extra);
    if (expandedId === id) setExpandedId(next[0]?.id ?? null);
  };

  const groups = PROVIDER_GROUPS;

  return (
    <section className="space-y-3">
      <div className="flex items-start justify-between gap-3">
        <div>
          <div className="text-sm font-medium text-slate-100">大模型 API</div>
          <div className="text-xs text-slate-500 mt-1 leading-5">
            保存多套配置，点「切换使用」立即生效；改完字段记得保存。Key 只存在本机。
          </div>
        </div>
        <button
          type="button"
          onClick={saveSection}
          disabled={saving}
          className="shrink-0 px-3 py-1.5 rounded text-xs bg-slate-700 text-slate-200 hover:bg-slate-600 disabled:opacity-50 transition"
        >
          {saving ? "保存中…" : saved ? "已保存" : "保存配置"}
        </button>
      </div>

      {active && (
        <div className="rounded-lg border border-sky-500/40 bg-sky-500/10 px-3 py-2.5">
          <div className="text-[11px] text-sky-300/80">当前对话使用</div>
          <div className="text-sm text-slate-100 mt-0.5">
            {active.name || "未命名"}
            {" · "}
            {profileCatalogLabel(active)}
          </div>
          <div className="text-[11px] text-slate-500 mt-0.5 truncate" title={active.base_url}>
            {active.base_url || "尚未填写 Base URL"}
            {active.model ? ` · API: ${active.model}` : ""}
          </div>
        </div>
      )}

      <div className="space-y-2">
        {profiles.map((p) => {
          const isActive = p.id === settings.active_llm_profile_id;
          const isExpanded = p.id === expandedId;
          const status = llmProfileStatus(p);
          const preset = presetByBaseUrl(p.base_url);
          return (
            <div
              key={p.id}
              className={`rounded-lg border overflow-hidden transition ${
                isActive
                  ? "border-sky-500/35"
                  : isExpanded
                    ? "border-slate-600"
                    : "border-slate-700/60"
              }`}
            >
              <div
                className={`flex items-center gap-2 px-3 py-2.5 ${
                  isActive ? "bg-sky-500/10" : isExpanded ? "bg-slate-800/80" : "bg-slate-800/40"
                }`}
              >
                <button
                  type="button"
                  onClick={() => setExpandedId(isExpanded ? null : p.id)}
                  className="flex-1 min-w-0 text-left"
                >
                  <div className="flex items-center gap-2 flex-wrap">
                    <span className="text-sm text-slate-100">{p.name || "未命名"}</span>
                    {isActive && (
                      <span className="px-1.5 py-0.5 rounded text-[10px] bg-sky-500/25 text-sky-200">
                        使用中
                      </span>
                    )}
                    <span
                      className={`px-1.5 py-0.5 rounded text-[10px] ${
                        status === "ready"
                          ? "bg-emerald-500/15 text-emerald-300"
                          : status === "no-key"
                            ? "bg-amber-500/15 text-amber-300"
                            : "bg-slate-600/40 text-slate-400"
                      }`}
                    >
                      {llmProfileStatusLabel(status)}
                    </span>
                  </div>
                  <div className="text-[11px] text-slate-500 mt-0.5 truncate">
                    {profileCatalogLabel(p)}
                    {p.valid_until ? ` · 有效至 ${p.valid_until.replace("T", " ")}` : ""}
                    {preset ? ` · ${preset.name}` : ""}
                  </div>
                </button>
                {!isActive && status === "ready" && (
                  <button
                    type="button"
                    onClick={() => activate(p.id)}
                    disabled={activatingId === p.id}
                    className="shrink-0 px-2.5 py-1 rounded text-xs font-medium bg-sky-500/20 text-sky-200 hover:bg-sky-500/30 transition disabled:opacity-50"
                  >
                    {activatingId === p.id ? "切换中…" : "切换使用"}
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => setExpandedId(isExpanded ? null : p.id)}
                  className="shrink-0 px-2 py-1 rounded text-xs text-slate-400 hover:text-slate-200 transition"
                  aria-expanded={isExpanded}
                >
                  {isExpanded ? "收起" : "编辑"}
                </button>
              </div>

              {isExpanded && (
                <div className="border-t border-slate-700/50 p-3 space-y-2.5 bg-slate-900/30">
                  <label className="block">
                    <div className="text-xs text-slate-400 mb-1">供应商预设</div>
                    <select
                      value={editingPreset?.name ?? ""}
                      onChange={(e) => {
                        const name = e.target.value;
                        if (!name) return;
                        const preset = PROVIDER_PRESETS.find((x) => x.name === name);
                        if (preset) applyPreset(preset);
                      }}
                      className={SELECT_CLASS}
                    >
                      <option value="">自定义 / 未匹配预设</option>
                      {groups.flatMap((g) =>
                        PROVIDER_PRESETS.filter(
                          (x) =>
                            x.group === g &&
                            (x.listed !== false || x.id === editingPreset?.id)
                        ).map((x) => (
                          <option key={x.name} value={x.name}>
                            {x.group} · {x.name}
                          </option>
                        ))
                      )}
                    </select>
                  </label>
                  {editingHint && (
                    <div className="text-[11px] text-amber-300/90 leading-5 rounded bg-amber-500/5 border border-amber-500/20 px-2.5 py-2">
                      {editingHint}
                    </div>
                  )}
                  <label className="block">
                    <div className="text-xs text-slate-400 mb-1">名称</div>
                    <input
                      value={p.name}
                      onChange={(e) => patchProfile(p.id, { name: e.target.value })}
                      className={INPUT_CLASS}
                    />
                  </label>
                  <label className="block">
                    <div className="text-xs text-slate-400 mb-1">Base URL（OpenAI 兼容接口）</div>
                    <input
                      value={p.base_url}
                      onChange={(e) => patchProfile(p.id, { base_url: e.target.value })}
                      placeholder="https://api.example.com/v1"
                      className={INPUT_CLASS}
                    />
                  </label>
                  <label className="block">
                    <div className="text-xs text-slate-400 mb-1">
                      API Key（仅保存在本机，不会上传）
                    </div>
                    <input
                      ref={p.id === editing?.id ? keyInputRef : undefined}
                      type="password"
                      value={p.api_key}
                      onChange={(e) => patchProfile(p.id, { api_key: e.target.value })}
                      placeholder="粘贴 Key…"
                      className={INPUT_CLASS}
                    />
                  </label>
                  <ProfileModelSettings profile={p} onChange={(patch) => patchProfile(p.id, patch)} />
                  <label className="flex items-center gap-2.5 text-sm text-slate-300 cursor-pointer">
                    <input
                      type="checkbox"
                      checked={p.thinking_enabled}
                      onChange={(e) =>
                        patchProfile(p.id, { thinking_enabled: e.target.checked })
                      }
                      className="w-4 h-4 accent-sky-500"
                    />
                    启用思考模式
                    <span className="text-xs text-slate-500">仅 DeepSeek 接口会附带该参数</span>
                  </label>
                  {p.thinking_enabled && (
                    <label className="block">
                      <div className="text-xs text-slate-400 mb-1">思考强度</div>
                      <select
                        value={p.reasoning_effort}
                        onChange={(e) =>
                          patchProfile(p.id, { reasoning_effort: e.target.value })
                        }
                        className={SELECT_CLASS}
                      >
                        <option value="low">low — 最快，适合简单问答</option>
                        <option value="high">high — 默认，兼顾速度与质量</option>
                        <option value="max">max — 最深思考，慢但更准</option>
                      </select>
                    </label>
                  )}
                  <div className="flex flex-wrap items-center gap-2 pt-1">
                    {!isActive && status === "ready" && (
                      <button
                        type="button"
                        onClick={() => activate(p.id)}
                        disabled={activatingId === p.id}
                        className="px-3 py-1.5 rounded text-xs font-medium bg-sky-500 text-white hover:bg-sky-400 transition disabled:opacity-50"
                      >
                        {activatingId === p.id ? "切换中…" : "切换并使用"}
                      </button>
                    )}
                    <button
                      type="button"
                      onClick={saveSection}
                      disabled={saving}
                      className="px-3 py-1.5 rounded text-xs bg-slate-700 text-slate-200 hover:bg-slate-600 disabled:opacity-50 transition"
                    >
                      保存此套
                    </button>
                    <button
                      type="button"
                      onClick={() => remove(p.id)}
                      className="px-3 py-1.5 rounded text-xs text-slate-500 hover:text-rose-400 transition"
                    >
                      删除
                    </button>
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>

      <div className="space-y-2 rounded-lg border border-dashed border-slate-700/60 p-3">
        <div className="text-xs text-slate-400">快捷添加（自动填 Base URL 和默认模型）</div>
        {groups.map((g) => (
          <div key={g} className="space-y-1.5">
            <div className="text-[11px] text-slate-500">{g}</div>
            <div className="flex flex-wrap gap-1.5">
              {listedProviderPresets()
                .filter((p) => p.group === g)
                .map((p) => (
                <button
                  key={p.name}
                  type="button"
                  title={p.hint || `添加 ${p.name}`}
                  onClick={() => addFromPreset(p)}
                  className="px-3 py-1.5 rounded-full text-xs bg-slate-700/60 text-slate-300 hover:bg-slate-600 transition"
                >
                  {p.name}
                </button>
              ))}
            </div>
            {g === "火山" && (
              <div className="text-[11px] text-slate-500 leading-5">
                Agent Plan 用 <span className="text-slate-400">/api/v3</span>
                ，Coding Plan 必须带 <span className="text-slate-400">/coding</span>
                。接入说明见
                <button
                  type="button"
                  onClick={() => ipc.openExternal("https://www.volcengine.com/docs/82379/2366394")}
                  className="ml-1 text-sky-400 hover:text-sky-300"
                >
                  火山 Agent Plan 文档
                </button>
                。
              </div>
            )}
            {g === "国际" && (
              <div className="text-[11px] text-slate-500 leading-5">
                Google Gemini Key 在{" "}
                <button
                  type="button"
                  onClick={() => ipc.openExternal("https://aistudio.google.com/apikey")}
                  className="text-sky-400 hover:text-sky-300"
                >
                  Google AI Studio
                </button>
                获取；选「Google Gemini」预设会自动填好兼容地址。
              </div>
            )}
          </div>
        ))}
        <button
          type="button"
          onClick={addCustom}
          className="px-3 py-1.5 rounded-full text-xs border border-dashed border-slate-600 text-slate-400 hover:text-slate-200 hover:border-slate-400 transition"
        >
          + 自定义接口
        </button>
      </div>
      <ModelCatalogPanel />
      {localError && <div className="text-xs text-rose-400">{localError}</div>}
    </section>
  );
}

function BrowserExtensionSection() {
  const [path, setPath] = useState("");
  const [ready, setReady] = useState(false);
  const [msg, setMsg] = useState("");
  const [err, setErr] = useState("");

  useEffect(() => {
    ipc
      .getBrowserExtension()
      .then((info) => {
        setPath(info.path);
        setReady(info.ready);
      })
      .catch((e) => setErr(String(e)));
  }, []);

  return (
    <section className="space-y-2.5">
      <div className="text-sm font-medium text-slate-100">
        浏览器扩展
        <span className="ml-2 text-xs font-normal text-slate-500">
          安装后会解压到安装目录，换电脑时选这个文件夹加载即可
        </span>
      </div>
      <div className="text-xs text-slate-400 break-all">
        {path ? `目录：${path}` : "路径加载中…"}
        {path && (
          <span className={ready ? "ml-2 text-emerald-400" : "ml-2 text-amber-400"}>
            {ready ? "已就绪" : "未解压完整"}
          </span>
        )}
      </div>
      <div className="text-xs text-slate-500 leading-5">
        Chrome / Edge 打开扩展页 → 开启开发者模式 → 加载已解压的扩展程序 → 选择上面这个文件夹。
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={async () => {
            setErr("");
            setMsg("");
            try {
              await ipc.openBrowserExtensionDir();
              setMsg("已打开文件夹");
            } catch (e) {
              setErr(String(e));
            }
          }}
          className="px-3.5 py-1.5 rounded bg-slate-700 text-slate-200 text-sm hover:bg-slate-600 transition"
        >
          打开扩展文件夹
        </button>
        <button
          type="button"
          onClick={() => ipc.openExternal("chrome://extensions")}
          className="px-3 py-1.5 rounded text-slate-400 text-sm hover:text-slate-200 transition"
        >
          打开 Chrome 扩展页
        </button>
        {msg && <span className="text-xs text-emerald-400">{msg}</span>}
        {err && <span className="text-xs text-rose-400">{err}</span>}
      </div>
    </section>
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
