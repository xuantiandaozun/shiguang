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
};

export const PROVIDER_PRESETS: ProviderPreset[] = [
  {
    id: "deepseek",
    name: "DeepSeek",
    group: "国内",
    baseUrl: "https://api.deepseek.com/v1",
    defaultCatalogModel: "deepseek-v4-flash",
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
    baseUrl: "https://ark.cn-beijing.volces.com/api/v3",
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
    defaultCatalogModel: "gemini-2.5-flash",
    hint: "Key 在 aistudio.google.com 获取。须走 OpenAI 兼容地址（已自动填好 /openai/ 后缀）。",
  },
  {
    id: "agentrouter",
    name: "AgentRouter 公益站",
    group: "国内",
    baseUrl: "https://agentrouter.org/v1",
    defaultCatalogModel: "claude-opus-4-6",
    hint: "Key 在 agentrouter.org/console/token 获取。须用 OpenAI 兼容地址 …/v1（Claude Code 文档里的根域名不含 /v1，不能用于本应用）。模型名用站内原名如 claude-opus-4-6。",
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
