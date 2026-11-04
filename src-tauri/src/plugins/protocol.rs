//! `plugin://` 协议：把插件目录里的界面文件提供给 iframe。
//! 地址：`plugin://localhost/<插件id>/<相对路径>`（WebView2 上是 `http://plugin.localhost/...`）。
//! `__sdk/` 前缀固定指向 core 插件的 `ui-sdk/`，即插件 UI 用的 SDK 和主题变量。
//! 每个响应都带严格 CSP：插件页面不能直接访问本机文件（asset:）、IPC 或外网，
//! 只能通过 postMessage 走宿主 SDK。
use std::borrow::Cow;
use tauri::http::{header, Request, Response, StatusCode};

const CSP: &str = "default-src 'none'; script-src 'self' 'unsafe-inline' plugin: http://plugin.localhost; \
style-src 'self' 'unsafe-inline' plugin: http://plugin.localhost; img-src 'self' data: blob: plugin: http://plugin.localhost; \
font-src 'self' data: plugin: http://plugin.localhost; connect-src 'none'; frame-src 'none'; form-action 'none'; base-uri 'none'";

fn mime(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "wasm" => "application/wasm",
        "md" | "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn respond(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header("Content-Security-Policy", CSP)
        .header(header::CACHE_CONTROL, "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .body(Cow::Owned(body))
        .unwrap()
}

fn not_found(message: &str) -> Response<Cow<'static, [u8]>> {
    respond(StatusCode::NOT_FOUND, "text/plain; charset=utf-8", message.as_bytes().to_vec())
}

/// 把请求路径解析成磁盘文件。只允许插件目录内的文件。
pub fn resolve(path: &str) -> Result<std::path::PathBuf, String> {
    let decoded = percent_decode(path.trim_start_matches('/'));
    let (id, rest) = decoded.split_once('/').ok_or("地址缺少插件 id")?;
    if rest.is_empty() || rest.contains('\\') {
        return Err("地址不合法".into());
    }
    let snapshot = super::snapshot();
    let (plugin_dir, rel) = if id == "__sdk" {
        let core = snapshot.plugin("core").ok_or("core 插件不存在")?;
        (core.dir.join("ui-sdk"), rest.to_string())
    } else {
        let plugin = snapshot.plugin(id).ok_or("插件不存在")?;
        if !plugin.enabled {
            return Err("插件未启用".into());
        }
        (plugin.dir.clone(), rest.to_string())
    };
    // 界面文件一律不允许越出插件目录（内置插件也一样）。
    let path = super::resolve_inside(&plugin_dir, &rel, false).map_err(|e| e.to_string())?;
    Ok(path)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

pub fn handle(request: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let path = request.uri().path().to_string();
    match resolve(&path) {
        Ok(file) => match std::fs::read(&file) {
            Ok(body) => respond(StatusCode::OK, mime(&path), body),
            Err(_) => not_found("文件不存在"),
        },
        Err(message) => not_found(&message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_and_rejects_bad_paths() {
        assert_eq!(percent_decode("a%20b/%E4%B8%AD.html"), "a b/中.html");
        assert!(resolve("/core/../x").is_err());
        assert!(resolve("/core").is_err());
        assert!(resolve("/__sdk/sdk.js").is_ok());
    }
}
