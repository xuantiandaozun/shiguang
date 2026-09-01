import type { SlashCommand } from "../lib/slash-commands";

export default function ChatSlashMenu({
  commands,
  activeIndex,
  onPick,
}: {
  commands: SlashCommand[];
  activeIndex: number;
  onPick: (cmd: SlashCommand) => void;
}) {
  if (!commands.length) {
    return (
      <div className="absolute bottom-full left-0 right-0 mb-1 rounded-lg border border-slate-700/80 bg-slate-900/98 shadow-xl px-3 py-2 text-xs text-slate-500">
        没有匹配的命令。在主面板「工作流」页创建后，可用 /工作流名 快速运行。
      </div>
    );
  }

  const meta = commands.filter((c) => c.kind === "meta");
  const workflows = commands.filter((c) => c.kind === "workflow");

  let idx = 0;

  const renderItem = (cmd: SlashCommand) => {
    const i = idx++;
    const active = i === activeIndex;
    return (
      <button
        key={cmd.id}
        type="button"
        onMouseDown={(e) => {
          e.preventDefault();
          onPick(cmd);
        }}
        className={`w-full text-left px-2.5 py-2 rounded-md transition ${
          active ? "bg-sky-500/20 text-sky-100" : "text-slate-200 hover:bg-slate-800/80"
        }`}
      >
        <div className="flex items-center gap-2">
          <span className="font-mono text-sky-300/90 shrink-0">/{cmd.trigger}</span>
          {cmd.kind === "workflow" && (
            <span className="text-[10px] px-1 py-0.5 rounded bg-indigo-500/15 text-indigo-300 shrink-0">
              工作流
            </span>
          )}
        </div>
        {cmd.description && (
          <div className="text-[11px] text-slate-500 mt-0.5 leading-4 truncate">{cmd.description}</div>
        )}
      </button>
    );
  };

  return (
    <div className="absolute bottom-full left-0 right-0 mb-1 max-h-56 overflow-y-auto scrollbar-thin rounded-lg border border-slate-700/80 bg-slate-900/98 shadow-xl py-1.5 px-1">
      {meta.map(renderItem)}
      {workflows.length > 0 && (
        <>
          {meta.length > 0 && (
            <div className="px-2.5 py-1 text-[10px] text-slate-600 uppercase tracking-wide">工作流</div>
          )}
          {workflows.map(renderItem)}
        </>
      )}
      <div className="px-2.5 pt-1.5 text-[10px] text-slate-600 border-t border-slate-800/80 mt-1">
        ↑↓ 选择 · Enter 运行 · Esc 关闭
      </div>
    </div>
  );
}
