import type { TerminalAiMemory } from "./useTerminalFiles";

export interface TerminalTaskCheckpoint {
  request: string;
  summary: string;
  related_paths: string[];
  working_directory?: string;
  checked_at: string;
}

// Explicit field selection prevents command output and other transient data from
// leaking into subsequent turns, even if the caller has a larger memory object.
export function createTerminalCheckpoint(request: string, summary: string, memory: TerminalAiMemory, checkedAt = new Date().toISOString()): TerminalTaskCheckpoint {
  return {
    request: request.slice(0, 1200),
    summary: summary.slice(0, 4000),
    related_paths: [...new Set(memory.related_paths)].slice(-20),
    working_directory: memory.working_directory,
    checked_at: checkedAt,
  };
}

export function terminalResultExcerpt(command: string, description: string, output: string, marker: string, exitCode: string) {
  let cleaned = output.replace(/\u001b\[[0-?]*[ -/]*[@-~]/g, "").replace(/\r/g, "");
  const start = new RegExp(`(?:^|\\n)${marker}:BEGIN\\n`).exec(cleaned);
  if (start) cleaned = cleaned.slice(start.index + start[0].length);
  const clean = cleaned.split(command).join("").split("\n").filter(line => !line.includes(marker)).join("\n").trim();
  const excerpt = clean.length > 6000 ? `${clean.slice(0, 2000)}\n[中间输出已省略]\n${clean.slice(-4000)}` : clean;
  return `操作：${description.slice(0, 600)}\n退出码：${exitCode}\n本步必要输出：${excerpt}`;
}
