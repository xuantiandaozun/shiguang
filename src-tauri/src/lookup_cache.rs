//! 外部参考数据的本地缓存：给 CLI / API 的稳定对照表（id↔名称、字段、选项）复用，
//! 避免每次任务都把大段实时输出灌进对话。不绑定某一家 CLI。

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const DEFAULT_TTL_SECS: i64 = 7 * 24 * 3600;
const MAX_KEY_CHARS: usize = 120;
const MAX_VALUE_CHARS: usize = 24_000;
const MAX_SUMMARY_CHARS: usize = 80;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LookupEntry {
    pub key: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub summary: String,
    pub value: String,
    #[serde(default = "default_ttl")]
    pub ttl_secs: i64,
    pub updated_at: String,
}

fn default_ttl() -> i64 {
    DEFAULT_TTL_SECS
}

pub struct LookupCache {
    path: PathBuf,
    inner: Mutex<BTreeMap<String, LookupEntry>>,
}

impl LookupCache {
    pub fn new(app_dir: &Path) -> Self {
        let path = app_dir.join("lookup-cache.json");
        let map = load_map(&path);
        Self {
            path,
            inner: Mutex::new(map),
        }
    }

    pub fn get(&self, key: &str) -> Result<Value> {
        let key = normalize_key(key)?;
        let guard = self.inner.lock().map_err(|e| anyhow!(e.to_string()))?;
        match guard.get(&key) {
            None => Ok(json!({
                "hit": false,
                "key": key,
                "note": "无缓存。请拉取外部数据，提炼成对照表后再 put；不要把 CLI 原始大段输出原样写入。",
            })),
            Some(entry) => {
                let now = crate::db::now_str();
                let fresh = is_fresh(&entry.updated_at, entry.ttl_secs, &now);
                Ok(json!({
                    "hit": true,
                    "fresh": fresh,
                    "key": entry.key,
                    "source": entry.source,
                    "summary": entry.summary,
                    "value": entry.value,
                    "ttl_secs": entry.ttl_secs,
                    "updated_at": entry.updated_at,
                    "note": if fresh {
                        "直接使用这份对照表，不必再调外部 CLI。"
                    } else {
                        "已过期。可先用这份，若名单可能变了再拉取并 put 覆盖。"
                    },
                }))
            }
        }
    }

    pub fn put(
        &self,
        key: &str,
        value: &str,
        source: Option<&str>,
        summary: Option<&str>,
        ttl_secs: Option<i64>,
    ) -> Result<Value> {
        let key = normalize_key(key)?;
        let value = value.trim();
        if value.is_empty() {
            bail!("value 不能为空");
        }
        if value.chars().count() > MAX_VALUE_CHARS {
            bail!(
                "value 过长（上限 {} 字）。请只保存提炼后的对照表（如 id↔名称），不要保存 CLI 原始输出。",
                MAX_VALUE_CHARS
            );
        }
        let ttl = match ttl_secs {
            Some(n) if n < 0 => bail!("ttl_secs 不能为负；0 表示不过期"),
            Some(n) => n.min(90 * 24 * 3600),
            None => DEFAULT_TTL_SECS,
        };
        let source = source.unwrap_or("").trim().chars().take(40).collect();
        let summary = summarize(summary, value);
        let entry = LookupEntry {
            key: key.clone(),
            source,
            summary,
            value: value.to_string(),
            ttl_secs: ttl,
            updated_at: crate::db::now_str(),
        };
        let mut guard = self.inner.lock().map_err(|e| anyhow!(e.to_string()))?;
        guard.insert(key.clone(), entry.clone());
        persist(&self.path, &guard)?;
        Ok(json!({
            "ok": true,
            "key": key,
            "summary": entry.summary,
            "ttl_secs": entry.ttl_secs,
            "updated_at": entry.updated_at,
            "bytes": entry.value.len(),
        }))
    }

    pub fn list(&self, source: Option<&str>) -> Result<Value> {
        let guard = self.inner.lock().map_err(|e| anyhow!(e.to_string()))?;
        let now = crate::db::now_str();
        let source = source.map(str::trim).filter(|s| !s.is_empty());
        let items: Vec<Value> = guard
            .values()
            .filter(|e| source.is_none_or(|s| e.source == s))
            .map(|e| list_item(e, &now))
            .collect();
        Ok(json!({ "count": items.len(), "items": items }))
    }

    pub fn delete(&self, key: &str) -> Result<Value> {
        let key = normalize_key(key)?;
        let mut guard = self.inner.lock().map_err(|e| anyhow!(e.to_string()))?;
        let removed = guard.remove(&key).is_some();
        if removed {
            persist(&self.path, &guard)?;
        }
        Ok(json!({ "ok": true, "deleted": removed, "key": key }))
    }

    /// 对话尾部的短目录：只有 key/摘要/新旧，不含 value。
    pub fn catalog_block(&self) -> Option<String> {
        let Ok(guard) = self.inner.lock() else {
            return None;
        };
        if guard.is_empty() {
            return None;
        }
        let now = crate::db::now_str();
        let mut out = String::from(
            "\n\n外部参考缓存（稳定对照：id↔名称、字段/选项。lookup_cache get 读取；没有或过期再拉取，只 put 提炼后的表）：",
        );
        for entry in guard.values() {
            let fresh = is_fresh(&entry.updated_at, entry.ttl_secs, &now);
            let age = age_label(&entry.updated_at, &now);
            let state = if fresh { "有效" } else { "已过期" };
            let summary = if entry.summary.is_empty() {
                ""
            } else {
                &entry.summary
            };
            if summary.is_empty() {
                out.push_str(&format!("\n- {} · {} · {}", entry.key, age, state));
            } else {
                out.push_str(&format!(
                    "\n- {} · {} · {} · {}",
                    entry.key, summary, age, state
                ));
            }
        }
        Some(out)
    }
}

fn list_item(entry: &LookupEntry, now: &str) -> Value {
    json!({
        "key": entry.key,
        "source": entry.source,
        "summary": entry.summary,
        "fresh": is_fresh(&entry.updated_at, entry.ttl_secs, now),
        "ttl_secs": entry.ttl_secs,
        "updated_at": entry.updated_at,
        "chars": entry.value.chars().count(),
    })
}

fn load_map(path: &Path) -> BTreeMap<String, LookupEntry> {
    let Ok(raw) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    match serde_json::from_str::<BTreeMap<String, LookupEntry>>(&raw) {
        Ok(map) => map,
        Err(e) => {
            log::warn!("读取 lookup-cache.json 失败，将重建: {}", e);
            BTreeMap::new()
        }
    }
}

fn persist(path: &Path, map: &BTreeMap<String, LookupEntry>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(map)?;
    fs::write(path, json)?;
    Ok(())
}

fn normalize_key(raw: &str) -> Result<String> {
    let key = raw.trim();
    if key.is_empty() {
        bail!("缺少 key");
    }
    if key.chars().count() > MAX_KEY_CHARS {
        bail!("key 过长（上限 {} 字）", MAX_KEY_CHARS);
    }
    let mut chars = key.chars();
    let first = chars.next().unwrap();
    if !is_key_start(first) || !chars.all(is_key_char) {
        bail!("key 仅允许字母/数字/中文与 . _ : - /，如 lark.base.projects");
    }
    Ok(key.to_string())
}

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

fn is_key_start(c: char) -> bool {
    c.is_ascii_alphanumeric() || is_cjk(c)
}

fn is_key_char(c: char) -> bool {
    is_key_start(c) || matches!(c, '.' | '_' | ':' | '-' | '/')
}

fn summarize(summary: Option<&str>, value: &str) -> String {
    let from_arg = summary.unwrap_or("").trim();
    let src = if from_arg.is_empty() { value } else { from_arg };
    src.chars()
        .filter(|c| !c.is_control())
        .take(MAX_SUMMARY_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

fn parse_local(ts: &str) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S").ok()
}

fn is_fresh(updated_at: &str, ttl_secs: i64, now: &str) -> bool {
    if ttl_secs <= 0 {
        return true;
    }
    let (Some(then), Some(now)) = (parse_local(updated_at), parse_local(now)) else {
        return false;
    };
    let age = (now - then).num_seconds();
    age >= 0 && age < ttl_secs
}

fn age_label(updated_at: &str, now: &str) -> String {
    let (Some(then), Some(now)) = (parse_local(updated_at), parse_local(now)) else {
        return updated_at.to_string();
    };
    let secs = (now - then).num_seconds().max(0);
    if secs < 60 {
        "刚刚".into()
    } else if secs < 3600 {
        format!("{}分钟前", secs / 60)
    } else if secs < 86400 {
        format!("{}小时前", secs / 3600)
    } else {
        format!("{}天前", secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_cache() -> (LookupCache, PathBuf) {
        let dir = std::env::temp_dir().join(format!("dh-lookup-{}", uuid::Uuid::new_v4()));
        let _ = fs::create_dir_all(&dir);
        (LookupCache::new(&dir), dir)
    }

    #[test]
    fn put_get_roundtrip_and_catalog() {
        let (cache, dir) = tmp_cache();
        cache
            .put(
                "lark.base.T1O7.projects",
                r#"[{"id":"recvnhrv72kYZE","name":"长虹新能源2.0"}]"#,
                Some("lark-cli"),
                Some("项目管理表 id↔名称"),
                Some(604800),
            )
            .unwrap();
        let got = cache.get("lark.base.T1O7.projects").unwrap();
        assert_eq!(got["hit"], true);
        assert_eq!(got["fresh"], true);
        assert!(got["value"].as_str().unwrap().contains("长虹新能源2.0"));
        let cat = cache.catalog_block().unwrap();
        assert!(cat.contains("lark.base.T1O7.projects"));
        assert!(cat.contains("有效"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn miss_and_reject_raw_dump() {
        let (cache, dir) = tmp_cache();
        let miss = cache.get("aliyun.ecs.regions").unwrap();
        assert_eq!(miss["hit"], false);
        let huge = "x".repeat(MAX_VALUE_CHARS + 10);
        assert!(cache
            .put("aliyun.ecs.regions", &huge, None, None, None)
            .is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ttl_zero_never_expires_and_stale_helper() {
        assert!(is_fresh("2020-01-01 00:00:00", 0, "2026-08-18 10:00:00"));
        assert!(!is_fresh(
            "2026-08-01 00:00:00",
            86400,
            "2026-08-18 10:00:00"
        ));
        assert!(is_fresh("2026-08-18 09:00:00", 7200, "2026-08-18 10:00:00"));
    }

    #[test]
    fn key_allows_namespaced_and_cjk() {
        assert!(normalize_key("lark.base.projects").is_ok());
        assert!(normalize_key("飞书/项目对照").is_ok());
        assert!(normalize_key("../etc/passwd").is_err());
        assert!(normalize_key("").is_err());
    }
}
