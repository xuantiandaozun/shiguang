//! 本机信息查询：硬件参数与实时运行状态。
//! 常规指标（OS/CPU/内存/磁盘/进程）走 sysinfo crate 原生读取；
//! GPU / 主板 / 电池等 sysinfo 覆盖不到的，用一次性 PowerShell CIM 查询
//! （输出强制 UTF-8 + JSON，避免中文系统下的编码与本地化解析问题），失败时降级为提示。

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sysinfo::{Disks, ProcessesToUpdate, System};

/// CREATE_NO_WINDOW：查询不弹黑色控制台窗口
const CREATE_NO_WINDOW: u32 = 0x08000000;
/// PowerShell CIM 查询的超时时间
const CIM_TIMEOUT_SECS: u64 = 20;

/// get_system_info 工具入口：category 决定返回哪一块
pub async fn query(category: &str, sort_by: &str, limit: usize) -> Result<Value> {
    match category {
        "overview" => overview().await,
        "cpu" => cpu_info(),
        "memory" => memory_info(true),
        "disk" => disk_info(),
        "gpu" => gpu_info().await,
        "battery" => battery_info().await,
        "process" => process_info(sort_by, limit),
        other => Err(anyhow!(
            "未知 category: {}（可选 overview/cpu/memory/disk/gpu/battery/process）",
            other
        )),
    }
}

fn gb(bytes: u64) -> f64 {
    (bytes as f64 / 1024.0 / 1024.0 / 1024.0 * 10.0).round() / 10.0
}

fn pct(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64 * 1000.0).round() / 10.0
    }
}

fn fmt_uptime(secs: u64) -> String {
    let d = secs / 86400;
    let h = secs % 86400 / 3600;
    let m = secs % 3600 / 60;
    if d > 0 {
        format!("{}天{}小时{}分", d, h, m)
    } else if h > 0 {
        format!("{}小时{}分", h, m)
    } else {
        format!("{}分钟", m)
    }
}

/// 采集 CPU/内存的快照。CPU 使用率需要两次采样取差值，中间隔一个最小采样间隔。
fn snapshot() -> System {
    let mut sys = System::new();
    sys.refresh_cpu_all();
    sys.refresh_memory();
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_all();
    sys.refresh_memory();
    sys
}

fn os_block() -> Value {
    json!({
        "os": format!("{} {}", System::name().unwrap_or_else(|| "Windows".into()),
                      System::os_version().unwrap_or_default()),
        "kernel": System::kernel_version().unwrap_or_default(),
        "host": System::host_name().unwrap_or_default(),
        "uptime": fmt_uptime(System::uptime()),
    })
}

fn cpu_brief(sys: &System) -> Value {
    let cpus = sys.cpus();
    let brand = cpus
        .first()
        .map(|c| c.brand().trim().to_string())
        .unwrap_or_default();
    let usage = cpus.iter().map(|c| c.cpu_usage() as f64).sum::<f64>() / cpus.len().max(1) as f64;
    json!({
        "brand": brand,
        "physical_cores": System::physical_core_count().unwrap_or(0),
        "logical_cores": cpus.len(),
        "frequency_mhz": cpus.first().map(|c| c.frequency()).unwrap_or(0),
        "usage_percent": (usage * 10.0).round() / 10.0,
    })
}

fn memory_brief(sys: &System) -> Value {
    json!({
        "total_gb": gb(sys.total_memory()),
        "used_gb": gb(sys.used_memory()),
        "available_gb": gb(sys.available_memory()),
        "used_percent": pct(sys.used_memory(), sys.total_memory()),
        "swap_total_gb": gb(sys.total_swap()),
        "swap_used_gb": gb(sys.used_swap()),
    })
}

fn disks_brief() -> Value {
    let disks = Disks::new_with_refreshed_list();
    Value::Array(
        disks
            .list()
            .iter()
            .map(|d| {
                let total = d.total_space();
                let avail = d.available_space();
                json!({
                    "mount": d.mount_point().to_string_lossy().replace('\\', "/"),
                    "name": d.name().to_string_lossy(),
                    "fs": d.file_system().to_string_lossy(),
                    "total_gb": gb(total),
                    "free_gb": gb(avail),
                    "used_percent": pct(total - avail, total),
                })
            })
            .collect(),
    )
}

async fn overview() -> Result<Value> {
    let sys = snapshot();
    let mut v = json!({
        "system": os_block(),
        "cpu": cpu_brief(&sys),
        "memory": memory_brief(&sys),
        "disks": disks_brief(),
    });
    // GPU 属附加信息，查不到不影响概览主体
    if let Ok(cim) = powershell_cim().await {
        if let Some(gpus) = cim.get("gpu") {
            v["gpu"] = gpus.clone();
        }
        if let Some(cs) = cim.get("computer") {
            v["computer"] = cs.clone();
        }
    }
    Ok(v)
}

fn cpu_info() -> Result<Value> {
    let sys = snapshot();
    let mut v = cpu_brief(&sys);
    v["system"] = os_block();
    v["per_core_usage_percent"] = Value::Array(
        sys.cpus()
            .iter()
            .map(|c| json!(((c.cpu_usage() as f64) * 10.0).round() / 10.0))
            .collect(),
    );
    Ok(v)
}

fn memory_info(with_top_processes: bool) -> Result<Value> {
    let mut sys = snapshot();
    let mut v = memory_brief(&sys);
    if with_top_processes {
        sys.refresh_processes(ProcessesToUpdate::All, true);
        v["top_memory_processes"] = top_processes(&sys, "memory", 10);
    }
    Ok(v)
}

fn disk_info() -> Result<Value> {
    Ok(json!({ "disks": disks_brief() }))
}

fn top_processes(sys: &System, sort_by: &str, limit: usize) -> Value {
    let mut procs: Vec<_> = sys.processes().values().collect();
    if sort_by == "cpu" {
        procs.sort_by(|a, b| {
            b.cpu_usage()
                .partial_cmp(&a.cpu_usage())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        procs.sort_by_key(|p| std::cmp::Reverse(p.memory()));
    }
    Value::Array(
        procs
            .into_iter()
            .take(limit)
            .map(|p| {
                json!({
                    "pid": p.pid().as_u32(),
                    "name": p.name().to_string_lossy(),
                    "memory_mb": (p.memory() as f64 / 1024.0 / 1024.0).round() as u64,
                    "cpu_percent": ((p.cpu_usage() as f64) * 10.0).round() / 10.0,
                })
            })
            .collect(),
    )
}

fn process_info(sort_by: &str, limit: usize) -> Result<Value> {
    let mut sys = snapshot();
    // 按 CPU 排序需要进程也有前后两次采样
    sys.refresh_processes(ProcessesToUpdate::All, true);
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let total_mem = sys.total_memory();
    Ok(json!({
        "sorted_by": if sort_by == "cpu" { "cpu" } else { "memory" },
        "memory_total_gb": gb(total_mem),
        "processes": top_processes(&sys, sort_by, limit),
    }))
}

async fn gpu_info() -> Result<Value> {
    let cim = powershell_cim().await?;
    Ok(json!({
        "gpu": cim.get("gpu").cloned().unwrap_or(Value::Null),
        "note": "adapter_ram 由 WMI 报告，部分核显/新卡数值不准确属正常现象",
    }))
}

async fn battery_info() -> Result<Value> {
    let cim = powershell_cim().await?;
    let bat = cim.get("battery").cloned().unwrap_or(Value::Array(vec![]));
    let empty = bat.as_array().map(|a| a.is_empty()).unwrap_or(true);
    if empty {
        Ok(json!({ "battery": Value::Null, "note": "未检测到电池（台式机或电池信息不可用）" }))
    } else {
        Ok(json!({ "battery": bat }))
    }
}

/// 一次性 PowerShell CIM 查询：整机型号 / GPU / 主板 / 电池。
/// 用 -EncodedCommand（UTF-16LE base64）传脚本，彻底绕开命令行转义与控制台编码问题。
async fn powershell_cim() -> Result<Value> {
    let script = r#"
[Console]::OutputEncoding=[System.Text.Encoding]::UTF8
$out = [ordered]@{}
try { $out.computer = Get-CimInstance Win32_ComputerSystem -ErrorAction Stop | Select-Object Manufacturer,Model,SystemType } catch { $out.computer = $null }
$out.gpu = @(Get-CimInstance Win32_VideoController | Select-Object Name,DriverVersion,CurrentHorizontalResolution,CurrentVerticalResolution)
try { $out.baseboard = Get-CimInstance Win32_BaseBoard -ErrorAction Stop | Select-Object Manufacturer,Product } catch { $out.baseboard = $null }
$out.battery = @(Get-CimInstance Win32_Battery | Select-Object Name,EstimatedChargeRemaining,BatteryStatus)
$out | ConvertTo-Json -Depth 4 -Compress
"#;
    use base64::Engine;
    let utf16: Vec<u8> = script
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);

    let mut cmd = tokio::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(target_os = "windows")]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let child = cmd
        .spawn()
        .map_err(|e| anyhow!("启动 PowerShell 失败: {}", e))?;
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(CIM_TIMEOUT_SECS),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| anyhow!("PowerShell CIM 查询超时"))?
    .map_err(|e| anyhow!("PowerShell CIM 查询失败: {}", e))?;
    if !out.status.success() {
        return Err(anyhow!("PowerShell 退出码异常: {:?}", out.status.code()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut v: Value = serde_json::from_str(text.trim()).map_err(|e| {
        anyhow!(
            "解析 CIM 结果失败: {} / 原文: {}",
            e,
            &text[..text.len().min(200)]
        )
    })?;
    normalize_array_field(&mut v, "gpu");
    normalize_array_field(&mut v, "battery");
    Ok(v)
}

/// PowerShell 单元素数组序列化时会退化成对象，统一还原成数组
fn normalize_array_field(v: &mut Value, key: &str) {
    if let Some(obj) = v.get(key).cloned() {
        if obj.is_object() {
            v[key] = Value::Array(vec![obj]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_formats() {
        assert_eq!(gb(16 * 1024 * 1024 * 1024), 16.0);
        assert_eq!(pct(1, 4), 25.0);
        assert_eq!(fmt_uptime(90061), "1天1小时1分");
        assert_eq!(fmt_uptime(120), "2分钟");
    }

    #[test]
    fn snapshot_works() {
        let sys = snapshot();
        assert!(sys.total_memory() > 0);
        assert!(!sys.cpus().is_empty());
    }
}
