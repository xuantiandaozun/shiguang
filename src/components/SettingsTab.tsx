import { useEffect, useState } from "react";
import { ipc, onEvent } from "../lib/ipc";
import type { LlmProfile, ProfileEntry, Settings } from "../lib/ipc";

type LlmPreset = {
  name: string;
  group: "国内" | "火山" | "国际";
  baseUrl: string;
  model: string;
  models?: string[];
  thinking?: boolean;
  hint?: string;
};

const LLM_PRESETS: LlmPreset[] = [
  {
    name: "DeepSeek",
    group: "国内",
    baseUrl: "https://api.deepseek.com/v1",
    model: "deepseek-v4-flash",
    models: ["deepseek-v4-flash", "deepseek-chat", "deepseek-reasoner"],
    thinking: true,
  },
  {
    name: "通义千问",
    group: "国内",
    baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    model: "qwen-plus",
    models: ["qwen-plus", "qwen-turbo", "qwen-max", "qwen-long"],
  },
  {
    name: "智谱 GLM",
    group: "国内",
    baseUrl: "https://open.bigmodel.cn/api/paas/v4",
    model: "glm-4-flash",
    models: ["glm-4-flash", "glm-4.5", "glm-4.5-air", "glm-z1-air"],
  },
  {
    name: "Kimi",
    group: "国内",
    baseUrl: "https://api.moonshot.cn/v1",
    model: "kimi-k2.5",
    models: ["kimi-k2.5", "moonshot-v1-auto", "moonshot-v1-128k"],
  },
  {
    name: "硅基流动",
    group: "国内",
    baseUrl: "https://api.siliconflow.cn/v1",
    model: "deepseek-ai/DeepSeek-V3",
    models: ["deepseek-ai/DeepSeek-V3", "deepseek-ai/DeepSeek-V3.1", "Qwen/Qwen3-235B-A22B"],
  },
  {
    name: "腾讯混元",
    group: "国内",
    baseUrl: "https://api.hunyuan.cloud.tencent.com/v1",
    model: "hunyuan-turbos-latest",
    models: ["hunyuan-turbos-latest", "hunyuan-large", "hunyuan-lite"],
  },
  {
    name: "MiniMax",
    group: "国内",
    baseUrl: "https://api.minimax.chat/v1",
    model: "MiniMax-M2.5",
    models: ["MiniMax-M2.5", "MiniMax-Text-01", "abab6.5s-chat"],
  },
  {
    name: "火山 Agent Plan",
    group: "火山",
    baseUrl: "https://ark.cn-beijing.volces.com/api/v3",
    model: "doubao-seed-2.0-pro",
    models: [
      "doubao-seed-2.0-pro",
      "doubao-seed-2.0-lite",
      "doubao-seed-2.0-mini",
      "doubao-seed-2.0-code",
      "deepseek-v4-flash",
      "glm-5.2",
    ],
    hint: "个人套餐 OpenAI 兼容接口。Key 在方舟控制台「开通管理」获取，请勿与 Coding Plan 混用。",
  },
  {
    name: "火山 Coding Plan",
    group: "火山",
    baseUrl: "https://ark.cn-beijing.volces.com/api/coding/v3",
    model: "doubao-seed-2.0-code",
    models: ["doubao-seed-2.0-code", "doubao-seed-2.0-pro", "ark-code-latest", "glm-4.7", "kimi-k2.5"],
    hint: "编程套餐必须走 /api/coding/v3，误用 /api/v3 会按量计费。",
  },
  {
    name: "OpenAI",
    group: "国际",
    baseUrl: "https://api.openai.com/v1",
    model: "gpt-4o-mini",
    models: ["gpt-4o-mini", "gpt-4o", "gpt-4.1", "o4-mini"],
  },
  {
    name: "OpenRouter",
    group: "国际",
    baseUrl: "https://openrouter.ai/api/v1",
    model: "openai/gpt-4o-mini",
    models: ["openai/gpt-4o-mini", "anthropic/claude-sonnet-4", "deepseek/deepseek-chat", "google/gemini-2.5-flash"],
  },
];

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
    model: "",
    thinking_enabled: false,
    reasoning_effort: "high",
  };
}

function suggestedModels(profile: LlmProfile): string[] {
  const hit = LLM_PRESETS.find(
    (p) => p.baseUrl.replace(/\/+$/, "") === profile.base_url.replace(/\/+$/, "")
  );
  return hit?.models ?? [];
}

export default function SettingsTab() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    ipc
      .getSettings()
      .then((s) =>
        setSettings({
          ...s,
          llm_profiles: s.llm_profiles ?? [],
          active_llm_profile_id: s.active_llm_profile_id ?? "",
        })
      )
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
      <LlmProfilesSection
        settings={settings}
        update={update}
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

function LlmProfilesSection({
  settings,
  update,
  onActivate,
}: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  onActivate: (next: Settings) => Promise<void>;
}) {
  const profiles = settings.llm_profiles ?? [];
  const [editingId, setEditingId] = useState<string | null>(
    settings.active_llm_profile_id || profiles[0]?.id || null
  );
  const [localError, setLocalError] = useState("");
  const editing = profiles.find((p) => p.id === editingId) ?? null;
  const active = profiles.find((p) => p.id === settings.active_llm_profile_id) ?? null;
  const models = editing ? suggestedModels(editing) : [];
  const editingHint = LLM_PRESETS.find(
    (p) => editing && p.baseUrl.replace(/\/+$/, "") === editing.base_url.replace(/\/+$/, "")
  )?.hint;

  const replaceProfiles = (next: LlmProfile[], extra: Partial<Settings> = {}) => {
    const activeId = extra.active_llm_profile_id ?? settings.active_llm_profile_id;
    const current = next.find((p) => p.id === activeId);
    update({
      llm_profiles: next,
      ...extra,
      ...(current
        ? {
            base_url: current.base_url,
            api_key: current.api_key,
            model: current.model,
            thinking_enabled: current.thinking_enabled,
            reasoning_effort: current.reasoning_effort,
          }
        : {}),
    });
  };

  const patchProfile = (id: string, patch: Partial<LlmProfile>) => {
    replaceProfiles(profiles.map((p) => (p.id === id ? { ...p, ...patch } : p)));
  };

  const addFromPreset = (preset: LlmPreset) => {
    const profile: LlmProfile = {
      ...emptyProfile(uniqueProfileName(preset.name, profiles)),
      base_url: preset.baseUrl,
      model: preset.model,
      thinking_enabled: Boolean(preset.thinking),
      reasoning_effort: "high",
    };
    const next = [...profiles, profile];
    replaceProfiles(next, profiles.length === 0 ? { active_llm_profile_id: profile.id } : {});
    setEditingId(profile.id);
    setLocalError("");
  };

  const addCustom = () => {
    const profile = emptyProfile(uniqueProfileName("自定义", profiles));
    const next = [...profiles, profile];
    replaceProfiles(next, profiles.length === 0 ? { active_llm_profile_id: profile.id } : {});
    setEditingId(profile.id);
  };

  const activate = async (id: string) => {
    const p = profiles.find((x) => x.id === id);
    if (!p) return;
    if (!p.api_key.trim()) {
      setLocalError("先填写这套配置的 API Key，再设为当前使用");
      setEditingId(id);
      return;
    }
    if (!p.base_url.trim() || !p.model.trim()) {
      setLocalError("Base URL 和模型都不能为空");
      setEditingId(id);
      return;
    }
    setLocalError("");
    const next: Settings = {
      ...settings,
      llm_profiles: profiles,
      active_llm_profile_id: id,
      base_url: p.base_url,
      api_key: p.api_key,
      model: p.model,
      thinking_enabled: p.thinking_enabled,
      reasoning_effort: p.reasoning_effort,
    };
    await onActivate(next);
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
    if (editingId === id) setEditingId(next[0].id);
  };

  const groups: Array<LlmPreset["group"]> = ["国内", "火山", "国际"];

  return (
    <section className="space-y-3">
      <div>
        <div className="text-sm font-medium text-slate-100">大模型 API</div>
        <div className="text-xs text-slate-500 mt-1 leading-5">
          可保存多套接口（不同供应商或 Key），手动选择当前对话用哪一套。Key 只存在本机。
        </div>
      </div>

      <label className="block">
        <div className="text-xs text-slate-400 mb-1">当前使用</div>
        <select
          value={settings.active_llm_profile_id}
          onChange={(e) => activate(e.target.value)}
          className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
        >
          {profiles.length === 0 && <option value="">尚未添加配置</option>}
          {profiles.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
              {p.model ? ` · ${p.model}` : ""}
              {p.api_key.trim() ? "" : "（未填 Key）"}
            </option>
          ))}
        </select>
        {active && (
          <div className="text-[11px] text-slate-500 mt-1 truncate" title={active.base_url}>
            {active.base_url || "尚未填写 Base URL"}
          </div>
        )}
      </label>

      <div className="space-y-1.5">
        {profiles.map((p) => {
          const isActive = p.id === settings.active_llm_profile_id;
          const isEditing = p.id === editingId;
          return (
            <div
              key={p.id}
              className={`rounded-lg border px-3 py-2 ${
                isActive
                  ? "border-sky-500/40 bg-sky-500/10"
                  : isEditing
                    ? "border-slate-600 bg-slate-800/80"
                    : "border-slate-700/60 bg-slate-800/40"
              }`}
            >
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => setEditingId(p.id)}
                  className="flex-1 min-w-0 text-left"
                >
                  <div className="flex items-center gap-2">
                    <span className="text-sm text-slate-100 truncate">{p.name || "未命名"}</span>
                    {isActive && (
                      <span className="shrink-0 px-1.5 py-0.5 rounded text-[10px] bg-sky-500/20 text-sky-300">
                        使用中
                      </span>
                    )}
                  </div>
                  <div className="text-[11px] text-slate-500 mt-0.5 truncate">
                    {p.model || "未填模型"} · {p.api_key.trim() ? "已填 Key" : "未填 Key"}
                  </div>
                </button>
                {!isActive && (
                  <button
                    type="button"
                    onClick={() => activate(p.id)}
                    className="shrink-0 px-2 py-1 rounded text-xs text-sky-300 hover:bg-sky-500/15 transition"
                  >
                    设为当前
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => setEditingId(p.id)}
                  className="shrink-0 px-2 py-1 rounded text-xs text-slate-400 hover:text-slate-200 transition"
                >
                  编辑
                </button>
                <button
                  type="button"
                  onClick={() => remove(p.id)}
                  className="shrink-0 px-2 py-1 rounded text-xs text-slate-500 hover:text-rose-400 transition"
                >
                  删除
                </button>
              </div>
            </div>
          );
        })}
      </div>

      {editing && (
        <div className="rounded-lg border border-slate-700/60 bg-slate-800/30 p-3 space-y-2.5">
          <div className="text-xs text-slate-400">编辑「{editing.name || "未命名"}」</div>
          {editingHint && <div className="text-[11px] text-amber-300/90 leading-5">{editingHint}</div>}
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">名称</div>
            <input
              value={editing.name}
              onChange={(e) => patchProfile(editing.id, { name: e.target.value })}
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
            />
          </label>
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">Base URL（OpenAI 兼容接口）</div>
            <input
              value={editing.base_url}
              onChange={(e) => patchProfile(editing.id, { base_url: e.target.value })}
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
            />
          </label>
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">API Key（仅保存在本机 SQLite，不会上传）</div>
            <input
              type="password"
              value={editing.api_key}
              onChange={(e) => patchProfile(editing.id, { api_key: e.target.value })}
              placeholder="sk-..."
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500 placeholder:text-slate-600"
            />
          </label>
          <label className="block">
            <div className="text-xs text-slate-400 mb-1">模型</div>
            <input
              list={`llm-models-${editing.id}`}
              value={editing.model}
              onChange={(e) => patchProfile(editing.id, { model: e.target.value })}
              className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
            />
            {models.length > 0 && (
              <datalist id={`llm-models-${editing.id}`}>
                {models.map((m) => (
                  <option key={m} value={m} />
                ))}
              </datalist>
            )}
          </label>
          <label className="flex items-center gap-2.5 text-sm text-slate-300 cursor-pointer">
            <input
              type="checkbox"
              checked={editing.thinking_enabled}
              onChange={(e) => patchProfile(editing.id, { thinking_enabled: e.target.checked })}
              className="w-4 h-4 accent-sky-500"
            />
            启用思考模式
            <span className="text-xs text-slate-500">仅 DeepSeek 接口会附带该参数</span>
          </label>
          {editing.thinking_enabled && (
            <label className="block">
              <div className="text-xs text-slate-400 mb-1">思考强度</div>
              <select
                value={editing.reasoning_effort}
                onChange={(e) => patchProfile(editing.id, { reasoning_effort: e.target.value })}
                className="w-full bg-slate-800 text-sm rounded px-3 py-2 outline-none border border-slate-700 focus:border-sky-500"
              >
                <option value="low">low — 最快，适合简单问答</option>
                <option value="high">high — 默认，兼顾速度与质量</option>
                <option value="max">max — 最深思考，慢但更准</option>
              </select>
            </label>
          )}
        </div>
      )}

      <div className="space-y-2">
        <div className="text-xs text-slate-400">快捷添加供应商</div>
        {groups.map((g) => (
          <div key={g} className="space-y-1.5">
            <div className="text-[11px] text-slate-500">{g}</div>
            <div className="flex flex-wrap gap-1.5">
              {LLM_PRESETS.filter((p) => p.group === g).map((p) => (
                <button
                  key={p.name}
                  type="button"
                  title={p.hint || `添加 ${p.name}：${p.model}`}
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
                。                接入说明见
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
          </div>
        ))}
        <button
          type="button"
          onClick={addCustom}
          className="px-3 py-1.5 rounded-full text-xs border border-dashed border-slate-600 text-slate-400 hover:text-slate-200 hover:border-slate-400 transition"
        >
          + 自定义
        </button>
      </div>
      {localError && <div className="text-xs text-rose-400">{localError}</div>}
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
