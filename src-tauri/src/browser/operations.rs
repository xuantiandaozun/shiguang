//! Structured page operations. Page JS observes once; Rust owns waits/cancellation.
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};
use tokio_util::sync::CancellationToken;

pub const TOOL_NAMES: &[&str] = &["browser_capabilities", "browser_action", "browser_wait", "browser_extract"];

pub fn definitions() -> Vec<Value> {
    let target = json!({
        "type":"object", "description":"定位条件全部 AND，文字精确匹配；ref 必须来自当前页。按可见文字或 aria 名称定位用 name（没有 text/aria 字段）；按 id 用 css。css 可用于非交互文本区域。scope 必须唯一，不跨 iframe/Shadow DOM。至少给出一个条件，不支持的字段会报错。",
        "properties": {
            "ref":{"type":"integer"}, "css":{"type":"string"},
            "role":{"type":"string"}, "name":{"type":"string"},
            "label":{"type":"string"}, "placeholder":{"type":"string"},
            "scope":{"description":"唯一 CSS 选择器或当前元素 ref"},
            "visual":{"type":"string","description":"按画面定位：用一句话描述目标外观和位置（如「右上角红色的发布按钮」「输入框右侧的相机图标」）。截取当前可见区域后由 Jev 找出位置，再用真实鼠标操作；目标须在屏幕上，不在时先滚动。只在页面结构里找不到目标时用（纯图标按钮、画布、无文字控件）；单独使用，不与其它条件组合。需要启用智能加速。"}
        }
    });
    let expect = json!({
        "type":"object", "description":"这一步之后应出现的结果；未满足则视为失败并停止后续步骤。",
        "properties": {
            "text":{"type":"string","description":"页面上应出现的文字（包含匹配）"},
            "gone_text":{"type":"string","description":"应消失的文字，如关闭的弹窗标题"},
            "url_contains":{"type":"string"},
            "timeout_ms":{"type":"integer","minimum":0,"maximum":15000,"description":"默认 5000"}
        }
    });
    let step_props = json!({
        "action":{"type":"string","enum":["click","double_click","hover","fill","press","select","set_checked","upload","dialog","locate"]},
        "target":target.clone(),
        "text":{"type":"string","description":"fill 的内容；select 的精确选项文字；dialog 的 prompt 输入"},
        "clear":{"type":"boolean","description":"fill 前清空原内容，默认 true"},
        "newline":{"type":"string","enum":["shift_enter","enter","insert"],"description":"富文本里换行的方式。默认 shift_enter（聊天框回车可能直接发送）；文章编辑器需要分段时用 enter"},
        "key":{"type":"string","description":"press 的按键：Enter、Escape、Tab、ArrowDown、Backspace、Control+A、Shift+Enter 等；给 target 时先聚焦它"},
        "checked":{"type":"boolean"},
        "files":{"type":"array","items":{"type":"string"},"description":"upload 的本机文件路径（绝对路径；相对路径按应用 temp 目录解析）"},
        "accept":{"type":"boolean","description":"dialog：true 确认，false 取消"},
        "option_scope":{"type":"string","description":"select 时选项不唯一，用 CSS 限定浮层"},
        "expect":expect,
        "settle_ms":{"type":"integer","minimum":0,"maximum":10000,"description":"动作后最长等待页面安静的毫秒数，默认 3000"}
    });
    let single_props = step_props.clone();
    vec![
        json!({"type":"function","function":{
            "name":"browser_capabilities",
            "description":"查看当前页面可用的通用操作、控件特征和限制。跨 React/Vue 等框架优先使用 browser_action/browser_wait/browser_extract，避免重复编写 DOM 查询、事件派发和固定休眠。检测不代表所有组件已适配。",
            "parameters":{"type":"object","properties":{},"required":[]}
        }}),
        json!({"type":"function","function":{
            "name":"browser_action",
            "description":"在当前标签页执行真实鼠标键盘操作（扩展调试通道发出 isTrusted 输入，React/Vue/富文本编辑器都按真人操作处理）。动作：click/double_click/hover；fill 输入；press 按键；select 精确选择原生或 ARIA 下拉的选项；set_checked 幂等勾选；upload 通过文件选择框上传本机文件；dialog 处理 alert/confirm；locate 只返回候选。target 按 ref 或 role/name/label/placeholder/css 唯一定位，执行前自动滚动、等位置稳定、检查禁用与遮挡，歧义/遮挡/禁用时不执行。每个动作后自动等页面安静并返回 effect：URL/导航、打开或关闭的弹窗、提示文字、表单错误、发出的请求、新标签、原生对话框——据此判断下一步，不必每步重拍快照。连续操作用 steps 一次提交（最多 20 步），遇到第一个失败或 expect 未满足即停，并说明停在哪一步；给关键步骤写 expect。发布/提交/删除仍需授权，effect 不等于业务成功。",
            "parameters":{"type":"object","properties":{
                "action":single_props["action"].clone(), "target":single_props["target"].clone(),
                "text":single_props["text"].clone(), "clear":single_props["clear"].clone(),
                "newline":single_props["newline"].clone(), "key":single_props["key"].clone(),
                "checked":single_props["checked"].clone(), "files":single_props["files"].clone(),
                "accept":single_props["accept"].clone(), "option_scope":single_props["option_scope"].clone(),
                "expect":single_props["expect"].clone(), "settle_ms":single_props["settle_ms"].clone(),
                "steps":{"type":"array","maxItems":20,"description":"多步顺序执行，每项字段同单步（action 必填）。给了 steps 就不要再给顶层 action",
                    "items":{"type":"object","properties":step_props,"required":["action"]}}
            },"required":[]}
        }}),
        json!({"type":"function","function":{
            "name":"browser_wait",
            "description":"等待具体页面条件，替代 setTimeout 和重复脚本；可取消，默认 5 秒，最多 15 秒，不执行点击。visible/hidden/enabled/text/value；text 为包含，value 为精确相等。目标歧义立即报错。等待超时不能证明先前提交未生效，先核验不要重复提交。",
            "parameters":{"type":"object","properties":{
                "target":target, "condition":{"type":"string","enum":["visible","hidden","enabled","text","value"]},
                "text":{"type":"string"}, "timeout_ms":{"type":"integer","minimum":0,"maximum":15000}
            },"required":["target","condition"]}
        }}),
        json!({"type":"function","function":{
            "name":"browser_extract",
            "description":"有界结构化读取当前已渲染的表格、列表卡片或指定字段，替代重复写 DOM 提取脚本。table 需唯一表格（可用 scope）；list 需 row_selector 与 fields；fields 读取唯一 scope 内一组字段。缺失字段返回 null；不自动分页/滚动，不保证币种或业务真实性。文章正文用 browser_read。",
            "parameters":{"type":"object","properties":{
                "kind":{"type":"string","enum":["table","list","fields"]},
                "scope":{"description":"唯一 CSS 选择器或当前 ref，默认 document"},
                "row_selector":{"type":"string"}, "limit":{"type":"integer","minimum":1,"maximum":100},
                "fields":{"type":"array","maxItems":12,"items":{"type":"object","properties":{
                    "name":{"type":"string"}, "selector":{"type":"string","description":"相对当前行的 CSS；:scope 表示行本身"},
                    "attribute":{"type":"string","enum":["href","title","aria-label","datetime"]}
                },"required":["name","selector"]}}
            },"required":["kind"]}
        }})
    ]
}

fn evaluation(method: &str, args: &Value) -> Value {
    // Never interpolate model-provided strings into executable JS.
    json!({"method": method, "args": args})
}

fn normalize(mut response: Value) -> Value {
    if let Some(mut result) = response.get("result").filter(|v| v.is_object()).cloned() {
        if let Some(channel) = response.get("channel") { result["channel"] = channel.clone(); }
        return result;
    }
    if !response.is_object() { return json!({"error":"页面操作库未返回对象，请更新扩展并重新加载页面"}); }
    if response.get("error").is_none() {
        response["error"] = json!("页面操作库未返回有效结果，请更新扩展并重新加载页面");
    }
    response
}

/// 上传路径在桌面端解析与校验：相对路径落在应用 temp 目录，必须是已存在的文件。
fn resolve_upload_files(step: &mut Value, temp_dir: Option<&Path>) -> Result<()> {
    let Some(files) = step.get_mut("files") else { return Ok(()) };
    let Some(items) = files.as_array_mut() else { bail!("files 必须是路径数组") };
    if items.is_empty() || items.len() > 20 {
        bail!("files 需要 1–20 个文件");
    }
    for item in items.iter_mut() {
        let raw = item.as_str().ok_or_else(|| anyhow!("files 里的每一项必须是路径字符串"))?;
        let path = PathBuf::from(raw.trim());
        let path = if path.is_absolute() {
            path
        } else {
            temp_dir.ok_or_else(|| anyhow!("无法解析相对路径 {raw}"))?.join(path)
        };
        if !path.is_file() {
            bail!("上传文件不存在：{}", path.display());
        }
        *item = json!(path.to_string_lossy());
    }
    Ok(())
}

const TARGET_FIELDS: &[&str] = &["ref", "css", "role", "name", "label", "placeholder", "scope", "visual"];

/// 把模型常猜的定位字段换成支持的写法：text/aria → name，selector → css，id/testid → css。
/// 其余未知字段直接报错，避免被静默忽略后只剩部分条件、定位到错误或不唯一的元素。
fn normalize_target(step: &mut Value) -> Result<()> {
    let Some(target) = step.get_mut("target").and_then(Value::as_object_mut) else { return Ok(()) };
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let keys: Vec<String> = target.keys().cloned().collect();
    for key in keys {
        if TARGET_FIELDS.contains(&key.as_str()) { continue; }
        let value = target.remove(&key).unwrap_or(Value::Null);
        let text = value.as_str().map(str::trim).filter(|s| !s.is_empty());
        let (field, mapped) = match key.as_str() {
            "text" | "aria" | "aria_label" | "aria-label" | "ariaLabel" | "title" => ("name", text.map(|s| json!(s))),
            "selector" => ("css", text.map(|s| json!(s))),
            "id" => ("css", text.map(|s| json!(format!("[id=\"{}\"]", quote(s))))),
            "testid" | "test_id" | "data-testid" | "data_testid" => ("css", text.map(|s| json!(format!("[data-testid=\"{}\"]", quote(s))))),
            _ => bail!("target 不支持字段 {key}；可用 ref/css/role/name/label/placeholder/scope/visual（按可见文字或 aria 名称定位用 name，按 id 用 css）"),
        };
        let Some(mapped) = mapped else { bail!("target.{key} 必须是非空字符串") };
        match target.get(field) {
            Some(existing) if existing != &mapped => bail!("target.{key} 与 target.{field} 冲突，只保留一个"),
            _ => { target.insert(field.into(), mapped); }
        }
    }
    // 模型偶尔把 ref 写成字符串 "7"
    if let Some(n) = target.get("ref").and_then(Value::as_str).and_then(|s| s.trim().parse::<u64>().ok()) {
        target.insert("ref".into(), json!(n));
    }
    Ok(())
}

/// browser_action 参数检查：单步需要 action，多步需要非空 steps；上传路径、定位字段就地规范化。
fn prepare_action_args(args: &Value, temp_dir: Option<&Path>) -> Result<(Value, u64)> {
    let mut args = args.clone();
    if let Some(steps) = args.get_mut("steps").and_then(Value::as_array_mut) {
        if steps.is_empty() || steps.len() > 20 {
            bail!("steps 需要 1–20 步");
        }
        for step in steps.iter_mut() {
            if step.get("action").and_then(Value::as_str).is_none() {
                bail!("steps 里的每一步都需要 action");
            }
            resolve_upload_files(step, temp_dir)?;
            normalize_target(step)?;
        }
        let secs = (20 + steps.len() as u64 * 15).min(300);
        return Ok((args, secs));
    }
    if args.get("action").and_then(Value::as_str).is_none() {
        bail!("需要 action，或用 steps 提交多步");
    }
    resolve_upload_files(&mut args, temp_dir)?;
    normalize_target(&mut args)?;
    Ok((args, 45))
}

// ---- 按画面定位（target.visual）----
// 页面结构里找不到目标时（纯图标按钮、画布、无文字控件）：截取视口 → Jev Box 找位置、
// 同一请求的是非题确认目标存在 → 框中心换算成视口坐标 → 引擎按坐标发真实鼠标事件。

const VISUAL_MIN_PRESENT: f64 = 0.6;
const VISUAL_MIN_BOX_CONFIDENCE: f64 = 0.3;

fn visual_desc(step: &Value) -> Option<&str> {
    step.pointer("/target/visual").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn uses_visual(args: &Value) -> bool {
    visual_desc(args).is_some()
        || args.get("steps").and_then(Value::as_array).is_some_and(|steps| steps.iter().any(|s| visual_desc(s).is_some()))
}

/// 是否采信 Jev 的定位：目标必须被判定为存在，框的置信度不能太低。
fn judge_visual(info: Value, present: f64, box_confidence: Option<f64>) -> std::result::Result<Value, Value> {
    let desc = info["description"].as_str().unwrap_or("目标").to_string();
    if present < VISUAL_MIN_PRESENT {
        return Err(json!({"error": format!("当前可见区域的截图里没有找到「{desc}」，未操作；目标可能在屏幕外（先滚动），或描述与外观不符"), "code": "visual_not_found", "located": info}));
    }
    if box_confidence.is_some_and(|c| c < VISUAL_MIN_BOX_CONFIDENCE) {
        return Err(json!({"error": format!("「{desc}」的位置不确定，未操作；换更具体的描述或先滚动到目标附近"), "code": "visual_uncertain", "located": info}));
    }
    Ok(info)
}

async fn call_action(app: &AppHandle, args: Value, secs: u64) -> Result<Value> {
    let state = app.state::<crate::AppState>();
    let mut payload = evaluation("action", &args);
    payload["__timeout_secs"] = json!(secs);
    state.browser.call("page_operation", payload).await.map(normalize).map(explain_engine_only)
}

/// 截取视口 PNG；返回 base64 与视口 CSS 宽高。高像素比屏幕上实际尺寸可能超限，按 PNG 头校正后重截一次。
async fn capture_viewport(app: &AppHandle) -> Result<(String, f64, f64)> {
    use base64::Engine as _;
    let state = app.state::<crate::AppState>();
    let mut max_side = 1500.0_f64;
    for _ in 0..2 {
        let shot = state
            .browser
            .call("page_operation", json!({"method": "capture", "args": {"max_side": max_side}}))
            .await
            .map(normalize)
            .map_err(|e| anyhow!("按画面定位需要扩展调试通道截图：{e}"))?;
        if let Some(error) = shot.get("error").and_then(Value::as_str) {
            bail!("截图失败：{error}");
        }
        let data = shot.get("data").and_then(Value::as_str).ok_or_else(|| anyhow!("截图没有返回图像"))?;
        let width = shot.get("width").and_then(Value::as_f64).ok_or_else(|| anyhow!("截图缺少视口宽度"))?;
        let height = shot.get("height").and_then(Value::as_f64).ok_or_else(|| anyhow!("截图缺少视口高度"))?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
        let (w, h) = crate::system_one::png_size(&bytes).ok_or_else(|| anyhow!("截图不是有效的 PNG"))?;
        if crate::system_one::image_size_ok(w, h) {
            return Ok((data.to_string(), width, height));
        }
        let longest = w.max(h) as f64;
        if longest <= 1568.0 {
            bail!("浏览器视口太小（截图 {w}×{h}），每边至少 280 像素");
        }
        max_side = (max_side * 1500.0 / longest).floor();
    }
    bail!("截图尺寸无法调整到 Jev 要求的 280–1568 像素")
}

/// Ok(Ok(定位信息)) 可以操作；Ok(Err(失败说明)) 未找到或不确定，直接返回给模型。
async fn locate_visual(app: &AppHandle, desc: &str) -> Result<std::result::Result<Value, Value>> {
    let state = app.state::<crate::AppState>();
    let settings = crate::commands::load_settings(&state.db);
    if !crate::system_one::configured(&settings) {
        return Ok(Err(json!({"error": "按画面定位需要在设置 → 智能加速里启用并配置 TypeSafe（Jev）", "code": "visual_unavailable"})));
    }
    let (png, width, height) = capture_viewport(app).await?;
    let http = reqwest::Client::new();
    let found = crate::system_one::locate_in_screenshot(Some(app), &http, &settings, &png, desc).await?;
    let (x, y) = found.bbox.center_in(width, height);
    let round = |v: f64| (v * 1000.0).round() / 1000.0;
    let info = json!({
        "description": desc,
        "present": round(found.present),
        "box": {"top": round(found.bbox.top), "bottom": round(found.bbox.bottom), "left": round(found.bbox.left), "right": round(found.bbox.right)},
        "box_confidence": found.bbox.confidence.map(round),
        "point": {"x": x, "y": y},
        "model": found.model,
    });
    Ok(judge_visual(info, found.present, found.bbox.confidence))
}

async fn run_visual_step(app: &AppHandle, mut step: Value) -> Result<Value> {
    let desc = visual_desc(&step).unwrap_or_default().to_string();
    let located = match locate_visual(app, &desc).await? {
        Ok(info) => info,
        Err(failure) => return Ok(failure),
    };
    if step.get("action").and_then(Value::as_str) == Some("locate") {
        return Ok(json!({"located": located}));
    }
    step["point"] = located["point"].clone();
    if let Some(obj) = step.as_object_mut() {
        obj.remove("target");
    }
    let mut result = call_action(app, step, 45).await?;
    result["located"] = located;
    Ok(result)
}

/// 含画面定位的多步：逐步执行（每个画面定位都要在前一步之后重新截图），遇到失败即停。
async fn run_with_visual(app: &AppHandle, args: Value) -> Result<Value> {
    let Some(steps) = args.get("steps").and_then(Value::as_array).cloned() else {
        return run_visual_step(app, args).await;
    };
    let total = steps.len();
    let mut results = Vec::with_capacity(total);
    for (i, step) in steps.into_iter().enumerate() {
        let result = if visual_desc(&step).is_some() {
            run_visual_step(app, step).await?
        } else {
            call_action(app, step, 45).await?
        };
        let error = result.get("error").and_then(Value::as_str).map(str::to_string);
        let dialog = result.pointer("/effect/native_dialog").is_some();
        results.push(result);
        if let Some(error) = error {
            return Ok(json!({"ok": false, "completed": i, "total": total, "stopped_at": i, "error": format!("第 {} 步失败：{error}", i + 1), "steps": results}));
        }
        if dialog {
            return Ok(json!({"ok": false, "completed": i + 1, "total": total, "stopped_at": i + 1, "error": "页面弹出了原生对话框", "steps": results}));
        }
    }
    Ok(json!({"ok": true, "completed": total, "total": total, "steps": results}))
}

/// 独立 CDP 通道没有真实输入引擎；把底层「未知 action」翻译成可行动的说明。
fn explain_engine_only(mut result: Value) -> Value {
    let unsupported = result.get("channel").and_then(Value::as_str) == Some("cdp")
        && result.get("error").and_then(Value::as_str).is_some_and(|e| e.contains("未知 action"));
    if unsupported {
        result["error"] = json!("该动作需要拾光浏览器扩展的真实输入引擎；当前走的是独立 CDP 通道。请确认扩展已连接后重试，或拆成 click/fill 单步。");
        result["code"] = json!("engine_unavailable");
    }
    result
}

pub async fn execute(app: &AppHandle, name: &str, args: &Value, cancel: &CancellationToken) -> Result<Value> {
    let method = match name {
        "browser_capabilities" => "capabilities", "browser_action" => "action",
        "browser_wait" => "probe", "browser_extract" => "extract",
        _ => bail!("未知页面操作"),
    };
    let state = app.state::<crate::AppState>();
    if name == "browser_action" {
        let temp_dir = crate::tempfs::temp_dir(app);
        let (args, secs) = prepare_action_args(args, Some(&temp_dir))?;
        let run = async {
            if uses_visual(&args) {
                run_with_visual(app, args).await
            } else {
                call_action(app, args, secs).await
            }
        };
        return tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("已中断"),
            result = run => result,
        };
    }
    if name != "browser_wait" {
        return tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("已中断"),
            result = state.browser.call("page_operation", evaluation(method, args)) => result.map(normalize),
        };
    }
    let mut args = args.clone();
    normalize_target(&mut args)?;
    let args = &args;
    let millis = args.get("timeout_ms").and_then(Value::as_u64).unwrap_or(5000).min(15000);
    wait_for_condition(millis, cancel, || async {
        state.browser.call("page_operation", evaluation(method, args)).await.map(normalize)
    }).await
}

async fn wait_for_condition<F, Fut>(millis: u64, cancel: &CancellationToken, mut observe: F) -> Result<Value>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    let started = tokio::time::Instant::now();
    // Zero means one observation, allowing at most one second for transport.
    let deadline = started + std::time::Duration::from_millis(if millis == 0 { 1000 } else { millis });
    loop {
        let observed = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("已中断"),
            result = tokio::time::timeout_at(deadline, observe()) => {
                match result {
                    Ok(value) => value?,
                    Err(_) => return Ok(json!({"error":"等待超时", "code":"timeout", "matched":false, "elapsed_ms":started.elapsed().as_millis()})),
                }
            }
        };
        let probe = &observed;
        if probe.get("error").is_some() || probe.get("matched").and_then(Value::as_bool) == Some(true) {
            return Ok(observed);
        }
        if !probe.get("matched").is_some_and(Value::is_boolean) {
            bail!("页面未返回有效等待状态，请确认扩展已更新或浏览器通道可用");
        }
        if millis == 0 || tokio::time::Instant::now() >= deadline {
            return Ok(json!({"error":"等待超时", "code":"timeout", "matched":false, "last_observation":observed, "elapsed_ms":started.elapsed().as_millis()}));
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("已中断"),
            _ = tokio::time::sleep_until((tokio::time::Instant::now() + std::time::Duration::from_millis(250)).min(deadline)) => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_are_data_not_javascript() {
        let args = json!({"text":"'); window.bad = true; //"});
        let payload = evaluation("action", &args);
        assert_eq!(payload["method"], "action");
        assert!(payload.get("expression").is_none());
        assert_eq!(payload["args"], args);
    }
    #[test]
    fn page_failures_remain_visible_to_tool_status() {
        let response = normalize(json!({"channel":"extension-debugger", "result":{"error":"ambiguous", "count":2}}));
        assert_eq!(response["error"], "ambiguous");
        assert_eq!(response["channel"], "extension-debugger");
        assert!(normalize(json!({"result":null})).get("error").is_some());
    }
    #[tokio::test]
    async fn wait_cancels_even_when_transport_stalls() {
        let cancel = CancellationToken::new();
        let signal = cancel.clone();
        let wait = wait_for_condition(15000, &cancel, || std::future::pending::<Result<Value>>());
        let trigger = async { tokio::time::sleep(std::time::Duration::from_millis(10)).await; signal.cancel(); };
        let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(1), async { tokio::join!(wait, trigger) }).await.unwrap();
        assert!(result.unwrap_err().to_string().contains("已中断"));
    }
    #[tokio::test]
    async fn wait_deadline_bounds_stalled_transport() {
        let result = wait_for_condition(10, &CancellationToken::new(), || std::future::pending::<Result<Value>>()).await.unwrap();
        assert_eq!(result["code"], "timeout");
    }
    #[tokio::test]
    async fn wait_does_not_retry_ambiguity_or_zero_timeout() {
        let calls = std::cell::Cell::new(0);
        let result = wait_for_condition(5000, &CancellationToken::new(), || {
            calls.set(calls.get() + 1);
            std::future::ready(Ok(json!({"error":"ambiguous"})))
        }).await.unwrap();
        assert_eq!(result["error"], "ambiguous");
        assert_eq!(calls.get(), 1);
        let result = wait_for_condition(0, &CancellationToken::new(), || std::future::ready(Ok(json!({"matched":false})))).await.unwrap();
        assert_eq!(result["code"], "timeout");
    }
    #[tokio::test]
    async fn wait_reobserves_until_condition_matches() {
        let calls = std::cell::Cell::new(0);
        let result = wait_for_condition(2000, &CancellationToken::new(), || {
            calls.set(calls.get() + 1);
            std::future::ready(Ok(json!({"matched":calls.get() == 2})))
        }).await.unwrap();
        assert_eq!(result["matched"], true);
        assert_eq!(calls.get(), 2);
    }
    #[test]
    fn action_args_require_action_or_steps_and_scale_timeout() {
        assert!(prepare_action_args(&json!({}), None).is_err());
        assert!(prepare_action_args(&json!({"steps": []}), None).is_err());
        assert!(prepare_action_args(&json!({"steps": [{"target": {"ref": 1}}]}), None).is_err());
        let (_, single) = prepare_action_args(&json!({"action": "click", "target": {"ref": 1}}), None).unwrap();
        let steps: Vec<Value> = (0..4).map(|_| json!({"action": "click", "target": {"ref": 1}})).collect();
        let (_, many) = prepare_action_args(&json!({"steps": steps}), None).unwrap();
        assert!(many > single);
        let too_many: Vec<Value> = (0..21).map(|_| json!({"action": "press", "key": "Tab"})).collect();
        assert!(prepare_action_args(&json!({"steps": too_many}), None).is_err());
    }
    #[test]
    fn upload_paths_are_resolved_and_must_exist() {
        let dir = std::env::temp_dir().join(format!("shiguang-upload-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        let (args, _) = prepare_action_args(&json!({"action": "upload", "target": {"ref": 1}, "files": ["a.png"]}), Some(&dir)).unwrap();
        assert_eq!(args["files"][0], json!(dir.join("a.png").to_string_lossy()));
        let missing = prepare_action_args(&json!({"steps": [{"action": "upload", "files": ["nope.png"]}]}), Some(&dir));
        assert!(missing.unwrap_err().to_string().contains("不存在"));
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn target_aliases_are_mapped_and_unknown_fields_rejected() {
        let (args, _) = prepare_action_args(&json!({"action":"click","target":{"text":"完全匹配","role":"option"}}), None).unwrap();
        assert_eq!(args["target"], json!({"name":"完全匹配","role":"option"}));
        let (args, _) = prepare_action_args(&json!({"action":"click","target":{"aria":"打开维度选择器"}}), None).unwrap();
        assert_eq!(args["target"], json!({"name":"打开维度选择器"}));
        let (args, _) = prepare_action_args(&json!({"steps":[{"action":"fill","target":{"id":"display\"Name"},"text":"x"}]}), None).unwrap();
        assert_eq!(args["steps"][0]["target"], json!({"css":"[id=\"display\\\"Name\"]"}));
        let (args, _) = prepare_action_args(&json!({"action":"click","target":{"ref":"7"}}), None).unwrap();
        assert_eq!(args["target"], json!({"ref":7}));
        assert!(prepare_action_args(&json!({"action":"click","target":{"xpath":"//a"}}), None).is_err());
        assert!(prepare_action_args(&json!({"action":"click","target":{"id":"a","css":"#b"}}), None).is_err());
        assert!(prepare_action_args(&json!({"action":"click","target":{"text":""}}), None).is_err());
    }

    #[test]
    fn visual_targets_are_detected_in_single_and_multi_step_actions() {
        assert!(uses_visual(&json!({"action":"click","target":{"visual":"发布按钮"}})));
        assert!(uses_visual(&json!({"steps":[{"action":"click","target":{"ref":1}},{"action":"click","target":{"visual":"相机图标"}}]})));
        assert!(!uses_visual(&json!({"action":"click","target":{"visual":"  "}})));
        assert!(!uses_visual(&json!({"steps":[{"action":"click","target":{"ref":1}}]})));
    }
    #[test]
    fn visual_location_requires_presence_and_a_confident_box() {
        let info = json!({"description":"发布按钮","point":{"x":10,"y":20}});
        assert!(judge_visual(info.clone(), 0.9, Some(0.8)).is_ok());
        assert!(judge_visual(info.clone(), 0.9, None).is_ok());
        assert_eq!(judge_visual(info.clone(), 0.4, Some(0.9)).unwrap_err()["code"], "visual_not_found");
        assert_eq!(judge_visual(info, 0.95, Some(0.1)).unwrap_err()["code"], "visual_uncertain");
    }
    #[test]
    fn cdp_channel_explains_engine_only_actions() {
        let r = explain_engine_only(json!({"channel":"cdp","error":"未知 action","code":"invalid_request"}));
        assert_eq!(r["code"], "engine_unavailable");
        let ok = explain_engine_only(json!({"channel":"extension-debugger","error":"未知 action"}));
        assert_eq!(ok["error"], "未知 action");
    }
    #[test]
    fn all_operations_have_structured_contracts() {
        let defs = definitions();
        for name in TOOL_NAMES {
            assert!(defs.iter().any(|d| d["function"]["name"] == *name));
        }
        assert_eq!(defs.len(), TOOL_NAMES.len());
    }
}
