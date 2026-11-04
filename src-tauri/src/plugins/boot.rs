//! 安全模式：启动时写标记，前端主界面渲染成功后清除。连续两次启动都没清除
//! （插件导致白屏/崩溃），第三次启动只加载内置插件。也可用 `--safe-mode` 参数或
//! 环境变量 `SHIGUANG_SAFE_MODE=1` 强制进入。
use std::path::{Path, PathBuf};

const FAILED_BOOTS_FOR_SAFE_MODE: u32 = 2;

fn marker(app_dir: &Path) -> PathBuf {
    app_dir.join("plugins").join(".boot")
}

/// 记录一次启动，返回本次是否应进入安全模式。
pub fn begin(app_dir: &Path) -> bool {
    let forced = std::env::args().any(|a| a == "--safe-mode")
        || std::env::var("SHIGUANG_SAFE_MODE").is_ok_and(|v| v == "1");
    let path = marker(app_dir);
    let failed: u32 = std::fs::read_to_string(&path).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, (failed + 1).to_string());
    let safe = forced || failed >= FAILED_BOOTS_FOR_SAFE_MODE;
    if safe {
        log::warn!("插件安全模式：只加载内置插件（连续 {failed} 次启动未完成，或手动指定）");
    }
    safe
}

pub fn finish(app_dir: &Path) {
    let _ = std::fs::remove_file(marker(app_dir));
}

#[cfg(test)]
mod tests {
    #[test]
    fn two_unfinished_boots_trigger_safe_mode() {
        let dir = std::env::temp_dir().join(format!("sg-boot-{}", uuid::Uuid::new_v4()));
        assert!(!super::begin(&dir));
        assert!(!super::begin(&dir));
        assert!(super::begin(&dir));
        super::finish(&dir);
        assert!(!super::begin(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }
}
