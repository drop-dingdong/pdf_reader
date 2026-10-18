//! 光栅化适配：**整页**与**瓦片**（D-02 / D-94）。
//!
//! 瓦片渲染走 PDFium 原生的"部分渲染"能力：
//! 把目标位图设为瓦片尺寸，再用 `PdfRenderConfig::set_origin(-x, -y)` 把页面左上角推到
//! 位图之外，于是只有目标区域被真正渲染 —— 而不是"整页渲染后裁剪"。
//!
//! ## 几何必须只有一套
//! 块的像素矩形与整页位图尺寸都由 [`TileGrid`] 给出：网格把"旋转后页尺寸 × 渲染比例"
//! 一次取整成**共享边界**，因此相邻块严格相接（无 1px 缝、也不会重叠）。
//! 这里**不得**自己 `round` 一遍尺寸 —— 两侧各取整一次就可能差 1px，接缝随之错位。

use std::sync::Arc;

use pdfcore::prelude::*;
use pdfium_render::prelude::{PdfBitmap, PdfBitmapFormat, PdfRenderConfig};

use crate::document::{to_pdfium_rotation, SharedDocument};
use crate::error::to_core_error;

/// [`PageRaster`] 的 PDFium 实现。
pub struct PdfiumRaster(pub(crate) Arc<SharedDocument>);

impl PageRaster for PdfiumRaster {
    fn render(&self, request: &RenderRequest) -> Result<Arc<RasterImage>> {
        if request.cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }

        let page_count = self.0.geometries.len() as u32;
        let geometry = self
            .0
            .geometries
            .get(request.page.get() as usize)
            .copied()
            .ok_or(CoreError::PageOutOfRange { page: request.page, page_count })?;
        let page = self.0.page(request.page)?;

        // 该页在"请求的最终旋转 + 渲染比例"下的分块网格。整页位图尺寸**取自网格**
        // （`render_px`），而不是在这里再算一次 —— 见文件头的"几何必须只有一套"。
        let grid = TileGrid::new(geometry.size.rotated(request.rotation), request.scale.0);
        let (page_width, page_height) = grid.render_px();
        let page_px = (page_width.max(1) as i32, page_height.max(1) as i32);

        let (bitmap_size, origin) = match request.tile {
            None => (page_px, (0, 0)),
            Some(tile) => {
                let (x, y, width, height) = grid.tile_px(tile.col, tile.row);
                if width == 0 || height == 0 {
                    return Err(CoreError::Internal(format!(
                        "瓦片越界：col={} row={}（网格 {}×{}）",
                        tile.col, tile.row, grid.side, grid.side
                    )));
                }
                // set_origin 的语义是"页面左上角在目标位图中的位置"，
                // 取负值即可让目标区域正好落在位图的 (0,0)。
                ((width as i32, height as i32), (-(x as i32), -(y as i32)))
            }
        };

        let mut bitmap = PdfBitmap::empty(bitmap_size.0, bitmap_size.1, PdfBitmapFormat::BGRA)
            .map_err(to_core_error)?;

        // 旋转由本适配层全权负责（`automatic_orientation = false`）：
        // 位图方向因此严格等于 `request.rotation`，与 `PagePlacement` 的约定一一对应。
        let config = PdfRenderConfig::new()
            .set_fixed_size(page_px.0, page_px.1)
            .set_origin(origin.0, origin.1)
            .rotate(to_pdfium_rotation(request.rotation), false);

        page.render_into_bitmap_with_config(&mut bitmap, &config).map_err(to_core_error)?;

        // 瓦片诊断：拼合错位时用它对照"输入像素矩形 / 实际位图尺寸 / PDFium 原点 / 比例"
        if std::env::var_os("PDF_READER_DIAGNOSE").is_some() {
            if let Some(tile) = request.tile {
                let (x, y, width, height) = grid.tile_px(tile.col, tile.row);
                eprintln!(
                    "[瓦片] 页={} col={} row={} 网格={} 全页px={}x{} 瓦片={:?} 位图={}x{} 原点={} 比例={:.4}",
                    request.page.get(),
                    tile.col,
                    tile.row,
                    grid.side,
                    page_px.0,
                    page_px.1,
                    (x, y, width, height),
                    bitmap_size.0,
                    bitmap_size.1,
                    origin.1,
                    request.scale.0
                );
            }
        }

        let width = bitmap.width() as u32;
        let height = bitmap.height() as u32;
        // PDFium 原生输出 BGRA；BGRA→RGBA 的转换统一由 pdfcore 的
        // `RasterImage::to_rgba8()` 负责（全项目唯一的格式转换点）。
        let data: Arc<[u8]> = Arc::from(bitmap.as_raw_bytes().into_boxed_slice());
        let image = RasterImage::new(width, height, PixelFormat::Bgra8, data)
            .ok_or_else(|| CoreError::Internal("位图字节数与宽高不符".to_string()))?;
        Ok(Arc::new(image))
    }
}
