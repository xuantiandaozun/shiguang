use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::Path;

/// DashScope 单图限制 10MB（base64 后），按 4/3 膨胀反推原始字节上限
const MAX_VISION_TOOL_BYTES: usize = 7 * 1024 * 1024;
/// 主模型直接看图时的单图上限（DeepSeek 文档为 32 MiB，留余量）
pub const MAX_NATIVE_IMAGE_BYTES: usize = 20 * 1024 * 1024;

fn mime_of(path: &Path) -> Option<&'static str> {
    crate::llm::models::native_image_mime(path).or_else(|| {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref()
        {
            Some("bmp") => Some("image/bmp"),
            _ => None,
        }
    })
}

/// 把本地图片编成 OpenAI 兼容的 data URL。失败时返回原因（格式/过大/读盘）。
pub fn encode_image_data_uri(path: &Path, max_bytes: usize) -> Result<String> {
    let mime = mime_of(path)
        .ok_or_else(|| anyhow!("不支持的图片格式（支持 png / jpg / jpeg / gif / webp / bmp）"))?;
    let bytes = std::fs::read(path).map_err(|e| anyhow!("读取图片失败: {}", e))?;
    if bytes.len() > max_bytes {
        bail!(
            "图片过大（约 {}MB，上限 {}MB），请先压缩再识别",
            bytes.len() / 1024 / 1024,
            max_bytes / 1024 / 1024
        );
    }
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{};base64,{}", mime, b64))
}

/// 调用视觉模型识别图片内容（OpenAI 兼容多模态格式，非流式一次性返回）。
/// 与聊天模型完全独立：独立的 base_url / api_key / model。
pub struct VisionResult {
    pub content: String,
    pub usage: crate::llm::client::TokenUsage,
}

pub async fn recognize_image(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    path: &Path,
    question: Option<&str>,
) -> Result<VisionResult> {
    let data_uri = encode_image_data_uri(path, MAX_VISION_TOOL_BYTES)?;

    let q = question
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("请详细描述这张图片的内容。如果是软件/网页截图，说明界面上的关键信息；如果图中有文字，请完整提取。");

    let body = json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "image_url", "image_url": { "url": data_uri } },
                { "type": "text", "text": q },
            ]
        }],
        "stream": false,
    });

    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let resp = crate::llm::provider_headers::apply_llm_headers(
        client.post(&url).bearer_auth(api_key),
        base_url,
    )
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow!("请求视觉模型失败: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let short: String = text.chars().take(300).collect();
        return Err(anyhow!("视觉模型 API 返回 {}: {}", status, short));
    }

    let v: Value = resp
        .json()
        .await
        .map_err(|e| anyhow!("解析视觉模型响应失败: {}", e))?;
    let content = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if content.is_empty() {
        bail!("视觉模型未返回有效内容");
    }
    Ok(VisionResult {
        content,
        usage: crate::llm::client::TokenUsage::from_response(&v),
    })
}
