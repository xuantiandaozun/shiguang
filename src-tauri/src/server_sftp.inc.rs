// 独立 SFTP 文件管理窗口（XFTP 风格左右双栏：本机 + 远程服务器）的后端 IPC。
// 通过 include!("server_sftp.inc.rs") 编入 servers 模块，共享 ServerHub / connect / dispatch。

#[tauri::command]
pub async fn server_sftp(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    action: String,
    server_id: String,
    payload: Option<Value>,
) -> std::result::Result<Value, String> {
    sftp_action(app, window, action, server_id, payload.unwrap_or(json!({})))
        .await
        .map_err(|e| format!("{e:#}"))
}

fn sftp_remote_remove_recursive(sftp: &Sftp, path: &Path, depth: usize) -> Result<()> {
    if depth > 64 {
        bail!("远程目录层级过深，无法继续删除");
    }
    let st = sftp.lstat(path)?;
    if st.is_dir() {
        let entries = sftp.readdir(path)?;
        for (child, _) in entries {
            let name = child.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.is_empty() || name == "." || name == ".." {
                continue;
            }
            sftp_remote_remove_recursive(sftp, &child, depth + 1)?;
        }
        sftp.rmdir(path)?;
    } else {
        sftp.unlink(path)?;
    }
    Ok(())
}

fn validate_single_name(name: &str) -> Result<&str> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.chars().any(char::is_control)
    {
        bail!("名称不合法：{name}");
    }
    Ok(trimmed)
}

async fn sftp_action(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    action: String,
    id: String,
    payload: Value,
) -> Result<Value> {
    let sftp_label = format!("sftp-{id}");
    let term_label = format!("terminal-{id}");
    let win_label = window.label();
    let is_servers_manager = win_label == "main"
        || win_label == "applet-servers-servers"
        || win_label.starts_with("applet-servers-servers-");

    if action == "open" {
        if !is_servers_manager && win_label != term_label && win_label != sftp_label {
            bail!("无权打开此服务器的文件管理窗口");
        }
        let state = app.state::<crate::AppState>();
        let server = state.servers.get(&id)?;
        state.servers.touch_usage(&id);
        let _ = app.emit("servers-changed", ());
        if let Some(w) = app.get_webview_window(&sftp_label) {
            w.show()?;
            w.unminimize()?;
            w.set_focus()?;
        } else {
            let w = tauri::WebviewWindowBuilder::new(
                &app,
                &sftp_label,
                tauri::WebviewUrl::App(format!("index.html#/sftp?server={id}").into()),
            )
            .title(format!("{} — 文件管理 (SFTP)", server.name))
            .inner_size(1280., 760.)
            .min_inner_size(900., 560.)
            .resizable(true)
            .maximizable(true)
            .center()
            .build()?;
            let _ = w.set_focus();
        }
        return Ok(json!({ "label": sftp_label }));
    }

    if win_label != sftp_label {
        bail!("无权访问此 SFTP 会话");
    }

    let state = app.state::<crate::AppState>();
    let hub = &state.servers;

    match action.as_str() {
        "server_info" => {
            let server = hub.get(&id)?;
            Ok(json!(server))
        }
        "local_roots" => {
            let mut quick = Vec::new();
            if let Some(p) = dirs::desktop_dir() {
                quick.push(json!({"label": "桌面", "path": p.to_string_lossy()}));
            }
            if let Some(p) = dirs::download_dir() {
                quick.push(json!({"label": "下载", "path": p.to_string_lossy()}));
            }
            if let Some(p) = dirs::document_dir() {
                quick.push(json!({"label": "文档", "path": p.to_string_lossy()}));
            }
            if let Some(p) = dirs::home_dir() {
                quick.push(json!({"label": "主目录", "path": p.to_string_lossy()}));
            }
            let mut drives = Vec::new();
            #[cfg(windows)]
            {
                for letter in b'A'..=b'Z' {
                    let root = format!("{}:\\", letter as char);
                    if Path::new(&root).exists() {
                        drives.push(root);
                    }
                }
            }
            #[cfg(not(windows))]
            {
                drives.push("/".to_string());
            }
            let default_path = dirs::desktop_dir()
                .or_else(dirs::home_dir)
                .map(|p| p.to_string_lossy().into_owned())
                .or_else(|| drives.first().cloned())
                .unwrap_or_else(|| ".".to_string());
            Ok(json!({
                "quick": quick,
                "drives": drives,
                "default_path": default_path,
            }))
        }
        "local_list" => {
            let requested = payload
                .get("path")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or_else(dirs::desktop_dir)
                .or_else(dirs::home_dir)
                .unwrap_or_else(|| PathBuf::from("."));

            tokio::task::spawn_blocking(move || -> Result<Value> {
                let canonical = std::fs::canonicalize(&requested).unwrap_or_else(|_| requested.clone());
                let clean_path = canonical
                    .to_string_lossy()
                    .trim_start_matches(r"\\?\")
                    .to_string();
                let dir_path = PathBuf::from(&clean_path);
                let mut entries = Vec::new();
                let mut truncated = false;
                for item in std::fs::read_dir(&dir_path)
                    .with_context(|| format!("读取本机目录失败：{}", dir_path.display()))?
                {
                    let Ok(entry) = item else { continue };
                    if entries.len() >= 2000 {
                        truncated = true;
                        break;
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let path = entry.path();
                    let meta = entry.metadata().ok();
                    let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                    let is_file = meta.as_ref().map(|m| m.is_file()).unwrap_or(false);
                    let size = if is_file {
                        meta.as_ref().map(|m| m.len())
                    } else {
                        None
                    };
                    let mtime = meta
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs());
                    entries.push(json!({
                        "name": name,
                        "path": path.to_string_lossy(),
                        "is_dir": is_dir,
                        "is_file": is_file,
                        "size": size,
                        "mtime": mtime,
                    }));
                }
                entries.sort_by(|a, b| {
                    let ad = a["is_dir"].as_bool().unwrap_or(false);
                    let bd = b["is_dir"].as_bool().unwrap_or(false);
                    bd.cmp(&ad).then_with(|| {
                        let an = a["name"].as_str().unwrap_or("").to_lowercase();
                        let bn = b["name"].as_str().unwrap_or("").to_lowercase();
                        an.cmp(&bn)
                    })
                });
                let parent = dir_path
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .filter(|p| !p.is_empty() && p != &clean_path);
                Ok(json!({
                    "path": clean_path,
                    "parent": parent,
                    "entries": entries,
                    "truncated": truncated,
                }))
            })
            .await?
        }
        "local_exists" => {
            let target = payload
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let p = Path::new(target);
            let exists = p.exists();
            let is_dir = p.is_dir();
            Ok(json!({ "exists": exists, "is_dir": is_dir }))
        }
        "local_mkdir" => {
            let parent = payload
                .get("parent")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("缺少 parent"))?;
            let name = validate_single_name(
                payload
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )?;
            let target = Path::new(parent).join(name);
            std::fs::create_dir(&target)
                .with_context(|| format!("创建文件夹失败：{}", target.display()))?;
            Ok(json!({ "path": target.to_string_lossy() }))
        }
        "local_rename" => {
            let path = payload
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let new_name = validate_single_name(
                payload
                    .get("new_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )?;
            let src = Path::new(path);
            let parent = src.parent().ok_or_else(|| anyhow!("无法重命名根目录"))?;
            let dst = parent.join(new_name);
            if dst.exists() {
                bail!("目标名称已存在：{new_name}");
            }
            std::fs::rename(src, &dst)
                .with_context(|| format!("重命名失败：{}", src.display()))?;
            Ok(json!({ "path": dst.to_string_lossy() }))
        }
        "local_delete" => {
            let path = payload
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("缺少 path"))?;
            let target = PathBuf::from(path);
            if !target.exists() {
                bail!("本机文件不存在：{path}");
            }
            tokio::task::spawn_blocking(move || -> Result<()> {
                trash::delete(&target)
                    .map_err(|e| anyhow!("移入回收站失败：{e}"))?;
                Ok(())
            })
            .await??;
            Ok(json!({ "deleted": true }))
        }
        "remote_list" => {
            let remote_path = payload
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or(".")
                .to_string();
            dispatch(
                &app,
                "sftp_list",
                &json!({ "server_id": id, "remote_path": remote_path }),
            )
            .await
        }
        "remote_mkdir" => {
            let remote_path = payload
                .get("path")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| s.starts_with('/'))
                .ok_or_else(|| anyhow!("远程目录路径必须以 / 开头"))?
                .to_string();
            let server = hub.get(&id)?;
            let handle = app.clone();
            tokio::task::spawn_blocking(move || -> Result<Value> {
                let state = handle.state::<crate::AppState>();
                let session = connect(&state.servers, &server)?;
                let sftp = session.sftp()?;
                sftp.mkdir(Path::new(&remote_path), 0o755)?;
                Ok(json!({ "path": remote_path }))
            })
            .await?
        }
        "remote_rename" => {
            let old_path = payload
                .get("old_path")
                .and_then(Value::as_str)
                .filter(|s| s.starts_with('/'))
                .ok_or_else(|| anyhow!("缺少有效的 old_path"))?
                .to_string();
            let new_path = payload
                .get("new_path")
                .and_then(Value::as_str)
                .filter(|s| s.starts_with('/'))
                .ok_or_else(|| anyhow!("缺少有效的 new_path"))?
                .to_string();
            let server = hub.get(&id)?;
            let handle = app.clone();
            tokio::task::spawn_blocking(move || -> Result<Value> {
                let state = handle.state::<crate::AppState>();
                let session = connect(&state.servers, &server)?;
                let sftp = session.sftp()?;
                sftp.rename(Path::new(&old_path), Path::new(&new_path), None)?;
                Ok(json!({ "path": new_path }))
            })
            .await?
        }
        "remote_delete" => {
            let remote_path = payload
                .get("path")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| s.starts_with('/') && *s != "/")
                .ok_or_else(|| anyhow!("不允许删除空路径或根目录"))?
                .to_string();
            let server = hub.get(&id)?;
            let handle = app.clone();
            tokio::task::spawn_blocking(move || -> Result<Value> {
                let state = handle.state::<crate::AppState>();
                let session = connect(&state.servers, &server)?;
                let sftp = session.sftp()?;
                sftp_remote_remove_recursive(&sftp, Path::new(&remote_path), 0)?;
                Ok(json!({ "deleted": true }))
            })
            .await?
        }
        "preview" => {
            let path = payload
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let server = hub.get(&id)?;
            let handle = app.clone();
            tokio::task::spawn_blocking(move || -> Result<Value> {
                let state = handle.state::<crate::AppState>();
                let session = connect(&state.servers, &server)?;
                let sftp = session.sftp()?;
                if !sftp.lstat(Path::new(&path))?.is_file() {
                    bail!("只支持普通文本文件预览");
                }
                let mut bytes = Vec::new();
                sftp.open(Path::new(&path))?
                    .take(131073)
                    .read_to_end(&mut bytes)?;
                terminal_preview_text(bytes)
            })
            .await?
        }
        "prepare_upload" => terminal_prepare_upload(&app, &id, &payload).await,
        "upload" => {
            let mut prepared = terminal_prepare_upload(&app, &id, &payload).await?;
            let overwrite = payload
                .get("overwrite")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if prepared["exists"] == true && !overwrite {
                bail!("远程目标已存在，请确认覆盖");
            }
            prepared["overwrite"] = json!(overwrite);
            prepared["server_id"] = json!(id);
            dispatch(&app, "sftp_upload", &prepared).await
        }
        "download" => {
            let remote_path = payload
                .get("remote_path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("缺少 remote_path"))?;
            let local_path = payload
                .get("local_path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("缺少 local_path"))?;
            let overwrite = payload
                .get("overwrite")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            dispatch(
                &app,
                "sftp_download",
                &json!({
                    "server_id": id,
                    "remote_path": remote_path,
                    "local_path": local_path,
                    "overwrite": overwrite,
                }),
            )
            .await
        }
        "transfer" | "cancel_transfer" => {
            let task_id = payload
                .get("task_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            {
                let tasks = hub.tasks.lock().unwrap();
                let task = tasks.get(task_id).ok_or_else(|| anyhow!("传输任务不存在"))?;
                if task.info.server_id != id
                    || (task.info.kind != "sftp_upload" && task.info.kind != "sftp_download")
                {
                    bail!("无权访问该传输任务");
                }
            }
            dispatch(
                &app,
                if action == "transfer" {
                    "get_remote_task"
                } else {
                    "cancel_remote_task"
                },
                &json!({ "task_id": task_id }),
            )
            .await
        }
        _ => bail!("未知 SFTP 操作：{action}"),
    }
}
