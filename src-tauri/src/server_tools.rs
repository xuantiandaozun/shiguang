//! Shared schemas for desktop AI and stdio MCP. No desktop dependencies.
use serde_json::{json, Value};

pub fn definitions() -> Vec<Value> {
    let s = || json!({"type":"string"});
    let id = || json!({"server_id":s()});
    let mut profile = json!({"name":s(),"host":s(),"port":{"type":"integer","minimum":1,"maximum":65535},"username":s(),"group":s(),"note":s(),"auth":{"type":"string","enum":["password","key","agent"]},"private_key":s(),"secret":{"type":"string","description":"密码或私钥口令，仅写入；省略保留，空串清除。已有凭据不会返回。"}});
    let mut update = profile.clone();
    update["server_id"] = s();
    profile["port"]["default"] = json!(22);
    let entries = vec![
        ("list_servers", "列出本地服务器连接，不返回密码。", json!({}), vec![]),
        ("get_server", "读取一项本地服务器连接，不返回密码。", id(), vec!["server_id"]),
        ("create_server", "新增本地 SSH 连接。secret 为密码或私钥口令。首次连接需要用户在拾光服务器页核对并信任指纹。", profile, vec!["name","host","username","auth"]),
        ("update_server", "修改本地连接，仅更新提供的字段。修改地址、端口会清除指纹信任。", update, vec!["server_id"]),
        ("delete_server", "删除本地连接及凭据，不删除远程文件。运行中任务存在时拒绝删除。", id(), vec!["server_id"]),
        ("test_server_connection", "检查 SSH 主机指纹；未信任时不发送凭据，返回待确认的指纹。已信任则测试登录。", id(), vec!["server_id"]),
        ("ssh_exec", "启动远程非交互命令，返回 task_id。每次独立 Shell；需明确工作目录。用 wait_remote_task 等待结果。破坏性操作须有用户授权。超时/取消仅断开连接，远程进程可能继续，勿自动重试。", json!({"server_id":s(),"command":s(),"workdir":s(),"timeout_seconds":{"type":"integer","minimum":1,"maximum":86400}}), vec!["server_id","command"]),
        ("sftp_list", "列出远程目录，返回解析后的绝对 path、entries 及 truncated；最多 2000 项。后续路径可基于返回的 path 拼接。", json!({"server_id":s(),"remote_path":s()}), vec!["server_id","remote_path"]),
        ("sftp_stat", "读取远程路径元数据。", json!({"server_id":s(),"remote_path":s()}), vec!["server_id","remote_path"]),
        ("sftp_upload", "上传单个文件或目录到精确远程路径（目录目标为根目录）；拒绝跟随符号链接。默认拒绝覆盖。先传临时文件，完成后重命名。返回 task_id。", json!({"server_id":s(),"local_path":s(),"remote_path":s(),"overwrite":{"type":"boolean"}}), vec!["server_id","local_path","remote_path"]),
        ("sftp_download", "下载文件或目录到本机绝对路径（目录目标为根目录）；拒绝符号链接，默认拒绝覆盖。返回 task_id。", json!({"server_id":s(),"local_path":s(),"remote_path":s(),"overwrite":{"type":"boolean"}}), vec!["server_id","local_path","remote_path"]),
        ("list_remote_tasks", "列出远程命令及传输任务，不返回命令正文。", json!({}), vec![]),
        ("get_remote_task", "查询任务、传输字节数及日志尾部。output_offset 可按字节读取日志，返回 next_offset；日志可能包含敏感命令输出。", json!({"task_id":s(),"output_offset":{"type":"integer","minimum":0}}), vec!["task_id"]),
        ("wait_remote_task", "挂起等待原任务，最长 60 秒；仍 running 时继续等待原 task_id。", json!({"task_id":s(),"timeout_seconds":{"type":"integer","minimum":1,"maximum":60},"output_offset":{"type":"integer","minimum":0}}), vec!["task_id"]),
        ("cancel_remote_task", "取消本地等待或传输并断开 SSH；远程命令可能仍运行，需核实实际状态。", json!({"task_id":s()}), vec!["task_id"]),
    ];
    entries.into_iter().map(|(name,description,properties,required)| json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})).collect()
}
