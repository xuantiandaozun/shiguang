//! 部分供应商需要改请求头或清洗模型名（AgentRouter 客户端校验、Gemini OpenRouter 前缀等）。

use reqwest::RequestBuilder;
use serde_json::Value;

pub fn is_agentrouter(base_url: &str) -> bool {
    base_url.to_ascii_lowercase().contains("agentrouter.org")
}

pub fn is_google_gemini(base_url: &str) -> bool {
    let u = base_url.to_ascii_lowercase();
    u.contains("generativelanguage") || u.contains("googleapis.com")
}

pub fn is_deepseek(base_url: &str) -> bool {
    base_url.to_ascii_lowercase().contains("api.deepseek.com")
}

/// Google 直连不要带 OpenRouter 的 `google/` 前缀；2.5/2.0 对新账号已 404，改发 3.5 Flash。
pub fn sanitize_model(base_url: &str, model: &str) -> String {
    let mut m = model.trim().to_string();
    if m.is_empty() {
        return m;
    }
    if is_deepseek(base_url)
        && matches!(
            m.as_str(),
            "deepseek-v4-flash" | "deepseek-v4-flash-vision-exp"
        )
    {
        return "deepseek-flash".into();
    }
    if !is_google_gemini(base_url) {
        return m;
    }
    if let Some(rest) = m.strip_prefix("google/") {
        m = rest.to_string();
    }
    match m.as_str() {
        "gemini-2.5-flash" | "gemini-2.5-pro" | "gemini-2.5-flash-lite" | "gemini-2.0-flash" => {
            "gemini-3.5-flash".into()
        }
        _ => m,
    }
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
    fn sanitizes_google_gemini_openrouter_prefix_and_retired_models() {
        let gemini = "https://generativelanguage.googleapis.com/v1beta/openai";
        assert_eq!(
            sanitize_model(gemini, "google/gemini-2.5-flash"),
            "gemini-3.5-flash"
        );
        assert_eq!(
            sanitize_model(gemini, "gemini-2.0-flash"),
            "gemini-3.5-flash"
        );
        assert_eq!(
            sanitize_model(gemini, "gemini-3.5-flash"),
            "gemini-3.5-flash"
        );
        assert_eq!(
            sanitize_model("https://openrouter.ai/api/v1", "google/gemini-2.5-flash"),
            "google/gemini-2.5-flash"
        );
    }

    #[test]
    fn migrates_retired_deepseek_flash_aliases_on_official_api() {
        let deepseek = "https://api.deepseek.com/v1";
        assert_eq!(
            sanitize_model(deepseek, "deepseek-v4-flash"),
            "deepseek-flash"
        );
        assert_eq!(
            sanitize_model(deepseek, "deepseek-v4-flash-vision-exp"),
            "deepseek-flash"
        );
        assert_eq!(
            sanitize_model("https://openrouter.ai/api/v1", "deepseek-v4-flash"),
            "deepseek-v4-flash"
        );
    }

    #[test]
    fn strips_reasoning_for_claude_on_agentrouter() {
        let mut body = json!({
            "model": "claude-opus-4-8",
            "reasoning_effort": "high",
            "messages": []
        });
        sanitize_agentrouter_body(&mut body, "https://agentrouter.org/v1", "claude-opus-4-8");
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["thinking"]["type"], "adaptive");
    }
}
