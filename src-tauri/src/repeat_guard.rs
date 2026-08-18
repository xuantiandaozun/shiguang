//! 重复工具调用的劝告：同一工具、相同参数连续打到阈值时提醒模型换方法。
//! 只追加模型可见说明，不拦截、不改工具结果。记账类工具对计数透明。

use serde_json::{json, Map, Value};

const THRESHOLDS: &[usize] = &[3, 5, 8];
const ARGS_PREVIEW_CHARS: usize = 500;

/// 夹在重复调用中间时，既不累加也不清零，避免把死循环「洗掉」。
const TRANSPARENT_TOOLS: &[&str] = &[
    "list_todos",
    "list_skills",
    "list_tasks",
    "list_profile",
    "lookup_cache",
    "get_tool_call_history",
];

#[derive(Debug, Default)]
pub struct RepeatGuard {
    last_key: Option<String>,
    count: usize,
}

impl RepeatGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次工具调用。若命中阈值，返回应追加到对话里的提醒正文。
    pub fn observe(&mut self, tool_name: &str, arguments: &str) -> Option<String> {
        if is_transparent(tool_name) {
            return None;
        }
        let key = chain_key(tool_name, arguments);
        if self.last_key.as_deref() == Some(key.as_str()) {
            self.count += 1;
        } else {
            self.last_key = Some(key);
            self.count = 1;
        }
        reminder_for(tool_name, arguments, self.count)
    }
}

fn is_transparent(tool_name: &str) -> bool {
    TRANSPARENT_TOOLS.contains(&tool_name)
}

fn chain_key(tool_name: &str, arguments: &str) -> String {
    format!("{tool_name}\n{}", canonical_args(arguments))
}

fn canonical_args(raw: &str) -> String {
    match serde_json::from_str::<Value>(raw.trim()) {
        Ok(value) => serde_json::to_string(&sort_value(&value)).unwrap_or_else(|_| raw.trim().to_string()),
        Err(_) => raw.trim().to_string(),
    }
}

fn sort_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for key in keys {
                out.insert(key.clone(), sort_value(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_value).collect()),
        other => other.clone(),
    }
}

fn reminder_for(tool_name: &str, arguments: &str, count: usize) -> Option<String> {
    let position = THRESHOLDS.iter().position(|&n| n == count)?;
    if position == 0 {
        return Some(
            "注意：你正在用完全相同的参数重复调用同一工具。请先看刚才的返回：若任务未完成，换参数或换方法，不要原样再打一次。"
                .to_string(),
        );
    }
    Some(format!(
        "注意：检测到重复工具调用：\n- 工具：{tool_name}\n- 连续次数：{count}\n- 参数：{}\n这些重复没有带来新进展。不要再用这组参数调用该工具。检查最近一次结果，换动作、换参数，或在证据已够时直接给结论。",
        args_preview(arguments)
    ))
}

fn args_preview(arguments: &str) -> String {
    let canonical = canonical_args(arguments);
    let total = canonical.chars().count();
    if total <= ARGS_PREVIEW_CHARS {
        return canonical;
    }
    let omitted = total - ARGS_PREVIEW_CHARS;
    format!(
        "{}…（另有 {omitted} 字）",
        canonical.chars().take(ARGS_PREVIEW_CHARS).collect::<String>()
    )
}

/// 追加到对话末尾、不入库、不给用户看的系统提醒。
pub fn reminder_message(text: &str) -> Value {
    json!({ "role": "system", "content": text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn third_identical_call_gets_generic_nudge() {
        let mut g = RepeatGuard::new();
        assert_eq!(g.observe("browser_snapshot", "{}"), None);
        assert_eq!(g.observe("browser_snapshot", "{}"), None);
        let third = g.observe("browser_snapshot", "{}").unwrap();
        assert!(third.contains("完全相同的参数"));
        assert!(!third.contains("连续次数"));
    }

    #[test]
    fn only_exact_thresholds_fire() {
        let mut g = RepeatGuard::new();
        let mut hits = Vec::new();
        for i in 1..=9 {
            if g.observe("search_files", r#"{"query":"发票"}"#).is_some() {
                hits.push(i);
            }
        }
        assert_eq!(hits, vec![3, 5, 8]);
    }

    #[test]
    fn later_threshold_names_tool_and_args() {
        let mut g = RepeatGuard::new();
        let mut fifth = None;
        for i in 1..=5 {
            let hit = g.observe("browser_click", r#"{"ref":1}"#);
            if i == 5 {
                fifth = hit;
            } else if i != 3 {
                assert!(hit.is_none(), "count {i} should be silent");
            }
        }
        let fifth = fifth.unwrap();
        assert!(fifth.contains("工具：browser_click"));
        assert!(fifth.contains("连续次数：5"));
        assert!(fifth.contains("\"ref\":1"));
    }

    #[test]
    fn different_arguments_reset_the_chain() {
        let mut g = RepeatGuard::new();
        assert!(g.observe("read_file", r#"{"path":"a.txt"}"#).is_none());
        assert!(g.observe("read_file", r#"{"path":"a.txt"}"#).is_none());
        assert!(g.observe("read_file", r#"{"path":"b.txt"}"#).is_none());
        assert!(g.observe("read_file", r#"{"path":"b.txt"}"#).is_none());
        assert!(g.observe("read_file", r#"{"path":"b.txt"}"#).unwrap().contains("完全相同"));
    }

    #[test]
    fn json_key_order_does_not_break_identity() {
        let mut g = RepeatGuard::new();
        g.observe("run_command", r#"{"argv":["git"],"workdir":"D:/repo"}"#);
        g.observe("run_command", r#"{"workdir":"D:/repo","argv":["git"]}"#);
        let third = g.observe("run_command", r#"{"argv":["git"],"workdir":"D:/repo"}"#);
        assert!(third.is_some());
    }

    #[test]
    fn transparent_tools_neither_count_nor_reset() {
        let mut g = RepeatGuard::new();
        g.observe("search_files", r#"{"query":"合同"}"#);
        assert!(g.observe("list_todos", "{}").is_none());
        assert!(g.observe("lookup_cache", r#"{"key":"x"}"#).is_none());
        g.observe("search_files", r#"{"query":"合同"}"#);
        let third = g.observe("search_files", r#"{"query":"合同"}"#).unwrap();
        assert!(third.contains("完全相同的参数"));
    }

    #[test]
    fn different_tracked_tool_resets_even_after_transparent() {
        let mut g = RepeatGuard::new();
        g.observe("browser_snapshot", "{}");
        g.observe("browser_snapshot", "{}");
        g.observe("list_skills", "{}");
        g.observe("read_file", r#"{"path":"a"}"#);
        assert!(g.observe("browser_snapshot", "{}").is_none());
    }

    #[test]
    fn long_arguments_are_previewed_not_dumped() {
        let mut g = RepeatGuard::new();
        let huge = format!(r#"{{"q":"{}"}}"#, "字".repeat(800));
        for _ in 0..4 {
            g.observe("discover_capabilities", &huge);
        }
        let fifth = g.observe("discover_capabilities", &huge).unwrap();
        assert!(fifth.contains("另有"));
        assert!(fifth.chars().count() < huge.chars().count());
    }
}
