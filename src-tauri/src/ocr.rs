//! 本地 OCR：基于百度 PaddleOCR（PP-OCRv5 mobile）+ RapidOCR ONNX 管线。
//! 首次使用自动从 ModelScope 下载模型到应用数据目录（约 15MB），之后完全离线。

use anyhow::{anyhow, bail, Context, Result};
use rapidocr_core::{
    config::PipelineConfig,
    model::{model_set_by_name, ModelCache, ModelDownloadMode},
    RapidOcr,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MODEL_SET: &str = "ppocrv5-ch-mobile";

#[derive(Debug, Clone, Serialize)]
pub struct OcrLineOut {
    pub text: String,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct OcrResult {
    pub path: String,
    pub engine: String,
    pub line_count: usize,
    pub text: String,
    pub lines: Vec<OcrLineOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

pub struct OcrEngine {
    model_dir: PathBuf,
    /// ONNX 会话不可跨线程共享可变引用，用互斥锁串行化识别
    inner: Mutex<Option<RapidOcr>>,
}

impl OcrEngine {
    pub fn new(app_data_dir: &Path) -> Self {
        let model_dir = app_data_dir.join("ocr-models");
        Self {
            model_dir,
            inner: Mutex::new(None),
        }
    }

    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }

    /// 阻塞调用：必要时下载模型并识别图片。应在 `spawn_blocking` 中执行。
    pub fn recognize(&self, path: &Path) -> Result<OcrResult> {
        if !path.exists() {
            bail!("图片不存在: {}", path.display());
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" => {}
            "" => bail!("无法识别图片格式（无扩展名）"),
            other => bail!("不支持的图片格式: .{}（支持 png/jpg/jpeg/gif/bmp/webp/tif）", other),
        }

        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow!("OCR 引擎锁异常"))?;
        if guard.is_none() {
            log::info!("初始化本地 OCR（模型目录: {:?}）…", self.model_dir);
            *guard = Some(Self::load(&self.model_dir)?);
            log::info!("本地 OCR 就绪");
        }
        let ocr = guard.as_mut().unwrap();
        let output = ocr
            .run_path(path)
            .with_context(|| format!("OCR 识别失败: {}", path.display()))?;

        let lines: Vec<OcrLineOut> = output
            .lines
            .into_iter()
            .filter(|l| !l.text.trim().is_empty())
            .map(|l| OcrLineOut {
                text: l.text,
                score: l.score,
            })
            .collect();
        let text = lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let hint = if text.is_empty() {
            Some("未识别到文字。若图片模糊/倾斜可换更清晰截图；需要理解画面内容请改用 read_image。".into())
        } else {
            None
        };

        Ok(OcrResult {
            path: path.to_string_lossy().replace('\\', "/"),
            engine: format!("PaddleOCR {}", MODEL_SET),
            line_count: lines.len(),
            text,
            lines,
            hint,
        })
    }

    fn load(model_dir: &Path) -> Result<RapidOcr> {
        std::fs::create_dir_all(model_dir)
            .with_context(|| format!("创建 OCR 模型目录失败: {}", model_dir.display()))?;
        let model_set = model_set_by_name(MODEL_SET)
            .ok_or_else(|| anyhow!("未知 OCR 模型集: {}", MODEL_SET))?;
        // 完整管线：检测 + 方向分类 + 识别，适合截图里的横排中文
        let pipeline = PipelineConfig::full();
        let cache = ModelCache::new(model_dir);
        cache
            .ensure_model_set_for_pipeline(model_set, pipeline, ModelDownloadMode::Missing)
            .context(
                "下载/校验 OCR 模型失败（首次约 15MB，需能访问 modelscope.cn）。可检查网络后重试。",
            )?;
        let cfg = cache.config_for(model_set).with_pipeline(pipeline);
        RapidOcr::from_config(cfg).context("加载 OCR 模型失败")
    }
}
