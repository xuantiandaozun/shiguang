import type { AutomationWorkflow, PluginSlashCommand } from "./ipc";

/** workflow=运行工作流；meta=内置命令（help）；prompt=插件提供，把预设文字填进输入框 */
export type SlashCommandKind = "workflow" | "meta" | "prompt";

export type SlashCommand = {
  id: string;
  /** 输入 `/` 后的匹配串（通常为工作流名称） */
  trigger: string;
  label: string;
  description?: string;
  kind: SlashCommandKind;
  workflowId?: number;
  /** kind=prompt 时填入输入框的文字 */
  prompt?: string;
};

export function buildSlashCommands(
  workflows: AutomationWorkflow[],
  pluginCommands: Array<PluginSlashCommand & { plugin: string }> = []
): SlashCommand[] {
  const items: SlashCommand[] = [];
  for (const w of workflows) {
    const name = w.name?.trim();
    if (!name) continue;
    items.push({
      id: `workflow-${w.id}`,
      trigger: name,
      label: name,
      description: w.description?.trim() || "运行此工作流",
      kind: "workflow",
      workflowId: w.id,
    });
  }
  // 插件命令：prompt 类型；core 插件声明的 help 等内置命令为 meta。
  for (const c of pluginCommands) {
    const kind: SlashCommandKind = c.kind === "meta" ? "meta" : "prompt";
    if (kind === "meta" && c.trigger !== "help") continue;
    if (items.some((i) => i.trigger.toLowerCase() === c.trigger.toLowerCase())) continue;
    items.push({
      id: kind === "meta" ? c.trigger : `plugin-${c.plugin}-${c.trigger}`,
      trigger: c.trigger,
      label: c.label ?? c.trigger,
      description: c.description,
      kind,
      prompt: c.prompt,
    });
  }
  if (!items.some((i) => i.id === "help")) {
    items.push({
      id: "help",
      trigger: "help",
      label: "help",
      description: "查看可用 / 快捷命令",
      kind: "meta",
    });
  }
  return items;
}

/** `/` 后已输入的查询串（不含前导 `/`） */
export function filterSlashCommands(commands: SlashCommand[], query: string): SlashCommand[] {
  const q = query.trim().toLowerCase();
  if (!q) return commands;
  return commands.filter((c) => {
    const trigger = c.trigger.toLowerCase();
    const label = c.label.toLowerCase();
    const desc = c.description?.toLowerCase() ?? "";
    return trigger.includes(q) || label.includes(q) || desc.includes(q);
  });
}

/** 发送整行 `/xxx` 时解析（Enter 直接发送） */
export function resolveSlashLine(
  text: string,
  commands: SlashCommand[]
): SlashCommand | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("/")) return null;
  const query = trimmed.slice(1).trim();
  if (!query) return null;
  const lower = query.toLowerCase();
  const exact = commands.find(
    (c) => c.trigger.toLowerCase() === lower || c.label.toLowerCase() === lower
  );
  if (exact) return exact;
  const wfMatches = commands.filter((c) => c.kind === "workflow");
  if (wfMatches.length === 1 && wfMatches[0].trigger.toLowerCase().startsWith(lower)) {
    return wfMatches[0];
  }
  return null;
}

export function slashInsertText(cmd: SlashCommand): string {
  return `/${cmd.trigger}`;
}
