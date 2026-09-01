//! 部分聚合网关（如 AgentRouter）会校验客户端身份，需附加与已放行 Coding Agent 一致的请求头。

use reqwest::RequestBuilder;
use serde_json::Value;

pub fn is_agentrouter(base_url: &str) -> bool {
    base_url.to_ascii_lowercase().contains("agentrouter.org")
}

/// 为 LLM HTTP 请求附加供应商要求的头；不改变 URL 与 body。
pub fn apply_llm_headers(req: RequestBuilder, base_url: &str) -> RequestBuilder {
    if !is_agentrouter(base_url) {
        return req;
    }
    // AgentRouter 仅放行部分 Coding Agent 客户端；参考社区 opencode-agentrouter 插件。
    req.header("http-referer", "https://github.com/RooVetGit/Roo-Cline")
        .header("x-title", "Roo Code")
        .header("User-Agent", "RooCode/3.52.1")
        .header("x-stainless-arch", "x64")
        .header("x-stainless-lang", "js")
        .header("x-stainless-os", "Windows")
        .header("x-stainless-package-version", "6.34.0")
        .header("x-stainless-runtime", "node")
        .header("x-stainless-runtime-version", "v22.12.0")
}

/// AgentRouter 上的 Claude 模型对部分 OpenAI 扩展字段敏感，发送前做最小清理。
pub fn sanitize_agentrouter_body(body: &mut Value, base_url: &str, model: &str) {
    if !is_agentrouter(base_url) {
        return;
    }
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    let model_lower = model.to_ascii_lowercase();
    if model_lower.starts_with("claude") {
        obj.remove("reasoning_effort");
        obj.remove("reasoning");
        if !obj.contains_key("thinking") {
            obj.insert("thinking".into(), serde_json::json!({ "type": "adaptive" }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_agentrouter_host() {
        assert!(is_agentrouter("https://agentrouter.org/v1"));
        assert!(!is_agentrouter("https://api.openai.com/v1"));
    }

    #[test]
    fn strips_reasoning_for_claude_on_agentrouter() {
        let mut body = json!({
            "model": "claude-opus-4-6",
            "reasoning_effort": "high",
            "messages": []
        });
        sanitize_agentrouter_body(&mut body, "https://agentrouter.org/v1", "claude-opus-4-6");
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["thinking"]["type"], "adaptive");
    }
}
