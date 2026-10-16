//! 光栅化适配：**整页**与**瓦片**（D-02）。
//!
//! 瓦片渲染走 PDFium 原生的"部分渲染"能力：
//! 把目标位图设为瓦片尺寸，再用 `PdfRenderConfig::set_origin(-x, -y)` 把页面左上角推到
//! 位图之外，于是只有目标区域被真正渲染 —— 而不是"整页渲染后裁剪"。

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

        // 全页在"请求的最终旋转 + 缩放"下的像素尺寸（= 位图坐标系）
        let full = geometry.size.rotated(request.rotation).scale(request.scale.0);
        let full_px = (full.width.round().max(1.0) as i32, full.height.round().max(1.0) as i32);

        let (bitmap_size, origin) = match request.tile {
            None => (full_px, (0, 0)),
            Some(tile) => {
                let pixels = tile.pixel_rect(Size::new(full_px.0 as f32, full_px.1 as f32));
                if pixels.is_empty() {
                    return Err(CoreError::Internal(format!(
                        "瓦片越界：col={} row={}",
                        tile.col, tile.row
                    )));
                }
                let size = (
                    pixels.width.round().max(1.0) as i32,
                    pixels.height.round().max(1.0) as i32,
                );
                // set_origin 的语义是"页面左上角在目标位图中的位置"，
                // 取负值即可让目标区域正好落在位图的 (0,0)。
                (size, (-(pixels.x.round() as i32), -(pixels.y.round() as i32)))
            }
        };

        let mut bitmap = PdfBitmap::empty(bitmap_size.0, bitmap_size.1, PdfBitmapFormat::BGRA)
            .map_err(to_core_error)?;

        // 旋转由本适配层全权负责（`automatic_orientation = false`）：
        // 位图方向因此严格等于 `request.rotation`，与 `PagePlacement` 的约定一一对应。
        let config = PdfRenderConfig::new()
            .set_fixed_size(full_px.0, full_px.1)
            .set_origin(origin.0, origin.1)
            .rotate(to_pdfium_rotation(request.rotation), false);

        page.render_into_bitmap_with_config(&mut bitmap, &config).map_err(to_core_error)?;

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
