//! 后端工厂：[`PdfBackend`] 的 PDFium 实现。

use std::sync::Arc;

use pdfcore::prelude::*;

use crate::document::{PdfiumDocument, SharedDocument};
use crate::raster::PdfiumRaster;
use crate::text::PdfiumTextLayer;

/// PDFium 后端。
///
/// 该类型无状态（每次 `open` 都从进程级 [`crate::pdfium`] 单例取绑定），
/// 因此可以安全地在多线程间共享，也可以放进 `Vec<Arc<dyn PdfBackend>>` 做多后端回退。
#[derive(Debug, Default, Clone, Copy)]
pub struct PdfiumBackend;

impl PdfiumBackend {
    /// 构造。
    pub fn new() -> Self {
        Self
    }
}

impl PdfBackend for PdfiumBackend {
    fn name(&self) -> &'static str {
        "pdfium"
    }

    fn capabilities(&self) -> Capabilities {
        // PDFium 全能力；`max_concurrency = 1` 由 `Capabilities::full()` 保证
        // （PDFium 不是线程安全的，调度器据此只起一个渲染线程）。
        Capabilities::full()
    }

    fn open(&self, source: Source<'_>, password: Option<&str>) -> Result<OpenedDocument> {
        let pdfium = crate::library::pdfium()?;
        // 打开时一次性备好页几何表与导航索引（D-01 / D-08）
        let shared = Arc::new(SharedDocument::open(pdfium, source, password)?);

        Ok(OpenedDocument::new(
            Arc::new(PdfiumDocument(shared.clone())),
            Arc::new(PdfiumRaster(shared.clone())),
            // PDFium 总能提供文本层；扫描件只是没有字符，而不是没有文本层
            Some(Arc::new(PdfiumTextLayer(shared))),
            self.capabilities(),
        ))
    }
}
