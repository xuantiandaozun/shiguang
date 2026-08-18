//! Agent Skills：可复用工作流说明书（兼容 Claude / Codex / Cursor 的 SKILL.md）。
//!
//! ## 两类技能
//! - **internal（内部）**：编译期嵌入（`builtin_skills`），只读；只能改代码重新打包更新。
//!   用户可启用/禁用，AI/UI 不能创建、覆盖、删除。
//! - **external（外部）**：`app_data/skills/<name>/`，AI 与用户可自由创建/修改/删除/从
//!   Claude·Codex·Cursor 同步；也承接由旧「工作流经验」迁移来的条目。
//!
//! 启用技能目录作为独立 system 消息插在系统提示之后、历史之前；
//! 命中后再 `load_skill` 拉全文。启用集不变则目录正文逐字节稳定，便于前缀缓存。

use crate::builtin_skills;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 单个技能的目录摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    /// internal = 应用内置只读；external = 用户目录可写
    pub scope: String,
    /// external 来源细分：local / synced / migrated；internal 固定为 builtin
    pub source: String,
    /// 同步来源：claude / codex / cursor / cursor-builtin；否则空
    pub synced_from: String,
    pub path: String,
    pub updated_at: String,
}

/// 本机其它工具目录里扫到的候选（尚未导入为本应用 external skill）
#[derive(Debug, Clone, Serialize)]
pub struct ExternalSkill {
    pub name: String,
    pub description: String,
    pub source: String,
    pub path: String,
    pub already_synced: bool,
}

struct Meta {
    enabled: HashMap<String, bool>,
    synced_from: HashMap<String, String>,
}

pub struct SkillStore {
    /// 外部技能根目录 app_data/skills
    dir: PathBuf,
    meta_path: PathBuf,
    meta: Mutex<Meta>,
}

impl SkillStore {
    pub fn new(app_dir: &Path) -> Self {
        let dir = app_dir.join("skills");
        let _ = fs::create_dir_all(&dir);
        let meta_path = dir.join(".meta.json");
        let meta = load_meta(&meta_path);
        Self {
            dir,
            meta_path,
            meta: Mutex::new(meta),
        }
    }

    pub fn root(&self) -> &Path {
        &self.dir
    }

    pub fn is_internal(name: &str) -> bool {
        builtin_skills::get(name).is_some()
    }

    /// 列出全部技能（内部 + 外部）
    pub fn list(&self) -> Vec<SkillInfo> {
        let meta = self.meta.lock().ok();
        let mut out = Vec::new();
        let mut names: HashSet<String> = HashSet::new();

        // 内部技能优先
        for (name, content) in builtin_skills::ALL {
            let fake_path = PathBuf::from(format!("{}/SKILL.md", name));
            let Ok(parsed) = parse_frontmatter(content, &fake_path) else {
                continue;
            };
            let enabled = meta
                .as_ref()
                .and_then(|m| m.enabled.get(*name).copied())
                .unwrap_or(true);
            names.insert(name.to_string());
            out.push(SkillInfo {
                name: parsed.name,
                description: parsed.description,
                enabled,
                scope: "internal".into(),
                source: "builtin".into(),
                synced_from: String::new(),
                path: format!("builtin://{}", name),
                updated_at: String::new(),
            });
        }

        // 外部技能（同名被内部占用时跳过，避免影子条目）
        let Ok(entries) = fs::read_dir(&self.dir) else {
            out.sort_by(|a, b| match (a.scope.as_str(), b.scope.as_str()) {
                ("internal", "external") => std::cmp::Ordering::Less,
                ("external", "internal") => std::cmp::Ordering::Greater,
                _ => a.name.cmp(&b.name),
            });
            return out;
        };
        for e in entries.flatten() {
            let path = e.path();
            if !path.is_dir() {
                continue;
            }
            let folder = match path.file_name().and_then(|s| s.to_str()) {
                Some(n) if !n.starts_with('.') => n.to_string(),
                _ => continue,
            };
            if names.contains(&folder) || Self::is_internal(&folder) {
                continue;
            }
            let skill_md = path.join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }
            let Ok(parsed) = parse_skill_file(&skill_md) else {
                continue;
            };
            // 若 frontmatter name 撞内部名，也跳过
            if Self::is_internal(&parsed.name) {
                continue;
            }
            let enabled = meta
                .as_ref()
                .and_then(|m| m.enabled.get(&folder).copied())
                .unwrap_or(true);
            let synced_from = meta
                .as_ref()
                .and_then(|m| m.synced_from.get(&folder).cloned())
                .unwrap_or_default();
            let source = if synced_from == "migrated" {
                "migrated".to_string()
            } else if synced_from.is_empty() {
                "local".to_string()
            } else {
                "synced".to_string()
            };
            let updated_at = file_mtime_str(&skill_md);
            names.insert(folder.clone());
            out.push(SkillInfo {
                name: parsed.name,
                description: parsed.description,
                enabled,
                scope: "external".into(),
                source,
                synced_from,
                path: path.to_string_lossy().replace('\\', "/"),
                updated_at,
            });
        }

        out.sort_by(|a, b| match (a.scope.as_str(), b.scope.as_str()) {
            ("internal", "external") => std::cmp::Ordering::Less,
            ("external", "internal") => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        });
        out
    }

    /// 已启用技能的模型侧目录（独立 system 消息，不要拼进时间尾巴）。
    /// 无启用项时返回 None，不发空目录。
    pub fn catalog_reminder(&self) -> Option<String> {
        let items: Vec<(String, String)> = self
            .list()
            .into_iter()
            .filter(|s| s.enabled)
            .map(|s| {
                let desc: String = collapse_ws(&s.description).chars().take(180).collect();
                (s.name, desc)
            })
            .collect();
        if items.is_empty() {
            return None;
        }
        let mut out = String::from("Skills 是可复用的任务说明。当前已启用：\n\n<available_skills>\n");
        for (name, desc) in items {
            out.push_str(&format!(
                "- `{}`: {}\n",
                xml_escape(&name),
                xml_escape(&desc)
            ));
        }
        out.push_str(
            "</available_skills>\n\n\
             用户点名某技能、或当前任务明显匹配某条描述时，先 load_skill，再按其步骤做；把技能当检查清单，不要当不可改的脚本。\n\
             本目录只有摘要；未加载前不要推断或执行技能正文。\n\
             若对话里已经出现该技能的 <skill_content>，直接遵循，不要再 load_skill。\n\
             目录没有明显匹配、路径不确定、或批量文件任务需要索引策略时，再调用 discover_capabilities。",
        );
        Some(out)
    }

    /// 兼容旧名：与 [`catalog_reminder`] 相同
    pub fn catalog_block(&self) -> Option<String> {
        self.catalog_reminder()
    }

    /// AI load_skill：禁用则拒绝
    pub fn load(&self, name: &str) -> Result<String> {
        let name = sanitize_name(name)?;
        let meta = self.meta.lock().map_err(|e| anyhow!(e.to_string()))?;
        if meta.enabled.get(&name) == Some(&false) {
            bail!("技能「{}」已禁用，请先启用再加载", name);
        }
        drop(meta);
        self.read_raw(&name)
    }

    /// UI 预览：允许读禁用技能
    pub fn read_raw(&self, name: &str) -> Result<String> {
        let name = sanitize_name(name)?;
        if let Some(content) = builtin_skills::get(&name) {
            return Ok(clamp_content(content, "builtin"));
        }
        let path = self.dir.join(&name).join("SKILL.md");
        if !path.is_file() {
            bail!("技能不存在: {}", name);
        }
        let content = fs::read_to_string(&path)?;
        Ok(clamp_content(
            &content,
            &path.to_string_lossy().replace('\\', "/"),
        ))
    }

    /// 创建或覆盖**外部**技能
    pub fn create(&self, name: &str, description: &str, body: &str) -> Result<SkillInfo> {
        let name = sanitize_name(name)?;
        if Self::is_internal(&name) {
            bail!(
                "「{}」是内部技能，不能创建或覆盖；内部技能只能通过修改代码重新打包更新",
                name
            );
        }
        let description = description.trim();
        if description.is_empty() {
            bail!("description 不能为空");
        }
        let body = body.trim();
        if body.is_empty() {
            bail!("body 不能为空");
        }
        let dir = self.dir.join(&name);
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("SKILL.md"),
            format_skill_md(&name, description, body),
        )?;
        {
            let mut meta = self.meta.lock().map_err(|e| anyhow!(e.to_string()))?;
            meta.enabled.entry(name.clone()).or_insert(true);
            // AI/用户写入后视为本地外部技能
            meta.synced_from.remove(&name);
            save_meta(&self.meta_path, &meta)?;
        }
        self.list()
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| anyhow!("创建后未能读取技能"))
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        let name = sanitize_name(name)?;
        if Self::is_internal(&name) {
            bail!("「{}」是内部技能，不能删除", name);
        }
        let dir = self.dir.join(&name);
        if !dir.is_dir() {
            bail!("技能不存在: {}", name);
        }
        fs::remove_dir_all(&dir)?;
        let mut meta = self.meta.lock().map_err(|e| anyhow!(e.to_string()))?;
        meta.enabled.remove(&name);
        meta.synced_from.remove(&name);
        save_meta(&self.meta_path, &meta)?;
        Ok(())
    }

    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<SkillInfo> {
        let name = sanitize_name(name)?;
        let exists = Self::is_internal(&name) || self.dir.join(&name).join("SKILL.md").is_file();
        if !exists {
            bail!("技能不存在: {}", name);
        }
        {
            let mut meta = self.meta.lock().map_err(|e| anyhow!(e.to_string()))?;
            meta.enabled.insert(name.clone(), enabled);
            save_meta(&self.meta_path, &meta)?;
        }
        self.list()
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| anyhow!("更新后未能读取技能"))
    }

    pub fn scan_external(&self) -> Vec<ExternalSkill> {
        let local_names: HashSet<String> = self.list().into_iter().map(|s| s.name).collect();
        let mut out = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for (source, root) in peer_skill_roots() {
            let Ok(entries) = fs::read_dir(&root) else {
                continue;
            };
            for e in entries.flatten() {
                let path = e.path();
                if !path.is_dir() {
                    continue;
                }
                let folder = match path.file_name().and_then(|s| s.to_str()) {
                    Some(n) if !n.starts_with('.') => n.to_string(),
                    _ => continue,
                };
                let skill_md = path.join("SKILL.md");
                if !skill_md.is_file() {
                    continue;
                }
                let Ok(parsed) = parse_skill_file(&skill_md) else {
                    continue;
                };
                // 不把与内部同名的候选列给用户导入（会冲突）
                if Self::is_internal(&parsed.name) || Self::is_internal(&folder) {
                    continue;
                }
                let key = (source.clone(), parsed.name.clone());
                if !seen.insert(key) {
                    continue;
                }
                out.push(ExternalSkill {
                    already_synced: local_names.contains(&parsed.name)
                        || local_names.contains(&folder),
                    name: parsed.name,
                    description: parsed.description,
                    source: source.clone(),
                    path: path.to_string_lossy().replace('\\', "/"),
                });
            }
        }
        out.sort_by(|a, b| a.source.cmp(&b.source).then(a.name.cmp(&b.name)));
        out
    }

    pub fn sync_from(
        &self,
        source: Option<&str>,
        names: Option<&[String]>,
        overwrite: bool,
    ) -> Result<Value> {
        let want: Option<HashSet<&str>> = names.map(|ns| ns.iter().map(|s| s.as_str()).collect());
        let source_filter = source.map(str::trim).filter(|s| !s.is_empty());

        let mut imported = 0usize;
        let mut skipped = 0usize;
        let mut updated = 0usize;
        let mut errors: Vec<String> = Vec::new();
        let mut details: Vec<Value> = Vec::new();

        for ext in self.scan_external() {
            if let Some(sf) = source_filter {
                if ext.source != sf {
                    continue;
                }
            }
            if let Some(ref want) = want {
                if !want.contains(ext.name.as_str()) {
                    continue;
                }
            }
            if Self::is_internal(&ext.name) {
                skipped += 1;
                details.push(json!({
                    "name": ext.name,
                    "action": "skipped",
                    "reason": "与内部技能同名，受保护",
                }));
                continue;
            }
            let dest = self.dir.join(&ext.name);
            if dest.exists() && !overwrite {
                skipped += 1;
                details.push(json!({
                    "name": ext.name,
                    "action": "skipped",
                    "reason": "本地已存在同名外部技能",
                }));
                continue;
            }
            let src = PathBuf::from(&ext.path);
            match copy_dir_recursive(&src, &dest) {
                Ok(()) => {
                    {
                        let mut meta = self.meta.lock().map_err(|e| anyhow!(e.to_string()))?;
                        meta.enabled.entry(ext.name.clone()).or_insert(true);
                        meta.synced_from
                            .insert(ext.name.clone(), ext.source.clone());
                        save_meta(&self.meta_path, &meta)?;
                    }
                    if overwrite && ext.already_synced {
                        updated += 1;
                        details.push(json!({
                            "name": ext.name,
                            "action": "updated",
                            "from": ext.source,
                        }));
                    } else {
                        imported += 1;
                        details.push(json!({
                            "name": ext.name,
                            "action": "imported",
                            "from": ext.source,
                        }));
                    }
                }
                Err(e) => {
                    errors.push(format!("{}: {}", ext.name, e));
                    details.push(json!({
                        "name": ext.name,
                        "action": "error",
                        "error": e.to_string(),
                    }));
                }
            }
        }

        Ok(json!({
            "imported": imported,
            "updated": updated,
            "skipped": skipped,
            "errors": errors,
            "details": details,
            "local_dir": self.dir.to_string_lossy().replace('\\', "/"),
        }))
    }

    /// 把旧版 SQLite workflows 一次性迁成外部 skills（幂等）
    pub fn migrate_workflows(&self, db: &crate::db::Db) -> Result<usize> {
        let done = db
            .get_setting("workflows_migrated_to_skills")
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(false);
        if done {
            return Ok(0);
        }
        let wfs = db.wf_list().unwrap_or_default();
        let mut count = 0usize;
        for w in &wfs {
            let base = if w.title.trim().is_empty() {
                format!("workflow-{}", w.id)
            } else {
                slugify(&w.title)
            };
            let mut name = if base.is_empty() {
                format!("workflow-{}", w.id)
            } else {
                base
            };
            // 撞内部名或已有文件时加 id 后缀
            if Self::is_internal(&name) || self.dir.join(&name).join("SKILL.md").is_file() {
                name = format!("{}-{}", name, w.id);
            }
            if Self::is_internal(&name) {
                continue;
            }
            let site_hint = if w.site.is_empty() {
                String::new()
            } else {
                format!("（站点 {}）", w.site)
            };
            let mut desc = format!(
                "由旧工作流经验迁移{}：{}",
                site_hint,
                if w.keywords.is_empty() {
                    w.title.clone()
                } else {
                    w.keywords.clone()
                }
            );
            if !w.keywords.is_empty() {
                desc.push_str(&format!("。触发关键词：{}", w.keywords));
            }
            let body = format!(
                "# {}\n\n{}\n\n{}",
                w.title,
                if w.site.is_empty() {
                    String::new()
                } else {
                    format!("相关站点：`{}`\n", w.site)
                },
                w.steps.trim()
            );
            match self.create(&name, &desc, &body) {
                Ok(_) => {
                    let mut meta = self.meta.lock().map_err(|e| anyhow!(e.to_string()))?;
                    meta.synced_from.insert(name.clone(), "migrated".into());
                    save_meta(&self.meta_path, &meta)?;
                    count += 1;
                }
                Err(e) => log::warn!("迁移工作流「{}」失败: {}", w.title, e),
            }
        }
        let _ = db.set_setting("workflows_migrated_to_skills", "true");
        if count > 0 {
            log::info!("已将 {} 条工作流经验迁移为外部 Skills", count);
        }
        Ok(count)
    }
}

fn clamp_content(content: &str, path_hint: &str) -> String {
    const MAX: usize = 30_000;
    if content.chars().count() > MAX {
        format!(
            "{}…\n\n（技能正文过长已截断，原 {} 字符；来源: {}）",
            content.chars().take(MAX).collect::<String>(),
            content.chars().count(),
            path_hint
        )
    } else {
        content.to_string()
    }
}

fn file_mtime_str(path: &Path) -> String {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| {
            chrono::DateTime::from_timestamp(d.as_secs() as i64, 0).map(|dt| {
                dt.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
        })
        .unwrap_or_default()
}

fn slugify(title: &str) -> String {
    let s: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else if (c as u32) > 127 && !c.is_control() {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_string();
    let s: String = s
        .chars()
        .take(40)
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    s
}

fn collapse_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 包一层模型可识别的技能正文标记；正文里若出现结束标签则转义，避免提前闭合。
pub fn wrap_skill_content(name: &str, body: &str) -> String {
    let safe = body.replace("</skill_content>", "&lt;/skill_content>");
    format!("<skill_content name=\"{}\">\n{safe}\n</skill_content>", xml_escape(name))
}

struct ParsedSkill {
    name: String,
    description: String,
}

fn parse_skill_file(path: &Path) -> Result<ParsedSkill> {
    let mut f = fs::File::open(path)?;
    let mut chunk = vec![0u8; 8192];
    let n = f.read(&mut chunk)?;
    let buf = String::from_utf8_lossy(&chunk[..n]).to_string();
    parse_frontmatter(&buf, path)
}

fn parse_frontmatter(content: &str, path: &Path) -> Result<ParsedSkill> {
    let folder_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed")
        .to_string();
    // builtin 传入的 path 可能是 `name/SKILL.md`，parent file_name 即 name
    let folder_name = if folder_name == "SKILL.md" || folder_name.is_empty() {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed")
            .to_string()
    } else {
        folder_name
    };

    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        let desc = trimmed
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .unwrap_or("（无描述）")
            .chars()
            .take(200)
            .collect();
        return Ok(ParsedSkill {
            name: folder_name,
            description: desc,
        });
    }
    let after = &trimmed[3..];
    let end = after
        .find("\n---")
        .ok_or_else(|| anyhow!("SKILL.md frontmatter 未闭合: {}", path.display()))?;
    let yaml = &after[..end];
    let name = extract_yaml_string(yaml, "name").unwrap_or(folder_name);
    let description =
        extract_yaml_string(yaml, "description").unwrap_or_else(|| "（无描述）".to_string());
    Ok(ParsedSkill { name, description })
}

fn extract_yaml_string(yaml: &str, key: &str) -> Option<String> {
    let lines: Vec<&str> = yaml.lines().collect();
    let prefix = format!("{}:", key);
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if !trimmed.starts_with(&prefix) {
            i += 1;
            continue;
        }
        let rest = trimmed[prefix.len()..].trim();
        if rest == "|" || rest == ">" || rest == ">-" || rest == "|-" || rest == "|+" {
            i += 1;
            let mut block = String::new();
            while i < lines.len() {
                let l = lines[i];
                if l.is_empty() || l.starts_with(' ') || l.starts_with('\t') {
                    if !block.is_empty() {
                        block.push('\n');
                    }
                    block.push_str(l.trim());
                    i += 1;
                } else {
                    break;
                }
            }
            let s = block.trim().to_string();
            return if s.is_empty() { None } else { Some(s) };
        }
        let s = rest
            .trim_matches(|c| c == '"' || c == '\'')
            .trim()
            .to_string();
        return if s.is_empty() { None } else { Some(s) };
    }
    None
}

fn format_skill_md(name: &str, description: &str, body: &str) -> String {
    let desc_block = if description.contains('\n') {
        let indented: String = description
            .lines()
            .map(|l| format!("  {}", l))
            .collect::<Vec<_>>()
            .join("\n");
        format!("|\n{}", indented)
    } else {
        description.to_string()
    };
    format!(
        "---\nname: {}\ndescription: {}\n---\n\n{}\n",
        name,
        desc_block,
        body.trim()
    )
}

fn sanitize_name(raw: &str) -> Result<String> {
    let s = raw.trim();
    if s.is_empty() {
        bail!("技能名不能为空");
    }
    if s.contains("..") || s.contains('/') || s.contains('\\') || s.starts_with('.') {
        bail!("技能名非法: {}", s);
    }
    if s.chars().any(|c| c.is_control() || "<>:\"|?*".contains(c)) {
        bail!("技能名含非法字符: {}", s);
    }
    Ok(s.to_string())
}

fn load_meta(path: &Path) -> Meta {
    let empty = Meta {
        enabled: HashMap::new(),
        synced_from: HashMap::new(),
    };
    let Ok(text) = fs::read_to_string(path) else {
        return empty;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return empty;
    };
    let mut enabled = HashMap::new();
    if let Some(obj) = v.get("enabled").and_then(|x| x.as_object()) {
        for (k, val) in obj {
            if let Some(b) = val.as_bool() {
                enabled.insert(k.clone(), b);
            }
        }
    }
    let mut synced_from = HashMap::new();
    if let Some(obj) = v.get("synced_from").and_then(|x| x.as_object()) {
        for (k, val) in obj {
            if let Some(s) = val.as_str() {
                synced_from.insert(k.clone(), s.to_string());
            }
        }
    }
    Meta {
        enabled,
        synced_from,
    }
}

fn save_meta(path: &Path, meta: &Meta) -> Result<()> {
    let v = json!({
        "enabled": meta.enabled,
        "synced_from": meta.synced_from,
    });
    let mut f = fs::File::create(path)?;
    f.write_all(serde_json::to_string_pretty(&v)?.as_bytes())?;
    Ok(())
}

fn peer_skill_roots() -> Vec<(String, PathBuf)> {
    let mut roots = Vec::new();
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let candidates = [
        ("claude", home.join(".claude").join("skills")),
        ("codex", home.join(".codex").join("skills")),
        ("cursor", home.join(".cursor").join("skills")),
        ("cursor-builtin", home.join(".cursor").join("skills-cursor")),
    ];
    for (label, p) in candidates {
        if p.is_dir() {
            roots.push((label.to_string(), p));
        }
    }
    roots
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
        fs::remove_dir_all(dst)?;
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            copy_dir_recursive(&entry.path(), &to)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_frontmatter() {
        let md = "---\nname: demo\ndescription: 一个演示技能\n---\n\n# Demo\n正文";
        let p = Path::new("/tmp/demo/SKILL.md");
        let parsed = parse_frontmatter(md, p).unwrap();
        assert_eq!(parsed.name, "demo");
        assert_eq!(parsed.description, "一个演示技能");
    }

    #[test]
    fn builtin_desktop_organize_parses() {
        let content = builtin_skills::get("desktop-organize").expect("embedded");
        let parsed = parse_frontmatter(content, Path::new("desktop-organize/SKILL.md")).unwrap();
        assert_eq!(parsed.name, "desktop-organize");
        assert!(!parsed.description.is_empty());
    }

    #[test]
    fn builtin_windows_cli_parses() {
        let content = builtin_skills::get("windows-cli").expect("embedded");
        let parsed = parse_frontmatter(content, Path::new("windows-cli/SKILL.md")).unwrap();
        assert_eq!(parsed.name, "windows-cli");
        assert!(parsed.description.contains("argv"));
        assert!(parsed.description.contains("PowerShell"));
        assert!(!parsed.description.contains("lark-cli"));
    }

    #[test]
    fn sanitize_rejects_path() {
        assert!(sanitize_name("../x").is_err());
        assert!(sanitize_name("ok-skill").is_ok());
    }

    #[test]
    fn create_rejects_internal_name() {
        let dir = std::env::temp_dir().join(format!("dh-skills-test-{}", uuid::Uuid::new_v4()));
        let _ = fs::create_dir_all(&dir);
        let store = SkillStore::new(&dir);
        let err = store.create("desktop-organize", "x", "# body").unwrap_err();
        assert!(err.to_string().contains("内部技能"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_block_lists_enabled_external_skills() {
        let dir = std::env::temp_dir().join(format!("dh-skills-cat-{}", uuid::Uuid::new_v4()));
        let _ = fs::create_dir_all(&dir);
        let store = SkillStore::new(&dir);
        store
            .create(
                "daily-report-feishu-base",
                "用户的日报记在飞书多维表格里，撰写或参考已提交的日报时使用",
                "# 日报",
            )
            .unwrap();
        let block = store.catalog_reminder().expect("catalog");
        assert!(block.contains("<available_skills>"));
        assert!(block.contains("`daily-report-feishu-base`"));
        assert!(block.contains("日报"));
        assert!(block.contains("load_skill"));
        assert!(block.contains("<skill_content>"));
        assert!(block.contains("未加载前不要推断"));
        assert!(!block.starts_with('\n'));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrap_skill_content_escapes_closing_tag() {
        let wrapped = wrap_skill_content("demo", "a</skill_content>b");
        assert!(wrapped.starts_with("<skill_content name=\"demo\">"));
        assert!(wrapped.contains("a&lt;/skill_content>b"));
        assert!(wrapped.ends_with("</skill_content>"));
    }

    #[test]
    fn catalog_reminder_escapes_description_markup() {
        let dir = std::env::temp_dir().join(format!("dh-skills-esc-{}", uuid::Uuid::new_v4()));
        let _ = fs::create_dir_all(&dir);
        let store = SkillStore::new(&dir);
        store
            .create("amp-skill", "a <b> & c", "# body")
            .unwrap();
        let block = store.catalog_reminder().unwrap();
        assert!(block.contains("a &lt;b&gt; &amp; c"));
        assert!(!block.contains("a <b> & c"));
        let _ = fs::remove_dir_all(&dir);
    }
}
