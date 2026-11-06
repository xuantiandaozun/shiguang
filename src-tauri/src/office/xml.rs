//! 极简 XML 扫描：按字节偏移给出标签与文本，供 OOXML 局部读取和原位修改（不重排、不丢未知内容）。

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// 开始标签（self_closing 为 `<x/>`）
    Start { name: String, start: usize, end: usize, self_closing: bool },
    End { name: String, start: usize, end: usize },
    Text { start: usize, end: usize },
}

/// 扫描整段 XML。注释、处理指令、CDATA 跳过（CDATA 在 OOXML 正文里基本不出现）。
pub fn tokens(s: &str) -> Vec<Tok> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            let start = i;
            while i < b.len() && b[i] != b'<' {
                i += 1;
            }
            out.push(Tok::Text { start, end: i });
            continue;
        }
        let start = i;
        if s[i..].starts_with("<!--") {
            i = s[i..].find("-->").map(|p| i + p + 3).unwrap_or(b.len());
            continue;
        }
        if s[i..].starts_with("<![CDATA[") {
            i = s[i..].find("]]>").map(|p| i + p + 3).unwrap_or(b.len());
            continue;
        }
        if s[i..].starts_with("<?") || s[i..].starts_with("<!") {
            i = s[i..].find('>').map(|p| i + p + 1).unwrap_or(b.len());
            continue;
        }
        // 找标签结束的 '>'，跳过引号里的内容
        let mut j = i + 1;
        let mut quote: Option<u8> = None;
        while j < b.len() {
            match (quote, b[j]) {
                (None, b'"') | (None, b'\'') => quote = Some(b[j]),
                (Some(q), c) if c == q => quote = None,
                (None, b'>') => break,
                _ => {}
            }
            j += 1;
        }
        let end = (j + 1).min(b.len());
        let inner = &s[i + 1..j.min(b.len())];
        if let Some(rest) = inner.strip_prefix('/') {
            out.push(Tok::End { name: rest.trim().to_string(), start, end });
        } else {
            let self_closing = inner.ends_with('/');
            let name = inner.trim_end_matches('/').split(|c: char| c.is_whitespace()).next().unwrap_or("").to_string();
            out.push(Tok::Start { name, start, end, self_closing });
        }
        i = end;
    }
    out
}

/// 找到与 toks[idx]（开始标签）匹配的结束标签下标；自闭合返回自身
pub fn matching_end(toks: &[Tok], idx: usize) -> usize {
    let Tok::Start { name, self_closing, .. } = &toks[idx] else { return idx };
    if *self_closing {
        return idx;
    }
    let mut depth = 0usize;
    for (k, t) in toks.iter().enumerate().skip(idx + 1) {
        match t {
            Tok::Start { name: n, self_closing: false, .. } if n == name => depth += 1,
            Tok::End { name: n, .. } if n == name => {
                if depth == 0 {
                    return k;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    toks.len() - 1
}

pub fn tok_start(t: &Tok) -> usize {
    match t {
        Tok::Start { start, .. } | Tok::End { start, .. } | Tok::Text { start, .. } => *start,
    }
}

pub fn tok_end(t: &Tok) -> usize {
    match t {
        Tok::Start { end, .. } | Tok::End { end, .. } | Tok::Text { end, .. } => *end,
    }
}

/// 取标签里某个属性的值（如 w:val）
pub fn attr<'a>(tag: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(p) = rest.find(key) {
        let before_ok = p == 0 || rest.as_bytes()[p - 1].is_ascii_whitespace();
        let after = &rest[p + key.len()..];
        let after_trim = after.trim_start();
        if before_ok && after_trim.starts_with('=') {
            let v = after_trim[1..].trim_start();
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                let body = &v[1..];
                return body.find(q).map(|e| &body[..e]);
            }
        }
        rest = &rest[p + key.len()..];
    }
    None
}

pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        let tail = &rest[p..];
        let Some(semi) = tail.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let ent = &tail[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            // XML 1.0 不允许的控制字符直接丢掉
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// 按 (起, 止, 新内容) 批量替换；区间不能重叠
pub fn splice(s: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    edits.sort_by_key(|e| e.0);
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    for (a, b, text) in edits {
        if a < pos {
            continue;
        }
        out.push_str(&s[pos..a]);
        out.push_str(&text);
        pos = b;
    }
    out.push_str(&s[pos..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_and_matches() {
        let s = r#"<w:p a="x>y"><w:r><w:t>Hi &amp; bye</w:t></w:r><w:br/></w:p>"#;
        let t = tokens(s);
        assert!(matches!(&t[0], Tok::Start { name, .. } if name == "w:p"));
        let end = matching_end(&t, 0);
        assert!(matches!(&t[end], Tok::End { name, .. } if name == "w:p"));
        let text = t.iter().find_map(|x| if let Tok::Text { start, end } = x { Some(&s[*start..*end]) } else { None }).unwrap();
        assert_eq!(unescape(text), "Hi & bye");
        assert!(t.iter().any(|x| matches!(x, Tok::Start { name, self_closing: true, .. } if name == "w:br")));
    }

    #[test]
    fn attrs_and_escape() {
        assert_eq!(attr(r#"<w:pStyle w:val="Heading1"/>"#, "w:val"), Some("Heading1"));
        assert_eq!(attr(r#"<a r:id="rId5" id='3'>"#, "id"), Some("3"));
        assert_eq!(escape("a<b&\"c\u{1}"), "a&lt;b&amp;&quot;c");
        assert_eq!(unescape("&#x4E2D;&#25991;&bogus"), "中文&bogus");
    }
}
