// Desktop-only interactive terminal. Deliberately not exposed by server tools/MCP.
#[tauri::command]
pub async fn server_terminal(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    action: String,
    server_id: String,
    data: Option<String>,
    cols: Option<u32>,
    rows: Option<u32>,
) -> std::result::Result<Value, String> {
    terminal_action(app, window, action, server_id, data, cols, rows)
        .await
        .map_err(|e| format!("{e:#}"))
}
async fn terminal_action(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    action: String,
    id: String,
    data: Option<String>,
    cols: Option<u32>,
    rows: Option<u32>,
) -> Result<Value> {
    let label = format!("terminal-{id}");
    if window.label() != "main" && window.label() != label {
        bail!("无权访问此终端");
    }
    let state = app.state::<crate::AppState>();
    let hub = &state.servers;
    if action == "list" {
        return Ok(json!(hub
            .terminals
            .lock()
            .unwrap()
            .iter()
            .map(|(id, t)| (id.clone(), t.status.clone()))
            .collect::<HashMap<_, _>>()));
    }
    if action == "open" {
        let server = hub.get(&id)?;
        if let Some(w) = app.get_webview_window(&label) {
            w.show()?;
            w.set_focus()?;
        } else {
            let w = tauri::WebviewWindowBuilder::new(
                &app,
                &label,
                tauri::WebviewUrl::App(format!("index.html#/terminal?server={id}").into()),
            )
            .title(format!("{} — SSH", server.name))
            .inner_size(1000., 680.)
            .min_inner_size(640., 400.)
            .build()?;
            let handle = app.clone();
            let server_id = id.clone();
            w.on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::Destroyed) {
                    let state = handle.state::<crate::AppState>();
                    if let Some(t) = state.servers.terminals.lock().unwrap().get(&server_id) {
                        let _ = t.sender.send(TerminalInput::Close);
                    };
                }
            });
        }
        return Ok(json!({}));
    }
    if action == "connect" {
        let server = hub.get(&id)?;
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let _guard = hub.config.lock().unwrap();
            let mut terminals = hub.terminals.lock().unwrap();
            if terminals
                .get(&id)
                .is_some_and(|t| t.status == "connecting" || t.status == "connected")
            {
                return Ok(json!({}));
            }
            terminals.insert(
                id.clone(),
                TerminalEntry {
                    sender: tx,
                    output: Vec::new(),
                    status: "connecting".into(),
                    error: None,
                },
            );
        }
        std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                let state = app.state::<crate::AppState>();
                let hub = &state.servers;
                let session = connect(hub, &server)?;
                let mut channel = session.channel_session()?;
                channel.request_pty(
                    "xterm-256color",
                    None,
                    Some((
                        cols.unwrap_or(100).clamp(20, 500),
                        rows.unwrap_or(30).clamp(5, 300),
                        0,
                        0,
                    )),
                )?;
                channel.handle_extended_data(ExtendedData::Merge)?;
                channel.shell()?;
                session.set_blocking(false);
                if let Some(t) = hub.terminals.lock().unwrap().get_mut(&id) {
                    t.status = "connected".into();
                }
                let mut pending = Vec::<u8>::new();
                loop {
                    for input in rx.try_iter() {
                        match input {
                            TerminalInput::Close => {
                                let _ = channel.close();
                                return Ok(());
                            }
                            TerminalInput::Resize(c, r) => {
                                let _ = channel.request_pty_size(c, r, None, None);
                            }
                            TerminalInput::Data(bytes) => {
                                if pending.len() + bytes.len() > 1024 * 1024 {
                                    bail!("终端输入积压过多，请重新连接");
                                }
                                pending.extend(bytes);
                            }
                        }
                    }
                    if !pending.is_empty() {
                        match channel.write(&pending) {
                            Ok(n) => {
                                pending.drain(..n);
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                    let mut buf = [0u8; 16384];
                    match channel.read(&mut buf) {
                        Ok(n) if n > 0 => {
                            let mut terminals = hub.terminals.lock().unwrap();
                            if let Some(t) = terminals.get_mut(&id) {
                                t.output.extend_from_slice(&buf[..n]);
                                if t.output.len() > 2 * 1024 * 1024 {
                                    let excess = t.output.len() - 2 * 1024 * 1024;
                                    t.output.drain(..excess);
                                }
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(e) => return Err(e.into()),
                        _ => {}
                    }
                    if channel.eof() {
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            })();
            let state = app.state::<crate::AppState>();
            if let Some(t) = state.servers.terminals.lock().unwrap().get_mut(&id) {
                t.status = "disconnected".into();
                t.error = result.err().map(|e| format!("{e:#}"));
            };
        });
        return Ok(json!({}));
    }
    let mut terminals = hub.terminals.lock().unwrap();
    let terminal = terminals
        .get_mut(&id)
        .ok_or_else(|| anyhow!("终端尚未连接"))?;
    match action.as_str() {
        "poll" => Ok(
            json!({ "status": terminal.status, "error": terminal.error, "output": std::mem::take(&mut terminal.output) }),
        ),
        "write" => {
            let text = data.unwrap_or_default();
            if text.len() > 65536 {
                bail!("单次输入过长");
            }
            terminal
                .sender
                .send(TerminalInput::Data(text.into_bytes()))?;
            Ok(json!({}))
        }
        "resize" => {
            terminal.sender.send(TerminalInput::Resize(
                cols.unwrap_or(100).clamp(20, 500),
                rows.unwrap_or(30).clamp(5, 300),
            ))?;
            Ok(json!({}))
        }
        "disconnect" => {
            let _ = terminal.sender.send(TerminalInput::Close);
            Ok(json!({}))
        }
        _ => bail!("未知终端操作"),
    }
}
