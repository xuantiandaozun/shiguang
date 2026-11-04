//! 插件设置：按插件 settings schema 渲染表单，值存在 settings 表的 `plugin.<id>` 键（JSON 对象）。
//! 启停覆盖存在 `plugins.enabled`（{id: bool}）。
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tauri::{AppHandle, Manager};

pub const ENABLED_KEY: &str = "plugins.enabled";

fn key(id: &str) -> String {
    format!("plugin.{id}")
}

/// 插件设置值：schema 默认值 + 已保存的值。
pub fn values(app: &AppHandle, id: &str) -> Value {
    let state = app.state::<crate::AppState>();
    let saved: Value = state
        .db
        .get_setting(&key(id))
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    with_defaults(id, saved)
}

fn with_defaults(id: &str, saved: Value) -> Value {
    let snapshot = super::snapshot();
    let mut out = serde_json::Map::new();
    if let Some(schema) = snapshot
        .plugin(id)
        .and_then(|p| p.manifest.settings.clone())
        .and_then(|rel| snapshot.read_json(id, &rel).ok())
    {
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            for (k, prop) in props {
                if let Some(default) = prop.get("default") {
                    out.insert(k.clone(), default.clone());
                }
            }
        }
    }
    if let Some(saved) = saved.as_object() {
        for (k, v) in saved {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}

/// 只含 schema 默认值（没有数据库时使用）。
pub fn defaults_for(plugin: &super::Plugin) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(rel) = &plugin.manifest.settings {
        if let Ok(path) = super::resolve_inside(&plugin.dir, rel, plugin.builtin) {
            if let Some(schema) = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
                if let Some(props) = schema.get("properties").and_then(Value::as_object) {
                    for (k, prop) in props {
                        if let Some(default) = prop.get("default") {
                            out.insert(k.clone(), default.clone());
                        }
                    }
                }
            }
        }
    }
    Value::Object(out)
}

pub fn save(app: &AppHandle, id: &str, values: &Value) -> anyhow::Result<Value> {
    if !values.is_object() {
        anyhow::bail!("插件设置必须是对象");
    }
    let state = app.state::<crate::AppState>();
    state.db.set_setting(&key(id), &values.to_string())?;
    Ok(self::values(app, id))
}

pub fn load_overrides(db: &crate::db::Db) -> BTreeMap<String, bool> {
    db.get_setting(ENABLED_KEY)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_overrides(db: &crate::db::Db, overrides: &BTreeMap<String, bool>) -> anyhow::Result<()> {
    db.set_setting(ENABLED_KEY, &serde_json::to_string(overrides)?)
}
