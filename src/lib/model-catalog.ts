import { providerByBaseUrl } from "./providers";

/**
 * 统一模型目录：逻辑模型 id 全局唯一，各供应商 API 模型名在 bindings 里单独维护。
 */

export type CatalogTag = "对话" | "代码" | "推理" | "视觉";

export type CatalogModel = {
  /** 全局唯一 id，切换供应商时保持不变 */
  id: string;
  /** 界面展示名 */
  label: string;
  tags?: CatalogTag[];
  vision?: boolean;
  thinking?: boolean;
  /** provider id → 该供应商下的 API 模型名 */
  bindings: Partial<Record<string, string>>;
};

export const CUSTOM_CATALOG_MODEL = "__custom__";

/** 程序内维护的统一模型列表 */
export const MODEL_CATALOG: CatalogModel[] = [
  {
    id: "deepseek-v4-flash",
    label: "DeepSeek V4 Flash",
    tags: ["对话"],
    bindings: { deepseek: "deepseek-v4-flash", "volc-agent": "deepseek-v4-flash" },
  },
  {
    id: "deepseek-v4-flash-vision",
    label: "DeepSeek V4 Flash Vision",
    tags: ["对话", "视觉"],
    vision: true,
    bindings: {
      deepseek: "deepseek-v4-flash-vision-exp",
      "volc-agent": "deepseek-v4-flash-vision-exp",
    },
  },
  {
    id: "deepseek-v4-pro",
    label: "DeepSeek V4 Pro",
    tags: ["对话"],
    bindings: { deepseek: "deepseek-v4-pro" },
  },
  {
    id: "deepseek-chat",
    label: "DeepSeek Chat",
    tags: ["对话"],
    bindings: { deepseek: "deepseek-chat", openrouter: "deepseek/deepseek-chat" },
  },
  {
    id: "deepseek-reasoner",
    label: "DeepSeek Reasoner",
    tags: ["推理"],
    thinking: true,
    bindings: { deepseek: "deepseek-reasoner" },
  },
  {
    id: "deepseek-v3",
    label: "DeepSeek V3",
    tags: ["对话"],
    bindings: { siliconflow: "deepseek-ai/DeepSeek-V3" },
  },
  {
    id: "deepseek-v3.1",
    label: "DeepSeek V3.1",
    tags: ["对话"],
    bindings: { siliconflow: "deepseek-ai/DeepSeek-V3.1" },
  },
  {
    id: "qwen-plus",
    label: "通义千问 Plus",
    tags: ["对话"],
    bindings: { dashscope: "qwen-plus" },
  },
  {
    id: "qwen-turbo",
    label: "通义千问 Turbo",
    tags: ["对话"],
    bindings: { dashscope: "qwen-turbo" },
  },
  {
    id: "qwen-max",
    label: "通义千问 Max",
    tags: ["对话"],
    bindings: { dashscope: "qwen-max" },
  },
  {
    id: "qwen-long",
    label: "通义千问 Long",
    tags: ["对话"],
    bindings: { dashscope: "qwen-long" },
  },
  {
    id: "qwen-vl-max",
    label: "通义千问 VL Max",
    tags: ["视觉"],
    vision: true,
    bindings: { dashscope: "qwen-vl-max" },
  },
  {
    id: "qwen-vl-plus",
    label: "通义千问 VL Plus",
    tags: ["视觉"],
    vision: true,
    bindings: { dashscope: "qwen-vl-plus" },
  },
  {
    id: "qwen-vl-ocr",
    label: "通义千问 VL OCR",
    tags: ["视觉"],
    vision: true,
    bindings: { dashscope: "qwen-vl-ocr" },
  },
  {
    id: "qwen3-235b",
    label: "Qwen3 235B",
    tags: ["对话"],
    bindings: { siliconflow: "Qwen/Qwen3-235B-A22B" },
  },
  {
    id: "qwen2.5-vl-72b",
    label: "Qwen2.5 VL 72B",
    tags: ["视觉"],
    vision: true,
    bindings: { siliconflow: "Qwen/Qwen2.5-VL-72B-Instruct" },
  },
  {
    id: "glm-4-flash",
    label: "GLM-4 Flash",
    tags: ["对话"],
    bindings: { zhipu: "glm-4-flash" },
  },
  {
    id: "glm-4.5",
    label: "GLM-4.5",
    tags: ["对话"],
    bindings: { zhipu: "glm-4.5" },
  },
  {
    id: "glm-4.5-air",
    label: "GLM-4.5 Air",
    tags: ["对话"],
    bindings: { zhipu: "glm-4.5-air" },
  },
  {
    id: "glm-z1-air",
    label: "GLM-Z1 Air",
    tags: ["推理"],
    bindings: { zhipu: "glm-z1-air" },
  },
  {
    id: "glm-4v-flash",
    label: "GLM-4V Flash",
    tags: ["视觉"],
    vision: true,
    bindings: { zhipu: "glm-4v-flash" },
  },
  {
    id: "glm-4.5v",
    label: "GLM-4.5V",
    tags: ["视觉"],
    vision: true,
    bindings: { zhipu: "glm-4.5v" },
  },
  {
    id: "glm-5.2",
    label: "GLM-5.2",
    tags: ["对话"],
    bindings: { "volc-agent": "glm-5.2" },
  },
  {
    id: "glm-4.7",
    label: "GLM-4.7",
    tags: ["代码"],
    bindings: { "volc-coding": "glm-4.7" },
  },
  {
    id: "kimi-k2.5",
    label: "Kimi K2.5",
    tags: ["对话"],
    bindings: { moonshot: "kimi-k2.5", "volc-coding": "kimi-k2.5" },
  },
  {
    id: "moonshot-v1-auto",
    label: "Moonshot V1 Auto",
    tags: ["对话"],
    bindings: { moonshot: "moonshot-v1-auto" },
  },
  {
    id: "moonshot-v1-128k",
    label: "Moonshot V1 128K",
    tags: ["对话"],
    bindings: { moonshot: "moonshot-v1-128k" },
  },
  {
    id: "moonshot-v1-vision",
    label: "Moonshot V1 Vision",
    tags: ["视觉"],
    vision: true,
    bindings: { moonshot: "moonshot-v1-128k-vision-preview" },
  },
  {
    id: "doubao-seed-2.0-pro",
    label: "豆包 Seed 2.0 Pro",
    tags: ["对话"],
    bindings: { "volc-agent": "doubao-seed-2.0-pro", "volc-coding": "doubao-seed-2.0-pro" },
  },
  {
    id: "doubao-seed-2.0-lite",
    label: "豆包 Seed 2.0 Lite",
    tags: ["对话"],
    bindings: { "volc-agent": "doubao-seed-2.0-lite" },
  },
  {
    id: "doubao-seed-2.0-mini",
    label: "豆包 Seed 2.0 Mini",
    tags: ["对话"],
    bindings: { "volc-agent": "doubao-seed-2.0-mini" },
  },
  {
    id: "doubao-seed-2.0-code",
    label: "豆包 Seed 2.0 Code",
    tags: ["代码"],
    bindings: { "volc-agent": "doubao-seed-2.0-code", "volc-coding": "doubao-seed-2.0-code" },
  },
  {
    id: "ark-code-latest",
    label: "方舟 Code Latest",
    tags: ["代码"],
    bindings: { "volc-coding": "ark-code-latest" },
  },
  {
    id: "gpt-4o-mini",
    label: "GPT-4o Mini",
    tags: ["对话", "视觉"],
    vision: true,
    bindings: { openai: "gpt-4o-mini", openrouter: "openai/gpt-4o-mini" },
  },
  {
    id: "gpt-4o",
    label: "GPT-4o",
    tags: ["对话", "视觉"],
    vision: true,
    bindings: { openai: "gpt-4o", openrouter: "openai/gpt-4o" },
  },
  {
    id: "gpt-4.1",
    label: "GPT-4.1",
    tags: ["对话", "视觉"],
    vision: true,
    bindings: { openai: "gpt-4.1" },
  },
  {
    id: "o4-mini",
    label: "O4 Mini",
    tags: ["推理"],
    thinking: true,
    bindings: { openai: "o4-mini" },
  },
  {
    id: "gemini-2.5-flash",
    label: "Gemini 2.5 Flash",
    tags: ["对话", "视觉"],
    vision: true,
    bindings: { "google-gemini": "gemini-2.5-flash", openrouter: "google/gemini-2.5-flash" },
  },
  {
    id: "gemini-3.7-flash",
    label: "Gemini 3.7 Flash",
    tags: ["对话", "视觉"],
    vision: true,
    bindings: { "google-gemini": "gemini-3.7-flash" },
  },
  {
    id: "gemini-3-flash-preview",
    label: "Gemini 3 Flash Preview",
    tags: ["对话", "视觉"],
    vision: true,
    bindings: { "google-gemini": "gemini-3-flash-preview" },
  },
  {
    id: "gemini-2.5-pro",
    label: "Gemini 2.5 Pro",
    tags: ["对话", "视觉"],
    vision: true,
    bindings: { "google-gemini": "gemini-2.5-pro" },
  },
  {
    id: "gemini-2.0-flash",
    label: "Gemini 2.0 Flash",
    tags: ["对话", "视觉"],
    vision: true,
    bindings: { "google-gemini": "gemini-2.0-flash" },
  },
  {
    id: "claude-sonnet-4",
    label: "Claude Sonnet 4",
    tags: ["对话", "视觉"],
    vision: true,
    bindings: { openrouter: "anthropic/claude-sonnet-4", agentrouter: "claude-sonnet-4-5-20250929" },
  },
  {
    id: "claude-opus-4-6",
    label: "Claude Opus 4.6",
    tags: ["对话", "推理"],
    vision: true,
    thinking: true,
    bindings: { agentrouter: "claude-opus-4-6" },
  },
  {
    id: "claude-opus-4-7",
    label: "Claude Opus 4.7",
    tags: ["对话", "推理"],
    vision: true,
    thinking: true,
    bindings: { agentrouter: "claude-opus-4-7" },
  },
  {
    id: "deepseek-r1",
    label: "DeepSeek R1",
    tags: ["推理"],
    thinking: true,
    bindings: { agentrouter: "deepseek-r1-0528", openrouter: "deepseek/deepseek-reasoner" },
  },
  {
    id: "deepseek-v3.1",
    label: "DeepSeek V3.1",
    tags: ["对话"],
    bindings: { agentrouter: "deepseek-v3.1" },
  },
  {
    id: "deepseek-v3.2",
    label: "DeepSeek V3.2",
    tags: ["对话"],
    bindings: { agentrouter: "deepseek-v3.2" },
  },
  {
    id: "glm-4.6",
    label: "GLM 4.6",
    tags: ["对话", "推理"],
    thinking: true,
    bindings: { agentrouter: "glm-4.6" },
  },
  {
    id: "glm-5.1",
    label: "GLM 5.1",
    tags: ["对话", "推理"],
    thinking: true,
    bindings: { agentrouter: "glm-5.1" },
  },
  {
    id: "hunyuan-turbos",
    label: "混元 Turbos",
    tags: ["对话"],
    bindings: { hunyuan: "hunyuan-turbos-latest" },
  },
  {
    id: "hunyuan-large",
    label: "混元 Large",
    tags: ["对话"],
    bindings: { hunyuan: "hunyuan-large" },
  },
  {
    id: "hunyuan-lite",
    label: "混元 Lite",
    tags: ["对话"],
    bindings: { hunyuan: "hunyuan-lite" },
  },
  {
    id: "hunyuan-vision",
    label: "混元 Vision",
    tags: ["视觉"],
    vision: true,
    bindings: { hunyuan: "hunyuan-vision" },
  },
  {
    id: "minimax-m2.5",
    label: "MiniMax M2.5",
    tags: ["对话"],
    bindings: { minimax: "MiniMax-M2.5" },
  },
  {
    id: "minimax-text-01",
    label: "MiniMax Text 01",
    tags: ["对话"],
    bindings: { minimax: "MiniMax-Text-01" },
  },
  {
    id: "abab6.5s-chat",
    label: "ABAB 6.5s Chat",
    tags: ["对话"],
    bindings: { minimax: "abab6.5s-chat" },
  },
];

const catalogById = new Map(MODEL_CATALOG.map((m) => [m.id, m]));

export function catalogModel(id: string): CatalogModel | undefined {
  return catalogById.get(id.trim());
}

export function catalogForProvider(providerId: string | undefined): CatalogModel[] {
  if (!providerId) return MODEL_CATALOG;
  return MODEL_CATALOG.filter((m) => Boolean(apiModelForProvider(m.id, providerId)));
}

export function catalogVisionModels(): CatalogModel[] {
  return MODEL_CATALOG.filter((m) => m.vision);
}

export function apiModelForProvider(
  catalogId: string,
  providerId: string | undefined,
  customApiModel?: string
): string | undefined {
  if (catalogId === CUSTOM_CATALOG_MODEL) {
    return customApiModel?.trim() || undefined;
  }
  const entry = catalogModel(catalogId);
  if (!entry) return catalogId.trim() || undefined;
  if (providerId && entry.bindings[providerId]) {
    return entry.bindings[providerId];
  }
  const bound = Object.values(entry.bindings);
  if (bound.length === 1) return bound[0];
  return catalogId;
}

export function resolveApiModel(
  catalogId: string,
  baseUrl: string,
  customApiModel?: string
): string {
  const provider = providerByBaseUrl(baseUrl);
  return (
    apiModelForProvider(catalogId, provider?.id, customApiModel) ??
    customApiModel?.trim() ??
    catalogId
  );
}

export function inferCatalogModel(apiModel: string, baseUrl: string): string {
  const model = apiModel.trim();
  if (!model) return "";
  const provider = providerByBaseUrl(baseUrl);
  if (provider) {
    for (const entry of MODEL_CATALOG) {
      if (entry.bindings[provider.id] === model) return entry.id;
    }
  }
  for (const entry of MODEL_CATALOG) {
    if (entry.id === model) return entry.id;
    if (Object.values(entry.bindings).includes(model)) return entry.id;
  }
  return CUSTOM_CATALOG_MODEL;
}

export function catalogSupportsVision(catalogId: string, apiModel?: string): boolean {
  if (catalogId === CUSTOM_CATALOG_MODEL) {
    return inferVisionFromApiName(apiModel ?? "");
  }
  const entry = catalogModel(catalogId);
  if (entry?.vision) return true;
  return inferVisionFromApiName(apiModel ?? catalogId);
}

function inferVisionFromApiName(name: string): boolean {
  const s = name.trim().toLowerCase();
  if (!s) return false;
  if (s.includes("vision") || s.includes("-vl") || s.includes("_vl") || s.includes("/vl")) {
    return true;
  }
  if (/(?:^|[/.])gpt-4o(?:-|$)|(?:^|[/.])gpt-4\.1(?:-|$)/.test(s)) return true;
  if (s.includes("gemini") || s.includes("claude")) return true;
  if (/glm-4(?:\.\d+)?v/.test(s)) return true;
  return false;
}

export function catalogOptionLabel(entry: CatalogModel, providerId?: string): string {
  const tags = entry.tags?.length ? ` · ${entry.tags.join("/")}` : "";
  const api = providerId ? apiModelForProvider(entry.id, providerId) : undefined;
  const apiHint = api && api !== entry.id ? ` → ${api}` : "";
  return `${entry.label}${tags}${apiHint}`;
}

export function bindingSummary(entry: CatalogModel): string {
  return Object.entries(entry.bindings)
    .map(([pid, api]) => `${pid}: ${api}`)
    .join(" · ") || "—";
}

export type CatalogGroup = "对话" | "代码" | "推理" | "视觉";

export function catalogByGroup(): Record<CatalogGroup, CatalogModel[]> {
  const groups: Record<CatalogGroup, CatalogModel[]> = {
    对话: [],
    代码: [],
    推理: [],
    视觉: [],
  };
  for (const m of MODEL_CATALOG) {
    if (m.vision && !m.tags?.some((t) => t !== "视觉")) {
      groups.视觉.push(m);
      continue;
    }
    const tag = m.tags?.find((t) => t === "代码" || t === "推理") ?? "对话";
    groups[tag === "代码" || tag === "推理" ? tag : "对话"].push(m);
  }
  return groups;
}
