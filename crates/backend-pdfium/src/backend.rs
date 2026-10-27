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
pub struct PdfiumBackend {
    /// 是否让 PDFium 把注释**烤进位图**。默认 `false`：应用有自己的标注叠层，
    /// 烤进去会双份绘制、而且删掉 overlay 注释后它还会从 PDF 里显示出来。
    /// 只有"验证写回外观"的测试/校验才显式打开（见 `with_annotations`）。
    render_annotations: bool,
}

impl PdfiumBackend {
    /// 构造：**不渲染** PDF 注释（应用叠层的语义）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 显式要求把 PDF 注释渲染进位图（验证注释外观时用）。
    pub fn with_annotations(mut self, render: bool) -> Self {
        self.render_annotations = render;
        self
    }

    /// 当前是否渲染注释。
    pub const fn renders_annotations(&self) -> bool {
        self.render_annotations
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
            Arc::new(PdfiumRaster {
                shared: shared.clone(),
                render_annotations: self.render_annotations,
            }),
            // PDFium 总能提供文本层；扫描件只是没有字符，而不是没有文本层
            Some(Arc::new(PdfiumTextLayer(shared))),
            self.capabilities(),
        ))
    }
}
