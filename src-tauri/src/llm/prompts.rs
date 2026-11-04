use crate::commands::Settings;

/// 系统提示词必须保持稳定（逐字节不变）：OpenAI 兼容接口的上下文缓存按前缀匹配，
/// 这里任何动态内容都会导致其后所有消息的缓存失效。
/// Skills 目录作为独立 system 消息插在本提示之后、历史之前（启用集不变则稳定）。
/// 当前时间、按需资料插在历史之后、本轮工具调用之前，同一轮只往后追加。
///
/// 正文在 core 插件的 `prompts/system.md`（改完即生效，不用重新编译）；
/// 其它启用插件的 `prompts.system_append` 按插件顺序追加在末尾。
///
/// 写法原则：只写「产品约束 / 工具 description 学不到的行为规则」。
/// 参数用法、整理细则、经验步骤等放 tools / Skills，不要在这里复述说明书。
pub fn system_prompt(settings: &Settings) -> String {
    let snapshot = crate::plugins::snapshot();
    let template = snapshot.prompt("core", "system").unwrap_or_else(|e| {
        log::error!("读取系统提示词失败: {e:#}");
        "你是拾光，运行在用户 Windows 桌面上的智能助手。默认使用简体中文。".to_string()
    });
    let rule_key = match settings.permission_level.as_str() {
        "confirmation" => "permission_confirmation",
        "autopilot" => "permission_autopilot",
        _ => "permission_balanced",
    };
    let permission_rule = snapshot.prompt("core", rule_key).unwrap_or_default();
    let subagent_roles = settings
        .subagent_roles
        .iter()
        .filter(|r| r.enabled)
        .map(|r| r.role.as_str())
        .collect::<Vec<_>>()
        .join("、");
    let permission = match settings.permission_level.as_str() {
        "confirmation" => "谨慎",
        "autopilot" => "高效",
        _ => "平衡",
    };
    let mut prompt = render(
        &template,
        &[
            ("root", settings.organize_root.as_str()),
            ("subagent_roles", subagent_roles.as_str()),
            ("temp", settings.temp_path.as_str()),
            ("desktop", settings.desktop_path.as_str()),
            ("permission", permission),
            ("permission_rule", permission_rule.trim_end_matches(['\r', '\n'])),
        ],
    );
    for plugin in snapshot.enabled_plugins().filter(|p| p.manifest.id != "core") {
        if plugin.manifest.prompts.contains_key("system_append") {
            if let Ok(extra) = snapshot.prompt(&plugin.manifest.id, "system_append") {
                let extra = extra.trim();
                if !extra.is_empty() {
                    prompt.push_str("\n\n");
                    prompt.push_str(extra);
                }
            }
        }
    }
    prompt
}

/// 只替换已知占位符 `{name}`，其它花括号原样保留。
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (key, value) in vars {
        out = out.replace(&format!("{{{key}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn render_replaces_known_placeholders_only() {
        assert_eq!(super::render("a {x} {y} {{z}}", &[("x", "1")]), "a 1 {y} {{z}}");
    }
}

