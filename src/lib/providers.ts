/** 供应商连接预设：只负责 Base URL / 分组 / 说明，不含模型列表 */

export type ProviderGroup = "国内" | "火山" | "国际";

export type ProviderPreset = {
  /** 稳定标识，用于模型目录里的 bindings 键 */
  id: string;
  name: string;
  group: ProviderGroup;
  baseUrl: string;
  /** 新建此供应商配置时的默认统一模型 id */
  defaultCatalogModel: string;
  thinking?: boolean;
  hint?: string;
  /** 为 false 时不出现在快捷添加；已保存的档案仍可按 Base URL 识别 */
  listed?: boolean;
};

export const PROVIDER_PRESETS: ProviderPreset[] = [
  {
    id: "aliyun-coding", name: "阿里 Coding Plan", group: "国内",
    baseUrl: "https://coding.dashscope.aliyuncs.com/v1", defaultCatalogModel: "__custom__",
    hint: "Coding Plan 专用接口。请填写套餐支持的实际模型名，并维护本配置自己的模型列表和有效期。",
  },
  {
    id: "deepseek",
    name: "DeepSeek",
    group: "国内",
    baseUrl: "https://api.deepseek.com/v1",
    defaultCatalogModel: "deepseek-v4.1-flash",
    thinking: true,
  },
  {
    id: "dashscope",
    name: "通义千问",
    group: "国内",
    baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    defaultCatalogModel: "qwen-plus",
  },
  {
    id: "zhipu",
    name: "智谱 GLM",
    group: "国内",
    baseUrl: "https://open.bigmodel.cn/api/paas/v4",
    defaultCatalogModel: "glm-4-flash",
  },
  {
    id: "moonshot",
    name: "Kimi",
    group: "国内",
    baseUrl: "https://api.moonshot.cn/v1",
    defaultCatalogModel: "kimi-k2.5",
  },
  {
    id: "siliconflow",
    name: "硅基流动",
    group: "国内",
    baseUrl: "https://api.siliconflow.cn/v1",
    defaultCatalogModel: "deepseek-v3",
  },
  {
    id: "hunyuan",
    name: "腾讯混元",
    group: "国内",
    baseUrl: "https://api.hunyuan.cloud.tencent.com/v1",
    defaultCatalogModel: "hunyuan-turbos",
  },
  {
    id: "minimax",
    name: "MiniMax",
    group: "国内",
    baseUrl: "https://api.minimax.chat/v1",
    defaultCatalogModel: "minimax-m2.5",
  },
  {
    id: "volc-agent",
    name: "火山 Agent Plan",
    group: "火山",
    baseUrl: "https://ark.cn-beijing.volces.com/api/plan/v3",
    defaultCatalogModel: "doubao-seed-2.0-pro",
    hint: "个人套餐 OpenAI 兼容接口。Key 在方舟控制台「开通管理」获取，请勿与 Coding Plan 混用。",
  },
  {
    id: "volc-coding",
    name: "火山 Coding Plan",
    group: "火山",
    baseUrl: "https://ark.cn-beijing.volces.com/api/coding/v3",
    defaultCatalogModel: "doubao-seed-2.0-code",
    hint: "编程套餐必须走 /api/coding/v3，误用 /api/v3 会按量计费。",
  },
  {
    id: "openai",
    name: "OpenAI",
    group: "国际",
    baseUrl: "https://api.openai.com/v1",
    defaultCatalogModel: "gpt-4o-mini",
  },
  {
    id: "openrouter",
    name: "OpenRouter",
    group: "国际",
    baseUrl: "https://openrouter.ai/api/v1",
    defaultCatalogModel: "gpt-4o-mini",
  },
  {
    id: "google-gemini",
    name: "Google Gemini",
    group: "国际",
    baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai",
    defaultCatalogModel: "gemini-3.5-flash",
    hint: "Key 在 aistudio.google.com 获取。须走 OpenAI 兼容地址（已自动填好 /openai/ 后缀）。新账号请选 3.5 Flash；不要填 OpenRouter 那种 google/ 前缀。2.5/2.0 已对新用户下架。",
  },
  {
    id: "agentrouter",
    name: "AgentRouter 公益站",
    group: "国内",
    baseUrl: "https://agentrouter.org/v1",
    defaultCatalogModel: "deepseek-v4.1-flash",
    listed: false,
    hint: "已从快捷添加中撤下。已保存的档案仍可用。须用 OpenAI 兼容地址 …/v1。",
  },
];

export function normalizeBaseUrl(url: string): string {
  return url.trim().replace(/\/+$/, "");
}

export function providerByBaseUrl(baseUrl: string): ProviderPreset | undefined {
  const key = normalizeBaseUrl(baseUrl);
  return PROVIDER_PRESETS.find((p) => normalizeBaseUrl(p.baseUrl) === key);
}

export function providerById(id: string): ProviderPreset | undefined {
  return PROVIDER_PRESETS.find((p) => p.id === id);
}

export const PROVIDER_GROUPS: ProviderGroup[] = ["国内", "火山", "国际"];

export function listedProviderPresets(): ProviderPreset[] {
  return PROVIDER_PRESETS.filter((p) => p.listed !== false);
}
