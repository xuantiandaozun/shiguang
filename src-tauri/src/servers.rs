//! Local SQLite connections and plaintext credentials, plus isolated SSH/SFTP tasks.
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use ssh2::{ExtendedData, OpenFlags, OpenType, RenameFlags, Session, Sftp};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom, Write},
    net::{Shutdown, TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub group: String,
    pub note: String,
    pub auth: String,
    pub private_key: String,
    pub fingerprint: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct RemoteTask {
    pub id: String,
    pub server_id: String,
    pub server_name: String,
    pub kind: String,
    pub status: String,
    pub bytes: u64,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub log_path: String,
}
struct TaskEntry {
    info: RemoteTask,
    cancel: CancellationToken,
    socket: Option<TcpStream>,
}
pub struct ServerHub {
    config: Mutex<()>,
    db: Mutex<Connection>,
    tasks: Mutex<HashMap<String, TaskEntry>>,
    dir: PathBuf,
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("缺少参数 {key}"))
}
fn validate(s: &Server) -> Result<()> {
    if s.name.trim().is_empty()
        || s.host.trim().is_empty()
        || s.username.trim().is_empty()
        || s.port == 0
    {
        bail!("名称、主机、用户名和有效端口不能为空");
    }
    if s.host
        .contains(|c: char| c.is_whitespace() || c.is_control())
        || s.username.contains(|c: char| c.is_control())
    {
        bail!("主机或用户名包含无效字符");
    }
    if !["password", "key", "agent"].contains(&s.auth.as_str()) {
        bail!("认证方式必须为 password、key 或 agent");
    }
    if s.auth == "key" && !Path::new(&s.private_key).is_absolute() {
        bail!("私钥必须为本机绝对路径");
    }
    Ok(())
}
impl ServerHub {
    pub fn new(dir: &Path) -> Result<Self> {
        let conn = Connection::open(dir.join("servers.db"))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS servers(id TEXT PRIMARY KEY, data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS server_secrets(id TEXT PRIMARY KEY, secret TEXT NOT NULL); CREATE TABLE IF NOT EXISTS remote_tasks(id TEXT PRIMARY KEY, data TEXT NOT NULL);")?;
        let mut tasks = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT data FROM remote_tasks ORDER BY rowid DESC LIMIT 200")?;
            for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
                let mut info: RemoteTask = serde_json::from_str(&row?)?;
                if info.status == "running" {
                    info.status = "unknown".into();
                    info.error = Some("拾光曾退出，远程执行状态未知，请核实后再操作".into());
                }
                tasks.insert(
                    info.id.clone(),
                    TaskEntry {
                        info,
                        cancel: CancellationToken::new(),
                        socket: None,
                    },
                );
            }
        }
        let dir = dir.join("remote-tasks");
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            config: Mutex::new(()),
            db: Mutex::new(conn),
            tasks: Mutex::new(tasks),
            dir,
        })
    }
    fn secret(&self, id: &str) -> Result<String> {
        use rusqlite::OptionalExtension;
        Ok(self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT secret FROM server_secrets WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or_default())
    }
    #[cfg(test)]
    fn set_secret(&self, id: &str, value: &str) -> Result<()> {
        self.db.lock().unwrap().execute(
            "INSERT OR REPLACE INTO server_secrets(id,secret) VALUES(?1,?2)",
            params![id, value],
        )?;
        Ok(())
    }
    fn list(&self) -> Result<Vec<Server>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT data FROM servers ORDER BY rowid")?;
        let mut result = Vec::new();
        for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
            result.push(serde_json::from_str(&row?)?);
        }
        Ok(result)
    }
    fn get(&self, id: &str) -> Result<Server> {
        let raw: String = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT data FROM servers WHERE id=?1", [id], |r| r.get(0))
            .context("服务器连接不存在")?;
        Ok(serde_json::from_str(&raw)?)
    }
    fn save(&self, server: &Server) -> Result<()> {
        self.db.lock().unwrap().execute(
            "INSERT OR REPLACE INTO servers(id,data) VALUES(?1,?2)",
            params![server.id, serde_json::to_string(server)?],
        )?;
        Ok(())
    }
    fn save_with_secret(&self, server: &Server, secret: Option<&str>) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO servers(id,data) VALUES(?1,?2)",
            params![server.id, serde_json::to_string(server)?],
        )?;
        if let Some(value) = secret {
            tx.execute(
                "INSERT OR REPLACE INTO server_secrets(id,secret) VALUES(?1,?2)",
                params![server.id, value],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    fn delete(&self, id: &str) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute("DELETE FROM server_secrets WHERE id=?1", [id])?;
        tx.execute("DELETE FROM servers WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }
    fn busy(&self, id: &str) -> bool {
        self.tasks
            .lock()
            .unwrap()
            .values()
            .any(|t| t.info.server_id == id && t.info.status == "running")
    }
    fn update_task(&self, id: &str, f: impl FnOnce(&mut RemoteTask)) -> Result<()> {
        let mut tasks = self.tasks.lock().unwrap();
        let entry = tasks.get_mut(id).ok_or_else(|| anyhow!("任务不存在"))?;
        f(&mut entry.info);
        self.db.lock().unwrap().execute(
            "INSERT OR REPLACE INTO remote_tasks(id,data) VALUES(?1,?2)",
            params![id, serde_json::to_string(&entry.info)?],
        )?;
        Ok(())
    }
    fn task(&self, id: &str, offset: Option<u64>) -> Result<Value> {
        let info = self
            .tasks
            .lock()
            .unwrap()
            .get(id)
            .ok_or_else(|| anyhow!("任务不存在"))?
            .info
            .clone();
        let mut file = std::fs::File::open(&info.log_path)?;
        let len = file.metadata()?.len();
        let start = offset.unwrap_or(len.saturating_sub(16000)).min(len);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = vec![];
        file.take(32000).read_to_end(&mut bytes)?;
        Ok(
            json!({"task":info,"output":String::from_utf8_lossy(&bytes),"output_offset":start,"next_offset":start+bytes.len() as u64,"has_more":(start+bytes.len() as u64)<len}),
        )
    }
}

fn handshake(s: &Server) -> Result<(Session, TcpStream, String)> {
    let addresses: Vec<_> = (s.host.as_str(), s.port)
        .to_socket_addrs()?
        .take(8)
        .collect();
    let mut connected = None;
    for addr in addresses {
        if let Ok(socket) = TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
            connected = Some(socket);
            break;
        }
    }
    let socket = connected.ok_or_else(|| anyhow!("无法连接 {}:{}", s.host, s.port))?;
    socket.set_read_timeout(Some(Duration::from_secs(10)))?;
    socket.set_write_timeout(Some(Duration::from_secs(10)))?;
    let control = socket.try_clone()?;
    let mut session = Session::new()?;
    session.set_timeout(10000);
    session.set_tcp_stream(socket);
    session.handshake()?;
    let (key, _) = session
        .host_key()
        .ok_or_else(|| anyhow!("服务器未返回主机密钥"))?;
    let fp = format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(key))
    );
    Ok((session, control, fp))
}
fn authenticate(hub: &ServerHub, session: &Session, server: &Server, fp: &str) -> Result<()> {
    if server.fingerprint.as_deref() != Some(fp) {
        bail!("服务器指纹未信任或已变化：{fp}。请在拾光服务器页核对指纹后信任，尚未发送凭据。");
    }
    let password = {
        let _guard = hub.config.lock().unwrap();
        let current = hub.get(&server.id)?;
        if serde_json::to_value(&current)? != serde_json::to_value(server)? {
            bail!("连接配置已变化，请重新操作");
        }
        hub.secret(&server.id)?
    };
    match server.auth.as_str() {
        "password" => session.userauth_password(&server.username, &password)?,
        "key" => {
            session.userauth_pubkey_file(
                &server.username,
                None,
                Path::new(&server.private_key),
                if password.is_empty() {
                    None
                } else {
                    Some(&password)
                },
            )?;
        }
        "agent" => session.userauth_agent(&server.username)?,
        _ => bail!("无效认证方式"),
    }
    if !session.authenticated() {
        bail!("SSH 身份认证失败");
    }
    Ok(())
}
fn connect(hub: &ServerHub, server: &Server) -> Result<Session> {
    let (session, _, fp) = handshake(server)?;
    authenticate(hub, &session, server, &fp)?;
    Ok(session)
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

pub async fn dispatch(app: &tauri::AppHandle, name: &str, args: &Value) -> Result<Value> {
    let state = app.state::<crate::AppState>();
    let hub = &state.servers;
    match name {
        "list_servers" => Ok(json!(hub.list()?)),
        "get_server" => Ok(json!(hub.get(required(args, "server_id")?)?)),
        "create_server" | "update_server" => {
            let _guard = hub.config.lock().unwrap();
            let is_new = name == "create_server";
            let old = if is_new {
                Server {
                    id: uuid::Uuid::new_v4().to_string(),
                    port: 22,
                    ..Default::default()
                }
            } else {
                hub.get(required(args, "server_id")?)?
            };
            if hub.busy(&old.id) {
                bail!("该服务器有运行中任务，完成或取消后再修改连接");
            }
            let mut value = serde_json::to_value(&old)?;
            for key in [
                "name",
                "host",
                "port",
                "username",
                "group",
                "note",
                "auth",
                "private_key",
            ] {
                if let Some(v) = args.get(key) {
                    value[key] = v.clone();
                }
            }
            let mut updated: Server = serde_json::from_value(value)?;
            validate(&updated)?;
            if updated.host != old.host || updated.port != old.port {
                updated.fingerprint = None;
            }
            // Credentials and connection metadata commit together.
            let secret = if let Some(value) = args.get("secret") {
                Some(
                    value
                        .as_str()
                        .ok_or_else(|| anyhow!("secret 必须为字符串"))?,
                )
            } else if updated.auth != old.auth {
                Some("")
            } else {
                None
            };
            hub.save_with_secret(&updated, secret)?;
            let _ = app.emit("servers-changed", ());
            Ok(json!(updated))
        }
        "delete_server" => {
            let _guard = hub.config.lock().unwrap();
            let id = required(args, "server_id")?;
            hub.get(id)?;
            if hub.busy(id) {
                bail!("该服务器有运行中任务，不能删除");
            }
            hub.delete(id)?;
            let _ = app.emit("servers-changed", ());
            Ok(json!({"deleted":true}))
        }
        "test_server_connection" => {
            let s = hub.get(required(args, "server_id")?)?;
            let app = app.clone();
            tokio::task::spawn_blocking(move || { let state = app.state::<crate::AppState>(); let (session, _, fp) = handshake(&s)?; if s.fingerprint.as_deref() != Some(&fp) { return Ok(json!({"status":"needs_trust","fingerprint":fp,"previous_fingerprint":s.fingerprint})); } authenticate(&state.servers,&session,&s,&fp)?; Ok(json!({"status":"connected","fingerprint":fp})) }).await?
        }
        "sftp_list" | "sftp_stat" => {
            let s = hub.get(required(args, "server_id")?)?;
            let path = required(args, "remote_path")?.to_string();
            let list = name == "sftp_list";
            let app = app.clone();
            tokio::task::spawn_blocking(move || {
                let state = app.state::<crate::AppState>();
                let session = connect(&state.servers, &s)?;
                let sftp = session.sftp()?;
                if !list {
                    return Ok(json!(sftp.stat(Path::new(&path)).map(stat_json)?));
                }
                let mut dir = sftp.opendir(Path::new(&path))?;
                let mut entries = vec![];
                let mut truncated = false;
                loop {
                    match dir.readdir() {
                        Ok((p, st)) => {
                            let n = p.to_string_lossy();
                            if n == "." || n == ".." {
                                continue;
                            }
                            if entries.len() >= 2000 {
                                truncated = true;
                                break;
                            }
                            entries.push(json!({"name":n,"stat":stat_json(st)}));
                        }
                        Err(e) if e.code() == ssh2::ErrorCode::Session(-16) => break,
                        Err(e) => return Err(e.into()),
                    }
                }
                Ok(json!({"entries":entries,"truncated":truncated}))
            })
            .await?
        }
        "ssh_exec" | "sftp_upload" | "sftp_download" => start(app, name, args),
        "list_remote_tasks" => {
            let mut tasks: Vec<_> = hub
                .tasks
                .lock()
                .unwrap()
                .values()
                .map(|t| t.info.clone())
                .collect();
            tasks.sort_by(|a, b| b.started_at.cmp(&a.started_at));
            Ok(json!(tasks))
        }
        "get_remote_task" | "wait_remote_task" => {
            let id = required(args, "task_id")?;
            let timeout = args
                .get("timeout_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(30)
                .clamp(1, 60);
            if name == "wait_remote_task" {
                let until = Instant::now() + Duration::from_secs(timeout);
                loop {
                    let running = hub
                        .tasks
                        .lock()
                        .unwrap()
                        .get(id)
                        .ok_or_else(|| anyhow!("任务不存在"))?
                        .info
                        .status
                        == "running";
                    if !running || Instant::now() >= until {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
            hub.task(id, args.get("output_offset").and_then(Value::as_u64))
        }
        "cancel_remote_task" => {
            let id = required(args, "task_id")?;
            {
                let tasks = hub.tasks.lock().unwrap();
                let t = tasks.get(id).ok_or_else(|| anyhow!("任务不存在"))?;
                if t.info.status == "running" {
                    t.cancel.cancel();
                    if let Some(socket) = &t.socket {
                        let _ = socket.shutdown(Shutdown::Both);
                    }
                }
            }
            hub.task(id, None)
        }
        _ => bail!("未知服务器工具 {name}"),
    }
}
fn stat_json(st: ssh2::FileStat) -> Value {
    json!({"size":st.size,"permissions":st.perm,"mtime":st.mtime,"is_dir":st.is_dir(),"is_file":st.is_file()})
}

fn start(app: &tauri::AppHandle, kind: &str, args: &Value) -> Result<Value> {
    let state = app.state::<crate::AppState>();
    let hub = &state.servers;
    let _guard = hub.config.lock().unwrap();
    let server = hub.get(required(args, "server_id")?)?;
    if server.fingerprint.is_none() {
        bail!("请先测试连接并在服务器页信任主机指纹");
    }
    if kind == "ssh_exec" {
        required(args, "command")?;
    } else {
        required(args, "remote_path")?;
        if !Path::new(required(args, "local_path")?).is_absolute() {
            bail!("本地路径必须为绝对路径");
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let log = hub.dir.join(format!("{id}.log"));
    std::fs::File::create(&log)?;
    let info = RemoteTask {
        id: id.clone(),
        server_id: server.id.clone(),
        server_name: server.name.clone(),
        kind: kind.into(),
        status: "running".into(),
        bytes: 0,
        exit_code: None,
        error: None,
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: None,
        log_path: log.to_string_lossy().into_owned(),
    };
    let cancel = CancellationToken::new();
    {
        let mut tasks = hub.tasks.lock().unwrap();
        if tasks
            .values()
            .filter(|t| t.info.status == "running")
            .count()
            >= 8
        {
            bail!("远程任务最多同时运行 8 项");
        }
        tasks.insert(
            id.clone(),
            TaskEntry {
                info: info.clone(),
                cancel: cancel.clone(),
                socket: None,
            },
        );
    }
    hub.update_task(&id, |_| {})?;
    let app = app.clone();
    let args = args.clone();
    let kind = kind.to_string();
    tokio::task::spawn_blocking(move || {
        let state = app.state::<crate::AppState>();
        let hub = &state.servers;
        let result = run(hub, &id, &server, &kind, &args, &cancel, &log);
        let _ = hub.update_task(&id, |t| {
            t.status = if cancel.is_cancelled() {
                "cancelled"
            } else if result.is_ok() {
                "done"
            } else {
                "failed"
            }
            .into();
            if let Err(e) = &result {
                t.error = Some(format!("{e:#}；命令失败或断开后请核实远程状态，勿自动重试"));
            }
            t.finished_at = Some(chrono::Utc::now().to_rfc3339());
        });
        if let Some(entry) = hub.tasks.lock().unwrap().get_mut(&id) {
            entry.socket = None;
        }
        let _ = app.emit("remote-tasks-changed", ());
    });
    Ok(json!({"task_id":info.id,"task":info}))
}
fn run(
    hub: &ServerHub,
    id: &str,
    s: &Server,
    kind: &str,
    args: &Value,
    cancel: &CancellationToken,
    log: &Path,
) -> Result<()> {
    let (session, socket, fp) = handshake(s)?;
    {
        let mut tasks = hub.tasks.lock().unwrap();
        tasks.get_mut(id).unwrap().socket = Some(socket);
    }
    if cancel.is_cancelled() {
        bail!("已取消");
    }
    authenticate(hub, &session, s, &fp)?;
    if kind == "ssh_exec" {
        let mut channel = session.channel_session()?;
        channel.handle_extended_data(ExtendedData::Merge)?;
        let command = required(args, "command")?;
        let command = match args
            .get("workdir")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            Some(dir) => format!("cd -- {} && {}", quote(dir), command),
            None => command.into(),
        };
        channel.exec(&command)?;
        channel.send_eof()?;
        session.set_blocking(false);
        let timeout = args
            .get("timeout_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(600)
            .clamp(1, 86400);
        let start = Instant::now();
        let mut output = std::fs::OpenOptions::new().append(true).open(log)?;
        let mut bytes = [0u8; 16384];
        let mut total = 0u64;
        loop {
            if cancel.is_cancelled() {
                bail!("已取消；远程进程可能继续运行");
            }
            if start.elapsed() >= Duration::from_secs(timeout) {
                bail!("命令超时；远程进程可能继续运行");
            }
            match channel.read(&mut bytes) {
                Ok(0) if channel.eof() => break,
                Ok(0) => std::thread::sleep(Duration::from_millis(20)),
                Ok(n) => {
                    total += n as u64;
                    if total > 64 * 1024 * 1024 {
                        bail!("命令输出超过 64 MiB，已断开；请在远端重定向日志");
                    }
                    output.write_all(&bytes[..n])?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => return Err(e.into()),
            }
        }
        session.set_blocking(true);
        channel.wait_close()?;
        let code = channel.exit_status()?;
        hub.update_task(id, |t| t.exit_code = Some(code))?;
        if code != 0 {
            bail!("远程命令退出码 {code}");
        }
        return Ok(());
    }
    let sftp = session.sftp()?;
    let remote = Path::new(required(args, "remote_path")?);
    let local = Path::new(required(args, "local_path")?);
    let mut transfer = Transfer {
        hub,
        id,
        sftp: &sftp,
        cancel,
        overwrite: args
            .get("overwrite")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        bytes: 0,
        files: 0,
    };
    transfer.tree(local, remote, kind == "sftp_upload", 0)?;
    Ok(())
}
struct Transfer<'a> {
    hub: &'a ServerHub,
    id: &'a str,
    sftp: &'a Sftp,
    cancel: &'a CancellationToken,
    overwrite: bool,
    bytes: u64,
    files: u64,
}
impl Transfer<'_> {
    fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            bail!("已取消传输");
        }
        Ok(())
    }
    fn tree(&mut self, local: &Path, remote: &Path, upload: bool, depth: usize) -> Result<()> {
        self.check()?;
        if depth > 64 || self.files > 100000 {
            bail!("目录超过传输深度或文件数量上限");
        }
        if upload {
            let st = std::fs::symlink_metadata(local)?;
            if is_link(&st) {
                bail!("不上传符号链接：{}", local.display());
            }
            if st.is_dir() {
                match self.sftp.lstat(remote) {
                    Ok(st) if st.is_dir() => (),
                    Ok(_) => bail!("远程目标不是普通目录"),
                    Err(e) if e.code() == ssh2::ErrorCode::SFTP(2) => {
                        self.sftp.mkdir(remote, 0o755)?
                    }
                    Err(e) => return Err(e.into()),
                }
                for child in std::fs::read_dir(local)? {
                    let child = child?;
                    self.tree(
                        &child.path(),
                        &remote_join(remote, &child.file_name().to_string_lossy()),
                        true,
                        depth + 1,
                    )?;
                }
                return Ok(());
            }
            if !st.is_file() {
                bail!("只支持普通文件和目录");
            }
        } else {
            let st = self.sftp.lstat(remote)?;
            if st.is_dir() {
                ensure_local_dir(local)?;
                let mut dir = self.sftp.opendir(remote)?;
                loop {
                    match dir.readdir() {
                        Ok((path, _)) => {
                            let name = path.to_string_lossy();
                            if name == "." || name == ".." {
                                continue;
                            }
                            safe_local_name(&name)?;
                            self.tree(
                                &local.join(name.as_ref()),
                                &remote_join(remote, name.as_ref()),
                                false,
                                depth + 1,
                            )?;
                        }
                        Err(e) if e.code() == ssh2::ErrorCode::Session(-16) => break,
                        Err(e) => return Err(e.into()),
                    }
                }
                return Ok(());
            }
            if !st.is_file() {
                bail!("不下载符号链接或特殊文件：{}", remote.display());
            }
        }
        self.files += 1;
        if upload {
            let permissions = match self.sftp.lstat(remote) {
                Ok(st) => {
                    if !st.is_file() || !self.overwrite {
                        bail!(
                            "远程目标已存在，覆盖需 overwrite=true：{}",
                            remote.display()
                        );
                    }
                    st.perm.unwrap_or(0o644) & 0o777
                }
                Err(e) if e.code() == ssh2::ErrorCode::SFTP(2) => 0o644,
                Err(e) => return Err(e.into()),
            };
            let temp = remote_temp(remote);
            let result = (|| -> Result<()> {
                let mut input = std::fs::File::open(local)?;
                let mut output = self.sftp.open_mode(
                    &temp,
                    OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUSIVE,
                    permissions as i32,
                    OpenType::File,
                )?;
                self.copy(&mut input, &mut output)?;
                output.close()?;
                self.check()?;
                let renamed = self.sftp.rename(
                    &temp,
                    remote,
                    Some(if self.overwrite {
                        RenameFlags::OVERWRITE | RenameFlags::ATOMIC
                    } else {
                        RenameFlags::empty()
                    }),
                );
                if let Err(error) = renamed {
                    // SFTP v3 servers often reject overwrite flags. Preserve the old
                    // file under a unique name until the replacement is committed.
                    if !self.overwrite || !matches!(error.code(), ssh2::ErrorCode::SFTP(4 | 8 | 11))
                    {
                        return Err(error.into());
                    }
                    if !self.sftp.lstat(remote)?.is_file() {
                        bail!("覆盖目标已变化，停止上传");
                    }
                    let backup = remote_temp(remote);
                    self.sftp
                        .rename(remote, &backup, Some(RenameFlags::empty()))?;
                    if let Err(commit) = self.sftp.rename(&temp, remote, Some(RenameFlags::empty()))
                    {
                        if self
                            .sftp
                            .rename(&backup, remote, Some(RenameFlags::empty()))
                            .is_err()
                        {
                            bail!(
                                "替换失败：{commit}；原文件保留于 {}，请核实并恢复",
                                backup.display()
                            );
                        }
                        return Err(anyhow!("替换失败，原文件已恢复：{commit}"));
                    }
                    if self.sftp.unlink(&backup).is_err() {
                        let log = self
                            .hub
                            .tasks
                            .lock()
                            .unwrap()
                            .get(self.id)
                            .unwrap()
                            .info
                            .log_path
                            .clone();
                        if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(log) {
                            let _ = writeln!(
                                file,
                                "上传完成；旧文件备份未能清理：{}",
                                backup.display()
                            );
                        }
                    }
                }
                Ok(())
            })();
            if result.is_err() {
                let _ = self.sftp.unlink(&temp);
            }
            result?;
        } else {
            ensure_local_dir(local.parent().ok_or_else(|| anyhow!("无效本地路径"))?)?;
            if let Ok(st) = std::fs::symlink_metadata(local) {
                if !st.is_file() || is_link(&st) || !self.overwrite {
                    bail!("本地目标已存在，覆盖需 overwrite=true：{}", local.display());
                }
            }
            let temp = local.with_file_name(format!(".shiguang-{}.part", uuid::Uuid::new_v4()));
            let result = (|| -> Result<()> {
                let mut input = self.sftp.open(remote)?;
                let mut output = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temp)?;
                self.copy(&mut input, &mut output)?;
                output.sync_all()?;
                drop(output);
                self.check()?;
                if self.overwrite {
                    std::fs::rename(&temp, local)?;
                } else {
                    std::fs::hard_link(&temp, local)?;
                    std::fs::remove_file(&temp)?;
                }
                Ok(())
            })();
            if result.is_err() {
                let _ = std::fs::remove_file(&temp);
            }
            result?;
        }
        Ok(())
    }
    fn copy(&mut self, input: &mut impl Read, output: &mut impl Write) -> Result<()> {
        let mut buf = [0; 65536];
        let mut last = Instant::now();
        loop {
            self.check()?;
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            output.write_all(&buf[..n])?;
            self.bytes += n as u64;
            if last.elapsed() > Duration::from_millis(500) {
                self.hub.update_task(self.id, |t| t.bytes = self.bytes)?;
                last = Instant::now();
            }
        }
        self.hub.update_task(self.id, |t| t.bytes = self.bytes)?;
        Ok(())
    }
}
fn remote_join(path: &Path, name: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}/{}",
        path.to_string_lossy().trim_end_matches('/'),
        name
    ))
}
fn remote_temp(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let parent = text.rsplit_once('/').map(|(p, _)| p).unwrap_or(".");
    remote_join(
        Path::new(parent),
        &format!(".shiguang-{}.part", uuid::Uuid::new_v4()),
    )
}
fn is_link(st: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        st.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        st.file_type().is_symlink()
    }
}
fn ensure_local_dir(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        if let Ok(st) = std::fs::symlink_metadata(ancestor) {
            if is_link(&st) || !st.is_dir() {
                bail!("本地目录包含链接或非目录：{}", ancestor.display());
            }
        }
    }
    std::fs::create_dir_all(path)?;
    Ok(())
}
fn safe_local_name(name: &str) -> Result<()> {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name.contains(|c: char| c.is_control() || "<>:\"/\\|?*".contains(c))
        || [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ]
        .contains(&stem.as_str())
    {
        bail!("远程文件名无法安全保存到 Windows：{name}");
    }
    Ok(())
}

#[tauri::command]
pub async fn server_action(
    app: tauri::AppHandle,
    name: String,
    args: Value,
) -> std::result::Result<Value, String> {
    dispatch(&app, &name, &args)
        .await
        .map_err(|e| format!("{e:#}"))
}
// Trust can only be changed from the desktop, never by an AI/MCP tool.
#[tauri::command]
pub async fn trust_server_host(
    app: tauri::AppHandle,
    server_id: String,
    fingerprint: String,
) -> std::result::Result<Value, String> {
    async fn trust(app: &tauri::AppHandle, id: &str, fp: &str) -> Result<Value> {
        let state = app.state::<crate::AppState>();
        let mut server = state.servers.get(id)?;
        let snapshot = server.clone();
        let observed =
            tokio::task::spawn_blocking(move || handshake(&snapshot).map(|(_, _, fp)| fp))
                .await??;
        let _guard = state.servers.config.lock().unwrap();
        let current = state.servers.get(id)?;
        if serde_json::to_value(&current)? != serde_json::to_value(&server)? {
            bail!("连接配置已变化，请重新测试连接");
        }
        if observed != fp {
            bail!("指纹已变化，请重新测试连接后核对");
        }
        server.fingerprint = Some(fp.into());
        state.servers.save(&server)?;
        let _ = app.emit("servers-changed", ());
        Ok(json!(server))
    }
    trust(&app, &server_id, &fingerprint)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shiguang-ssh-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    #[test]
    fn sqlite_roundtrip_and_restart_do_not_expose_secret() {
        let dir = test_dir();
        {
            let hub = ServerHub::new(&dir).unwrap();
            let s = Server {
                id: "test".into(),
                name: "测试".into(),
                host: "localhost".into(),
                port: 22,
                username: "user".into(),
                auth: "password".into(),
                ..Default::default()
            };
            hub.save(&s).unwrap();
            hub.set_secret(&s.id, "saved-password").unwrap();
            assert!(!serde_json::to_string(&hub.list().unwrap())
                .unwrap()
                .contains("saved-password"));
        }
        {
            let hub = ServerHub::new(&dir).unwrap();
            assert_eq!(hub.secret("test").unwrap(), "saved-password");
            assert_eq!(hub.get("test").unwrap().name, "测试");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    #[ignore = "requires Python paramiko; starts a loopback-only SSH fixture"]
    fn loopback_ssh_sftp() {
        use std::io::BufRead;
        struct Fixture(std::process::Child);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let dir = test_dir();
        let remote = dir.join("remote");
        let mut child = std::process::Command::new("python")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/ssh-test-fixture.py"))
            .arg(&remote)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let _fixture = Fixture(child);
        let fixture: Value = serde_json::from_str(&line).expect("SSH fixture must start");
        let hub = ServerHub::new(&dir).unwrap();
        let mut server = Server {
            id: "fixture".into(),
            name: "fixture".into(),
            host: "127.0.0.1".into(),
            port: fixture["port"].as_u64().unwrap() as u16,
            username: "fixture".into(),
            auth: "password".into(),
            fingerprint: Some(fixture["fingerprint"].as_str().unwrap().into()),
            ..Default::default()
        };
        hub.save(&server).unwrap();
        hub.set_secret("fixture", "fixture-password").unwrap();
        let (session, _, fp) = handshake(&server).unwrap();
        assert_eq!(fp, server.fingerprint.clone().unwrap());
        authenticate(&hub, &session, &server, &fp).unwrap();
        drop(session);
        let mut wrong = server.clone();
        wrong.fingerprint = Some("SHA256:wrong".into());
        let (session, _, fp) = handshake(&wrong).unwrap();
        assert!(authenticate(&hub, &session, &wrong, &fp).is_err());
        drop(session);
        fn job(
            hub: &ServerHub,
            s: &Server,
            kind: &str,
            args: Value,
            cancel: CancellationToken,
        ) -> Result<RemoteTask> {
            let id = uuid::Uuid::new_v4().to_string();
            let log = hub.dir.join(format!("{id}.log"));
            std::fs::File::create(&log)?;
            let info = RemoteTask {
                id: id.clone(),
                server_id: s.id.clone(),
                server_name: s.name.clone(),
                kind: kind.into(),
                status: "running".into(),
                bytes: 0,
                exit_code: None,
                error: None,
                started_at: "test".into(),
                finished_at: None,
                log_path: log.to_string_lossy().into_owned(),
            };
            hub.tasks.lock().unwrap().insert(
                id.clone(),
                TaskEntry {
                    info,
                    cancel: cancel.clone(),
                    socket: None,
                },
            );
            run(hub, &id, s, kind, &args, &cancel, &log)?;
            let info = hub.tasks.lock().unwrap().get(&id).unwrap().info.clone();
            Ok(info)
        }
        let info = job(
            &hub,
            &server,
            "ssh_exec",
            json!({"command":"fixture_large"}),
            CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(info.exit_code, Some(0));
        let output = std::fs::read(&info.log_path).unwrap();
        assert_eq!(output.len(), 262144);
        assert_eq!(output.iter().filter(|&&c| c == b'e').count(), 131072);
        assert!(job(
            &hub,
            &server,
            "ssh_exec",
            json!({"command":"fixture_fail"}),
            CancellationToken::new()
        )
        .is_err());
        assert!(job(
            &hub,
            &server,
            "ssh_exec",
            json!({"command":"fixture_slow","timeout_seconds":1}),
            CancellationToken::new()
        )
        .is_err());
        let cancel = CancellationToken::new();
        let signal = cancel.clone();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            signal.cancel();
        });
        assert!(job(
            &hub,
            &server,
            "ssh_exec",
            json!({"command":"fixture_slow"}),
            cancel
        )
        .is_err());
        stopper.join().unwrap();
        let source = dir.join("source");
        std::fs::create_dir_all(source.join("子目录")).unwrap();
        let content = vec![42u8; 200000];
        std::fs::write(source.join("子目录/data.bin"), &content).unwrap();
        let upload = json!({"local_path":source,"remote_path":"/uploaded"});
        let task = job(
            &hub,
            &server,
            "sftp_upload",
            upload.clone(),
            CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(task.bytes, 200000);
        assert_eq!(
            std::fs::read(remote.join("uploaded/子目录/data.bin")).unwrap(),
            content
        );
        assert!(job(
            &hub,
            &server,
            "sftp_upload",
            upload.clone(),
            CancellationToken::new()
        )
        .is_err());
        let session = connect(&hub, &server).unwrap();
        let sftp = session.sftp().unwrap();
        assert!(sftp
            .stat(Path::new("/uploaded/子目录/data.bin"))
            .unwrap()
            .is_file());
        let mut directory = sftp.opendir(Path::new("/uploaded")).unwrap();
        assert_eq!(directory.readdir().unwrap().0.to_string_lossy(), "子目录");
        drop(directory);
        drop(sftp);
        drop(session);
        let mut upload_overwrite = upload.clone();
        upload_overwrite["overwrite"] = json!(true);
        job(
            &hub,
            &server,
            "sftp_upload",
            upload_overwrite,
            CancellationToken::new(),
        )
        .unwrap();
        let destination = dir.join("download");
        let download = json!({"local_path":destination,"remote_path":"/uploaded"});
        job(
            &hub,
            &server,
            "sftp_download",
            download.clone(),
            CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(destination.join("子目录/data.bin")).unwrap(),
            content
        );
        assert!(job(
            &hub,
            &server,
            "sftp_download",
            download.clone(),
            CancellationToken::new()
        )
        .is_err());
        let mut overwrite = download;
        overwrite["overwrite"] = json!(true);
        job(
            &hub,
            &server,
            "sftp_download",
            overwrite,
            CancellationToken::new(),
        )
        .unwrap();
        server.auth = "key".into();
        server.private_key = remote.join("client.pem").to_string_lossy().into_owned();
        hub.save(&server).unwrap();
        hub.set_secret(&server.id, "").unwrap();
        let session = connect(&hub, &server).unwrap();
        assert!(session.authenticated());
        drop(session);
        drop(hub);
        drop(_fixture);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn rejects_windows_path_escape_and_devices() {
        for name in [
            "../x", "a/b", "a\\b", "x:stream", "NUL.txt", "COM1", "x.", "x ",
        ] {
            assert!(safe_local_name(name).is_err(), "{name}");
        }
        assert!(safe_local_name("应用.log").is_ok());
    }
    #[test]
    fn remote_paths_keep_unix_separators_on_windows() {
        assert_eq!(
            remote_join(Path::new("/var/www"), "app").to_string_lossy(),
            "/var/www/app"
        );
        assert!(remote_temp(Path::new("/var/www/file"))
            .to_string_lossy()
            .starts_with("/var/www/.shiguang-"));
    }
    #[test]
    fn shell_directory_is_quoted() {
        assert_eq!(quote("a'b"), "'a'\"'\"'b'");
    }
    #[test]
    fn profile_cannot_have_invalid_auth_or_relative_key() {
        let mut s = Server {
            name: "test".into(),
            host: "localhost".into(),
            port: 22,
            username: "root".into(),
            auth: "key".into(),
            private_key: "key.pem".into(),
            ..Default::default()
        };
        assert!(validate(&s).is_err());
        s.auth = "agent".into();
        assert!(validate(&s).is_ok());
    }
}

#[tauri::command]
pub fn reveal_server_secret(
    app: tauri::AppHandle,
    server_id: String,
) -> std::result::Result<String, String> {
    app.state::<crate::AppState>()
        .servers
        .secret(&server_id)
        .map_err(|e| e.to_string())
}
