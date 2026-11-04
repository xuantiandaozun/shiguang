//! 监听插件目录，文件变化 300ms 内无新变化后整体重载，并重启变化插件的 MCP 服务。
use notify::{RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

const DEBOUNCE: Duration = Duration::from_millis(300);

/// 变化路径所属的插件 id（根目录下第一层目录名）。
fn plugin_of(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let first = rel.components().next()?.as_os_str().to_str()?.to_string();
    (!first.starts_with('.')).then_some(first)
}

/// 启动监听线程。返回的 watcher 需要一直持有。
pub fn spawn(roots: Vec<PathBuf>) -> Option<notify::RecommendedWatcher> {
    let (tx, rx) = mpsc::channel::<PathBuf>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            for path in event.paths {
                let _ = tx.send(path);
            }
        }
    })
    .map_err(|e| log::warn!("插件目录监听启动失败: {e}"))
    .ok()?;
    for root in &roots {
        if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
            log::warn!("监听插件目录 {} 失败: {e}", root.display());
        }
    }
    std::thread::Builder::new()
        .name("plugin-watch".into())
        .spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut changed = vec![first];
                while let Ok(path) = rx.recv_timeout(DEBOUNCE) {
                    changed.push(path);
                }
                let mut plugins = HashSet::new();
                for path in &changed {
                    for root in &roots {
                        if let Some(id) = plugin_of(root, path) {
                            plugins.insert(id);
                        }
                    }
                }
                if plugins.is_empty() {
                    continue;
                }
                for id in &plugins {
                    super::mcp::restart_plugin(id);
                }
                log::info!("插件文件变化，重新加载: {:?}", plugins);
                super::host().reload();
            }
        })
        .ok()?;
    Some(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_paths_to_plugin_ids() {
        let root = Path::new("D:/p");
        assert_eq!(plugin_of(root, Path::new("D:/p/web/tools.json")).as_deref(), Some("web"));
        assert_eq!(plugin_of(root, Path::new("D:/p/.history/x")), None);
        assert_eq!(plugin_of(root, Path::new("D:/other/x")), None);
    }
}
