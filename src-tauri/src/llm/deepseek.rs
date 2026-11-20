//! DeepSeek 平台原生能力封装：
//! - 联网 Search / Fetch API (`/v1/tools/search`, `/v1/tools/fetch`)
//! - Files API (`/files` 上传/列表/删除，支持 `user_data` / `text_recognition` / `rich_layout_recognition`)
//! - 语音合成 TTS (`/v1/tools/tts`) 与语音识别 ASR (`/v1/tools/asr`)
//! - 用户元信息标记 (`latest_reminder`)

use crate::commands::Settings;
use crate::llm::client::LlmConfig;
use crate::llm::provider_headers::{deepseek_root_url, is_deepseek};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MIB: usize = 1024 * 1024;
const MAX_TTS_BILLING_CHARS: usize = 9_000;
const MAX_TTS_CHUNK_BYTES: usize = 48 * 1024;
const MAX_ASR_BYTES: usize = 8 * MIB;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepSeekFileObject {
    pub id: String,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsrResult {
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileUploadPlan {
    pub purpose: String,
    pub max_bytes: usize,
    pub is_image: bool,
}

/// 仅当当前激活主模型使用 DeepSeek 接口且已填 Key 时返回 `(root_url, api_key)`。
pub fn resolve_deepseek_credentials(settings: &Settings) -> Option<(String, String)> {
    if is_deepseek(&settings.base_url) && !settings.api_key.trim().is_empty() {
        return Some((
            deepseek_root_url(&settings.base_url),
            settings.api_key.trim().to_string(),
        ));
    }
    None
}

/// 为 DeepSeek 原生能力解析当前主模型配置（仅主模型为 DeepSeek 时启用）。
pub fn resolve_deepseek_chat_config(
    settings: &Settings,
    needs_flash_features: bool,
) -> Result<LlmConfig> {
    if !is_deepseek(&settings.base_url) || settings.api_key.trim().is_empty() {
        bail!("当前主模型未使用 DeepSeek，或尚未填写 DeepSeek API Key。");
    }
    let mut cfg = LlmConfig::new(&settings.base_url, &settings.api_key, &settings.model);
    if needs_flash_features && cfg.model != "deepseek-flash" {
        // 联网工具、文件解析与多模态对话在 DeepSeek 文档中均指定使用 deepseek-flash。
        cfg.model = "deepseek-flash".to_string();
    }
    Ok(cfg)
}

/// 按 RFC 7578 构造 `multipart/form-data` 请求体。
/// 文本字段严格放在文件字段之前（DeepSeek Files API 要求 `purpose` 在 `file` 之前）。
pub fn build_multipart_body(
    fields: &[(&str, &str)],
    file_field: &str,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> (String, Vec<u8>) {
    let boundary = format!("----ShiGuangDs{}", uuid::Uuid::new_v4().simple());
    let mut body = Vec::with_capacity(bytes.len() + 1024);

    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    }

    let safe_name = filename.replace(['"', '\r', '\n'], "_");
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"{file_field}\"; filename=\"{safe_name}\"\r\nContent-Type: {content_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// 根据文件扩展名决定 DeepSeek Files API 的 `purpose` 和大小限制。
pub fn classify_file_purpose(path: &Path, doc_purpose: &str) -> Result<FileUploadPlan> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    let normalized_doc = match doc_purpose {
        "text_recognition" => "text_recognition",
        _ => "rich_layout_recognition",
    };

    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" => Ok(FileUploadPlan {
            purpose: "user_data".to_string(),
            max_bytes: 64 * MIB,
            is_image: true,
        }),
        "bmp" | "tif" | "tiff" => Ok(FileUploadPlan {
            purpose: normalized_doc.to_string(),
            max_bytes: 100 * MIB,
            is_image: false,
        }),
        "pdf" | "doc" | "docx" | "ppt" | "pptx" => Ok(FileUploadPlan {
            purpose: normalized_doc.to_string(),
            max_bytes: 100 * MIB,
            is_image: false,
        }),
        "txt" | "md" | "markdown" | "csv" | "tsv" | "json" | "jsonl" | "log" | "xml"
        | "yaml" | "yml" | "toml" | "ini" | "conf" | "html" | "htm" | "css" | "js"
        | "jsx" | "ts" | "tsx" | "py" | "rs" | "go" | "java" | "c" | "cpp" | "cc"
        | "h" | "hpp" | "cs" | "rb" | "php" | "sql" | "sh" | "bash" | "ps1" | "bat"
        | "xlsx" | "xls" | "epub" | "mobi" => Ok(FileUploadPlan {
            purpose: "user_data".to_string(),
            max_bytes: 32 * MIB,
            is_image: false,
        }),
        _ => bail!("DeepSeek 文件接口暂不支持该文件格式（.{ext}）"),
    }
}

fn mime_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("doc") => "application/msword",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("ppt") => "application/vnd.ms-powerpoint",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("xls") => "application/vnd.ms-excel",
        Some("json") => "application/json",
        Some("txt") | Some("md") | Some("log") | Some("csv") => "text/plain",
        _ => "application/octet-stream",
    }
}

/// 上传并解析文件到 DeepSeek Files API (`POST /files`)。
pub async fn upload_file(
    client: &reqwest::Client,
    root: &str,
    api_key: &str,
    path: &Path,
    doc_purpose: &str,
    cancel: Option<&CancellationToken>,
) -> Result<(DeepSeekFileObject, FileUploadPlan)> {
    let plan = classify_file_purpose(path, doc_purpose)?;
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| anyhow!("读取文件信息失败: {e}"))?;
    let size = meta.len() as usize;
    if size == 0 {
        bail!("文件为空，无法上传: {}", path.display());
    }
    if size > plan.max_bytes {
        bail!(
            "文件「{}」超过 DeepSeek 上限（{} MiB）",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("file"),
            plan.max_bytes / MIB
        );
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| anyhow!("读取文件失败: {e}"))?;
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("attachment")
        .to_string();
    let (ct, body) = build_multipart_body(
        &[("purpose", plan.purpose.as_str())],
        "file",
        &filename,
        mime_for_path(path),
        &bytes,
    );

    let url = format!("{}/files", root.trim_end_matches('/'));
    let req = client
        .post(&url)
        .timeout(Duration::from_secs(260))
        .bearer_auth(api_key)
        .header(reqwest::header::CONTENT_TYPE, ct)
        .body(body)
        .send();

    let resp = match cancel {
        Some(c) => tokio::select! {
            biased;
            _ = c.cancelled() => bail!("已中断文件上传"),
            r = req => r.map_err(|e| anyhow!("上传文件到 DeepSeek 失败: {e}"))?,
        },
        None => req
            .await
            .map_err(|e| anyhow!("上传文件到 DeepSeek 失败: {e}"))?,
    };

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("DeepSeek Files API 返回 {}: {}", status.as_u16(), extract_api_error(&text));
    }
    let file_obj: DeepSeekFileObject =
        serde_json::from_str(&text).map_err(|e| anyhow!("解析 DeepSeek 文件响应失败: {e}"))?;
    Ok((file_obj, plan))
}

pub async fn list_files(
    client: &reqwest::Client,
    root: &str,
    api_key: &str,
    purpose: Option<&str>,
) -> Result<Vec<DeepSeekFileObject>> {
    let mut url = format!("{}/files?limit=100&order=desc", root.trim_end_matches('/'));
    if let Some(p) = purpose.filter(|s| !s.trim().is_empty()) {
        url.push_str(&format!("&purpose={}", url::form_urlencoded::byte_serialize(p.as_bytes()).collect::<String>()));
    }
    let resp = client
        .get(&url)
        .timeout(Duration::from_secs(20))
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(|e| anyhow!("获取 DeepSeek 文件列表失败: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("DeepSeek Files API 返回 {}: {}", status.as_u16(), extract_api_error(&text));
    }
    let v: Value = serde_json::from_str(&text)?;
    let items = v
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let files = items
        .into_iter()
        .filter_map(|item| serde_json::from_value::<DeepSeekFileObject>(item).ok())
        .collect();
    Ok(files)
}

pub async fn delete_file(
    client: &reqwest::Client,
    root: &str,
    api_key: &str,
    file_id: &str,
) -> Result<()> {
    let url = format!("{}/files/{}", root.trim_end_matches('/'), file_id.trim());
    let resp = client
        .delete(&url)
        .timeout(Duration::from_secs(20))
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(|e| anyhow!("删除 DeepSeek 文件失败: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("DeepSeek 删除文件返回 {}: {}", status.as_u16(), extract_api_error(&text));
    }
    Ok(())
}

/// DeepSeek Search API (`POST /v1/tools/search`)。
pub async fn search(
    client: &reqwest::Client,
    root: &str,
    api_key: &str,
    query: &str,
    count: usize,
    need_content: bool,
    ori_question: Option<&str>,
    cancel: &CancellationToken,
) -> Result<Value> {
    let trimmed: String = query.trim().chars().take(128).collect();
    if trimmed.is_empty() {
        bail!("搜索词不能为空");
    }
    let mut payload = json!({
        "query": trimmed,
        "count": count.clamp(1, 50),
        "need_content": need_content,
    });
    if let Some(oq) = ori_question.map(str::trim).filter(|s| !s.is_empty()) {
        let short: String = oq.chars().take(128).collect();
        payload["ori_question"] = json!(short);
    }
    let url = format!("{}/v1/tools/search", root.trim_end_matches('/'));
    let req = client
        .post(&url)
        .timeout(Duration::from_secs(20))
        .bearer_auth(api_key)
        .json(&payload)
        .send();
    let resp = tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("已中断"),
        r = req => r.map_err(|e| anyhow!("DeepSeek Search 请求失败: {e}"))?,
    };
    let status = resp.status();
    let text = tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("已中断"),
        r = resp.text() => r.unwrap_or_default(),
    };
    if !status.is_success() {
        bail!("DeepSeek Search 返回 {}: {}", status.as_u16(), extract_api_error(&text));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| anyhow!("解析 DeepSeek Search 响应失败: {e}"))?;
    Ok(v)
}

/// DeepSeek Fetch API (`POST /v1/tools/fetch`)。
pub async fn fetch(
    client: &reqwest::Client,
    root: &str,
    api_key: &str,
    target_url: &str,
    cancel: &CancellationToken,
) -> Result<Value> {
    let url = format!("{}/v1/tools/fetch", root.trim_end_matches('/'));
    let req = client
        .post(&url)
        .timeout(Duration::from_secs(25))
        .bearer_auth(api_key)
        .json(&json!({ "url": target_url }))
        .send();
    let resp = tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("已中断"),
        r = req => r.map_err(|e| anyhow!("DeepSeek Fetch 请求失败: {e}"))?,
    };
    let status = resp.status();
    let text = tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("已中断"),
        r = resp.text() => r.unwrap_or_default(),
    };
    if !status.is_success() {
        bail!("DeepSeek Fetch 返回 {}: {}", status.as_u16(), extract_api_error(&text));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| anyhow!("解析 DeepSeek Fetch 响应失败: {e}"))?;
    Ok(v)
}

/// 按 DeepSeek TTS 规则计算计费字符数：汉字计 2 个字符，其他字符计 1 个字符。
pub fn count_tts_billing_chars(text: &str) -> usize {
    text.chars().map(|c| if is_hanzi(c) { 2 } else { 1 }).sum()
}

fn is_hanzi(c: char) -> bool {
    matches!(
        c as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF | 0x20000..=0x2EBFF
    )
}

/// 将超长文本按段落/句边界拆分为多个安全片段，每段不超过 `MAX_TTS_BILLING_CHARS` 与 `MAX_TTS_CHUNK_BYTES`。
pub fn split_tts_chunks(text: &str) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if count_tts_billing_chars(trimmed) <= MAX_TTS_BILLING_CHARS
        && trimmed.len() <= MAX_TTS_CHUNK_BYTES
    {
        return vec![trimmed.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut cur_billing = 0usize;

    let flush = |chunks: &mut Vec<String>, current: &mut String, cur_billing: &mut usize| {
        let s = current.trim();
        if !s.is_empty() {
            chunks.push(s.to_string());
        }
        current.clear();
        *cur_billing = 0;
    };

    for paragraph in trimmed.split_inclusive(['\n', '。', '！', '？', '.', '!', '?']) {
        let p_billing = count_tts_billing_chars(paragraph);
        if p_billing > MAX_TTS_BILLING_CHARS || paragraph.len() > MAX_TTS_CHUNK_BYTES {
            if !current.is_empty() {
                flush(&mut chunks, &mut current, &mut cur_billing);
            }
            for ch in paragraph.chars() {
                let cb = if is_hanzi(ch) { 2 } else { 1 };
                let clen = ch.len_utf8();
                if cur_billing + cb > MAX_TTS_BILLING_CHARS
                    || current.len() + clen > MAX_TTS_CHUNK_BYTES
                {
                    flush(&mut chunks, &mut current, &mut cur_billing);
                }
                current.push(ch);
                cur_billing += cb;
            }
            continue;
        }
        if cur_billing + p_billing > MAX_TTS_BILLING_CHARS
            || current.len() + paragraph.len() > MAX_TTS_CHUNK_BYTES
        {
            flush(&mut chunks, &mut current, &mut cur_billing);
        }
        current.push_str(paragraph);
        cur_billing += p_billing;
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim().to_string());
    }
    chunks
}

/// DeepSeek TTS (`POST /v1/tools/tts`)，返回 WAV 音频字节。
pub async fn synthesize_tts(
    client: &reqwest::Client,
    root: &str,
    api_key: &str,
    text: &str,
    voice_id: &str,
    filter_markdown: bool,
) -> Result<Vec<u8>> {
    let voice = match voice_id.trim() {
        "tide" => "tide",
        _ => "stella",
    };
    let url = format!("{}/v1/tools/tts", root.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .timeout(Duration::from_secs(90))
        .bearer_auth(api_key)
        .json(&json!({
            "text": text,
            "voice_id": voice,
            "format": "wav",
            "filter_markdown": filter_markdown,
        }))
        .send()
        .await
        .map_err(|e| anyhow!("DeepSeek TTS 请求失败: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let err_text = resp.text().await.unwrap_or_default();
        bail!("DeepSeek TTS 返回 {}: {}", status.as_u16(), extract_api_error(&err_text));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| anyhow!("读取 DeepSeek TTS 音频失败: {e}"))?;
    if bytes.is_empty() {
        bail!("DeepSeek TTS 未返回音频数据");
    }
    Ok(bytes.to_vec())
}

/// DeepSeek ASR (`POST /v1/tools/asr`)，接收 16-bit PCM WAV 或 Ogg Opus 字节。
pub async fn transcribe_asr(
    client: &reqwest::Client,
    root: &str,
    api_key: &str,
    audio_bytes: &[u8],
    filename: &str,
    content_type: &str,
) -> Result<AsrResult> {
    if audio_bytes.is_empty() {
        bail!("录音为空");
    }
    if audio_bytes.len() > MAX_ASR_BYTES {
        bail!("音频超过 DeepSeek ASR 8 MiB 上限");
    }
    let (ct, body) = build_multipart_body(&[], "file", filename, content_type, audio_bytes);
    let url = format!("{}/v1/tools/asr", root.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .timeout(Duration::from_secs(90))
        .bearer_auth(api_key)
        .header(reqwest::header::CONTENT_TYPE, ct)
        .body(body)
        .send()
        .await
        .map_err(|e| anyhow!("DeepSeek ASR 请求失败: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("DeepSeek ASR 返回 {}: {}", status.as_u16(), extract_api_error(&text));
    }
    let parsed: AsrResult =
        serde_json::from_str(&text).map_err(|e| anyhow!("解析 DeepSeek ASR 响应失败: {e}"))?;
    Ok(parsed)
}

pub fn extract_api_error(raw: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(raw) {
        if let Some(msg) = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .or_else(|| v.get("message").and_then(Value::as_str))
        {
            return msg.to_string();
        }
    }
    raw.chars().take(240).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_places_purpose_before_file() {
        let (ct, body) = build_multipart_body(
            &[("purpose", "rich_layout_recognition")],
            "file",
            "report.pdf",
            "application/pdf",
            b"%PDF-1.4",
        );
        assert!(ct.starts_with("multipart/form-data; boundary="));
        let text = String::from_utf8_lossy(&body);
        let purpose_pos = text.find("name=\"purpose\"").unwrap();
        let file_pos = text.find("name=\"file\"").unwrap();
        assert!(purpose_pos < file_pos);
        assert!(text.contains("rich_layout_recognition"));
        assert!(text.contains("filename=\"report.pdf\""));
    }

    #[test]
    fn classifies_document_and_image_purposes() {
        let pdf = classify_file_purpose(Path::new("a.pdf"), "rich_layout_recognition").unwrap();
        assert_eq!(pdf.purpose, "rich_layout_recognition");
        assert_eq!(pdf.max_bytes, 100 * MIB);

        let txt = classify_file_purpose(Path::new("notes.md"), "rich_layout_recognition").unwrap();
        assert_eq!(txt.purpose, "user_data");
        assert_eq!(txt.max_bytes, 32 * MIB);

        let img = classify_file_purpose(Path::new("pic.png"), "text_recognition").unwrap();
        assert_eq!(img.purpose, "user_data");
        assert!(img.is_image);
        assert_eq!(img.max_bytes, 64 * MIB);
    }

    #[test]
    fn tts_billing_chars_and_chunking_respect_limits() {
        assert_eq!(count_tts_billing_chars("你好ab"), 6);
        let long = "这是一句测试文本。".repeat(1500);
        let chunks = split_tts_chunks(&long);
        assert!(chunks.len() >= 2);
        for chunk in chunks {
            assert!(count_tts_billing_chars(&chunk) <= MAX_TTS_BILLING_CHARS);
            assert!(chunk.len() <= MAX_TTS_CHUNK_BYTES);
        }
    }
}
