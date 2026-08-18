//! 公开网页：搜索发现、抓取正文。已打开的浏览器标签仍走 browser_*。

use anyhow::{anyhow, bail, Result};
use regex::Regex;
use reqwest::redirect::Policy;
use serde_json::{json, Value};
use std::net::IpAddr;
use std::sync::OnceLock;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;

const SEARCH_MAX_RESULTS: usize = 8;
const FETCH_TIMEOUT_SECS: u64 = 20;
const SEARCH_TIMEOUT_SECS: u64 = 15;
const MAX_URL_CHARS: usize = 2048;
const MAX_RESPONSE_BYTES: usize = 2_000_000;
const MAX_BODY_CHARS: usize = 80_000;
const USER_AGENT: &str = "ShiGuang/0.1 (desktop assistant; +https://github.com)";

pub async fn execute(
    name: &str,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    match name {
        "web_search" => search(args, cancel).await,
        "web_fetch" => fetch(args, cancel).await,
        _ => bail!("未知网页工具: {name}"),
    }
}

async fn search(args: &Value, cancel: &CancellationToken) -> Result<Value> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("缺少 query"))?;
    let client = http_client(SEARCH_TIMEOUT_SECS)?;
    let mut last_err = None;
    for (engine, url) in search_endpoints(query) {
        if cancel.is_cancelled() {
            bail!("已中断");
        }
        match fetch_html(&client, &url, cancel).await {
            Ok((status, html)) if status >= 200 && status < 400 => {
                let results = parse_search_html(engine, &html);
                if !results.is_empty() {
                    return Ok(json!({
                        "query": query,
                        "engine": engine,
                        "results": results,
                        "note": "需要某条结果的全文时用 web_fetch。用户已经打开的页面用 browser_*，不要用本工具。",
                    }));
                }
            }
            Ok((status, _)) => last_err = Some(format!("{engine} 返回 HTTP {status}")),
            Err(e) => last_err = Some(format!("{engine}: {e}")),
        }
    }
    Err(anyhow!(
        "网页搜索失败：{}。可稍后再试，或让用户在浏览器打开后再用 browser_*。",
        last_err.unwrap_or_else(|| "没有可用结果".into())
    ))
}

fn search_endpoints(query: &str) -> Vec<(&'static str, String)> {
    let q: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    vec![
        (
            "bing",
            format!("https://cn.bing.com/search?q={q}&setlang=zh-CN"),
        ),
        (
            "duckduckgo",
            format!("https://html.duckduckgo.com/html/?q={q}"),
        ),
    ]
}

async fn fetch(args: &Value, cancel: &CancellationToken) -> Result<Value> {
    let raw = args
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("缺少 url"))?;
    let url = validate_fetch_url(raw)?;
    let client = http_client(FETCH_TIMEOUT_SECS)?;
    let (status, body, content_type) = fetch_url(&client, url.as_str(), cancel).await?;
    let text = if is_html(&content_type, &body) {
        html_to_text(&body)
    } else {
        body
    };
    let total = text.chars().count();
    let truncated = total > MAX_BODY_CHARS;
    let content: String = text.chars().take(MAX_BODY_CHARS).collect();
    Ok(json!({
        "url": url.as_str(),
        "status": status,
        "content_type": content_type,
        "content": content,
        "truncated": truncated,
        "note": "引用时用 markdown 链接。这是公开网页抓取；操作用户已打开的标签请用 browser_*。",
    }))
}

fn http_client(timeout_secs: u64) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(10))
        .redirect(Policy::limited(5))
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| anyhow!("创建 HTTP 客户端失败: {e}"))
}

async fn fetch_html(
    client: &reqwest::Client,
    url: &str,
    cancel: &CancellationToken,
) -> Result<(u16, String)> {
    let (status, body, _) = fetch_url(client, url, cancel).await?;
    Ok((status, body))
}

async fn fetch_url(
    client: &reqwest::Client,
    url: &str,
    cancel: &CancellationToken,
) -> Result<(u16, String, String)> {
    let request = client.get(url).send();
    let resp = tokio::select! {
        _ = cancel.cancelled() => bail!("已中断"),
        result = request => result.map_err(|e| anyhow!("请求失败: {e}"))?,
    };
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if let Some(len) = resp.content_length() {
        if len > MAX_RESPONSE_BYTES as u64 {
            bail!("页面过大（{} 字节）", len);
        }
    }
    let bytes = tokio::select! {
        _ = cancel.cancelled() => bail!("已中断"),
        result = resp.bytes() => result.map_err(|e| anyhow!("读取失败: {e}"))?,
    };
    if bytes.len() > MAX_RESPONSE_BYTES {
        bail!("页面过大（{} 字节）", bytes.len());
    }
    if is_binary(&content_type) {
        bail!("不支持的内容类型: {content_type}");
    }
    let body = String::from_utf8_lossy(&bytes).into_owned();
    Ok((status, body, content_type))
}

pub fn validate_fetch_url(raw: &str) -> Result<Url> {
    if raw.chars().count() > MAX_URL_CHARS {
        bail!("网址过长");
    }
    let url = Url::parse(raw).map_err(|_| anyhow!("网址无效"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        bail!("只支持 http/https 网址");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("网址不能包含账号密码");
    }
    let host = url.host_str().ok_or_else(|| anyhow!("网址缺少主机名"))?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        bail!("不能抓取本机地址");
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            bail!("不能抓取该地址");
        }
    }
    Ok(url)
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.octets() == [169, 254, 169, 254]
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified() || v6.is_multicast(),
    }
}

fn is_html(content_type: &str, body: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    ct.contains("text/html") || ct.contains("application/xhtml") || body.trim_start().starts_with("<!")
}

fn is_binary(content_type: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    if ct.is_empty() {
        return false;
    }
    if ct.contains("text/") || ct.contains("json") || ct.contains("xml") || ct.contains("html") {
        return false;
    }
    true
}

pub fn parse_search_html(engine: &str, html: &str) -> Vec<Value> {
    match engine {
        "bing" => parse_bing(html),
        _ => parse_duckduckgo(html),
    }
}

fn parse_bing(html: &str) -> Vec<Value> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r#"(?is)<li class="b_algo".*?<h2[^>]*>\s*<a[^>]+href="(https?://[^"]+)"[^>]*>(.*?)</a>"#,
        )
        .expect("bing regex")
    });
    collect_results(re, html, None)
}

fn parse_duckduckgo(html: &str) -> Vec<Value> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"(?is)<a[^>]+class="[^"]*result__a[^"]*"[^>]+href="([^"]+)"[^>]*>(.*?)</a>"#)
            .expect("ddg regex")
    });
    collect_results(re, html, Some(decode_ddg_href))
}

fn collect_results(
    re: &Regex,
    html: &str,
    rewrite: Option<fn(&str) -> Option<String>>,
) -> Vec<Value> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for cap in re.captures_iter(html) {
        let href = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let url = match rewrite {
            Some(f) => f(href).unwrap_or_else(|| href.to_string()),
            None => href.to_string(),
        };
        if !url.starts_with("http://") && !url.starts_with("https://") {
            continue;
        }
        if url.contains("bing.com/aclick") || url.contains("microsoft.com/en-us/bing") {
            continue;
        }
        if !seen.insert(url.clone()) {
            continue;
        }
        let title = strip_tags(cap.get(2).map(|m| m.as_str()).unwrap_or("")).trim().to_string();
        if title.is_empty() {
            continue;
        }
        out.push(json!({ "title": title, "url": url }));
        if out.len() >= SEARCH_MAX_RESULTS {
            break;
        }
    }
    out
}

fn decode_ddg_href(href: &str) -> Option<String> {
    let parsed = Url::parse(href)
        .or_else(|_| Url::parse(&format!("https:{href}")))
        .ok()?;
    if let Some((_, v)) = parsed.query_pairs().find(|(k, _)| k == "uddg") {
        return Some(v.into_owned());
    }
    if parsed.scheme() == "http" || parsed.scheme() == "https" {
        Some(parsed.to_string())
    } else {
        None
    }
}

pub fn html_to_text(html: &str) -> String {
    static SCRIPT: OnceLock<Regex> = OnceLock::new();
    static STYLE: OnceLock<Regex> = OnceLock::new();
    static NOSCRIPT: OnceLock<Regex> = OnceLock::new();
    static TAG: OnceLock<Regex> = OnceLock::new();
    let script = SCRIPT.get_or_init(|| Regex::new(r"(?is)<script[^>]*>.*?</script>").expect("script regex"));
    let style = STYLE.get_or_init(|| Regex::new(r"(?is)<style[^>]*>.*?</style>").expect("style regex"));
    let noscript =
        NOSCRIPT.get_or_init(|| Regex::new(r"(?is)<noscript[^>]*>.*?</noscript>").expect("noscript regex"));
    let tag = TAG.get_or_init(|| Regex::new(r"(?is)<[^>]+>").expect("tag regex"));
    let mut text = script.replace_all(html, " ").into_owned();
    text = style.replace_all(&text, " ").into_owned();
    text = noscript.replace_all(&text, " ").into_owned();
    text = text.replace("<br>", "\n").replace("<br/>", "\n").replace("<br />", "\n");
    text = text.replace("</p>", "\n\n").replace("</div>", "\n").replace("</h1>", "\n\n");
    text = text.replace("</h2>", "\n\n").replace("</li>", "\n");
    text = tag.replace_all(&text, " ").into_owned();
    text = decode_entities(&text);
    let mut out = String::new();
    let mut blank = 0u8;
    for line in text.lines() {
        let line = collapse_ws(line);
        if line.is_empty() {
            blank = blank.saturating_add(1);
            if blank <= 1 {
                out.push('\n');
            }
            continue;
        }
        blank = 0;
        out.push_str(&line);
        out.push('\n');
    }
    out.trim().to_string()
}

fn strip_tags(html: &str) -> String {
    static TAG: OnceLock<Regex> = OnceLock::new();
    let tag = TAG.get_or_init(|| Regex::new(r"(?is)<[^>]+>").expect("tag regex"));
    decode_entities(&tag.replace_all(html, "").into_owned())
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_and_loopback() {
        assert!(validate_fetch_url("file:///etc/passwd").is_err());
        assert!(validate_fetch_url("https://user:pass@example.com/").is_err());
        assert!(validate_fetch_url("http://127.0.0.1/secret").is_err());
        assert!(validate_fetch_url("http://169.254.169.254/latest").is_err());
        assert!(validate_fetch_url("https://example.com/a").is_ok());
    }

    #[test]
    fn html_to_text_strips_markup() {
        let html = "<html><script>alert(1)</script><p>你好 <b>世界</b></p></html>";
        let text = html_to_text(html);
        assert!(text.contains("你好 世界"));
        assert!(!text.contains("alert"));
    }

    #[test]
    fn parse_bing_results() {
        let html = r#"<ol><li class="b_algo"><h2><a href="https://example.com/page">示例 <strong>标题</strong></a></h2></li>
            <li class="b_algo"><h2><a href="https://example.org/b">第二页</a></h2></li></ol>"#;
        let results = parse_search_html("bing", html);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["url"], "https://example.com/page");
        assert_eq!(results[0]["title"], "示例 标题");
    }

    #[test]
    fn parse_ddg_uddg_links() {
        let html = r#"<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdocs">文档首页</a>"#;
        let results = parse_search_html("duckduckgo", html);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["url"], "https://example.com/docs");
        assert_eq!(results[0]["title"], "文档首页");
    }
}
