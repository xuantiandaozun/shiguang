#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::{anyhow, Context, Result};
use shiguang_lib::ntfs_helper::{
    execute_request_in_dir, response_path, validate_request_path, HelperRequest,
};
use std::path::{Path, PathBuf};

fn main() {
    if let Err(error) = run() {
        // The helper has no console in production. A best-effort sibling error
        // response is written when the request path was valid enough to derive it.
        if let Some(path) = request_arg().and_then(|path| error_response_path(&path).ok()) {
            let response = serde_json::json!({
                "version": shiguang_lib::ntfs_helper::PROTOCOL_VERSION,
                "request_id": request_id_from_path(&path).unwrap_or_default(),
                "result": null,
                "error": error.to_string(),
            });
            let _ = write_atomic(&path, &serde_json::to_vec(&response).unwrap_or_default());
        }
    }
}

fn run() -> Result<()> {
    let request_file = request_arg().ok_or_else(|| anyhow!("缺少 --request 参数"))?;
    let (dir, request_id) = validate_request_path(&request_file)?;
    let bytes = std::fs::read(&request_file).context("读取 helper 请求失败")?;
    let request: HelperRequest = serde_json::from_slice(&bytes).context("解析 helper 请求失败")?;
    if request.request_id != request_id {
        return Err(anyhow!("请求正文 ID 与文件名不一致"));
    }
    let response = execute_request_in_dir(request, Some(&dir));
    let target = response_path(&dir, &request_id);
    write_atomic(&target, &serde_json::to_vec(&response)?)
}

fn request_arg() -> Option<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--request" {
            return args.next().map(PathBuf::from);
        }
    }
    None
}

fn error_response_path(request: &Path) -> Result<PathBuf> {
    let (dir, id) = validate_request_path(request)?;
    Ok(response_path(&dir, &id))
}

fn request_id_from_path(path: &Path) -> Option<String> {
    path.file_name()?
        .to_str()?
        .strip_prefix("response-")?
        .strip_suffix(".json")
        .map(str::to_string)
}

fn write_atomic(target: &Path, bytes: &[u8]) -> Result<()> {
    let temp = target.with_extension("json.tmp");
    std::fs::write(&temp, bytes)?;
    std::fs::rename(temp, target)?;
    Ok(())
}
