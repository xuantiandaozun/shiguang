use notify::{EventKind, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// 后台监听桌面目录，新文件命中已审核规则时自动归类。
/// 独立线程运行，与窗口生命周期无关，程序常驻托盘期间一直生效。
pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        if let Err(e) = run(app.clone()) {
            log::warn!("桌面监听器退出: {}", e);
        }
    });
}

fn run(app: AppHandle) -> notify::Result<()> {
    let desktop = match crate::organizer::scanner::desktop_dir() {
        Ok(d) => d,
        Err(_) => return Ok(()),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(&desktop, RecursiveMode::NonRecursive)?;

    let mut pending: Vec<PathBuf> = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(900)) {
            Ok(Ok(event)) => {
                let relevant = matches!(
                    event.kind,
                    EventKind::Create(_)
                        | EventKind::Modify(notify::event::ModifyKind::Name(_))
                        | EventKind::Any
                );
                if relevant {
                    for p in event.paths {
                        if !pending.contains(&p) {
                            pending.push(p);
                        }
                    }
                }
            }
            Ok(Err(err)) => {
                log::debug!("watch 事件错误: {}", err);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if pending.is_empty() {
                    continue;
                }
                let batch: Vec<PathBuf> = std::mem::take(&mut pending);
                process(&app, batch);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

fn process(app: &AppHandle, paths: Vec<PathBuf>) {
    {
        let state = app.state::<crate::AppState>();
        if state.auto_paused.load(Ordering::Relaxed) {
            return;
        }
    }
    for path in paths {
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().map(|s| s.to_string_lossy().to_string()) else {
            continue;
        };
        if name.starts_with('.') || name == "desktop.ini" {
            continue;
        }
        let state = app.state::<crate::AppState>();
        match crate::organizer::rules::apply_to_file(&state.db, &path) {
            Ok(Some((file, dst))) => {
                crate::notify_user(app, "已自动整理", &format!("{}\n→ {}", file, dst));
                let _ = app.emit(
                    "rule-applied",
                    serde_json::json!({ "file": file, "target": dst }),
                );
                let _ = app.emit("history-changed", ());
            }
            Ok(None) => {}
            Err(e) => {
                log::warn!("自动整理失败 {}: {}", path.display(), e);
            }
        }
    }
}
