//! 编译期嵌入的内部 Skills。
//! 只能通过改仓库里的 markdown + 重新打包更新；运行时只读。
//! 新增内部技能：在 `builtin-skills/<name>/SKILL.md` 加文件，并在 `ALL` 里登记一行。

/// (文件夹名/技能名, 完整 SKILL.md 文本)
pub static ALL: &[(&str, &str)] = &[(
    "desktop-organize",
    include_str!("../builtin-skills/desktop-organize/SKILL.md"),
)];

pub fn get(name: &str) -> Option<&'static str> {
    ALL.iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
}

pub fn names() -> impl Iterator<Item = &'static str> {
    ALL.iter().map(|(n, _)| *n)
}
