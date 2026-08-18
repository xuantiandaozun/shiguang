//! 工具结果的模型侧保留策略：超长输出保留头尾，全文落到临时目录。
//! 只回答「模型看见什么、省略了多少、完整内容在哪」；工具自己的业务字段仍由调用方决定。

use serde_json::Value;
use std::path::{Path, PathBuf};

/// 刚产出的工具结果：超过此字符数才裁剪并尝试落盘
pub const FRESH_THRESHOLD_CHARS: usize = 8_192;
const FRESH_HEAD_CHARS: usize = 4_096;
const FRESH_TAIL_CHARS: usize = 1_024;

/// 对话过长时，较早工具结果压到此规模（仍保留头尾和落盘路径）
pub const STALE_THRESHOLD_CHARS: usize = 2_000;
const STALE_HEAD_CHARS: usize = 800;
const STALE_TAIL_CHARS: usize = 200;

/// 对话总长度（序列化字节）超过该值时，压缩较早的工具结果
pub const CONTEXT_TRIM_BYTES: usize = 80_000;
const KEEP_RECENT_MESSAGES: usize = 8;

const OMISSION_MARK: &str = "[... 中段已省略";

#[derive(Debug, Clone, Copy)]
pub struct BoundConfig {
    pub threshold_chars: usize,
    pub head_chars: usize,
    pub tail_chars: usize,
}

pub const FRESH: BoundConfig = BoundConfig {
    threshold_chars: FRESH_THRESHOLD_CHARS,
    head_chars: FRESH_HEAD_CHARS,
    tail_chars: FRESH_TAIL_CHARS,
};

pub const STALE: BoundConfig = BoundConfig {
    threshold_chars: STALE_THRESHOLD_CHARS,
    head_chars: STALE_HEAD_CHARS,
    tail_chars: STALE_TAIL_CHARS,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundText {
    pub text: String,
    pub truncated: bool,
    pub omitted_chars: usize,
}

/// 按 Unicode 字符（非字节）做头尾保留。文本不超过阈值则原样返回。
pub fn bound_text(text: &str, cfg: BoundConfig, spill_path: Option<&str>) -> BoundText {
    let total = text.chars().count();
    if total <= cfg.threshold_chars {
        return BoundText {
            text: text.to_string(),
            truncated: false,
            omitted_chars: 0,
        };
    }
    let notice = omission_notice(
        total.saturating_sub(cfg.head_chars + cfg.tail_chars),
        spill_path,
    );
    let notice_chars = notice.chars().count();
    let mut head_chars = cfg.head_chars;
    let mut tail_chars = cfg.tail_chars;
    while head_chars + tail_chars + notice_chars > cfg.threshold_chars && head_chars > 0 {
        head_chars -= 1;
    }
    while head_chars + tail_chars + notice_chars > cfg.threshold_chars && tail_chars > 0 {
        tail_chars -= 1;
    }
    if head_chars + tail_chars >= total {
        return BoundText {
            text: text.to_string(),
            truncated: false,
            omitted_chars: 0,
        };
    }
    let omitted = total - head_chars - tail_chars;
    let notice = omission_notice(omitted, spill_path);
    BoundText {
        text: format!(
            "{}{}{}",
            take_chars(text, head_chars),
            notice,
            take_last_chars(text, tail_chars)
        ),
        truncated: true,
        omitted_chars: omitted,
    }
}

/// 超长则把全文写入 `spill_dir`，模型侧只保留头尾 + 路径说明。
/// 若文本里已有落盘路径，再次压缩时尽量从该文件读回全文，避免把预览当成原文再存一份。
pub fn bound_and_spill(
    spill_dir: Option<&Path>,
    tool_name: &str,
    call_id: &str,
    text: &str,
    cfg: BoundConfig,
) -> String {
    if text.chars().count() <= cfg.threshold_chars {
        return text.to_string();
    }
    let existing = existing_spill_path(text);
    let source = existing
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .filter(|full| full.chars().count() >= text.chars().count())
        .unwrap_or_else(|| text.to_string());
    let spilled = match &existing {
        Some(path) => Some(path.clone()),
        None if already_bounded(text) => None,
        None => spill_dir.and_then(|dir| spill_to_dir(dir, tool_name, call_id, &source).ok()),
    };
    bound_text(&source, cfg, spilled.as_deref()).text
}

/// 对话过长时压缩较早的 tool 消息；最近若干条保持原样。
pub fn trim_old_tool_messages(messages: &mut [Value], spill_dir: Option<&Path>) {
    let total: usize = messages.iter().map(|m| m.to_string().len()).sum();
    if total <= CONTEXT_TRIM_BYTES {
        return;
    }
    let cutoff = messages.len().saturating_sub(KEEP_RECENT_MESSAGES);
    for m in messages.iter_mut().take(cutoff) {
        if m.get("role").and_then(|r| r.as_str()) != Some("tool") {
            continue;
        }
        let Some(content) = m.get("content").and_then(|c| c.as_str()) else {
            continue;
        };
        if content.chars().count() <= STALE.threshold_chars {
            continue;
        }
        let call_id = m
            .get("tool_call_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let bounded = bound_and_spill(spill_dir, "tool", call_id, content, STALE);
        m["content"] = Value::String(bounded);
    }
}

fn omission_notice(omitted_chars: usize, spill_path: Option<&str>) -> String {
    match spill_path {
        Some(path) => format!(
            "\n\n{OMISSION_MARK} {omitted_chars} 字 ...]\n完整内容：{path}\n需要中间细节时用 read_file 读取该文件，不要用相同参数重跑。\n\n"
        ),
        None => format!(
            "\n\n{OMISSION_MARK} {omitted_chars} 字 ...]\n完整内容未能写入临时文件；若需要中段细节，请缩小范围后重试。\n\n"
        ),
    }
}

fn already_bounded(text: &str) -> bool {
    text.contains(OMISSION_MARK)
}

/// 模型侧文本是否已经是头尾裁剪后的预览
pub fn is_bounded(text: &str) -> bool {
    already_bounded(text)
}

fn existing_spill_path(text: &str) -> Option<String> {
    let key = "完整内容：";
    let start = text.find(key)? + key.len();
    let rest = text.get(start..)?;
    let line = rest.lines().next()?.trim();
    if line.is_empty() || line.contains("未能写入") {
        return None;
    }
    Some(line.to_string())
}

fn spill_to_dir(dir: &Path, tool_name: &str, call_id: &str, text: &str) -> std::io::Result<String> {
    std::fs::create_dir_all(dir)?;
    let mut path = dir.join(spill_filename(tool_name, call_id));
    if path.exists() {
        path = dir.join(spill_filename(
            tool_name,
            &format!("{}-{}", call_id, unique_suffix()),
        ));
    }
    std::fs::write(&path, text.as_bytes())?;
    Ok(path.to_string_lossy().replace('\\', "/"))
}

fn spill_filename(tool_name: &str, call_id: &str) -> PathBuf {
    PathBuf::from(format!(
        "tool-spill-{}-{}.txt",
        sanitize_segment(tool_name, 40),
        sanitize_segment(call_id, 24)
    ))
}

fn sanitize_segment(raw: &str, max_chars: usize) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(max_chars)
        .collect();
    if cleaned.is_empty() {
        "x".to_string()
    } else {
        cleaned
    }
}

fn unique_suffix() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "dup".to_string())
}

fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn take_last_chars(s: &str, n: usize) -> &str {
    let count = s.chars().count();
    if count <= n {
        return s;
    }
    match s.char_indices().nth(count - n) {
        Some((i, _)) => &s[i..],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn long_text(chars: usize) -> String {
        "测".repeat(chars)
    }

    #[test]
    fn short_text_stays_intact() {
        let s = "短结果";
        let bound = bound_text(s, FRESH, None);
        assert!(!bound.truncated);
        assert_eq!(bound.text, s);
    }

    #[test]
    fn prunes_middle_and_keeps_head_tail_by_chars() {
        let s = long_text(10_000);
        let bound = bound_text(&s, FRESH, Some("temp/tool-spills/a.txt"));
        assert!(bound.truncated);
        assert_eq!(bound.omitted_chars, 10_000 - FRESH_HEAD_CHARS - FRESH_TAIL_CHARS);
        assert!(bound.text.chars().count() <= FRESH_THRESHOLD_CHARS);
        assert!(bound.text.starts_with(&long_text(FRESH_HEAD_CHARS)));
        assert!(bound.text.ends_with(&long_text(FRESH_TAIL_CHARS)));
        assert!(bound.text.contains("完整内容：temp/tool-spills/a.txt"));
        assert!(bound.text.contains("read_file"));
    }

    #[test]
    fn does_not_split_multibyte_chars() {
        let s = format!("{}中{}", "a".repeat(FRESH_THRESHOLD_CHARS), "b".repeat(100));
        let bound = bound_text(&s, FRESH, None);
        assert!(bound.truncated);
        assert!(bound.text.is_char_boundary(bound.text.len()));
        assert!(!bound.text.contains('\u{FFFD}'));
    }

    #[test]
    fn spill_then_bound_writes_file_and_points_to_it() {
        let dir = std::env::temp_dir().join(format!(
            "deskhelper-retention-{}",
            unique_suffix()
        ));
        let text = long_text(9_000);
        let out = bound_and_spill(Some(&dir), "browser_read", "call-1", &text, FRESH);
        assert!(out.contains(OMISSION_MARK));
        let path = existing_spill_path(&out).expect("spill path");
        let saved = std::fs::read_to_string(&path).unwrap();
        assert_eq!(saved, text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn already_bounded_short_text_not_rewritten() {
        let original = format!(
            "HEAD\n\n{OMISSION_MARK} 10 字 ...]\n完整内容：D:/tmp/x.txt\n需要中间细节时用 read_file 读取该文件，不要用相同参数重跑。\n\nTAIL"
        );
        let out = bound_and_spill(None, "read_file", "id", &original, STALE);
        assert_eq!(out, original);
    }

    #[test]
    fn restale_reads_full_spill_instead_of_preview() {
        let dir = std::env::temp_dir().join(format!(
            "deskhelper-retention-restale-{}",
            unique_suffix()
        ));
        let full = long_text(9_000);
        let first = bound_and_spill(Some(&dir), "browser_read", "call-2", &full, FRESH);
        assert!(first.contains(OMISSION_MARK));
        let second = bound_and_spill(Some(&dir), "browser_read", "call-2", &first, STALE);
        assert!(second.contains(OMISSION_MARK));
        assert!(second.chars().count() <= STALE_THRESHOLD_CHARS);
        let path = existing_spill_path(&second).expect("path kept");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), full);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trim_old_tool_messages_keeps_recent_and_prunes_old() {
        let dir = std::env::temp_dir().join(format!(
            "deskhelper-retention-trim-{}",
            unique_suffix()
        ));
        let huge = json!({
            "role": "tool",
            "tool_call_id": "old",
            "content": long_text(90_000),
        });
        let recent = json!({
            "role": "tool",
            "tool_call_id": "new",
            "content": long_text(3_000),
        });
        let mut messages = vec![huge];
        for i in 0..KEEP_RECENT_MESSAGES {
            let mut m = recent.clone();
            m["tool_call_id"] = json!(format!("new-{i}"));
            messages.push(m);
        }
        trim_old_tool_messages(&mut messages, Some(&dir));
        let old = messages[0]["content"].as_str().unwrap();
        assert!(old.contains(OMISSION_MARK));
        assert!(old.chars().count() <= STALE_THRESHOLD_CHARS);
        let kept = messages.last().unwrap()["content"].as_str().unwrap();
        assert_eq!(kept.chars().count(), 3_000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
