//! Markdown 内部图片的 PNG 字节缓存与 `pd-img` loader。
//!
//! 图片身份是 `(record_id, revision)`：record_id 来自 preview/被引用点记录，
//! revision 在像素变化时递增。这里只存 PNG 字节，不碰浮窗的 TextureHandle。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use egui::load::{Bytes, BytesLoadResult, BytesLoader, BytesPoll, LoadError};
use egui::ColorImage;
use pulldown_cmark::{Event, Options, Parser, Tag};

const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_IMAGES: usize = 32;
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;

/// 一个 Markdown 图片引用。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImageRef {
    /// 稳定图片记录 id
    pub record_id: String,
    /// 图片内容版本
    pub revision: u64,
}

/// 解析 `pd-img://<id>?rev=<n>`；其它 URI 返回 `None`。
pub fn parse_ref(uri: &str) -> Option<ImageRef> {
    let rest = uri.strip_prefix("pd-img://")?;
    let (id, query) = rest.split_once('?').unwrap_or((rest, ""));
    if id.is_empty() {
        return None;
    }
    let revision = query
        .split('&')
        .find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == "rev").then_some(value)
        })
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    Some(ImageRef {
        record_id: id.to_string(),
        revision,
    })
}

/// 生成内部图片 URI（rev 必带，避免读取旧版本）。
pub fn image_uri(record_id: &str, revision: u64) -> String {
    format!("pd-img://{record_id}?rev={revision}")
}

/// 从 Markdown 源里取出内部图片引用（去重，保序）。
pub fn markdown_refs(text: &str) -> Vec<ImageRef> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_MATH);
    let mut refs = Vec::new();
    for event in Parser::new_ext(text, options) {
        if let Event::Start(Tag::Image { dest_url, .. }) = event {
            if let Some(reference) = parse_ref(&dest_url) {
                if !refs.contains(&reference) {
                    refs.push(reference);
                }
            }
        }
    }
    refs
}

#[derive(Default)]
struct Inner {
    map: HashMap<String, Arc<[u8]>>,
    /// record_id → 当前 revision；URI 里的 rev 只是观察值，不作为主键。
    latest: HashMap<String, u64>,
    order: VecDeque<String>,
    bytes: usize,
}

/// 会话内 PNG 字节缓存；克隆共享同一份数据。
#[derive(Clone, Default)]
pub struct ImageStore {
    inner: Arc<Mutex<Inner>>,
}

impl ImageStore {
    fn key(reference: &ImageRef) -> String {
        format!("{}@{}", reference.record_id, reference.revision)
    }

    /// 登记一张图片；超过单张/总量上限时忽略或淘汰最旧。
    pub fn insert(&self, reference: &ImageRef, png: Arc<[u8]>) {
        if png.is_empty() || png.len() > MAX_IMAGE_BYTES {
            return;
        }
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let key = Self::key(reference);
        if let Some(old) = inner.map.remove(&key) {
            inner.bytes = inner.bytes.saturating_sub(old.len());
            inner.order.retain(|existing| existing != &key);
        }
        inner.bytes = inner.bytes.saturating_add(png.len());
        inner
            .latest
            .entry(reference.record_id.clone())
            .and_modify(|current| *current = (*current).max(reference.revision))
            .or_insert(reference.revision);
        inner.order.push_back(key.clone());
        inner.map.insert(key, png);
        while inner.map.len() > MAX_IMAGES || inner.bytes > MAX_TOTAL_BYTES {
            let Some(evicted) = inner.order.pop_front() else {
                break;
            };
            if let Some(old) = inner.map.remove(&evicted) {
                inner.bytes = inner.bytes.saturating_sub(old.len());
            }
        }
    }

    /// 取指定 revision 的 PNG 字节。
    pub fn get(&self, reference: &ImageRef) -> Option<Arc<[u8]>> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.map.get(&Self::key(reference)).cloned())
    }

    /// 记录某个 record_id 的当前 revision（即使 PNG 已被淘汰）。
    pub fn set_revision(&self, record_id: &str, revision: u64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner
                .latest
                .entry(record_id.to_string())
                .and_modify(|current| *current = (*current).max(revision))
                .or_insert(revision);
        }
    }

    /// 当前 revision（没有记录时 `None`）。
    pub fn latest_revision(&self, record_id: &str) -> Option<u64> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.latest.get(record_id).copied())
    }

    /// 按 record_id 取当前 revision 的 PNG；URI 里的 rev 不参与主键。
    ///
    /// 如果最新 revision 的字节已被淘汰、但同 record 的旧 revision 还有字节，
    /// 回退到“有字节的最新 revision”。字节是按当前来源渲染的，内容仍然正确。
    pub fn get_latest(&self, record_id: &str) -> Option<(u64, Arc<[u8]>)> {
        let inner = self.inner.lock().ok()?;
        if let Some(revision) = inner.latest.get(record_id).copied() {
            let key = format!("{record_id}@{revision}");
            if let Some(png) = inner.map.get(&key) {
                return Some((revision, png.clone()));
            }
        }
        let mut best: Option<(u64, Arc<[u8]>)> = None;
        for (key, png) in &inner.map {
            let Some((stored_id, revision)) = key.rsplit_once('@') else {
                continue;
            };
            if stored_id != record_id {
                continue;
            }
            let Ok(revision) = revision.parse::<u64>() else {
                continue;
            };
            if best.as_ref().is_none_or(|(current, _)| revision > *current) {
                best = Some((revision, png.clone()));
            }
        }
        best
    }

    /// 当前图片张数。
    pub fn len(&self) -> usize {
        self.inner.lock().map(|inner| inner.map.len()).unwrap_or(0)
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 总字节数。
    pub fn bytes(&self) -> usize {
        self.inner.lock().map(|inner| inner.bytes).unwrap_or(0)
    }

    /// 清空（切换文档时调用）。
    pub fn clear(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.map.clear();
            inner.latest.clear();
            inner.order.clear();
            inner.bytes = 0;
        }
    }
}

/// `egui` 的 `pd-img` bytes loader；只认内部图片，不碰文件/网络。
pub struct InternalImageLoader {
    images: ImageStore,
}

impl InternalImageLoader {
    /// loader id
    pub const ID: &'static str = "pd-internal-image";

    /// 用共享图片库构造。
    pub fn new(images: ImageStore) -> Self {
        Self { images }
    }
}

impl BytesLoader for InternalImageLoader {
    fn id(&self) -> &str {
        Self::ID
    }

    fn load(&self, _ctx: &egui::Context, uri: &str) -> BytesLoadResult {
        let Some(reference) = parse_ref(uri) else {
            return Err(LoadError::NotSupported);
        };
        let Some((_revision, png)) = self.images.get_latest(&reference.record_id) else {
            return Err(LoadError::Loading(format!(
                "内部图片尚未生成：{}@{}",
                reference.record_id, reference.revision
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

/// 把 Markdown 里的 `pd-img` 图片 URI 替换成 `data:image/png;base64,...`。
///
/// 只用于**导出到外部**；内部注释/JSON 始终保留稳定的 `pd-img` 引用。
/// 缓存里暂时没有的图片保留原引用，不阻塞导出。
pub fn materialize_markdown(text: &str, images: &ImageStore) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(position) = rest.find("](pd-img://") {
        let start = position + 2;
        let Some(close) = rest[start..].find(')') else {
            break;
        };
        let close = start + close;
        let uri = &rest[start..close];
        let Some(reference) = parse_ref(uri) else {
            out.push_str(&rest[..close + 1]);
            rest = &rest[close + 1..];
            continue;
        };
        let Some((_revision, png)) = images.get_latest(&reference.record_id) else {
            out.push_str(&rest[..close + 1]);
            rest = &rest[close + 1..];
            continue;
        };
        out.push_str(&rest[..start]);
        out.push_str("data:image/png;base64,");
        out.push_str(&base64(&png));
        rest = &rest[close..];
    }
    out.push_str(rest);
    out
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let value = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((value >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(value & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// 把 egui 的 `ColorImage` 编码成 PNG。
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
    fn parses_pd_img_refs() {
        assert_eq!(
            parse_ref("pd-img://custom-1?rev=3"),
            Some(ImageRef {
                record_id: "custom-1".to_string(),
                revision: 3
            })
        );
        assert_eq!(parse_ref("https://example.com/a.png"), None);
        assert_eq!(
            parse_ref(&image_uri("custom-1", 7)),
            Some(ImageRef {
                record_id: "custom-1".to_string(),
                revision: 7
            })
        );
    }

    #[test]
    fn extracts_image_refs_from_markdown() {
        let refs = markdown_refs("![a](pd-img://custom-1?rev=2)\n![b](https://x/y.png)");
        assert_eq!(
            refs,
            vec![ImageRef {
                record_id: "custom-1".to_string(),
                revision: 2
            }]
        );
    }

    #[test]
    fn materializes_cached_images_as_base64() {
        let store = ImageStore::default();
        let image = ColorImage::new([2, 2], vec![egui::Color32::RED; 4]);
        let png = encode_png(&image).expect("PNG");
        store.insert(
            &ImageRef {
                record_id: "custom-1".to_string(),
                revision: 1,
            },
            png.clone(),
        );
        let markdown = "![a](pd-img://custom-1?rev=1)";
        let exported = materialize_markdown(markdown, &store);
        assert!(exported.starts_with("![a](data:image/png;base64,"));
        assert!(!exported.contains("pd-img"));
        assert_eq!(base64(b"hello"), "aGVsbG8=");
    }

    #[test]
    fn stores_and_serves_png() {
        let store = ImageStore::default();
        let image = ColorImage::new([2, 2], vec![egui::Color32::RED; 4]);
        let png = encode_png(&image).expect("PNG");
        let reference = ImageRef {
            record_id: "custom-1".to_string(),
            revision: 1,
        };
        store.insert(&reference, png);
        assert!(store.get(&reference).is_some());
        let loader = InternalImageLoader::new(store.clone());
        assert!(matches!(
            loader.load(&egui::Context::default(), "pd-img://custom-1?rev=1"),
            Ok(BytesPoll::Ready { .. })
        ));
        // URI 的 rev 只作观察值；loader 按 record_id 返回当前 revision。
        assert!(matches!(
            loader.load(&egui::Context::default(), "pd-img://custom-1?rev=2"),
            Ok(BytesPoll::Ready { .. })
        ));
        assert_eq!(store.latest_revision("custom-1"), Some(1));

        // 最新 revision 还没有字节时，回退到同 record 的已有字节。
        store.set_revision("custom-1", 2);
        let (revision, _) = store.get_latest("custom-1").expect("回退到已有字节");
        assert_eq!(revision, 1);
    }
}
