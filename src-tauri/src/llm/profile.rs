//! 个人信息：固定字段（设置页维护）+ 自由条目（AI 聊天中通过工具维护）。
//! 仅在任务场景需要时才注入对话（求职/招聘/发帖等，或用户主动要求），平时不加载，
//! 避免无关对话占用上下文、泄露隐私。

use crate::commands::Settings;
use crate::db::ProfileEntry;

/// 需要使用个人信息的场景关键词（命中任意一个即注入）
const SCENARIO_KEYWORDS: &[&str] = &[
    // 求职招聘
    "找工作", "求职", "岗位", "简历", "面试", "招聘", "应聘", "投简历", "投递",
    "boss直聘", "boss", "拉勾", "猎聘", "智联", "前程无忧", "实习",
    "招呼语", "打招呼", "和hr", "跟hr",
    // 发帖/自媒体
    "发帖", "帖子", "推文", "发推", "自媒体", "公众号", "小红书", "知乎", "微博",
    "署名", "个人简介", "自我介绍",
    // 网站账号资料维护
    "个人主页", "主页资料", "修改资料", "编辑资料", "完善资料", "资料设置",
    "账号设置", "个人设置", "个人中心", "改简介", "更新资料", "账号资料",
    // 填表/注册
    "报名表", "填写资料", "填资料", "注册账号", "申请表",
];

/// 用户主动要求使用个人信息的表达
const EXPLICIT_KEYWORDS: &[&str] = &[
    "我的信息", "个人信息", "我的资料", "个人资料", "我的经历", "我的情况",
    "用我的", "根据我的", "结合我的", "按我的情况",
];

pub fn should_inject(text: &str) -> bool {
    let t = text.to_lowercase();
    SCENARIO_KEYWORDS.iter().any(|k| t.contains(&k.to_lowercase()))
        || EXPLICIT_KEYWORDS.iter().any(|k| t.contains(&k.to_lowercase()))
}

/// 构造注入对话末尾的个人信息段；固定字段与自由条目均为空时返回 None
pub fn injection_block(settings: &Settings, entries: &[ProfileEntry]) -> Option<String> {
    let mut fixed: Vec<String> = Vec::new();
    let pairs = [
        ("真实姓名", &settings.profile_name),
        ("自媒体号名称", &settings.profile_alias),
        ("性别", &settings.profile_gender),
        ("出生年月", &settings.profile_birth),
        ("手机", &settings.profile_phone),
        ("邮箱", &settings.profile_email),
        ("所在城市", &settings.profile_city),
    ];
    for (k, v) in pairs {
        let v = v.trim();
        if !v.is_empty() {
            fixed.push(format!("{}：{}", k, v));
        }
    }
    if fixed.is_empty() && entries.is_empty() {
        return None;
    }
    let mut s = String::from(
        "\n\n## 用户个人信息（仅在涉及求职/发帖/填表/账号资料维护等需要本人信息的场景使用；与当前任务无关的条目不要强行提及；不要向第三方页面泄露任务不需要的字段）\n\
        名称使用规则：对外展示一律优先用「自媒体号名称」（发帖、署名、简介、网站资料维护等绝大多数场景）；「真实姓名」仅当任务明确需要真实身份时使用（如招聘网站简历、求职沟通、正式报名表），其余场景不得出现真实姓名。",
    );
    if !fixed.is_empty() {
        s.push_str("\n### 基本资料\n");
        s.push_str(&fixed.join("\n"));
    }
    if !entries.is_empty() {
        s.push_str("\n### 补充资料\n");
        for e in entries {
            s.push_str(&format!("\n【{}】{}", e.label, e.content));
        }
    }
    Some(s)
}
