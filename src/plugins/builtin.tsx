// 内置插件引用的 React 组件（plugin.json 里的 "component": "builtin:名字"）。
// 只有内置插件可以用 component；用户插件用 entry（iframe 页面）或 view（声明式 JSON）。
import React from "react";
import FloatBall from "../windows/FloatBall";
import ChatPanel from "../windows/ChatPanel";
import MainWindow from "../windows/MainWindow";
import Reminder from "../windows/Reminder";
import DelegationWindow from "../windows/DelegationWindow";
import AgentRunWindow from "../windows/AgentRunWindow";
import ReplyReader from "../windows/ReplyReader";
import TodosTab from "../components/TodosTab";
import TasksTab from "../components/TasksTab";
import ServersTab from "../components/ServersTab";
import DelegationsTab from "../components/DelegationsTab";
import WorkspacesTab from "../components/WorkspacesTab";
import RecorderTab from "../components/RecorderTab";
import SkillsTab from "../components/SkillsTab";
import UsageTab from "../components/UsageTab";
import SettingsTab from "../components/SettingsTab";
import PluginsTab from "./PluginsTab";

const ServerTerminal = React.lazy(() => import("../windows/ServerTerminal"));

// 用函数延迟建表：MainWindow 等组件又会引用插件挂载模块，避免循环导入时取到未初始化的绑定。
const table = (): Record<string, React.ComponentType> => ({
  FloatBall,
  ChatPanel,
  MainWindow,
  Reminder,
  DelegationWindow,
  AgentRunWindow,
  ReplyReader,
  ServerTerminal,
  TodosTab,
  TasksTab,
  ServersTab,
  DelegationsTab,
  WorkspacesTab,
  RecorderTab,
  SkillsTab,
  UsageTab,
  SettingsTab,
  PluginsTab,
});
let cached: Record<string, React.ComponentType> | null = null;

export function builtinComponent(ref?: string): React.ComponentType | null {
  if (!ref?.startsWith("builtin:")) return null;
  cached ??= table();
  return cached[ref.slice("builtin:".length)] ?? null;
}

/** 内置窗口路由。插件清单加载前也能立即渲染，避免启动闪屏。 */
export const BUILTIN_WINDOWS: Record<string, string> = {
  floatball: "builtin:FloatBall",
  chat: "builtin:ChatPanel",
  main: "builtin:MainWindow",
  terminal: "builtin:ServerTerminal",
  reminder: "builtin:Reminder",
  delegation: "builtin:DelegationWindow",
  "agent-run": "builtin:AgentRunWindow",
  "reply-reader": "builtin:ReplyReader",
};
