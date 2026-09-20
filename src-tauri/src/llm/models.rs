//! 内部维护的模型能力表：当前设置里的主模型若本身能看图，就直接把图片发给它。

use serde_json::Value;
use std::path::Path;

/// 确认支持图像输入的模型（精确匹配，大小写不敏感）。
const VISION_MODELS: &[&str] = &[
    "deepseek-flash",
    "deepseek-v4-flash",
    "deepseek-v4-flash-vision-exp",
    "qwen-vl-max",
    "qwen-vl-plus",
    "qwen-vl-ocr",
    "glm-4v-flash",
    "glm-4v-plus",
    "glm-4.5v",
    "gpt-4o",
    "gpt-4o-mini",
    "gpt-4.1",
    "gpt-4.1-mini",
    "hunyuan-vision",
    "moonshot-v1-8k-vision-preview",
    "moonshot-v1-32k-vision-preview",
    "moonshot-v1-128k-vision-preview",
    "openai/gpt-4o",
    "openai/gpt-4o-mini",
    "google/gemini-2.5-flash",
    "gemini-2.5-flash",
    "gemini-2.5-pro",
    "gemini-3.6-flash",
    "gemini-3.7-flash",
    "gemini-3.5-flash",
    "gemini-3.1-pro-preview",
    "gemini-3-flash-preview",
    "gemini-2.0-flash",
    "anthropic/claude-sonnet-4",
    "qwen/qwen2.5-vl-72b-instruct",
];

pub const ATTACH_BLOCK: &str = "【用户附带的文件】";

pub fn supports_vision(model: &str) -> bool {
    let id = model.trim();
    if id.is_empty() {
        return false;
    }
    let lower = id.to_ascii_lowercase();
    if VISION_MODELS.iter().any(|m| m.eq_ignore_ascii_case(id)) {
        return true;
    }
    infer_vision(&lower)
}

fn infer_vision(lower: &str) -> bool {
    if lower.contains("vision")
        || lower.contains("-vl")
        || lower.contains("_vl")
        || lower.contains("/vl")
    {
        return true;
    }
    let name = lower.rsplit('/').next().unwrap_or(lower);
    if name.starts_with("gpt-4o") || name.starts_with("gpt-4.1") || name.starts_with("gpt-5") {
        return true;
    }
    if lower.contains("gemini") || lower.contains("claude") {
        return true;
    }
    // glm-4v / glm-4.5v，不要误伤 glm-4.5
    let bytes = name.as_bytes();
    if let Some(i) = name.find("glm-4") {
        let rest = &bytes[i + 5..];
        if rest.first() == Some(&b'v') {
            return true;
        }
        if rest.first() == Some(&b'.') {
            let mut j = 1;
            while j < rest.len() && rest[j].is_ascii_digit() {
                j += 1;
            }
            if j < rest.len() && rest[j] == b'v' {
                return true;
            }
        }
    }
    false
}

pub fn is_native_image_path(path: &str) -> bool {
    native_image_mime(Path::new(path)).is_some()
}

pub fn native_image_mime(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

pub fn is_image_file(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some(
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "bmp"
                | "webp"
                | "ico"
                | "tif"
                | "tiff"
                | "heic"
                | "avif"
        )
    )
}

pub fn image_attachment_hint(native_vision: bool) -> &'static str {
    if native_vision {
        "图片：画面已直接附上，请直接看图。只要文字、想省 token 可用 ocr_image"
    } else {
        "图片：抽文字用 ocr_image（更省 token）；理解画面用 read_image"
    }
}

/// 从用户消息正文里取出附件图片路径。
pub fn image_paths_in_user_text(text: &str) -> Vec<String> {
    let Some(block) = text.split(ATTACH_BLOCK).nth(1) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in block.lines() {
        let line = line.trim();
        let Some(rest) = line.split_once(". ") else {
            continue;
        };
        if rest.0.chars().any(|c| !c.is_ascii_digit()) {
            continue;
        }
        let path = rest.1.split('（').next().unwrap_or(rest.1).trim();
        if is_native_image_path(path) {
            out.push(path.to_string());
        }
    }
    out
}

/// 工具结果里若带截图/待看图路径，取出来交给当前视觉模型直接看。
pub fn image_path_from_tool_content(content: &Value) -> Option<String> {
    let raw = match content {
        Value::String(s) => s.as_str(),
        other => {
            return image_path_from_json(other);
        }
    };
    if let Ok(v) = serde_json::from_str::<Value>(raw) {
        return image_path_from_json(&v);
    }
    None
}

fn image_path_from_json(v: &Value) -> Option<String> {
    if v.get("native_image").and_then(Value::as_bool) == Some(true) {
        if let Some(p) = v.get("path").and_then(Value::as_str) {
            if is_native_image_path(p) {
                return Some(p.to_string());
            }
        }
    }
    if let Some(p) = v.get("saved").and_then(Value::as_str) {
        if is_native_image_path(p) {
            return Some(p.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deepseek_v41_flash_has_native_vision() {
        assert!(supports_vision("deepseek-flash"));
        assert!(supports_vision("deepseek-v4-flash"));
        assert!(!supports_vision("deepseek-chat"));
        assert!(supports_vision("deepseek-v4-flash-vision-exp"));
    }

    #[test]
    fn infers_common_vl_names() {
        assert!(supports_vision("qwen-vl-max"));
        assert!(supports_vision("Qwen/Qwen2.5-VL-72B-Instruct"));
        assert!(supports_vision("glm-4.5v"));
        assert!(supports_vision("gpt-4o-mini"));
        assert!(!supports_vision("glm-4.5"));
        assert!(!supports_vision("qwen-plus"));
        assert!(!supports_vision("deepseek-chat"));
    }

    #[test]
    fn parses_attachment_block() {
        let text = "看看这张图\n\n【用户附带的文件】\n1. C:/tmp/a.png（图片：抽文字用 ocr_image；理解画面用 read_image）\n2. C:/tmp/note.txt（请用 read_file 读取内容）\n";
        assert_eq!(image_paths_in_user_text(text), vec!["C:/tmp/a.png"]);
    }

    #[test]
    fn reads_screenshot_path_from_tool_json() {
        let content = json!({
            "saved": "D:/shots/shot-1.png",
            "hint": "截图已保存"
        });
        assert_eq!(
            image_path_from_tool_content(&json!(content.to_string())),
            Some("D:/shots/shot-1.png".into())
        );
        assert_eq!(
            image_path_from_tool_content(&json!({
                "path": "C:/img.webp",
                "native_image": true
            })),
            Some("C:/img.webp".into())
        );
    }
}
