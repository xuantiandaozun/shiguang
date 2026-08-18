//! 外部 CLI 的 JSON 入参/出参：避免「命令行拼 JSON」和「整段 stdout 截断后再 read_file」。

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};
use std::path::Path;

/// 为解析 JSON 最多读取的日志字节（超出则标记 source_truncated）
pub const MAX_JSON_LOG_BYTES: u64 = 1_500_000;
/// 完整 JSON 可内联进工具结果的上限（字节）
const INLINE_JSON_BYTES: usize = 6_000;
/// 数组预览条数
const PREVIEW_ITEMS: usize = 6;
/// 对象预览键数
const PREVIEW_KEYS: usize = 12;
/// 预览里字符串截断字符数
const PREVIEW_STR_CHARS: usize = 80;
const MAX_FILES: usize = 16;
const MAX_FILE_BYTES: usize = 256 * 1024;
const LEFTOVER_CHARS: usize = 400;

#[derive(Debug, Clone)]
pub struct CapturedJson {
    /// 体积允许时的完整 JSON；过大则为 None
    pub value: Option<Value>,
    pub summary: Value,
    pub file: Option<String>,
    pub chars: usize,
    pub leftover: String,
    pub note: String,
    pub pointer_error: Option<String>,
}

/// 把 `files` 参数写到工作目录。值为字符串则原样写入；对象/数组序列化为 JSON。
pub fn write_workdir_files(dir: &Path, files: &Value) -> Result<Vec<String>> {
    let object = files
        .as_object()
        .ok_or_else(|| anyhow!("files 必须是对象，键为文件名、值为文本或 JSON"))?;
    if object.len() > MAX_FILES {
        bail!("files 最多 {} 个文件", MAX_FILES);
    }
    let mut written = Vec::with_capacity(object.len());
    for (name, value) in object {
        validate_file_name(name)?;
        let content = file_content(value)?;
        if content.len() > MAX_FILE_BYTES {
            bail!("files 中 {} 超过 256KB", name);
        }
        std::fs::write(dir.join(name), content.as_bytes())?;
        written.push(name.clone());
    }
    Ok(written)
}

fn validate_file_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        bail!("files 文件名无效: {name}（只允许字母数字、点、短横线、下划线，不能含路径）");
    }
    Ok(())
}

fn file_content(value: &Value) -> Result<String> {
    match value {
        Value::String(s) => {
            if s.contains('\0') {
                bail!("files 内容不能包含 NUL");
            }
            Ok(s.clone())
        }
        Value::Null => bail!("files 的值不能为 null"),
        other => Ok(serde_json::to_string(other)?),
    }
}

/// 从 CLI stdout 提取 JSON。成功则按体积内联或落临时文件，并给出摘要。
pub fn capture_stdout_json(
    text: &str,
    source_truncated: bool,
    pointer: &str,
    save_dir: &Path,
    task_id: &str,
) -> Result<Option<CapturedJson>> {
    let Some((mut value, start, end)) = extract_best_json(text) else {
        return Ok(None);
    };
    let leftover = leftover_text(text, start, end);
    let mut pointer_error = None;
    let pointer = pointer.trim();
    if !pointer.is_empty() {
        match value.pointer(pointer) {
            Some(selected) => value = selected.clone(),
            None => {
                pointer_error = Some(format!(
                    "json_pointer {pointer} 不存在。已保留完整 JSON 的 json_summary；对象键见 keys。"
                ));
            }
        }
    }

    let compact = serde_json::to_string(&value)?;
    let chars = compact.len();
    let summary = json_summary(&value);
    let (inline, file) = if chars <= INLINE_JSON_BYTES {
        (Some(value), None)
    } else {
        std::fs::create_dir_all(save_dir)?;
        let path = save_dir.join(format!("cli-json-{task_id}.json"));
        std::fs::write(&path, compact.as_bytes())?;
        (None, Some(path.to_string_lossy().replace('\\', "/")))
    };

    let note = if pointer_error.is_some() {
        pointer_error.clone().unwrap_or_default()
    } else if inline.is_some() {
        "stdout 已解析为 JSON，完整结果在 json 字段；不要再重定向到文件后 read_file。".to_string()
    } else {
        "stdout 已解析为 JSON，体积较大未内联。请优先用 json_summary 回答；只要子集时对同一 task_id 再 check_task 并带 json_pointer。完整内容在 json_file，不要把 CLI 输出再重定向一遍。".to_string()
    };
    let note = if source_truncated {
        format!("{note} 原始日志超过读取上限，若摘要缺项请给 CLI 加过滤/分页后重跑。")
    } else {
        note
    };

    Ok(Some(CapturedJson {
        value: inline,
        summary,
        file,
        chars,
        leftover,
        note,
        pointer_error,
    }))
}

fn leftover_text(text: &str, start: usize, end: usize) -> String {
    let mut leftover = String::new();
    let before = text.get(..start).unwrap_or("").trim();
    let after = text.get(end..).unwrap_or("").trim();
    if !before.is_empty() {
        leftover.push_str(before);
    }
    if !after.is_empty() {
        if !leftover.is_empty() {
            leftover.push('\n');
        }
        leftover.push_str(after);
    }
    if leftover.chars().count() <= LEFTOVER_CHARS {
        leftover
    } else {
        leftover.chars().take(LEFTOVER_CHARS).collect::<String>() + "…"
    }
}

fn extract_best_json(text: &str) -> Option<(Value, usize, usize)> {
    if let Some((value, start, end)) = extract_one_json(text) {
        let rest = text.get(end..).unwrap_or("").trim();
        if rest.starts_with('{') || rest.starts_with('[') {
            if let Some((arr, nd_start, nd_end)) = extract_ndjson(text) {
                return Some((arr, nd_start, nd_end));
            }
        }
        return Some((value, start, end));
    }
    extract_ndjson(text)
}

fn extract_one_json(text: &str) -> Option<(Value, usize, usize)> {
    let start = text.find(['{', '['])?;
    let slice = text.get(start..)?;
    let mut stream = serde_json::Deserializer::from_str(slice).into_iter::<Value>();
    let value = stream.next()?.ok()?;
    let end = start + stream.byte_offset();
    Some((value, start, end))
}

fn extract_ndjson(text: &str) -> Option<(Value, usize, usize)> {
    let mut items = Vec::new();
    let mut start = None;
    let mut end = 0usize;
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let line_len = line.len();
        let leading = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        let content_start = offset + leading;
        offset += line_len;
        if trimmed.is_empty() {
            continue;
        }
        if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
            if items.is_empty() {
                continue;
            }
            break;
        }
        let json_line = trimmed.trim_end();
        match serde_json::from_str::<Value>(json_line) {
            Ok(value) => {
                if start.is_none() {
                    start = Some(content_start);
                }
                end = content_start + json_line.len();
                items.push(value);
            }
            Err(_) if items.is_empty() => continue,
            Err(_) => break,
        }
    }
    if items.len() < 2 {
        return None;
    }
    Some((Value::Array(items), start?, end.min(text.len())))
}

fn json_summary(value: &Value) -> Value {
    match value {
        Value::Array(arr) => json!({
            "type": "array",
            "length": arr.len(),
            "preview": arr.iter().take(PREVIEW_ITEMS).map(compact_preview).collect::<Vec<_>>(),
        }),
        Value::Object(map) => {
            let keys: Vec<String> = map.keys().take(PREVIEW_KEYS).cloned().collect();
            let mut preview = Map::new();
            for (key, val) in map.iter().take(PREVIEW_KEYS) {
                preview.insert(key.clone(), compact_preview(val));
            }
            json!({
                "type": "object",
                "key_count": map.len(),
                "keys": keys,
                "preview": preview,
            })
        }
        Value::String(s) => json!({
            "type": "string",
            "length": s.chars().count(),
            "preview": truncate_chars(s, PREVIEW_STR_CHARS),
        }),
        other => json!({
            "type": json_type_name(other),
            "value": other,
        }),
    }
}

fn compact_preview(value: &Value) -> Value {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::String(s) => Value::String(truncate_chars(s, PREVIEW_STR_CHARS)),
        Value::Array(arr) => json!({ "_type": "array", "_len": arr.len() }),
        Value::Object(map) => {
            let mut out = Map::new();
            const KEEP: usize = 8;
            for (key, val) in map.iter().take(KEEP) {
                out.insert(key.clone(), compact_preview(val));
            }
            if map.len() > KEEP {
                out.insert("_more_keys".into(), json!(map.len() - KEEP));
            }
            Value::Object(out)
        }
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_json_after_banner_and_ignores_trailing() {
        let text = "Active token: abc\n{\"ok\":true,\"n\":2}\nbye\n";
        let (value, start, end) = extract_one_json(text).unwrap();
        assert_eq!(value, json!({"ok": true, "n": 2}));
        assert!(text[..start].contains("Active token"));
        assert!(text[end..].contains("bye"));
    }

    #[test]
    fn extracts_ndjson_as_array() {
        let text = "hint\n{\"id\":1}\n{\"id\":2}\n";
        let (value, _, _) = extract_best_json(text).unwrap();
        assert_eq!(value, json!([{"id": 1}, {"id": 2}]));
    }

    #[test]
    fn summary_reports_array_length_and_preview() {
        let value = json!([
            {"id": "a", "name": "甲"},
            {"id": "b", "name": "乙"},
        ]);
        let summary = json_summary(&value);
        assert_eq!(summary["type"], "array");
        assert_eq!(summary["length"], 2);
        assert_eq!(summary["preview"][0]["name"], "甲");
    }

    #[test]
    fn write_workdir_files_serializes_objects_and_rejects_paths() {
        let dir = std::env::temp_dir().join(format!("dh-cli-json-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let written = write_workdir_files(
            &dir,
            &json!({
                "payload.json": {"records": [{"name": "日报"}]},
                "note.txt": "hello"
            }),
        )
        .unwrap();
        assert_eq!(written.len(), 2);
        let body = std::fs::read_to_string(dir.join("payload.json")).unwrap();
        assert!(body.contains("日报"));
        assert!(write_workdir_files(&dir, &json!({ "../x.json": "{}" })).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capture_inlines_small_json_and_applies_pointer() {
        let dir = std::env::temp_dir().join(format!("dh-cli-cap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let captured = capture_stdout_json(
            r#"{"data":{"items":[{"id":1}]}}"#,
            false,
            "/data/items",
            &dir,
            "t1",
        )
        .unwrap()
        .unwrap();
        assert_eq!(captured.value, Some(json!([{"id": 1}])));
        assert!(captured.file.is_none());
        assert!(captured.note.contains("json"));
        let missing = capture_stdout_json(r#"{"data":{"items":[1]}}"#, false, "/nope", &dir, "t2")
            .unwrap()
            .unwrap();
        assert!(missing.pointer_error.is_some());
        assert!(missing.value.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capture_writes_large_json_to_file() {
        let dir = std::env::temp_dir().join(format!("dh-cli-big-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let items: Vec<Value> = (0..400)
            .map(|i| json!({"id": i, "name": format!("item-{i}")}))
            .collect();
        let text = serde_json::to_string(&items).unwrap();
        assert!(text.len() > INLINE_JSON_BYTES);
        let captured = capture_stdout_json(&text, false, "", &dir, "t9")
            .unwrap()
            .unwrap();
        assert!(captured.value.is_none());
        let path = captured.file.expect("json_file");
        assert!(std::path::Path::new(&path).exists());
        assert_eq!(captured.summary["length"], 400);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
