import type { LlmProfile } from "./ipc";
import {
  CUSTOM_CATALOG_MODEL,
  MODEL_CATALOG,
  catalogForProvider,
  catalogModel,
  catalogOptionLabel,
  catalogSupportsVision,
  catalogContextWindow,
  catalogVisionModels,
  inferCatalogModel,
  resolveApiModel,
  sanitizeApiModel,
  apiModelForProvider,
} from "./model-catalog";
import {
  PROVIDER_GROUPS,
  PROVIDER_PRESETS,
  providerByBaseUrl,
  type ProviderGroup,
  type ProviderPreset,
} from "./providers";

export {
  CUSTOM_CATALOG_MODEL,
  MODEL_CATALOG,
  catalogByGroup,
  catalogForProvider,
  catalogModel,
  catalogOptionLabel,
  catalogSupportsVision,
  catalogContextWindow,
  catalogVisionModels,
  bindingSummary,
  inferCatalogModel,
  sanitizeApiModel,
} from "./model-catalog";
export {
  PROVIDER_GROUPS,
  PROVIDER_PRESETS,
  listedProviderPresets,
  providerByBaseUrl,
  providerById,
  normalizeBaseUrl,
} from "./providers";
export type { CatalogModel, CatalogGroup } from "./model-catalog";
export type { ProviderPreset, ProviderGroup } from "./providers";

/** @deprecated 兼容旧引用 */
export type LlmPreset = ProviderPreset & { baseUrl: string; model: string; models: never[] };

/** @deprecated 使用 PROVIDER_PRESETS + MODEL_CATALOG */
export const LLM_PRESETS: LlmPreset[] = PROVIDER_PRESETS.map((p) => ({
  ...p,
  baseUrl: p.baseUrl,
  model: p.defaultCatalogModel,
  models: [] as never[],
}));

export type LlmProfileStatus = "ready" | "no-key" | "incomplete";

export function llmProfileStatus(profile: {
  base_url: string;
  api_key: string;
  model: string;
  catalog_model?: string;
}): LlmProfileStatus {
  const catalogId = profile.catalog_model || inferCatalogModel(profile.model, profile.base_url);
  if (
    !profile.base_url.trim() ||
    !catalogId ||
    (catalogId === CUSTOM_CATALOG_MODEL && !profile.model.trim())
  ) {
    return "incomplete";
  }
  if (!profile.api_key.trim()) return "no-key";
  return "ready";
}

export function llmProfileStatusLabel(status: LlmProfileStatus): string {
  switch (status) {
    case "ready":
      return "可切换";
    case "no-key":
      return "待填 Key";
    case "incomplete":
      return "待完善";
  }
}

export function profileCatalogModel(profile: LlmProfile): string {
  if (profile.catalog_model?.trim()) return profile.catalog_model.trim();
  return inferCatalogModel(profile.model, profile.base_url);
}

export function profileCatalogLabel(profile: LlmProfile): string {
  const id = profileCatalogModel(profile);
  if (id === CUSTOM_CATALOG_MODEL) return profile.model || "自定义模型";
  return catalogModel(id)?.label ?? id;
}

export function syncProfileModel(profile: LlmProfile): LlmProfile {
  const catalogId = profileCatalogModel(profile);
  const model =
    catalogId === CUSTOM_CATALOG_MODEL
      ? sanitizeApiModel(profile.base_url, profile.model)
      : resolveApiModel(catalogId, profile.base_url, profile.model);
  return { ...profile, catalog_model: catalogId, model };
}

export function migrateProfile(profile: LlmProfile): LlmProfile {
  const model = sanitizeApiModel(profile.base_url, profile.model);
  const provider = providerByBaseUrl(profile.base_url);
  let catalogId = profile.catalog_model?.trim() || "";
  const bound =
    catalogId && catalogId !== CUSTOM_CATALOG_MODEL
      ? apiModelForProvider(catalogId, provider?.id)
      : undefined;
  if (!catalogId || catalogId === CUSTOM_CATALOG_MODEL || (provider && !bound)) {
    catalogId = inferCatalogModel(model || profile.model, profile.base_url);
  }
  return syncProfileModel({ ...profile, catalog_model: catalogId, model });
}

export function migrateSettingsProfiles(profiles: LlmProfile[]): LlmProfile[] {
  return profiles.map(migrateProfile);
}

export function presetByBaseUrl(baseUrl: string): ProviderPreset | undefined {
  return providerByBaseUrl(baseUrl);
}

export function modelsForBaseUrl(baseUrl: string): { id: string; vision?: boolean }[] {
  const provider = providerByBaseUrl(baseUrl);
  return catalogForProvider(provider?.id).map((m) => ({
    id: m.id,
    vision: m.vision,
  }));
}

export function lookupModel(
  modelOrCatalogId: string,
  baseUrl?: string
): { id: string; vision?: boolean } {
  const id = modelOrCatalogId.trim();
  const catalog = catalogModel(id);
  if (catalog) {
    return { id: catalog.id, vision: catalog.vision };
  }
  if (baseUrl) {
    const inferred = inferCatalogModel(id, baseUrl);
    const entry = catalogModel(inferred);
    if (entry) return { id: entry.id, vision: entry.vision };
  }
  return { id, vision: catalogSupportsVision(CUSTOM_CATALOG_MODEL, id) };
}

export function modelOptionLabel(
  entry: { id: string; vision?: boolean },
  providerId?: string
): string {
  const catalog = catalogModel(entry.id);
  if (catalog) return catalogOptionLabel(catalog, providerId);
  if (entry.vision) return `${entry.id} · 支持图像`;
  return entry.id;
}

export function allKnownModels(): { id: string; vision?: boolean }[] {
  return MODEL_CATALOG.map((m) => ({ id: m.id, vision: m.vision }));
}

export function applyProviderPreset(
  profile: LlmProfile,
  preset: ProviderPreset,
  keepKey = true
): LlmProfile {
  const catalogId = profileCatalogModel(profile);
  const nextCatalog =
    catalogId && catalogId !== CUSTOM_CATALOG_MODEL && apiModelExistsForProvider(catalogId, preset.id)
      ? catalogId
      : preset.defaultCatalogModel;
  const next: LlmProfile = {
    ...profile,
    name: profile.name.trim() ? profile.name : preset.name,
    base_url: preset.baseUrl,
    catalog_model: nextCatalog,
    thinking_enabled: preset.thinking ?? profile.thinking_enabled,
    api_key: keepKey ? profile.api_key : "",
  };
  return syncProfileModel(next);
}

function apiModelExistsForProvider(catalogId: string, providerId: string): boolean {
  const entry = catalogModel(catalogId);
  return Boolean(entry?.bindings[providerId]);
}

export function patchProfileCatalog(
  profile: LlmProfile,
  catalogId: string,
  customApiModel?: string
): LlmProfile {
  const next: LlmProfile = {
    ...profile,
    catalog_model: catalogId,
    model: catalogId === CUSTOM_CATALOG_MODEL ? (customApiModel ?? profile.model) : profile.model,
  };
  return syncProfileModel(next);
}

/** @deprecated 使用 catalogVisionModels */
export const VISION_MODELS = catalogVisionModels().map((m) => ({
  id: m.id,
  vision: true as const,
}));
