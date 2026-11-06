import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface Todo {
  id: number;
  title: string;
  note: string;
  due_at: string | null;
  repeat_rule: string;
  priority: number;
  status: string;
  reminded: boolean;
  /** 提醒方式：notify=仅系统通知 / popup=弹窗 / popup_input=弹窗+输入框（内容发给 AI） */
  remind_mode: string;
  created_at: string;
  done_at: string | null;
}

export interface Rule {
  id: number;
  name: string;
  match_type: string;
  pattern: string;
  target_folder: string;
  enabled: boolean;
  approved: boolean;
  created_at: string;
}

export interface PlanCategory {
  name: string;
  action?: "move" | "delete";
  target_folder: string;
  files: string[];
}

export interface Plan {
  id: number;
  summary: string;
  categories: PlanCategory[];
  status: string;
  batch_id: string | null;
  created_at: string;
}

export interface BatchPath {
  src: string;
  dst: string;
  op_type: string;
}

export interface BatchSummary {
  batch_id: string;
  created_at: string;
  count: number;
  undone: boolean;
  paths: BatchPath[];
}

export interface LlmProfile {
  id: string;
  name: string;
  base_url: string;
  api_key: string;
  /** 统一模型目录 id；__custom__ 表示手写 API 模型名 */
  catalog_model?: string;
  /** 实际发给 API 的模型名（由目录解析） */
  model: string;
  thinking_enabled: boolean;
  reasoning_effort: string;
  /** 自定义模型是否支持直接接收图像；未设置时按模型目录/名称自动判断 */
  supports_vision?: boolean | null;
  models?: ProfileModel[];
  valid_until?: string;
}

export interface ProfileModel {
  model: string;
  label: string;
  supports_vision: boolean;
  context_window: number;
}

export interface SubagentRole {
  role: "research" | "files" | "browser";
  enabled: boolean;
  profile_id: string;
  model: string;
}

export interface Settings {
  base_url: string;
  api_key: string;
  model: string;
  llm_profiles: LlmProfile[];
  active_llm_profile_id: string;
  organize_root: string;
  auto_organize: boolean;
  autostart: boolean;
  desktop_path: string;
  thinking_enabled: boolean;
  reasoning_effort: string;
  vision_base_url: string;
  vision_api_key: string;
  vision_model: string;
  /** 视觉模型在统一目录中的 id */
  vision_catalog_model?: string;
  subagent_thinking_enabled: boolean;
  subagent_reasoning_effort: string;
  subagent_model: string;
  subagent_roles: SubagentRole[];
  compact_model: string;
  context_window: number;
  /** 当前生效主模型的最终识图能力（由档案配置派生） */
  model_supports_vision: boolean;
  read_image_enabled: boolean;
  system_one_enabled: boolean;
  system_one_base_url: string;
  system_one_api_key: string;
  system_one_model: string;
  system_one_min_confidence: number;
  system_one_browser_assist: boolean;
  system_one_context_assist: boolean;
  /** 子代理模型在统一目录中的 id */
  subagent_catalog_model?: string;
  command_tools_enabled: boolean;
  /** confirmation=每次确认 / balanced=仅敏感操作确认 / autopilot=非危险操作自动执行 */
  permission_level: string;
  profile_name: string;
  profile_alias: string;
  profile_gender: string;
  profile_birth: string;
  profile_phone: string;
  profile_email: string;
  profile_city: string;
  temp_path: string;
}

export interface TempInfo {
  path: string;
  file_count: number;
  dir_count: number;
  total_bytes: number;
}

export interface ProfileEntry {
  id: number;
  label: string;
  content: string;
  updated_at: string;
}

export interface ChatMsg {
  id: number;
  role: string;
  content: string;
  created_at: string;
}

export interface SessionInfo {
  id: number;
  title: string;
  created_at: string;
  updated_at: string;
  msg_count: number;
}

export interface SessionView {
  session_id: number;
  messages: ChatMsg[];
}

export interface WorkspaceLink { label: string; url: string }
export interface WorkspaceData {
  aliases?: string[];
  summary?: string;
  deploy?: string;
  verify_commands?: string[];
  tools?: string;
  links?: (WorkspaceLink | string)[];
  rules?: string[];
  notes?: string[];
  default_agent?: CodeAgentKind | "";
  agent_mode?: "plan" | "edit" | "full" | "";
}
export interface Workspace {
  id: number;
  name: string;
  root: string;
  data: WorkspaceData;
  last_used_at: string | null;
  created_at: string;
  updated_at: string;
}
export type CodeAgentKind = "claude_code" | "codex" | "cursor" | "pi";
export const CODE_AGENT_LABELS: Record<CodeAgentKind, string> = { claude_code: "Claude Code", codex: "Codex", cursor: "Cursor", pi: "Pi" };
export interface CodeAgentInfo {
  kind: CodeAgentKind;
  label: string;
  path: string | null;
  version: string | null;
  enabled: boolean;
  available: boolean;
  path_override: string;
}
export type AgentRunStatus = "running" | "asking" | "idle" | "error" | "cancelled" | "closed";
export interface AgentRun {
  id: string;
  agent: CodeAgentKind;
  workspace_id: number | null;
  session_id: number;
  title: string;
  root: string;
  mode: string;
  status: AgentRunStatus;
  data: Record<string, any>;
  created_at: string;
  updated_at: string;
}
export interface AgentRunEvent {
  id: number;
  run_id: string;
  kind: string;
  content: string;
  created_at: string;
}
export interface AgentRunView {
  run: AgentRun;
  events: AgentRunEvent[];
  agent_label: string;
}

export interface DelegationTask {
  id: string;
  title: string;
  origin: string;
  status: "running" | "waiting_peer" | "waiting_user" | "complete" | "blocked" | "error" | "cancelled";
  question: string | null;
  result: string | null;
  created_at: string;
  updated_at: string;
}

export interface DelegationMessage {
  id: number;
  task_id: string;
  role: "external" | "user" | "shiguang" | "tool" | "system";
  content: string;
  created_at: string;
}

export interface DelegationView {
  task: DelegationTask;
  messages: DelegationMessage[];
}

export interface BgTask {
  id: string;
  label: string;
  command: string;
  shell: string;
  shell_selection: string;
  transport: string;
  success_exit_codes: number[];
  /** running / done / failed / cancelled / timeout */
  status: string;
  exit_code: number | null;
  pid: number;
  started_at: string;
  finished_at: string | null;
  log_path: string;
}

export interface SkillInfo {
  name: string;
  description: string;
  enabled: boolean;
  /** internal = 应用内置只读；external = 用户目录可写 */
  scope: string;
  source: string;
  synced_from: string;
  path: string;
  updated_at: string;
}

export interface ExternalSkill {
  name: string;
  description: string;
  source: string;
  path: string;
  already_synced: boolean;
}

export interface SyncSkillsResult {
  imported: number;
  updated: number;
  skipped: number;
  errors: string[];
  details: unknown[];
  local_dir: string;
}

export interface AutomationWorkflow {
  id: number;
  name: string;
  description: string;
  prompt: string;
  schedule_rule: "manual" | "once" | "daily" | "weekly";
  next_run_at: string | null;
  enabled: boolean;
  run_count: number;
  last_run_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface AutomationWorkflowInput {
  id?: number;
  name: string;
  description: string;
  prompt: string;
  schedule_rule: string;
  next_run_at: string | null;
  enabled: boolean;
}

export interface BrowserRecipe {
  id: number;
  name: string;
  site_pattern: string;
  goal: string;
  recipe_json: string;
  verification_json: string;
  success_count: number;
  failure_count: number;
  last_used_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface ExecResult {
  plan_id: number;
  batch_id: string;
  moved: number;
  deleted: number;
  skipped: number;
}

export interface AskOption {
  label: string;
  description: string;
}

export interface AskQuestion {
  id: string;
  question: string;
  header?: string | null;
  options: AskOption[];
  multi_select: boolean;
  details?: string | null;
}

export interface AskUserPrompt {
  questions: AskQuestion[];
}

export interface AskAnswer {
  id: string;
  selected: string[];
  custom?: string | null;
}

export interface SessionTodo {
  content: string;
  status: "pending" | "in_progress" | "completed";
}

export interface SessionTodoSnapshot {
  session_id: number;
  todos: SessionTodo[];
}

export interface LlmUsageTotals {
  requests: number;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cache_hit_tokens: number;
  cache_miss_tokens: number;
}

export interface LlmUsageBySource {
  source: string;
  requests: number;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cache_hit_tokens: number;
  cache_miss_tokens: number;
}

export interface LlmUsageDay {
  day: string;
  requests: number;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cache_hit_tokens: number;
  cache_miss_tokens: number;
}

export interface LlmUsagePeriod {
  totals: LlmUsageTotals;
  by_source: LlmUsageBySource[];
}

export interface LlmUsageSnapshot {
  today: LlmUsagePeriod;
  last_7d: LlmUsagePeriod;
  all: LlmUsagePeriod;
  daily: LlmUsageDay[];
  recent: LlmUsageRequest[];
}

export interface LlmUsageRequest {
  id: number;
  source: string;
  model: string;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cache_hit_tokens: number;
  cache_miss_tokens: number;
  created_at: string;
}

export const ipc = {
  serverTerminal: <T = unknown>(action: string, serverId: string, extra: Record<string, unknown> = {}) => invoke<T>("server_terminal", { action, serverId, ...extra }),
  revealServerSecret: (serverId: string) => invoke<string>("reveal_server_secret", { serverId }),
  serverAction: <T = unknown>(name: string, args: Record<string, unknown> = {}) => invoke<T>("server_action", { name, args }),
  trustServerHost: (serverId: string, fingerprint: string) => invoke<ServerConnection>("trust_server_host", { serverId, fingerprint }),
  sendChat: (text: string, attachments?: string[]) =>
    invoke<number>("send_chat_message", { text, attachments: attachments ?? null }),
  importClipboardAttachments: (includeImage = true) =>
    invoke<{ paths: string[]; skipped_dirs: number }>("import_clipboard_attachments", {
      includeImage,
    }),
  savePastedFile: (filename: string, dataBase64: string) =>
    invoke<string>("save_pasted_file", { filename, dataBase64 }),
  stopChat: () => invoke<void>("stop_chat_message"),
  /** 在资源管理器中定位聊天消息里的文件（支持相对路径），返回解析后的完整路径 */
  revealInFolder: (path: string) => invoke<string>("reveal_in_folder", { path }),
  saveReplyMarkdown: (path: string, content: string) => invoke<void>("save_reply_markdown", { path, content }),
  getCurrentSession: () => invoke<SessionView>("get_current_session"),
  listSessions: () => invoke<SessionInfo[]>("list_sessions"),
  newSession: () => invoke<SessionView>("new_session"),
  switchSession: (id: number) => invoke<SessionView>("switch_session", { id }),
  deleteSession: (id: number) => invoke<SessionView>("delete_session", { id }),
  recallMessage: (id: number) => invoke<void>("recall_message", { id }),
  loadChat: () => invoke<ChatMsg[]>("load_chat_history"),
  clearChat: () => invoke<void>("clear_chat_history"),
  listDelegations: () => invoke<DelegationTask[]>("list_delegations"),
  getDelegation: (id: string) => invoke<DelegationView>("get_delegation", { id }),
  showDelegationWindow: (id: string) => invoke<void>("show_delegation_window", { id }),
  createDelegation: (title: string, goal: string) => invoke<DelegationTask>("create_delegation", { title, goal }),
  replyDelegation: (id: string, content: string) => invoke<DelegationTask>("reply_delegation", { id, content }),
  cancelDelegation: (id: string) => invoke<DelegationTask>("cancel_delegation", { id }),
  recorderStatus: () => invoke<RecorderStatus>("recorder_status"),
  recorderList: () => invoke<Recording[]>("recorder_list"),
  recorderStart: (title?: string, url?: string, opts?: RecorderStartOpts) =>
    invoke<{ id: string; title: string }>("recorder_start", { title: title || null, url: url || null, opts: opts ?? null }),
  recorderStop: () => invoke<{ id: string; duration_s: number }>("recorder_stop"),
  recorderExport: (id: string, opts?: RecorderExportOpts) => invoke<{ path: string }>("recorder_export", { id, opts: opts ?? null }),
  recorderCancelExport: () => invoke<void>("recorder_cancel_export"),
  recorderProject: (id: string) => invoke<RecorderProject>("recorder_project", { id }),
  recorderSaveTimeline: (id: string, timeline: TimelineDoc) => invoke<TimelineDoc>("recorder_save_timeline", { id, timeline }),
  recorderDelete: (id: string) => invoke<void>("recorder_delete", { id }),
  recorderReveal: (path: string) => invoke<void>("recorder_reveal", { path }),
  captureOverlayInit: (id: string) => invoke<{ image: string; width: number; height: number; windows: { hwnd: number; title: string; process: string; rect: [number, number, number, number] }[]; copy: boolean }>("capture_overlay_init", { id }),
  captureOverlayShow: () => invoke<void>("capture_overlay_show"),
  captureFinish: (id: string, kind: "region" | "window", rect: [number, number, number, number] | null, hwnd: number | null, copy: boolean | null) =>
    invoke<ScreenshotResult>("capture_finish", { id, kind, rect, hwnd, copy }),
  captureCancel: (id: string) => invoke<void>("capture_cancel", { id }),
  screenshotCapture: (mode: "interactive" | "screen" | "window", opts: { window?: string; includeSelf?: boolean; copy?: boolean } = {}) =>
    invoke<ScreenshotResult>("screenshot_capture", { mode, window: opts.window ?? null, includeSelf: opts.includeSelf ?? null, copy: opts.copy ?? null }),
  screenshotWindows: (includeSelf?: boolean) => invoke<ScreenWindow[]>("screenshot_windows", { includeSelf: includeSelf ?? null }),
  screenshotRecent: (limit?: number) => invoke<{ path: string; name: string }[]>("screenshot_recent", { limit: limit ?? null }),
  screenshotCopy: (path: string) => invoke<void>("screenshot_copy", { path }),
  listWorkspaces: () => invoke<Workspace[]>("list_workspaces"),
  saveWorkspace: (id: number, name: string, root: string, data: WorkspaceData) => invoke<Workspace>("save_workspace", { id, name, root, data }),
  deleteWorkspace: (id: number) => invoke<void>("delete_workspace", { id }),
  detectWorkspace: (root: string) => invoke<Record<string, any>>("detect_workspace", { root }),
  listCodeAgents: () => invoke<CodeAgentInfo[]>("list_code_agents"),
  setCodeAgent: (kind: string, enabled: boolean, path: string) => invoke<CodeAgentInfo[]>("set_code_agent", { kind, enabled, path }),
  listAgentRuns: () => invoke<AgentRun[]>("list_agent_runs"),
  getAgentRun: (id: string, after = 0) => invoke<AgentRunView>("get_agent_run", { id, after }),
  replyAgentRun: (id: string, content: string) => invoke<unknown>("reply_agent_run", { id, content }),
  stopAgentRun: (id: string) => invoke<unknown>("stop_agent_run", { id }),
  showAgentRunWindow: (id: string) => invoke<void>("show_agent_run_window", { id }),
  finishAgentRunWorktree: (id: string, action: "apply" | "discard") => invoke<unknown>("finish_agent_run_worktree", { id, action }),
  toggleChat: () => invoke<void>("toggle_chat"),
  openMain: () => invoke<void>("open_main_window"),
  hideChat: () => invoke<void>("hide_chat"),
  /** 在系统默认浏览器/程序中打开 http(s) 链接或本地文件 */
  openExternal: (target: string) => invoke<void>("open_external", { target }),
  getBrowserExtension: () =>
    invoke<{ path: string; ready: boolean }>("get_browser_extension_info"),
  openBrowserExtensionDir: () => invoke<void>("open_browser_extension_dir"),
  /** 幂等展示聊天窗（区别于 toggle 的开/关切换） */
  showChat: () => invoke<void>("show_chat_window"),

  listTodos: (filter?: string) => invoke<Todo[]>("list_todos", { filter }),
  addTodo: (
    title: string,
    note: string,
    dueAt: string | null,
    repeatRule: string,
    priority: number,
    remindMode?: string
  ) => invoke<Todo>("add_todo", { title, note, dueAt, repeatRule, priority, remindMode: remindMode ?? "notify" }),
  updateTodo: (
    id: number,
    title: string,
    note: string,
    dueAt: string | null,
    repeatRule: string,
    priority: number,
    remindMode?: string
  ) =>
    invoke<void>("update_todo", { id, title, note, dueAt, repeatRule, priority, remindMode: remindMode ?? "notify" }),
  deleteTodo: (id: number) => invoke<void>("delete_todo", { id }),
  setTodoDone: (id: number, done: boolean) => invoke<void>("set_todo_done", { id, done }),
  /** 把待办提醒延后 minutes 分钟，返回新的到期时间 */
  snoozeTodo: (id: number, minutes?: number) =>
    invoke<string>("snooze_todo_cmd", { id, minutes: minutes ?? 10 }),

  getPendingPlan: () => invoke<Plan | null>("get_pending_plan"),
  executePlan: (planId: number) => invoke<ExecResult>("execute_plan_cmd", { planId }),
  cancelPlan: (planId: number) => invoke<void>("cancel_plan", { planId }),
  getPendingAsk: () => invoke<AskUserPrompt | null>("get_pending_ask"),
  answerAskUser: (answers: AskAnswer[]) => invoke<void>("answer_ask_user", { answers }),
  dismissAskUser: () => invoke<void>("dismiss_ask_user"),
  getSessionTodos: (sessionId?: number | null) =>
    invoke<SessionTodo[]>("get_session_todos", { sessionId: sessionId ?? null }),

  listBatches: () => invoke<BatchSummary[]>("list_batches"),
  undoBatch: (batchId: string) => invoke<number>("undo_batch_cmd", { batchId }),

  listRules: () => invoke<Rule[]>("list_rules"),
  upsertRule: (rule: { id?: number; name: string; matchType: string; pattern: string; targetFolder: string }) =>
    invoke<number>("upsert_rule", { rule }),
  toggleRule: (id: number, enabled: boolean) => invoke<void>("toggle_rule", { id, enabled }),
  deleteRule: (id: number) => invoke<void>("delete_rule", { id }),

  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
  getTempInfo: () => invoke<TempInfo>("get_temp_info"),
  clearTempDir: () => invoke<TempInfo>("clear_temp_dir"),

  listProfileEntries: () => invoke<ProfileEntry[]>("list_profile_entries"),
  saveProfileEntry: (id: number | null, label: string, content: string) =>
    invoke<number>("save_profile_entry", { id, label, content }),
  deleteProfileEntry: (id: number) => invoke<void>("delete_profile_entry", { id }),

  listBgTasks: () => invoke<BgTask[]>("list_bg_tasks"),
  stopBgTask: (id: string) => invoke<void>("stop_bg_task", { id }),
  readBgTaskTail: (id: string, maxChars?: number) =>
    invoke<string>("read_bg_task_tail", { id, maxChars: maxChars ?? null }),

  listAutomationWorkflows: () => invoke<AutomationWorkflow[]>("list_automation_workflows"),
  saveAutomationWorkflow: (input: AutomationWorkflowInput) =>
    invoke<AutomationWorkflow>("save_automation_workflow", { input }),
  deleteAutomationWorkflow: (id: number) => invoke<void>("delete_automation_workflow", { id }),
  runAutomationWorkflow: (id: number) => invoke<number>("run_automation_workflow_cmd", { id }),
  listBrowserRecipes: () => invoke<BrowserRecipe[]>("list_browser_recipes_cmd"),
  deleteBrowserRecipe: (id: number) => invoke<void>("delete_browser_recipe_cmd", { id }),

  listSkills: () => invoke<SkillInfo[]>("list_skills_cmd"),
  createSkill: (name: string, description: string, body: string) =>
    invoke<SkillInfo>("create_skill_cmd", { name, description, body }),
  deleteSkill: (name: string) => invoke<void>("delete_skill_cmd", { name }),
  setSkillEnabled: (name: string, enabled: boolean) =>
    invoke<SkillInfo>("set_skill_enabled", { name, enabled }),
  scanExternalSkills: () => invoke<ExternalSkill[]>("scan_external_skills"),
  syncSkills: (source?: string | null, names?: string[] | null, overwrite?: boolean) =>
    invoke<SyncSkillsResult>("sync_skills_cmd", {
      source: source ?? null,
      names: names ?? null,
      overwrite: overwrite ?? false,
    }),
  readSkillContent: (name: string) => invoke<string>("read_skill_content", { name }),
  getLlmUsageStats: () => invoke<LlmUsageSnapshot>("get_llm_usage_stats"),

  // ---------- 插件 ----------
  listPlugins: () => invoke<PluginManifestList>("list_plugins"),
  reloadPlugins: () => invoke<PluginManifestList>("reload_plugins"),
  setPluginEnabled: (id: string, enabled: boolean) => invoke<unknown>("set_plugin_enabled", { id, enabled }),
  getPluginSettings: (id: string) => invoke<Record<string, unknown>>("get_plugin_settings", { id }),
  savePluginSettings: (id: string, values: Record<string, unknown>) =>
    invoke<Record<string, unknown>>("save_plugin_settings", { id, values }),
  pluginVersions: (id: string) => invoke<PluginVersion[]>("plugin_versions", { id }),
  rollbackPlugin: (id: string, version: string) => invoke<unknown>("rollback_plugin", { id, version }),
  readPluginView: (id: string, path: string) => invoke<unknown>("read_plugin_view", { id, path }),
  openPluginsDir: () => invoke<void>("open_plugins_dir"),
  setPluginSafeMode: (enabled: boolean) => invoke<PluginManifestList>("set_plugin_safe_mode", { enabled }),
  pluginsBootOk: () => invoke<void>("plugins_boot_ok"),
  pluginCallTool: <T = unknown>(pluginId: string, tool: string, args: Record<string, unknown> = {}) =>
    invoke<T>("plugin_call_tool", { pluginId, tool, args }),
  openPluginWindow: (pluginId: string, route: string, query?: string) =>
    invoke<void>("open_plugin_window", { pluginId, route, query: query ?? null }),
};

/** 插件界面挂载项：component（仅内置）/ entry（iframe 页面）/ view（声明式 JSON）三选一。 */
export interface PluginUiItem {
  id?: string;
  route?: string;
  label?: string;
  icon?: string;
  order?: number;
  group?: string;
  group_label?: string;
  nav?: boolean;
  lazy?: boolean;
  component?: string;
  entry?: string;
  view?: string;
}

export interface PluginSlashCommand {
  trigger: string;
  label?: string;
  description?: string;
  kind?: "meta" | "prompt";
  /** kind=prompt：选择后发给 AI 的文字 */
  prompt?: string;
}

export interface PluginUi {
  tabs?: PluginUiItem[];
  windows?: PluginUiItem[];
  tool_labels?: Record<string, string>;
  slash_commands?: PluginSlashCommand[];
  [slot: string]: unknown;
}

export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  description: string;
  order: number;
  builtin: boolean;
  enabled: boolean;
  permissions: string[];
  tools: string[];
  ui: PluginUi | null;
  settings_schema: JsonSchema | null;
}

export interface JsonSchema {
  type?: string;
  title?: string;
  description?: string;
  properties?: Record<string, JsonSchema & { default?: unknown; enum?: unknown[]; format?: string }>;
  required?: string[];
  [key: string]: unknown;
}

export interface PluginError {
  plugin: string;
  path: string;
  message: string;
}

export interface PluginManifestList {
  generation: number;
  safe_mode: boolean;
  plugins: PluginInfo[];
  tool_labels: Record<string, string>;
  errors: PluginError[];
}

export interface PluginVersion {
  version: string;
  reason: string | null;
  at: string | null;
}

export interface RecorderStatus {
  recording: boolean; id?: string; title?: string; url?: string; channel?: string;
  elapsed_s?: number; frames?: number; events?: number; clicks?: number; marks?: number; ended_reason?: string | null;
}
export interface Recording {
  id: string; title: string; url: string; started_at: string; duration_s: number; clicks: number; marks: number;
  size: [number, number]; end_reason: string | null; exported: string | null; exported_at: string | null; thumb: string; dir: string;
}
/** 录制来源：browser 网页标签页；screen 整块屏幕；select 让用户框选区域或点选窗口；window 按名称找窗口 */
export interface RecorderStartOpts { source?: "browser" | "screen" | "window" | "region" | "select"; window?: string; monitor?: number; include_self?: boolean }
export interface RecorderExportOpts { zoom?: boolean; style?: "plain" | "padded"; zoom_scale?: number; cursor_scale?: number; fps?: number; preview?: boolean; burn_subtitles?: boolean; tighten?: boolean }
export interface RecorderExportProgress { id: string; phase: "ffmpeg" | "audio" | "font" | "scene" | "render" | "done"; done?: number; total?: number; ok?: boolean; scene?: number; scenes?: number; font?: string }
export interface ScreenshotResult { path: string; width: number; height: number; copied: boolean }
export interface ScreenWindow { hwnd: number; pid: number; title: string; process: string; rect: [number, number, number, number]; minimized: boolean }
export interface ZoomSeg { start: number; end: number; scale: number; focus?: [number, number] }
export interface MaskSeg { start: number; end: number; rect: [number, number, number, number] }
export interface CutSeg { start: number; end: number }
export interface NarrationItem { at: number; text: string; audio?: string }
export interface MusicTrack { path: string; volume: number; duck: number; fade_in: number; fade_out: number }
export interface SubtitleStyle { preset?: string; [k: string]: unknown }
export interface SceneItem { id: string; kind: "insert" | "overlay"; at: number; duration: number; file: string; title?: string; template?: string; vars?: Record<string, unknown> }
export interface TimelineDoc {
  zooms_edited: boolean; zooms: ZoomSeg[]; masks: MaskSeg[]; cuts: CutSeg[]; narration: NarrationItem[]; burn_subtitles?: boolean | null;
  music?: MusicTrack | null; tighten?: boolean | null;
  subtitle_style?: SubtitleStyle | null; scenes?: SceneItem[];
}
export interface RecorderProject {
  meta: { id: string; title: string; width: number; height: number };
  duration: number; dir: string;
  frames: [number, string][];
  clicks: { t: number; label: string }[];
  marks: { t: number; label: string; note: string }[];
  timeline: TimelineDoc;
  preview: string | null;
  burn_default: boolean;
  tighten_default: boolean;
  music_library: string[];
  music_dir: string;
  subtitle_presets?: { id: string; label: string }[];
  scene_templates?: { id: string; label: string; overlay: boolean }[];
}

export function onEvent<T>(name: string, cb: (payload: T) => void): Promise<() => void> {
  return listen<T>(name, (e) => cb(e.payload));
}

export interface ServerConnection {
  id: string; name: string; host: string; port: number; username: string; group: string; note: string; auth: "password" | "key" | "agent"; private_key: string; fingerprint: string | null;
}
export interface RemoteTask {
  id: string; server_id: string; server_name: string; kind: string; status: string; bytes: number; exit_code: number | null; error: string | null; started_at: string; finished_at: string | null; log_path: string;
}
export interface RemoteTaskView { task: RemoteTask; output: string; output_offset: number; next_offset: number; has_more: boolean; }
