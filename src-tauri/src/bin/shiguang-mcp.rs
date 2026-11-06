//! Stdio MCP entry point for Codex and Cursor. The desktop app owns all task execution.
#[path = "../server_tools.rs"]
mod server_tools;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    // 每个请求单独处理：ask_main 可能阻塞几十分钟，不能卡住 ping 等其它请求。
    let stdout = std::sync::Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
    while let Some(line) = lines.next_line().await? {
        let req: Value = match serde_json::from_str(&line) { Ok(v) => v, Err(_) => continue };
        let Some(id) = req.get("id").cloned() else { continue };
        let stdout = stdout.clone();
        tokio::spawn(async move {
            let response = match handle(&req).await {
                Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
                Err(e) => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":e.to_string()}}),
            };
            let mut out = stdout.lock().await;
            let _ = out.write_all(response.to_string().as_bytes()).await;
            let _ = out.write_all(b"\n").await;
            let _ = out.flush().await;
        });
    }
    Ok(())
}

/// 外派模式：拾光启动 Claude Code / Codex 时通过环境变量指定运行编号，只暴露回问主 AI 的两个工具。
fn agent_run() -> Option<String> {
    std::env::var("SHIGUANG_AGENT_RUN").ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn agent_tools() -> Vec<Value> {
    vec![
        json!({"name":"ask_main","description":"向拾光（外派你的主助手，背后是用户）提问并等待回答。用于：需求不清、需要扩大改动范围、要动线上数据或装新依赖、需要线上/浏览器实测结果、需要用户决定的事。问题要具体，给出你倾向的方案和理由。调用会阻塞直到拾光回答。","inputSchema":{"type":"object","properties":{"question":{"type":"string"},"context":{"type":"string","description":"可选：已查到的事实、候选方案"}},"required":["question"]}}),
        json!({"name":"report_checkpoint","description":"到达任务约定的关键节点时汇报并等待拾光审核；拾光回复后按回复继续（可能是继续、调整范围或停止）。","inputSchema":{"type":"object","properties":{"milestone":{"type":"string"},"done":{"type":"string","description":"已完成的内容"},"evidence":{"type":"string","description":"证据：测试/构建结果、关键 diff"},"next":{"type":"string","description":"打算的下一步"}},"required":["milestone","done"]}}),
    ]
}

async fn call_agent_tool(run: &str, name: &str, args: Value) -> Result<Value> {
    let kind = match name {
        "ask_main" => "ask",
        "report_checkpoint" => "checkpoint",
        _ => return Err(anyhow!("未知工具")),
    };
    let asked = bridge(json!({"action":"agent_ask","id":run,"kind":kind,"payload":args})).await?;
    if let Some(answer) = asked.get("answer").and_then(Value::as_str) {
        return Ok(json!(answer));
    }
    let qid = asked.get("qid").and_then(Value::as_str).ok_or_else(|| anyhow!("拾光没有受理这个问题"))?.to_string();
    let start = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        let polled = match bridge(json!({"action":"agent_poll","id":run,"qid":qid})).await {
            Ok(v) => v,
            Err(_) if start.elapsed().as_secs() < 1800 => continue,
            Err(e) => return Err(e),
        };
        if let Some(answer) = polled.get("answer").and_then(Value::as_str) {
            return Ok(json!(answer));
        }
        if polled.get("cancelled").and_then(Value::as_bool) == Some(true) {
            return Err(anyhow!("外派任务已被中断，请停止工作并简短总结当前进度"));
        }
        if start.elapsed().as_secs() >= 1800 {
            return Ok(json!("拾光 30 分钟内没有回答。请按现有信息做最稳妥的选择，并在最终总结里写明这个待确认的问题。"));
        }
    }
}

async fn handle(req: &Value) -> Result<Value> {
    if let Some(run) = agent_run() {
        return match req.get("method").and_then(Value::as_str).unwrap_or("") {
            "initialize" => Ok(json!({
                "protocolVersion":"2025-11-25",
                "capabilities":{"tools":{}},
                "serverInfo":{"name":"shiguang","version":env!("CARGO_PKG_VERSION")},
                "instructions":"你是拾光外派的编码智能体。需要决定、需要线上实测或需求不清时用 ask_main 提问；到约定的关键节点用 report_checkpoint 汇报。两者都会阻塞到拾光回复。"
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools":agent_tools()})),
            "tools/call" => {
                let params = req.get("params").ok_or_else(|| anyhow!("missing params"))?;
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                match call_agent_tool(&run, name, args).await {
                    Ok(answer) => {
                        let text = answer.as_str().map(str::to_string).unwrap_or_else(|| answer.to_string());
                        Ok(json!({"content":[{"type":"text","text":text}]}))
                    }
                    Err(e) => Ok(json!({"isError":true,"content":[{"type":"text","text":e.to_string()}]})),
                }
            }
            _ => Err(anyhow!("method not found")),
        };
    }
    match req.get("method").and_then(Value::as_str).unwrap_or("") {
        "initialize" => Ok(json!({
            "protocolVersion":"2025-11-25",
            "capabilities":{"tools":{}},
            "serverInfo":{"name":"shiguang-delegation","version":env!("CARGO_PKG_VERSION")},
            "instructions":"服务器操作直接使用 list_servers/ssh_exec/sftp_* 等工具，不需要委托模型；长操作返回 task_id，用 wait_remote_task 等待；已有密码不会自动返回。首次信任主机指纹需在拾光服务器页确认。取消或断线后远程命令可能继续，先核实状态再操作。新目标才使用 delegate_to_shiguang。goal 必须明确对象、环境、验收标准、输出绝对路径和范围；项目上下文由委托方提供，不让拾光自行搜源码或全盘找文件。复杂任务约定少量关键节点，用 report_checkpoint 回传并暂停等待委托方审核。后续保留原 id，用 wait_shiguang_delegation 挂起等待节点或结果，不逐次get历史或实时监控。waiting_peer 时检查 checkpoint/question 的结果、证据及下一步，符合目标则用 reply 明确继续范围，跑偏则立即用原id纠偏。blocked/error 先读 result，补充信息后 resume 原任务，勿新建或无限重试。丢失 id 先 list/get 核实。waiting_user 等用户，委托方不可代答。blocked/error/cancelled 不可报告为完成；complete 也须核对验收标准。"
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools":tool_list().await})),
        "tools/call" => {
            let params = req.get("params").ok_or_else(|| anyhow!("missing params"))?;
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let output = call(name, args).await;
            match output {
                Ok(data) => {
                    let data = with_continuation(data);
                    Ok(json!({"content":[{"type":"text","text":data.to_string()}],"structuredContent":{"data":data}}))
                },
                Err(e) => Ok(json!({"isError":true,"content":[{"type":"text","text":e.to_string()}]})),
            }
        }
        _ => Err(anyhow!("method not found")),
    }
}

/// 服务器工具的说明以 servers 插件为准（主程序运行时向它获取，改插件即生效）；
/// 主程序未运行时退回编译时的副本。
async fn server_tool_definitions() -> Vec<Value> {
    match bridge(json!({"action":"server_tool_definitions"})).await {
        Ok(value) => match value.get("tools").and_then(Value::as_array) {
            Some(tools) if !tools.is_empty() => tools.clone(),
            _ => server_tools::definitions(),
        },
        Err(_) => server_tools::definitions(),
    }
}

async fn tool_list() -> Vec<Value> {
    let mut tools = delegation_tools();
    tools.extend(server_tool_definitions().await);
    tools
}

fn delegation_tools() -> Vec<Value> {
    vec![
        json!({"name":"delegate_to_shiguang","description":"为新目标创建精准委托。goal 写清对象/环境、必要上下文、允许与排除范围、完成标准、产出绝对路径及关键节点。缺项先由委托方补齐，不让拾光主动搜源码/全盘找文件。约定节点要求 report_checkpoint 回传证据并暂停审核。已有任务用原 id reply/resume 继续。","inputSchema":{"type":"object","properties":{"title":{"type":"string"},"goal":{"type":"string"},"origin":{"type":"string"}},"required":["goal"]}}),
        json!({"name":"wait_shiguang_delegation","description":"挂起等待关键节点、问题或结果，返回任务状态及 checkpoint，不重复返回完整历史。waiting_peer 时审核结果/证据/下一步，用 reply 批准限定阶段或及时纠偏；常规阶段自主执行，不逐次监控。running 超时继续等待，勿新建。","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":60}},"required":["id"]}}),
        json!({"name":"get_shiguang_delegation","description":"读取一项委托的状态及完整交流记录。","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}}),
        json!({"name":"list_shiguang_delegations","description":"列出拾光中的委托任务。","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"reply_shiguang_delegation","description":"回答上下文问题、审核关键节点或纠偏原任务。节点通过说明下一阶段范围；跑偏明确停止的动作、正确对象/事实/路径和下次节点。正在运行时会中断本轮并沿用原历史执行修正。不能代答 waiting_user。","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"content":{"type":"string"}},"required":["id","content"]}}),
        json!({"name":"resume_shiguang_delegation","description":"继续 blocked/error 的原委托，保留原 id、目标和全部历史。步骤预算耗尽可不填 content；缺信息或出错先补充事实/修复意见。running 时直接返回当前任务，不中断或重启；waiting_peer 用 reply 回答，waiting_user 等用户。不要在相同阻碍下无限续跑。","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"content":{"type":"string"}},"required":["id"]}}),
        json!({"name":"cancel_shiguang_delegation","description":"中断一项委托任务。","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}}),
    ]
}

async fn call(name: &str, args: Value) -> Result<Value> {
    if server_tool_definitions().await.iter().any(|t| t["name"].as_str() == Some(name)) {
        return bridge(json!({"action":"server_tool","name":name,"arguments":args})).await;
    }
    let get = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    match name {
        "delegate_to_shiguang" => bridge(json!({"action":"create","title":get("title"),"goal":get("goal"),"origin":get("origin")})).await,
        "get_shiguang_delegation" => bridge(json!({"action":"get","id":get("id")})).await,
        "list_shiguang_delegations" => bridge(json!({"action":"list"})).await,
        "reply_shiguang_delegation" => bridge(json!({"action":"reply","id":get("id"),"content":get("content")})).await,
        "resume_shiguang_delegation" => bridge(json!({"action":"resume","id":get("id"),"content":get("content")})).await,
        "cancel_shiguang_delegation" => bridge(json!({"action":"cancel","id":get("id")})).await,
        "wait_shiguang_delegation" => {
            let id = get("id");
            let timeout = args.get("timeout_seconds").and_then(Value::as_u64).unwrap_or(30).clamp(1, 60);
            let start = tokio::time::Instant::now();
            loop {
                let view = bridge(json!({"action":"get","id":id})).await?;
                let status = view.pointer("/task/status").and_then(Value::as_str).unwrap_or("");
                if status != "running" || start.elapsed().as_secs() >= timeout { return Ok(wait_update(view)); }
                tokio::time::sleep(std::time::Duration::from_millis(750)).await;
            }
        }
        _ => Err(anyhow!("未知工具")),
    }
}

fn wait_update(mut view: Value) -> Value {
    if let Some(object) = view.as_object_mut() {
        object.remove("messages");
        object.insert("history_available_via".to_string(), json!("get_shiguang_delegation"));
    }
    view
}

fn with_continuation(mut data: Value) -> Value {
    if let Some(items) = data.as_array_mut() {
        for item in items { *item = with_continuation(item.take()); }
        return data;
    }
    let task = data.get("task").unwrap_or(&data);
    if task.get("server_id").is_some() && task.get("kind").is_some() {
        if task.get("status").and_then(Value::as_str) == Some("running") {
            if let Some(id) = task.get("id").and_then(Value::as_str) {
                let next = json!({"tool":"wait_remote_task","arguments":{"task_id":id},"reuse_existing":true,"hint":"等待原服务器任务，勿重复执行命令或传输。"});
                data["continuation"] = next;
            }
        }
        return data;
    }
    let Some(id) = task.get("id").and_then(Value::as_str) else { return data; };
    let status = task.get("status").and_then(Value::as_str).unwrap_or("");
    let (tool, hint) = match status {
        "running" => ("wait_shiguang_delegation", "继续等待原任务，勿新建委托。"),
        "waiting_peer" => ("reply_shiguang_delegation", "检查 checkpoint/question：关键节点需核对结果、证据和下一步，通过后明确继续范围；发现跑偏立即纠正原任务。缺上下文则提供准确事实，勿让拾光自行扩大搜索。"),
        "waiting_user" => ("wait_shiguang_delegation", "等待用户回答，委托方不可代答。"),
        "blocked" | "error" => ("resume_shiguang_delegation", "先检查 result 中的阻碍，补充信息或修复后复用原任务；步骤耗尽可直接继续。不要重新创建委托或无限重试。"),
        _ => return data,
    };
    let continuation = json!({"task_id":id,"tool":tool,"arguments":{"id":id},"reuse_existing":true,"hint":hint});
    data["continuation"] = continuation;
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_tasks_wait_with_remote_task_id() {
        let output = with_continuation(json!({"task_id":"remote-one","task":{"id":"remote-one","server_id":"server-one","kind":"ssh_exec","status":"running"}}));
        assert_eq!(output["continuation"]["tool"], "wait_remote_task");
        assert_eq!(output["continuation"]["arguments"]["task_id"], "remote-one");
        for status in ["failed", "cancelled", "unknown", "done"] {
            assert!(with_continuation(json!({"id":"remote-one","server_id":"server-one","kind":"sftp_upload","status":status})).get("continuation").is_none());
        }
    }

    #[test]
    fn waiting_returns_checkpoint_without_repeated_history() {
        let view = wait_update(json!({"task":{"id":"one","status":"waiting_peer","question":"请审核"},"checkpoint":{"milestone":"对象确认","next_step":"执行验收"},"messages":[{"content":"large tool history"}]}));
        assert!(view.get("messages").is_none());
        assert_eq!(view["checkpoint"]["milestone"], "对象确认");
        assert_eq!(view["task"]["question"], "请审核");
        let output = with_continuation(view);
        assert_eq!(output["continuation"]["tool"], "reply_shiguang_delegation");
        assert_eq!(output["continuation"]["arguments"]["id"], "one");
    }

    #[test]
    fn recoverable_states_direct_clients_to_original_task() {
        for status in ["blocked", "error"] {
            let view = with_continuation(json!({"task":{"id":"original","status":status},"messages":[{"content":"existing"}]}));
            assert_eq!(view["continuation"]["tool"], "resume_shiguang_delegation");
            assert_eq!(view["continuation"]["arguments"]["id"], "original");
            assert_eq!(view["messages"][0]["content"], "existing");
        }
    }

    #[test]
    fn user_confirmation_and_terminal_tasks_are_not_resumed() {
        let waiting = with_continuation(json!({"id":"one","status":"waiting_user"}));
        assert_eq!(waiting["continuation"]["tool"], "wait_shiguang_delegation");
        for status in ["complete", "cancelled"] {
            assert!(with_continuation(json!({"id":"one","status":status})).get("continuation").is_none());
        }
        let list = with_continuation(json!([{"id":"one","status":"blocked"}]));
        assert_eq!(list[0]["continuation"]["task_id"], "one");
    }
}

async fn bridge(payload: Value) -> Result<Value> {
    let dir = std::env::var_os("SHIGUANG_DATA_DIR").map(std::path::PathBuf::from)
        .or_else(|| dirs::data_dir().map(|p| p.join("com.deskhelper.win")))
        .ok_or_else(|| anyhow!("找不到拾光数据目录"))?;
    let info: Value = serde_json::from_slice(&std::fs::read(dir.join("mcp-bridge.json")).map_err(|_| anyhow!("拾光未运行，请先启动桌面应用"))?)?;
    let port = info.get("port").and_then(Value::as_u64).ok_or_else(|| anyhow!("桥接端口无效"))?;
    let token = info.get("token").and_then(Value::as_str).ok_or_else(|| anyhow!("桥接令牌无效"))?;
    // 本机桥接不能走代理：终端里的 HTTP_PROXY（如 Clash）会把 127.0.0.1 请求转出去，
    // 代理一出问题就返回非 JSON 的 502，表现为“响应解析失败”。
    let client = reqwest::Client::builder().no_proxy().build()?;
    let response = client.post(format!("http://127.0.0.1:{port}/"))
        .header("x-shiguang-token", token).json(&payload).send().await
        .map_err(|_| anyhow!("无法连接拾光，请确认桌面应用正在运行"))?;
    let status = response.status();
    let body = response.text().await.map_err(|e| anyhow!("读取拾光响应失败（HTTP {status}）：{e}"))?;
    let value: Value = serde_json::from_str(&body).map_err(|_| {
        let head: String = body.chars().take(120).collect();
        anyhow!("拾光返回的不是有效响应（HTTP {status}）：{head}。可能端口被其他程序占用，请重启拾光后重试")
    })?;
    if let Some(e) = value.get("error").and_then(Value::as_str) { return Err(anyhow!(e.to_string())); }
    Ok(value)
}
