use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Default)]
pub struct AssistantResp {
    pub content: String,
    /// 思考模式下的思维链（DeepSeek reasoning_content），工具调用轮次必须回传
    pub reasoning_content: String,
    pub tool_calls: Vec<ToolCall>,
    /// 用户中断时为 true；content 保留已流出的部分内容
    pub interrupted: bool,
}

/// 以 SSE 流式方式调用 OpenAI 兼容的 /chat/completions，文本增量经 on_text 回调透出，
/// 思维链增量经 on_reasoning 透出，工具调用增量在内存中聚合后整体返回。
/// cancel 触发时立即丢弃 HTTP 流（请求随之中断），
/// 已聚合的内容/工具调用随 interrupted=true 返回。
pub async fn stream_chat(
    client: &reqwest::Client,
    cfg: &LlmConfig,
    body: &Value,
    cancel: &tokio_util::sync::CancellationToken,
    on_text: impl Fn(&str),
    on_reasoning: impl Fn(&str),
) -> Result<AssistantResp> {
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .bearer_auth(&cfg.api_key)
        .json(body)
        .send()
        .await
        .map_err(|e| anyhow!("请求 LLM 失败: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let short: String = text.chars().take(300).collect();
        return Err(anyhow!("LLM API 返回 {}: {}", status, short));
    }

    let mut out = AssistantResp::default();
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();

    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => {
                out.interrupted = true;
                return Ok(out);
            }
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(|e| anyhow!("读取响应流失败: {}", e))?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let line = buf[..pos].trim_end().to_string();
            let rest = buf.split_off(pos + 1);
            buf = rest;
            if !line.starts_with("data:") {
                continue;
            }
            let data = line["data:".len()..].trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            let Some(delta) = v
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"))
            else {
                continue;
            };
            if let Some(rc) = delta.get("reasoning_content").and_then(|c| c.as_str()) {
                if !rc.is_empty() {
                    out.reasoning_content.push_str(rc);
                    on_reasoning(rc);
                }
            }
            if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                if !content.is_empty() {
                    out.content.push_str(content);
                    on_text(content);
                }
            }
            if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tcs {
                    let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    while out.tool_calls.len() <= idx {
                        out.tool_calls.push(ToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                        });
                    }
                    let acc = &mut out.tool_calls[idx];
                    if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                        if !id.is_empty() {
                            acc.id = id.to_string();
                        }
                    }
                    if let Some(f) = tc.get("function") {
                        if let Some(n) = f.get("name").and_then(|n| n.as_str()) {
                            acc.name.push_str(n);
                        }
                        if let Some(a) = f.get("arguments").and_then(|a| a.as_str()) {
                            acc.arguments.push_str(a);
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}
