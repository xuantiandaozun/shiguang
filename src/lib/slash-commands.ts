import type { AutomationWorkflow } from "./ipc";

export type SlashCommandKind = "workflow" | "meta";

export type SlashCommand = {
  id: string;
  /** 输入 `/` 后的匹配串（通常为工作流名称） */
  trigger: string;
  label: string;
  description?: string;
  kind: SlashCommandKind;
  workflowId?: number;
};

export function buildSlashCommands(workflows: AutomationWorkflow[]): SlashCommand[] {
  const items: SlashCommand[] = [
    {
      id: "help",
      trigger: "help",
      label: "help",
      description: "查看可用 / 快捷命令",
      kind: "meta",
    },
  ];
  for (const w of workflows) {
    items.push({
      id: `workflow-${w.id}`,
      trigger: w.name,
      label: w.name,
      description: w.description?.trim() || "运行此工作流",
      kind: "workflow",
      workflowId: w.id,
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
