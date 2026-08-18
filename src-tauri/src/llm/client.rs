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

/// 一次 /chat/completions 调用的 token 用量。字段兼容 DeepSeek / OpenAI / 通义等。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cache_hit_tokens: i64,
    pub cache_miss_tokens: i64,
}

impl TokenUsage {
    pub fn is_empty(&self) -> bool {
        self.prompt_tokens == 0 && self.completion_tokens == 0 && self.total_tokens == 0
    }

    /// 从完整响应或 SSE 分片里提取 usage。无 usage / 空对象则返回默认值。
    pub fn from_response(v: &Value) -> Self {
        let Some(usage) = v.get("usage").filter(|u| u.is_object()) else {
            return Self::default();
        };
        if usage.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            return Self::default();
        }
        let prompt = json_i64(usage.get("prompt_tokens"));
        let completion = json_i64(usage.get("completion_tokens"));
        let mut total = json_i64(usage.get("total_tokens"));
        if total <= 0 {
            total = prompt + completion;
        }

        let mut cache_hit = json_i64(usage.get("prompt_cache_hit_tokens"));
        if cache_hit == 0 {
            cache_hit = json_i64(
                usage
                    .get("prompt_tokens_details")
                    .and_then(|d| d.get("cached_tokens")),
            );
        }
        if cache_hit == 0 {
            cache_hit = json_i64(usage.get("cache_read_input_tokens"));
        }

        let reported_miss = usage.get("prompt_cache_miss_tokens").is_some();
        let mut cache_miss = json_i64(usage.get("prompt_cache_miss_tokens"));
        let cache_reported = reported_miss
            || usage.get("prompt_cache_hit_tokens").is_some()
            || usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .is_some()
            || usage.get("cache_read_input_tokens").is_some();
        if cache_reported && !reported_miss && prompt > 0 {
            cache_miss = (prompt - cache_hit).max(0);
        }

        Self {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            cache_hit_tokens: cache_hit,
            cache_miss_tokens: cache_miss,
        }
    }
}

fn json_i64(v: Option<&Value>) -> i64 {
    let Some(v) = v else { return 0 };
    v.as_i64()
        .or_else(|| v.as_u64().map(|n| n.min(i64::MAX as u64) as i64))
        .or_else(|| v.as_f64().map(|n| n as i64))
        .unwrap_or(0)
}

#[derive(Debug, Default)]
pub struct AssistantResp {
    pub content: String,
    /// 思考模式下的思维链（DeepSeek reasoning_content），工具调用轮次必须回传
    pub reasoning_content: String,
    pub tool_calls: Vec<ToolCall>,
    /// 用户中断时为 true；content 保留已流出的部分内容
    pub interrupted: bool,
    pub usage: TokenUsage,
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
            let parsed = TokenUsage::from_response(&v);
            if !parsed.is_empty() {
                out.usage = parsed;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn usage_parses_deepseek_cache_fields() {
        let v = json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 1200,
                "completion_tokens": 80,
                "total_tokens": 1280,
                "prompt_cache_hit_tokens": 1000,
                "prompt_cache_miss_tokens": 200
            }
        });
        assert_eq!(
            TokenUsage::from_response(&v),
            TokenUsage {
                prompt_tokens: 1200,
                completion_tokens: 80,
                total_tokens: 1280,
                cache_hit_tokens: 1000,
                cache_miss_tokens: 200,
            }
        );
    }

    #[test]
    fn usage_parses_openai_cached_tokens() {
        let v = json!({
            "usage": {
                "prompt_tokens": 500,
                "completion_tokens": 40,
                "total_tokens": 540,
                "prompt_tokens_details": { "cached_tokens": 420 }
            }
        });
        let u = TokenUsage::from_response(&v);
        assert_eq!(u.cache_hit_tokens, 420);
        assert_eq!(u.cache_miss_tokens, 80);
        assert_eq!(u.total_tokens, 540);
    }

    #[test]
    fn usage_ignores_null_and_empty() {
        assert!(TokenUsage::from_response(&json!({ "delta": "hi" })).is_empty());
        assert!(TokenUsage::from_response(&json!({ "usage": null })).is_empty());
        assert!(TokenUsage::from_response(&json!({ "usage": {} })).is_empty());
    }
}
