//! 会话内临时图片库：把截图/图片 preview 以 PNG 字节登记到内存，并让
//! Markdown 通过 `pd-img://<id>` 引用它们。
//!
//! 这是 A 阶段（会话内）实现：不写 sidecar、不落盘；重启后引用失效。
//! 内存有硬上限，超出后按登记顺序淘汰最旧的图片。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use egui::load::{Bytes, BytesLoadResult, BytesLoader, BytesPoll, LoadError};
use egui::ColorImage;

/// 单张图片字节上限（避免一张超大截图把内存吃光）。
const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
/// 图片条数上限。
const MAX_IMAGES: usize = 32;
/// 图片库总字节上限。
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;

/// 一条已登记的内部图片。
#[derive(Clone)]
struct InternalImage {
    png: Arc<[u8]>,
    label: String,
}

#[derive(Default)]
struct Inner {
    map: HashMap<String, InternalImage>,
    order: VecDeque<String>,
    bytes: usize,
}

/// 会话内图片库；克隆后共享同一份数据（loader 与 app 各持一份）。
#[derive(Clone, Default)]
pub struct InternalImages {
    inner: Arc<Mutex<Inner>>,
}

impl InternalImages {
    /// 登记一张图片；`png` 为空或超过单张上限时忽略。
    pub fn insert(&self, id: impl Into<String>, label: impl Into<String>, png: Arc<[u8]>) {
        let id = id.into();
        if id.is_empty() || png.is_empty() || png.len() > MAX_IMAGE_BYTES {
            return;
        }
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if let Some(old) = inner.map.remove(&id) {
            inner.bytes = inner.bytes.saturating_sub(old.png.len());
            inner.order.retain(|existing| existing != &id);
        }
        inner.bytes = inner.bytes.saturating_add(png.len());
        inner.order.push_back(id.clone());
        inner.map.insert(
            id,
            InternalImage {
                png,
                label: label.into(),
            },
        );
        while inner.map.len() > MAX_IMAGES || inner.bytes > MAX_TOTAL_BYTES {
            let Some(evicted) = inner.order.pop_front() else {
                break;
            };
            if let Some(old) = inner.map.remove(&evicted) {
                inner.bytes = inner.bytes.saturating_sub(old.png.len());
            }
        }
    }

    /// 取一张图片的 PNG 字节。
    pub fn get(&self, id: &str) -> Option<Arc<[u8]>> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.map.get(id).map(|image| image.png.clone()))
    }

    /// 列出可插入的图片 `(id, 标签)`，按登记顺序。
    pub fn list(&self) -> Vec<(String, String)> {
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        inner
            .order
            .iter()
            .filter_map(|id| {
                inner
                    .map
                    .get(id)
                    .map(|image| (id.clone(), image.label.clone()))
            })
            .collect()
    }

    /// 当前图片张数。
    pub fn len(&self) -> usize {
        self.inner.lock().map(|inner| inner.map.len()).unwrap_or(0)
    }

    /// 图片库是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 总字节数。
    pub fn bytes(&self) -> usize {
        self.inner.lock().map(|inner| inner.bytes).unwrap_or(0)
    }

    /// 清空图片库（切换文件/关闭文档时调用）。
    pub fn clear(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.map.clear();
            inner.order.clear();
            inner.bytes = 0;
        }
    }
}

/// `egui` 的 PNG 图片库加载器：只认 `pd-img://`，不碰文件/网络。
pub struct InternalImageLoader {
    images: InternalImages,
}

impl InternalImageLoader {
    /// loader 的唯一 id；用于 `Context::is_loader_installed`。
    pub const ID: &'static str = "pd-internal-image";

    /// 用共享图片库构造。
    pub fn new(images: InternalImages) -> Self {
        Self { images }
    }
}

impl BytesLoader for InternalImageLoader {
    fn id(&self) -> &str {
        Self::ID
    }

    fn load(&self, _ctx: &egui::Context, uri: &str) -> BytesLoadResult {
        let Some(id) = uri.strip_prefix("pd-img://") else {
            return Err(LoadError::NotSupported);
        };
        let Some(png) = self.images.get(id) else {
            return Err(LoadError::Loading(format!(
                "内部图片尚未生成或已被淘汰：{id}"
            )));
        };
        Ok(BytesPoll::Ready {
            size: None,
            bytes: Bytes::Shared(png),
            mime: Some("image/png".to_string()),
        })
    }

    fn forget(&self, _uri: &str) {}

    fn forget_all(&self) {}

    fn byte_size(&self) -> usize {
        self.images.bytes()
    }
}

/// 把 egui 的 `ColorImage` 编码成 PNG；失败返回 `None`。
pub fn encode_png(image: &ColorImage) -> Option<Arc<[u8]>> {
    let width = u32::try_from(image.size[0]).ok()?;
    let height = u32::try_from(image.size[1]).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let mut rgba = Vec::with_capacity(image.pixels.len().saturating_mul(4));
    for pixel in &image.pixels {
        rgba.extend_from_slice(&pixel.to_array());
    }
    let buffer = image::RgbaImage::from_raw(width, height, rgba)?;
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .ok()?;
    Some(Arc::from(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_png_and_serves_it_through_the_loader() {
        let image = ColorImage::new([2, 2], vec![egui::Color32::RED; 4]);
        let png = encode_png(&image).expect("PNG");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        let images = InternalImages::default();
        images.insert("custom-1", "图一", png);
        assert_eq!(
            images.list(),
            vec![("custom-1".to_string(), "图一".to_string())]
        );
        let loader = InternalImageLoader::new(images);
        let result = loader.load(&egui::Context::default(), "pd-img://custom-1");
        match result {
            Ok(BytesPoll::Ready { bytes, mime, .. }) => {
                assert!(bytes.starts_with(&[0x89, b'P', b'N', b'G']));
                assert_eq!(mime.as_deref(), Some("image/png"));
            }
            _ => panic!("内部图片应立即可用"),
        }
    }

    #[test]
    fn missing_internal_image_is_a_loading_error() {
        let loader = InternalImageLoader::new(InternalImages::default());
        assert!(matches!(
            loader.load(&egui::Context::default(), "pd-img://missing"),
            Err(LoadError::Loading(_))
        ));
    }

    #[test]
    fn only_pd_img_scheme_is_supported() {
        let loader = InternalImageLoader::new(InternalImages::default());
        assert!(matches!(
            loader.load(&egui::Context::default(), "file:///tmp/x.png"),
            Err(LoadError::NotSupported)
        ));
    }
}
