//! 光栅化适配：**整页**与**瓦片**（D-02 / D-94 / D-95）。
//!
//! 瓦片渲染走 PDFium 原生的"部分渲染"能力：
//! 把目标位图设为块尺寸，再用 `PdfRenderConfig::set_origin(-x, -y)` 把页面左上角推到
//! 位图之外，于是只有目标区域被真正渲染 —— 而不是"整页渲染后裁剪"。
//!
//! ## 请求形状：块矩形 + 期望像素尺寸（D-95）
//! 上层给出的是 [`RenderRegion::page_rect`]（**PDF 页面空间**的矩形，未旋转）与
//! [`RenderRegion::pixel_size`]（期望输出尺寸）。本适配层负责：
//! 1. 把页面空间矩形映射到设备像素（旋转 → 显示坐标 → × 比例）；
//! 2. **校验**映射结果是整像素、且尺寸与期望一致 —— 不一致返回 [`CoreError::Internal`]；
//! 3. 按整页位图尺寸 `set_fixed_size` + 负 `set_origin`，只渲染这块区域。
//!
//! 第 2 步是刻意的**硬失败**：几何推导不一致时静默拉伸的后果是"看起来有图，
//! 但同一行文字在接缝处错位"（D-84/D-86 的现象），比报错难查得多。

use std::sync::Arc;

use pdfcore::prelude::*;
use pdfium_render::prelude::{PdfBitmap, PdfBitmapFormat, PdfRenderConfig};

use crate::document::{to_pdfium_rotation, SharedDocument};
use crate::error::to_core_error;

/// [`PageRaster`] 的 PDFium 实现。
pub struct PdfiumRaster {
    pub(crate) shared: Arc<SharedDocument>,
    /// 是否把 PDF 注释渲染进位图；应用默认关闭（叠层自己画），测试可开。
    pub(crate) render_annotations: bool,
}

impl PageRaster for PdfiumRaster {
    fn render(&self, request: &RenderRequest) -> Result<Arc<RasterImage>> {
        if request.cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }

        let page_count = self.shared.geometries.len() as u32;
        let geometry = self
            .shared
            .geometries
            .get(request.page.get() as usize)
            .copied()
            .ok_or(CoreError::PageOutOfRange {
                page: request.page,
                page_count,
            })?;
        let page = self.shared.page(request.page)?;

        let scale = request.scale.0;
        if !(scale > 0.0) {
            return Err(CoreError::Internal(format!("非法渲染比例：{scale}")));
        }

        // 整页在"请求的最终旋转 + 渲染比例"下的像素尺寸（= 位图坐标系）。
        //
        // 这是 `set_fixed_size` 的入参：PDFium 会把整页映射到这个尺寸，块只是它的一块。
        // 该值与 `pdfcore::tiling::TileGrid::render_px` 是同一个表达式（round(旋转后尺寸 × 比例)），
        // 因此块的页内位置由上面的区域换算得出后，两者必须对齐 —— 否则下面的校验会报错。
        let displayed = geometry.size.rotated(request.rotation);
        let page_px = (
            (displayed.width * scale).round().max(1.0) as i32,
            (displayed.height * scale).round().max(1.0) as i32,
        );

        let (bitmap_size, origin) = match (request.tile, request.region) {
            (None, None) => (page_px, (0, 0)),
            (Some(tile), Some(region)) => {
                let (left, top, size) =
                    device_rect(region, geometry.size, request.rotation, scale)?;
                if !block_fits(left, top, size, page_px) {
                    return Err(CoreError::Internal(format!(
                        "块越界：col={} row={} 区域=({left},{top},{},{}) 整页px={page_px:?}",
                        tile.col, tile.row, size.0, size.1
                    )));
                }
                // set_origin 的语义是"页面左上角在目标位图中的位置"，
                // 取负值即可让目标区域正好落在位图的 (0,0)。
                (size, (-left, -top))
            }
            (Some(_), None) => {
                return Err(CoreError::Internal(
                    "瓦片请求缺少区域（块矩形 + 期望像素尺寸）".to_string(),
                ))
            }
            (None, Some(_)) => {
                return Err(CoreError::Internal(
                    "整页请求不应携带块区域（否则会渲染出尺寸不符的位图）".to_string(),
                ))
            }
        };

        let mut bitmap = PdfBitmap::empty(bitmap_size.0, bitmap_size.1, PdfBitmapFormat::BGRA)
            .map_err(to_core_error)?;

        // **只把"用户旋转"交给 PDFium**：`FPDF_RenderPageBitmap` 会自己应用页面自带的
        // `/Rotate`（见 pdfium-render 的注释：display transform 已包含 intrinsic rotation）。
        // `request.rotation` 是"页面 /Rotate ⊕ 用户旋转"的**最终**旋转，直接传会叠加两次：
        // /Rotate 90 的页会显示成 180°，而且目标位图按 90° 算、内容按 180° 画 → 被压扁。
        // 这里先减掉页面自带的旋转，剩下的才是要额外施加的旋转。
        let page_rotation = geometry.rotation;
        let extra_rotation = request.rotation.compose(page_rotation.inverse());
        //
        // 旋转由本适配层全权负责（`automatic_orientation = false`）：
        // 位图方向因此严格等于 `request.rotation`，与 `PagePlacement` 的约定一一对应。
        // **必须显式关闭**：`PdfRenderConfig::new()` 默认打开
        // `FPDF_REVERSE_BYTE_ORDER`（do_set_flag_reverse_byte_order = true），
        // 于是 PDFium 往 BGRA 位图里写的是 **RGBA** 字节 —— 我们再按 Bgra8 交换一次，
        // 红蓝就对调了（实测现象：黄色页显示成青色、蓝色字显示成红色）。
        // 关掉它，`PdfBitmapFormat::BGRA` 才真的是 B,G,R,A，与 `PixelFormat::Bgra8` 一致。
        let config = PdfRenderConfig::new()
            .render_annotations(self.render_annotations)
            .set_reverse_byte_order(false)
            .set_fixed_size(page_px.0, page_px.1)
            .set_origin(origin.0, origin.1)
            .rotate(to_pdfium_rotation(extra_rotation), false);

        page.render_into_bitmap_with_config(&mut bitmap, &config)
            .map_err(to_core_error)?;

        // 块诊断：拼合错位时用它对照"块的页面矩形 / 期望与实得尺寸 / PDFium 原点 / 比例"
        if std::env::var_os("PDF_READER_DIAGNOSE").is_some() {
            if let (Some(tile), Some(region)) = (request.tile, request.region) {
                eprintln!(
                    "[瓦片] 页={} col={} row={} 整页px={}x{} 区域pt=({:.2},{:.2},{:.2},{:.2}) 期望={}x{} 位图={}x{} 原点={} 比例={:.4}",
                    request.page.get(),
                    tile.col,
                    tile.row,
                    page_px.0,
                    page_px.1,
                    region.page_rect.x,
                    region.page_rect.y,
                    region.page_rect.width,
                    region.page_rect.height,
                    region.pixel_size.0,
                    region.pixel_size.1,
                    bitmap_size.0,
                    bitmap_size.1,
                    origin.1,
                    scale
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

/// 把"页面空间矩形 + 期望像素尺寸"换算成整页位图里的整数像素矩形 `(left, top, (w, h))`。
///
/// 换算链：页面空间 →（[`Rotation::map_point`]）→ 旋转后的显示坐标（point）→ × 比例 → 像素。
///
/// **严格校验**（D-95）：
/// * 结果必须落在整像素上（容差 0.01px）—— 上层的块边界由整数像素导出，偏移即说明
///   两侧几何不一致（会让内容整体平移、接缝错位）；
/// * 像素尺寸必须等于期望值 —— 不满足就报错，**绝不静默拉伸**。
fn device_rect(
    region: RenderRegion,
    page_size: Size,
    rotation: Rotation,
    scale: f32,
) -> Result<(i32, i32, (i32, i32))> {
    let rect = region.page_rect;
    if !(rect.width > 0.0) || !(rect.height > 0.0) {
        return Err(CoreError::Internal(format!("块区域为空：{rect:?}")));
    }
    // 四角映射后取外接矩形（90°/270° 会交换宽高）
    let corners = [
        Point::new(rect.x, rect.y),
        Point::new(rect.right(), rect.y),
        Point::new(rect.x, rect.bottom()),
        Point::new(rect.right(), rect.bottom()),
    ];
    let mut min = Point::new(f32::MAX, f32::MAX);
    let mut max = Point::new(f32::MIN, f32::MIN);
    for corner in corners {
        if corner.x < -1.0
            || corner.y < -1.0
            || corner.x > page_size.width + 1.0
            || corner.y > page_size.height + 1.0
        {
            return Err(CoreError::Internal(format!(
                "块区域超出页面：{rect:?}（页 {page_size:?}）"
            )));
        }
        let display = rotation.map_point(corner, page_size);
        let device = Point::new(display.x * scale, display.y * scale);
        min = Point::new(min.x.min(device.x), min.y.min(device.y));
        max = Point::new(max.x.max(device.x), max.y.max(device.y));
    }

    let (width, height) = (max.x - min.x, max.y - min.y);
    // 只允许亚像素级误差：上层给出的块边界是整数像素，这里必须落回整数
    for value in [min.x, min.y, width, height] {
        if !value.is_finite() || (value - value.round()).abs() > 0.01 {
            return Err(CoreError::Internal(format!(
                "块区域未落在整像素上：left={:.4} top={:.4} w={:.4} h={:.4}（比例 {scale}）",
                min.x, min.y, width, height
            )));
        }
    }
    let size = (width.round() as i32, height.round() as i32);
    if size != (region.pixel_size.0 as i32, region.pixel_size.1 as i32) {
        return Err(CoreError::Internal(format!(
            "块尺寸不符：推导 {:?} ≠ 期望 {:?}（区域 {:?}，比例 {scale}）",
            size, region.pixel_size, rect
        )));
    }
    Ok((min.x.round() as i32, min.y.round() as i32, size))
}

/// 块矩形必须落在整页位图内（否则 `set_origin` 会渲染出页外内容或空图）。
fn block_fits(left: i32, top: i32, size: (i32, i32), page_px: (i32, i32)) -> bool {
    left >= 0
        && top >= 0
        && left + size.0 <= page_px.0
        && top + size.1 <= page_px.1
        && size.0 > 0
        && size.1 > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(page_rect: Rect, pixel_size: (u32, u32)) -> RenderRegion {
        RenderRegion {
            page_rect,
            pixel_size,
        }
    }

    /// 不旋转：页面空间矩形直接按"y 翻转 × 比例"落到像素上。
    #[test]
    fn device_rect_maps_page_space_without_rotation() {
        let page = Size::new(612.0, 792.0);
        // 页面左下角 306×396 的区域，2 倍 → 612×792 像素，落在位图底部
        let (left, top, size) = device_rect(
            region(Rect::new(0.0, 0.0, 306.0, 396.0), (612, 792)),
            page,
            Rotation::None,
            2.0,
        )
        .expect("应能换算");
        assert_eq!((left, top), (0, 792), "PDF 底部 → 位图底部");
        assert_eq!(size, (612, 792));
    }

    /// 90°：页面空间矩形会旋转，宽高互换。
    #[test]
    fn device_rect_rotates_with_the_page() {
        let page = Size::new(612.0, 792.0);
        // 页面左下角 100×200 → 旋转 90° 后在位图里是 200×100（再 ×2）
        let (_, _, size) = device_rect(
            region(Rect::new(0.0, 0.0, 100.0, 200.0), (400, 200)),
            page,
            Rotation::Clockwise90,
            2.0,
        )
        .expect("应能换算");
        assert_eq!(size, (400, 200), "旋转后宽高互换");
    }

    /// 期望尺寸与推导不一致 → 必须报错，**不得静默拉伸**（D-95）。
    #[test]
    fn device_rect_rejects_a_size_mismatch() {
        let page = Size::new(612.0, 792.0);
        let error = device_rect(
            region(Rect::new(0.0, 0.0, 306.0, 396.0), (611, 792)),
            page,
            Rotation::None,
            2.0,
        )
        .expect_err("尺寸不符必须报错");
        assert!(matches!(error, CoreError::Internal(_)), "实际 {error:?}");
    }

    /// 未落在整像素上的区域 → 报错（否则整块内容会被平移/缩放）。
    #[test]
    fn device_rect_rejects_half_pixel_regions() {
        let page = Size::new(612.0, 792.0);
        // 比例 1.0 时 10.5pt 落在半像素上
        let error = device_rect(
            region(Rect::new(10.5, 0.0, 100.0, 100.0), (100, 100)),
            page,
            Rotation::None,
            1.0,
        )
        .expect_err("半像素区域必须报错");
        assert!(matches!(error, CoreError::Internal(_)), "实际 {error:?}");
    }

    /// 超出页面的区域 → 报错。
    #[test]
    fn device_rect_rejects_out_of_page_regions() {
        let page = Size::new(612.0, 792.0);
        assert!(device_rect(
            region(Rect::new(600.0, 0.0, 100.0, 100.0), (100, 100)),
            page,
            Rotation::None,
            1.0
        )
        .is_err());
        assert!(device_rect(
            region(Rect::new(0.0, 0.0, 0.0, 100.0), (0, 100)),
            page,
            Rotation::None,
            1.0
        )
        .is_err());
    }
}
