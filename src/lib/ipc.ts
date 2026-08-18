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
  model: string;
  thinking_enabled: boolean;
  reasoning_effort: string;
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
  subagent_thinking_enabled: boolean;
  subagent_reasoning_effort: string;
  subagent_model: string;
  command_tools_enabled: boolean;
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

export interface ExecResult {
  plan_id: number;
  batch_id: string;
  moved: number;
  deleted: number;
  skipped: number;
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
  sendChat: (text: string, attachments?: string[]) =>
    invoke<number>("send_chat_message", { text, attachments: attachments ?? null }),
  stopChat: () => invoke<void>("stop_chat_message"),
  getCurrentSession: () => invoke<SessionView>("get_current_session"),
  listSessions: () => invoke<SessionInfo[]>("list_sessions"),
  newSession: () => invoke<SessionView>("new_session"),
  switchSession: (id: number) => invoke<SessionView>("switch_session", { id }),
  deleteSession: (id: number) => invoke<SessionView>("delete_session", { id }),
  recallMessage: (id: number) => invoke<void>("recall_message", { id }),
  loadChat: () => invoke<ChatMsg[]>("load_chat_history"),
  clearChat: () => invoke<void>("clear_chat_history"),
  toggleChat: () => invoke<void>("toggle_chat"),
  openMain: () => invoke<void>("open_main_window"),
  hideChat: () => invoke<void>("hide_chat"),
  /** 在系统默认浏览器/程序中打开 http(s) 链接或本地文件 */
  openExternal: (target: string) => invoke<void>("open_external", { target }),
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
};

export function onEvent<T>(name: string, cb: (payload: T) => void): Promise<() => void> {
  return listen<T>(name, (e) => cb(e.payload));
}
