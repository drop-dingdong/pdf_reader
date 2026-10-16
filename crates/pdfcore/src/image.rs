//! 位图载体：后端渲染出来的像素，缓存与 UI 之间传递的货物。
//!
//! 设计要点：像素数据放在 `Arc<[u8]>` 里，因此「缓存持有一份 + UI 上传纹理时需要一份」不会
//! 触发拷贝；`RasterImage` 本身不可变，可安全跨线程共享。

use std::sync::Arc;

use crate::geometry::Size;

/// 像素格式。
///
/// 需要它的原因是：**PDFium 原生输出 BGRA，而 egui 纹理要求 RGBA**。把格式显式建模出来，
/// 转换就只发生在 [`RasterImage::to_rgba8`] 这一个地方，而不是散落在渲染与 UI 代码里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// 每像素 4 字节，R、G、B、A 顺序。
    Rgba8,
    /// 每像素 4 字节，B、G、R、A 顺序（PDFium 的默认输出）。
    Bgra8,
    /// 每像素 1 字节灰度。
    Gray8,
}

impl PixelFormat {
    /// 每像素字节数。
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgba8 | Self::Bgra8 => 4,
            Self::Gray8 => 1,
        }
    }

    /// 是否含 alpha 通道。
    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::Rgba8 | Self::Bgra8)
    }
}

/// 一页的渲染结果（不可变位图）。
#[derive(Debug, Clone)]
pub struct RasterImage {
    /// 像素宽（设备像素，非 point）。
    pub width: u32,
    /// 像素高（设备像素，非 point）。
    pub height: u32,
    /// 像素格式。
    pub format: PixelFormat,
    /// 紧凑排列的像素数据：长度为 `width * height * format.bytes_per_pixel()`。
    data: Arc<[u8]>,
}

impl RasterImage {
    /// 构造；当 `data` 长度与宽高、格式不匹配时返回 `None`
    /// （宁可在这里拒绝，也不要让 UI 拿到会越界读的数据）。
    pub fn new(width: u32, height: u32, format: PixelFormat, data: Arc<[u8]>) -> Option<Self> {
        let expected = Self::expected_len(width, height, format);
        if data.len() < expected {
            return None;
        }
        Some(Self { width, height, format, data })
    }

    /// 期望的字节数。
    pub fn expected_len(width: u32, height: u32, format: PixelFormat) -> usize {
        width as usize * height as usize * format.bytes_per_pixel()
    }

    /// 行跨距（本项目不做到行对齐，恒等于 `width * bpp`；提供此方法是为了让后端适配层显式处理
    /// PDFium 的 `stride` 参数时有个统一入口）。
    pub fn stride(&self) -> usize {
        self.width as usize * self.format.bytes_per_pixel()
    }

    /// 像素尺寸。
    pub fn size(&self) -> Size {
        Size::new(self.width as f32, self.height as f32)
    }

    /// 原始像素数据。
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// 数据字节数（供缓存做容量估算）。
    pub fn byte_len(&self) -> usize {
        self.data.len()
    }

    /// 是否为零尺寸。
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// 转换为 egui 可直接上传的 RGBA8 数据。
    ///
    /// * `Bgra8`：交换 R/B 通道；
    /// * `Gray8`：复制到三个通道，alpha 置 255；
    /// * `Rgba8`：直接拷贝（仍然会分配，可用 [`RasterImage::data`] 避免）。
    pub fn to_rgba8(&self) -> Vec<u8> {
        match self.format {
            PixelFormat::Rgba8 => self.data.to_vec(),
            PixelFormat::Bgra8 => {
                let mut out = Vec::with_capacity(self.data.len());
                for px in self.data.chunks_exact(4) {
                    out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
                out
            }
            PixelFormat::Gray8 => {
                let mut out = Vec::with_capacity(self.data.len() * 4);
                for &g in self.data.iter() {
                    out.extend_from_slice(&[g, g, g, 255]);
                }
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_truncated_data() {
        let data: Arc<[u8]> = Arc::from([0u8; 6].as_slice());
        assert!(RasterImage::new(2, 2, PixelFormat::Rgba8, data).is_none());
    }

    #[test]
    fn converts_bgra_to_rgba() {
        // 一个像素：B=1, G=2, R=3, A=4
        let data: Arc<[u8]> = Arc::from([1u8, 2, 3, 4].as_slice());
        let image = RasterImage::new(1, 1, PixelFormat::Bgra8, data).unwrap();
        assert_eq!(image.to_rgba8(), vec![3, 2, 1, 4]);
    }

    #[test]
    fn converts_gray_to_rgba() {
        let data: Arc<[u8]> = Arc::from([7u8, 9].as_slice());
        let image = RasterImage::new(2, 1, PixelFormat::Gray8, data).unwrap();
        assert_eq!(image.to_rgba8(), vec![7, 7, 7, 255, 9, 9, 9, 255]);
    }

    #[test]
    fn stride_and_size() {
        let data: Arc<[u8]> = Arc::from([0u8; 8].as_slice());
        let image = RasterImage::new(2, 1, PixelFormat::Rgba8, data).unwrap();
        assert_eq!(image.stride(), 8);
        assert_eq!(image.size(), Size::new(2.0, 1.0));
        assert_eq!(image.byte_len(), 8);
    }
}
