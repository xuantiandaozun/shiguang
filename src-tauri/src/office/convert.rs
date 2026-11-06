//! 格式转换：调用本机的 Microsoft Office / WPS（COM 自动化）或 LibreOffice（无界面命令行）。
//! PDF 页面转图片用 Windows 自带的 Windows.Data.Pdf 渲染，不需要额外软件。

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Debug)]
enum App {
    Word,
    Excel,
    PowerPoint,
}

fn app_for(ext: &str) -> Option<App> {
    match ext {
        "doc" | "docx" | "docm" | "dot" | "dotx" | "rtf" | "odt" | "wps" | "txt" | "htm" | "html" | "mht" | "xml" => Some(App::Word),
        "xls" | "xlsx" | "xlsm" | "xlsb" | "csv" | "ods" | "et" => Some(App::Excel),
        "ppt" | "pptx" | "pptm" | "pps" | "ppsx" | "odp" | "dps" => Some(App::PowerPoint),
        _ => None,
    }
}

/// 目标扩展名 → Office 的保存格式代码（WPS 兼容同一套）
fn format_code(app: App, ext: &str) -> Option<i32> {
    match (app, ext) {
        (App::Word, "pdf") => Some(17),
        (App::Word, "docx") => Some(16),
        (App::Word, "doc") => Some(0),
        (App::Word, "rtf") => Some(6),
        (App::Word, "txt") => Some(7),
        (App::Word, "html" | "htm") => Some(10),
        (App::Word, "odt") => Some(23),
        (App::Word, "xps") => Some(18),
        (App::Excel, "pdf") => Some(-1),
        (App::Excel, "xlsx") => Some(51),
        (App::Excel, "xls") => Some(56),
        (App::Excel, "csv") => Some(62),
        (App::Excel, "ods") => Some(60),
        (App::Excel, "html" | "htm") => Some(44),
        (App::PowerPoint, "pdf") => Some(32),
        (App::PowerPoint, "pptx") => Some(24),
        (App::PowerPoint, "ppt") => Some(1),
        (App::PowerPoint, "png") => Some(18),
        (App::PowerPoint, "jpg" | "jpeg") => Some(17),
        (App::PowerPoint, "odp") => Some(35),
        _ => None,
    }
}

fn prog_ids(app: App) -> [(&'static str, &'static str); 2] {
    match app {
        App::Word => [("Word.Application", "office"), ("KWPS.Application", "wps")],
        App::Excel => [("Excel.Application", "office"), ("KET.Application", "wps")],
        App::PowerPoint => [("PowerPoint.Application", "office"), ("KWPP.Application", "wps")],
    }
}

#[cfg(windows)]
fn progid_exists(id: &str) -> bool {
    use windows_sys::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, HKEY, HKEY_CLASSES_ROOT, KEY_READ};
    let key: Vec<u16> = format!("{id}\\CLSID").encode_utf16().chain(std::iter::once(0)).collect();
    let mut h: HKEY = std::ptr::null_mut();
    unsafe {
        let ok = RegOpenKeyExW(HKEY_CLASSES_ROOT, key.as_ptr(), 0, KEY_READ, &mut h) == 0;
        if ok {
            RegCloseKey(h);
        }
        ok
    }
}

#[cfg(not(windows))]
fn progid_exists(_: &str) -> bool {
    false
}

fn soffice() -> Option<PathBuf> {
    let mut cands = Vec::new();
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Ok(p) = std::env::var(var) {
            cands.push(PathBuf::from(&p).join("LibreOffice").join("program").join("soffice.exe"));
        }
    }
    cands.into_iter().find(|p| p.exists())
}

/// 本机可用的转换引擎
pub fn engines() -> Value {
    let mut v = json!({});
    for app in [App::Word, App::Excel, App::PowerPoint] {
        for (id, kind) in prog_ids(app) {
            if progid_exists(id) {
                v[format!("{kind}_{}", format!("{app:?}").to_lowercase())] = json!(true);
            }
        }
    }
    if let Some(p) = soffice() {
        v["libreoffice"] = json!(p.to_string_lossy());
    }
    v
}

const PS_SCRIPT: &str = r#"param([string]$Kind, [string]$ProgId, [string]$In, [string]$Out, [int]$Fmt)
$ErrorActionPreference = 'Stop'
$app = New-Object -ComObject $ProgId
# 可能连到用户正在用的实例：只有原本没打开任何文档时才在结束后退出
$pre = 0
try {
  switch ($Kind) {
    'Word' {
      $pre = $app.Documents.Count
      if ($pre -eq 0) { try { $app.Visible = $false } catch {} }
      try { $app.DisplayAlerts = 0 } catch {}
      $doc = $app.Documents.Open($In, $false, $true, $false)
      try { $doc.SaveAs2($Out, $Fmt) } catch { $doc.SaveAs($Out, $Fmt) }
      $doc.Close($false)
    }
    'Excel' {
      $pre = $app.Workbooks.Count
      if ($pre -eq 0) { try { $app.Visible = $false } catch {} }
      try { $app.DisplayAlerts = $false } catch {}
      $wb = $app.Workbooks.Open($In, 0, $true)
      if ($Fmt -lt 0) { $wb.ExportAsFixedFormat(0, $Out) } else { $wb.SaveAs($Out, $Fmt) }
      $wb.Close($false)
    }
    'PowerPoint' {
      $pre = $app.Presentations.Count
      $pres = $app.Presentations.Open($In, $true, $false, $false)
      $pres.SaveAs($Out, $Fmt)
      $pres.Close()
    }
  }
} finally {
  if ($pre -eq 0) { try { $app.Quit() } catch {} }
  [System.Runtime.InteropServices.Marshal]::ReleaseComObject($app) | Out-Null
}
"#;

fn run_with_timeout(mut cmd: Command, secs: u64) -> Result<(bool, String)> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out_file = std::env::temp_dir().join(format!("sg-convert-{}.log", uuid::Uuid::new_v4()));
    let log = std::fs::File::create(&out_file)?;
    cmd.stdout(log.try_clone()?).stderr(log);
    let mut child = cmd.spawn().context("启动转换程序失败")?;
    let deadline = Instant::now() + Duration::from_secs(secs);
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let bytes = std::fs::read(&out_file).unwrap_or_default();
    let _ = std::fs::remove_file(&out_file);
    let text = crate::reader::decode_text(&bytes);
    match status {
        Some(s) => Ok((s.success(), text)),
        None => bail!("转换超时（{secs} 秒），可能被软件的弹窗卡住：{}", text.trim()),
    }
}

fn via_com(kind: App, prog: &str, input: &Path, output: &Path, fmt: i32) -> Result<()> {
    let script = std::env::temp_dir().join("shiguang-office-convert.ps1");
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(PS_SCRIPT.as_bytes());
    std::fs::write(&script, bytes)?;
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-Kind")
        .arg(format!("{kind:?}"))
        .arg("-ProgId")
        .arg(prog)
        .arg("-In")
        .arg(input)
        .arg("-Out")
        .arg(output)
        .arg("-Fmt")
        .arg(fmt.to_string());
    let (ok, log) = run_with_timeout(cmd, 240)?;
    if !ok {
        bail!("{}", log.trim().lines().take(6).collect::<Vec<_>>().join(" "));
    }
    Ok(())
}

fn via_soffice(soffice: &Path, input: &Path, output: &Path, ext: &str) -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("sg-lo-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp)?;
    let mut cmd = Command::new(soffice);
    cmd.args(["--headless", "--norestore", "--convert-to", ext, "--outdir"]).arg(&tmp).arg(input);
    let (ok, log) = run_with_timeout(cmd, 240)?;
    let produced = std::fs::read_dir(&tmp)?.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.is_file());
    let result = match (ok, produced) {
        (true, Some(p)) => std::fs::copy(&p, output).map(|_| ()).context("复制转换结果失败"),
        _ => Err(anyhow!("LibreOffice 转换失败：{}", log.trim())),
    };
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// 转换；engine：auto / office / wps / libreoffice
pub fn convert(input: &Path, output: &Path, engine: &str) -> Result<Value> {
    let in_ext = input.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let out_ext = output.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if in_ext == "pdf" && matches!(out_ext.as_str(), "png" | "jpg" | "jpeg") {
        bail!("PDF 转图片请用 pdf_edit 的 to_images");
    }
    // PDF → Word：Word 能打开 PDF 并转换版式
    let app = if in_ext == "pdf" { Some(App::Word) } else { app_for(&in_ext) }.ok_or_else(|| anyhow!("不支持的源格式 .{in_ext}"))?;
    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tried = Vec::new();
    if let Some(code) = format_code(app, &out_ext) {
        for (prog, kind) in prog_ids(app) {
            if engine != "auto" && engine != kind {
                continue;
            }
            if !progid_exists(prog) {
                continue;
            }
            match via_com(app, prog, input, output, code) {
                Ok(()) if output.exists() => return Ok(json!({"ok": true, "engine": kind, "output": output.to_string_lossy().replace('\\', "/")})),
                Ok(()) => tried.push(format!("{kind}：没有生成文件")),
                Err(e) => tried.push(format!("{kind}：{e:#}")),
            }
        }
    }
    if engine == "auto" || engine == "libreoffice" {
        if let Some(lo) = soffice() {
            match via_soffice(&lo, input, output, &out_ext) {
                Ok(()) => return Ok(json!({"ok": true, "engine": "libreoffice", "output": output.to_string_lossy().replace('\\', "/")})),
                Err(e) => tried.push(format!("libreoffice：{e:#}")),
            }
        }
    }
    if tried.is_empty() {
        bail!(".{in_ext} → .{out_ext}：本机没有可用的转换软件（需要 Microsoft Office、WPS 或 LibreOffice 之一），或该组合不受支持");
    }
    bail!("转换失败：{}", tried.join("；"))
}

/// PDF 页面渲染成 PNG（Windows 自带 PDF 渲染）
#[cfg(windows)]
pub fn pdf_to_images(input: &Path, out_dir: &Path, pages: Option<&str>, width: u32) -> Result<Vec<String>> {
    use ::windows::core::HSTRING;
    use ::windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use ::windows::Storage::StorageFile;
    use ::windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
    unsafe {
        let _ = ::windows::Win32::System::Com::CoInitializeEx(None, ::windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    let abs = std::fs::canonicalize(input)?;
    let abs = abs.to_string_lossy().trim_start_matches(r"\\?\").to_string();
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(abs.as_str()))?.get().context("打开 PDF 失败")?;
    let doc = PdfDocument::LoadFromFileAsync(&file)?.get().context("加载 PDF 失败（可能已加密）")?;
    let total = doc.PageCount()?;
    let sel = match pages {
        Some(p) => super::pdf::parse_pages(p, total)?,
        None => (1..=total.min(50)).collect(),
    };
    std::fs::create_dir_all(out_dir)?;
    let stem = input.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let render = |p: u32, w: u32| -> Result<Vec<u8>> {
        let page = doc.GetPage(p - 1)?;
        let opts = PdfPageRenderOptions::new()?;
        opts.SetDestinationWidth(w)?;
        let stream = InMemoryRandomAccessStream::new()?;
        page.RenderWithOptionsToStreamAsync(&stream, &opts)?.get()?;
        let size = stream.Size()? as u32;
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
        reader.LoadAsync(size)?.get()?;
        let mut buf = vec![0u8; size as usize];
        reader.ReadBytes(&mut buf)?;
        Ok(buf)
    };
    // 渲染宽度按显示缩放计（如 150% 时实际像素会放大），第一页量出比例后校正
    let mut ask = width;
    let mut files = Vec::new();
    for (k, p) in sel.into_iter().enumerate() {
        let mut buf = render(p, ask)?;
        if k == 0 {
            if let Some((got, _)) = crate::system_one::png_size(&buf) {
                if got != width && got > 0 {
                    ask = ((width as f64) * (width as f64) / got as f64).round().max(1.0) as u32;
                    buf = render(p, ask)?;
                }
            }
        }
        let out = out_dir.join(format!("{stem}_p{p}.png"));
        std::fs::write(&out, &buf)?;
        files.push(out.to_string_lossy().replace('\\', "/"));
    }
    Ok(files)
}

#[cfg(not(windows))]
pub fn pdf_to_images(_: &Path, _: &Path, _: Option<&str>, _: u32) -> Result<Vec<String>> {
    bail!("PDF 转图片只支持 Windows")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_codes() {
        assert_eq!(format_code(App::Word, "pdf"), Some(17));
        assert_eq!(format_code(App::Excel, "pdf"), Some(-1));
        assert_eq!(format_code(App::PowerPoint, "png"), Some(18));
        assert_eq!(app_for("et"), Some(App::Excel));
        assert_eq!(format_code(App::Word, "xlsx"), None);
    }

    #[test]
    #[ignore]
    fn live_engines_and_pdf_render() {
        println!("引擎：{}", engines());
        let dir = std::env::temp_dir().join(format!("sg-conv-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("a.png");
        image::RgbImage::from_pixel(400, 300, image::Rgb([30, 120, 200])).save(&img).unwrap();
        let pdf = dir.join("a.pdf");
        super::super::pdf::images_to_pdf(&[img], &pdf).unwrap();
        let files = pdf_to_images(&pdf, &dir.join("png"), None, 800).unwrap();
        println!("{files:?}");
        assert_eq!(files.len(), 1);
        let w = image::image_dimensions(&files[0]).unwrap().0;
        assert!((798..=802).contains(&w), "宽度 {w}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 调试用：SG_PDF=路径 渲染成 PNG
    #[test]
    #[ignore]
    fn render_env_pdf() {
        let Ok(p) = std::env::var("SG_PDF") else { return };
        let p = std::path::PathBuf::from(p);
        let files = pdf_to_images(&p, &p.with_extension("pages"), Some("1"), 1200).unwrap();
        println!("{files:?}");
    }
}
