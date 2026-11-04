//! 内部 Skills：由内置插件的 `skills/<name>/SKILL.md` 提供，运行时只读，改文件即生效。
//! 新增内部技能：在对应内置插件的 skills 目录加一个子目录（plugin.json 声明 "skills": "skills"）。

/// (技能名, 完整 SKILL.md 文本)，按插件顺序。
pub fn all() -> Vec<(String, String)> {
    crate::plugins::snapshot().internal_skills()
}

pub fn get(name: &str) -> Option<String> {
    all().into_iter().find(|(n, _)| n == name).map(|(_, c)| c)
}

pub fn names() -> Vec<String> {
    all().into_iter().map(|(n, _)| n).collect()
}
