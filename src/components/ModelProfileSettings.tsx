import { useState } from "react";
import type { LlmProfile, ProfileModel, Settings, SubagentRole } from "../lib/ipc";

const INPUT = "w-full rounded border border-slate-700 bg-slate-800 px-3 py-2 text-sm outline-none focus:border-sky-500";

export function ProfileModelSettings({ profile, onChange }: { profile: LlmProfile; onChange: (patch: Partial<LlmProfile>) => void }) {
  const [name, setName] = useState("");
  const [error, setError] = useState("");
  const models = profile.models ?? [];
  const selected = models.find((m) => m.model === profile.model);
  const patchModels = (next: ProfileModel[], active = profile.model) => {
    const model = next.find((m) => m.model === active) ?? next[0];
    onChange({ models:next, model:model?.model ?? "", catalog_model:"__custom__", supports_vision:model?.supports_vision ?? false });
  };
  const add = () => {
    const model = name.trim();
    if (!model) { setError("请输入套餐支持的模型名称"); return; }
    if (models.some((m) => m.model === model)) { setError("此配置已添加该模型"); return; }
    patchModels([...models, { model, label:model, supports_vision:false, context_window:128_000 }], profile.model || model);
    setName(""); setError("");
  };
  const updateModel = (patch: Partial<ProfileModel>) => patchModels(models.map((m) => m.model === profile.model ? { ...m, ...patch } : m));
  const expired = profile.valid_until && new Date(profile.valid_until).getTime() <= Date.now();
  return <div className="space-y-3">
    <div className="grid grid-cols-2 gap-3">
      <label className="block text-xs text-slate-400">套餐有效期（本地时间，可留空）
        <input type="datetime-local" value={profile.valid_until ?? ""} onChange={(e) => onChange({ valid_until:e.target.value })} className={`${INPUT} mt-1`} />
      </label>
      <p className="self-end pb-2 text-xs text-slate-500">到期后停止调用，请切换配置或续期。</p>
    </div>
    {expired && <p role="status" className="text-xs text-amber-300">本套餐已到期，请续期或切换配置。</p>}
    <label className="block text-xs text-slate-400">当前模型
      <select value={profile.model} onChange={(e) => patchModels(models, e.target.value)} className={`${INPUT} mt-1`} disabled={!models.length}>
        {!models.length && <option value="">请先添加模型</option>}
        {models.map((m) => <option key={m.model} value={m.model}>{m.label || m.model}{m.label && m.label !== m.model ? ` · ${m.model}` : ""}</option>)}
      </select>
    </label>
    {selected && <div className="grid grid-cols-2 gap-3">
      <label className="block text-xs text-slate-400">显示名称<input value={selected.label} onChange={(e) => updateModel({ label:e.target.value })} className={`${INPUT} mt-1`} /></label>
      <label className="block text-xs text-slate-400">上下文窗口（Token）<input type="number" min={16000} max={20000000} value={selected.context_window} onChange={(e) => updateModel({ context_window:Number(e.target.value) })} className={`${INPUT} mt-1`} /></label>
      <label className="flex items-center gap-2 text-xs text-slate-300"><input type="checkbox" checked={selected.supports_vision} onChange={(e) => updateModel({ supports_vision:e.target.checked })} />支持图像输入</label>
      <button type="button" onClick={() => patchModels(models.filter((m) => m.model !== selected.model))} className="justify-self-end text-xs text-slate-400 hover:text-rose-300">移除此模型</button>
    </div>}
    <label className="block text-xs text-slate-400">手动添加本套餐模型
      <div className="mt-1 flex gap-2"><input value={name} onChange={(e) => { setName(e.target.value); setError(""); }} onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); add(); } }} placeholder="填写控制台提供的实际模型名称" className={INPUT} /><button type="button" onClick={add} className="shrink-0 rounded bg-slate-700 px-3 text-xs hover:bg-slate-600">添加模型</button></div>
    </label>
    <p className="text-xs text-slate-500">模型列表仅属于这套配置，切换供应商后使用各自已添加的模型。</p>
    {error && <p role="alert" className="text-xs text-rose-300">{error}</p>}
  </div>;
}

const ROLES: { role:SubagentRole["role"]; name:string; hint:string }[] = [
  { role:"research", name:"资料调研", hint:"公开网页检索、阅读材料与证据汇总" },
  { role:"files", name:"文件分析", hint:"在指定范围查找文件、阅读文档和分析内容" },
  { role:"browser", name:"浏览器操作", hint:"网页实测、后台操作和表单；关键节点回传由主代理审核" },
];

export function SubagentSettings({ settings, update }: { settings:Settings; update:(patch:Partial<Settings>) => void }) {
  const configs = settings.subagent_roles ?? ROLES.map(({ role }) => ({ role, enabled:true, profile_id:"", model:"" }));
  const patch = (role:SubagentRole["role"], change:Partial<SubagentRole>) => update({ subagent_roles:configs.map((r) => r.role === role ? { ...r, ...change } : r) });
  return <section className="space-y-3">
    <div className="text-sm font-medium text-slate-100">子代理</div>
    <p className="text-xs text-slate-500">启用后，主代理会按任务自动委托，简单操作直接完成。主代理负责目标、关键节点审核和最终验收。</p>
    {ROLES.map(({ role, name, hint }) => {
      const config = configs.find((r) => r.role === role) ?? { role, enabled:false, profile_id:"", model:"" };
      const profile = settings.llm_profiles.find((p) => p.id === (config.profile_id || settings.active_llm_profile_id));
      return <div key={role} className="border-b border-slate-700/50 pb-3">
        <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={config.enabled} onChange={(e) => patch(role, { enabled:e.target.checked })} />{name}</label>
        <p className="my-1.5 text-xs text-slate-500">{hint}</p>
        <div className="grid grid-cols-2 gap-3">
          <label className="text-xs text-slate-400">供应商配置<select disabled={!config.enabled} value={config.profile_id} onChange={(e) => patch(role, { profile_id:e.target.value, model:"" })} className={`${INPUT} mt-1`}><option value="">跟随主配置</option>{settings.llm_profiles.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}</select></label>
          <label className="text-xs text-slate-400">模型<select disabled={!config.enabled} value={config.model} onChange={(e) => patch(role, { model:e.target.value })} className={`${INPUT} mt-1`}><option value="">跟随该配置当前模型</option>{(profile?.models ?? []).map((m) => <option key={m.model} value={m.model}>{m.label || m.model}</option>)}</select></label>
        </div>
      </div>;
    })}
    <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={settings.subagent_thinking_enabled} onChange={(e) => update({ subagent_thinking_enabled:e.target.checked })} />子代理启用思考模式（DeepSeek）</label>
    {settings.subagent_thinking_enabled && <label className="block text-xs text-slate-400">思考强度<select value={settings.subagent_reasoning_effort} onChange={(e) => update({ subagent_reasoning_effort:e.target.value })} className={`${INPUT} mt-1`}><option value="low">low</option><option value="high">high</option><option value="max">max</option></select></label>}
  </section>;
}
