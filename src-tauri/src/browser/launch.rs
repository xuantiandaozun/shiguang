//! 拉起系统默认浏览器（用户自己的浏览器，带 Profile / 登录态）。
//! 背景：Chrome 136+ 起 `--remote-debugging-port` 对默认用户数据目录已失效，
//! 想操作用户的真实浏览器只能走拾光扩展通道——浏览器一启动扩展就连回本地桥。
//! 解析顺序：注册表 UserChoice → exe 直启（裸启动，不开新页面）；
//! 解析失败退化为 `cmd /c start <url>` 走系统 URL 关联（仅 navigate 有 url 时可用）。

use anyhow::{anyhow, Result};
use std::path::PathBuf;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// GUI 程序里拉起 reg/cmd 这类控制台子进程时禁止弹黑窗
fn hide_window(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
}

/// 启动系统默认浏览器。优先裸启动 exe（打开其启动页/恢复会话，不新开页面，
/// 之后由扩展通道统一 navigate，避免重复开标签）；fallback_url 仅在注册表
/// 解析失败时兜底使用。
pub fn launch_default(fallback_url: Option<&str>) -> Result<()> {
    if let Some(exe) = default_browser_exe() {
        std::process::Command::new(&exe)
            .spawn()
            .map_err(|e| anyhow!("启动默认浏览器失败({}): {}", exe.display(), e))?;
        log::info!("已拉起系统默认浏览器: {}", exe.display());
        return Ok(());
    }
    if let Some(url) = fallback_url {
        let mut c = std::process::Command::new("cmd");
        // start 第一个引号参数是窗口标题，必须占一个空串
        c.args(["/c", "start", "", url]);
        hide_window(&mut c);
        c.spawn()
            .map_err(|e| anyhow!("通过系统关联打开网址失败: {}", e))?;
        log::info!("已通过系统默认程序打开: {}", url);
        return Ok(());
    }
    Err(anyhow!("无法解析系统默认浏览器"))
}

/// 注册表解析默认浏览器 exe：
/// HKCU\...\UrlAssociations\https\UserChoice → ProgId → HKCR\<ProgId>\shell\open\command
fn default_browser_exe() -> Option<PathBuf> {
    let prog_id = reg_value(
        r"HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice",
        Some("ProgId"),
    )?;
    let cmdline = reg_value(&format!(r"HKCR\{}\shell\open\command", prog_id), None)?;
    let exe = extract_exe(&cmdline)?;
    if exe.exists() {
        Some(exe)
    } else {
        None
    }
}

/// `reg query` 读注册表；value_name=None 时读默认值（/ve）。返回 REG_SZ 之后的内容。
/// 输出格式随语言环境不同，但类型字段恒为 "REG_SZ"，以此切分最稳。
fn reg_value(key: &str, value_name: Option<&str>) -> Option<String> {
    let mut c = std::process::Command::new("reg");
    c.arg("query").arg(key);
    match value_name {
        Some(v) => {
            c.args(["/v", v]);
        }
        None => {
            c.arg("/ve");
        }
    }
    hide_window(&mut c);
    let out = c.output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once("REG_SZ").map(|(_, v)| v.trim().to_string()))
        .filter(|s| !s.is_empty())
        .last()
}

/// 从打开命令中提取 exe 路径：
/// `"C:\...\chrome.exe" --single-argument %1` 或 `C:\...\firefox.exe -osint -url "%1"`
fn extract_exe(cmdline: &str) -> Option<PathBuf> {
    let s = cmdline.trim();
    if let Some(rest) = s.strip_prefix('"') {
        let end = rest.find('"')?;
        return Some(PathBuf::from(&rest[..end]));
    }
    let idx = s.to_lowercase().find(".exe")?;
    Some(PathBuf::from(&s[..idx + 4]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_exe_from_command_lines() {
        assert_eq!(
            extract_exe(r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --single-argument %1"#)
                .unwrap()
                .to_str()
                .unwrap(),
            r"C:\Program Files\Google\Chrome\Application\chrome.exe"
        );
        assert_eq!(
            extract_exe(r"C:\Program Files\Mozilla Firefox\firefox.exe -osint -url %1")
                .unwrap()
                .to_str()
                .unwrap(),
            r"C:\Program Files\Mozilla Firefox\firefox.exe"
        );
        assert!(extract_exe("").is_none());
        assert!(extract_exe("--no-exe-here").is_none());
    }
}
