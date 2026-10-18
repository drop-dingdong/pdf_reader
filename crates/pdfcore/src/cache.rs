//! 带内存上限的 LRU 位图缓存 —— [`RenderCache`] 的默认实现。
//!
//! ## 为什么需要它
//! `pdfcore` 只定义了 [`RenderCache`] 这个接口；真正跑起来的阅读器必须有一个"能自我约束内存"
//! 的实现，否则长时间翻页会把整本 PDF 的位图都留在内存里。
//!
//! ## 淘汰粒度
//! 以 [`RenderKey`]（页 + 量化缩放 + 旋转 + 瓦片）为单位淘汰，因此**分块渲染时淘汰的是单块瓦片**，
//! 而不是整页 —— 这正是分块想要的弹性。
//!
//! ## 实现取舍
//! LRU 顺序用 `VecDeque` + 线性查找实现：条目数通常是"可见页 ±1 的几个缩放档"（几十到几百），
//! 每帧几次查找的开销可忽略。若将来条目数达到数万，可换成侵入式链表（接口不变）。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::ids::PageIndex;
use crate::image::RasterImage;
use crate::render::{CacheStats, RenderCache, RenderKey};

/// 默认容量上限：512 MiB（见 architecture.md §3.1 的体积预算）。
pub const DEFAULT_CAPACITY_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Default)]
struct Inner {
    images: HashMap<RenderKey, Arc<RasterImage>>,
    /// LRU 顺序：队首最旧、队尾最新。
    order: VecDeque<RenderKey>,
    bytes: u64,
    hits: u64,
    misses: u64,
}

/// 带内存上限的 LRU 缓存。
#[derive(Debug)]
pub struct LruCache {
    inner: Mutex<Inner>,
    capacity_bytes: u64,
}

impl LruCache {
    /// 以给定的字节上限构造（0 会被视为上限 1，保证"至少能放一张图"的语义不至于失效）。
    pub fn new(capacity_bytes: u64) -> Self {
        Self { inner: Mutex::new(Inner::default()), capacity_bytes: capacity_bytes.max(1) }
    }

    /// 用默认上限（512 MiB）构造。
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_CAPACITY_BYTES)
    }

    /// 当前上限。
    pub fn capacity_bytes(&self) -> u64 {
        self.capacity_bytes
    }

    /// 当前占用的字节数。
    pub fn used_bytes(&self) -> u64 {
        self.inner.lock().unwrap().bytes
    }

    /// 把键移到 LRU 队尾（最新）。
    fn touch(order: &mut VecDeque<RenderKey>, key: &RenderKey) {
        if let Some(position) = order.iter().position(|existing| existing == key) {
            order.remove(position);
        }
        order.push_back(*key);
    }

    /// 淘汰到容量以内。
    ///
    /// 特例：**即使单张位图超过上限，也保留最后一张**（否则会出现"存进去立刻被丢掉、
    /// 于是每帧都重新渲染"的抖动，比超一点内存更糟）。
    fn evict(inner: &mut Inner, capacity_bytes: u64) {
        while inner.bytes > capacity_bytes && inner.images.len() > 1 {
            let Some(oldest) = inner.order.pop_front() else { break };
            if let Some(image) = inner.images.remove(&oldest) {
                inner.bytes = inner.bytes.saturating_sub(image.byte_len() as u64);
            }
        }
    }
}

impl RenderCache for LruCache {
    fn get(&self, key: &RenderKey) -> Option<Arc<RasterImage>> {
        let mut inner = self.inner.lock().unwrap();
        match inner.images.get(key).cloned() {
            Some(image) => {
                inner.hits += 1;
                Self::touch(&mut inner.order, key);
                Some(image)
            }
            None => {
                inner.misses += 1;
                None
            }
        }
    }

    fn insert(&self, key: RenderKey, image: Arc<RasterImage>) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(previous) = inner.images.insert(key, image.clone()) {
            inner.bytes = inner.bytes.saturating_sub(previous.byte_len() as u64);
        }
        inner.bytes = inner.bytes.saturating_add(image.byte_len() as u64);
        Self::touch(&mut inner.order, &key);
        Self::evict(&mut inner, self.capacity_bytes);
    }

    fn remove_page(&self, page: PageIndex) {
        let mut inner = self.inner.lock().unwrap();
        let doomed: Vec<RenderKey> = inner
            .images
            .keys()
            .filter(|key| key.page == page)
            .copied()
            .collect();
        for key in doomed {
            if let Some(image) = inner.images.remove(&key) {
                inner.bytes = inner.bytes.saturating_sub(image.byte_len() as u64);
            }
            if let Some(position) = inner.order.iter().position(|existing| *existing == key) {
                inner.order.remove(position);
            }
        }
    }

    fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.images.clear();
        inner.order.clear();
        inner.bytes = 0;
    }

    fn stats(&self) -> CacheStats {
        let inner = self.inner.lock().unwrap();
        CacheStats {
            entries: inner.images.len(),
            bytes: inner.bytes,
            hits: inner.hits,
            misses: inner.misses,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rotation;
    use crate::image::PixelFormat;
    use crate::render::Scale;

    /// 造一张指定字节数的位图（1 像素 4 字节，宽=bytes/4、高=1）。
    fn image(bytes: usize) -> Arc<RasterImage> {
        assert_eq!(bytes % 4, 0, "测试用位图必须 4 字节对齐");
        let pixels = (bytes / 4) as u32;
        let data: Arc<[u8]> = Arc::from(vec![0u8; bytes].into_boxed_slice());
        Arc::new(RasterImage::new(pixels, 1, PixelFormat::Rgba8, data).expect("位图"))
    }

    fn key(page: u32, scale: f32, tile: Option<(u32, u32)>) -> RenderKey {
        match tile {
            None => RenderKey::new(PageIndex::new(page), Scale::from_zoom(scale), Rotation::None),
            Some((col, row)) => RenderKey::tiled(
                PageIndex::new(page),
                Scale::from_zoom(scale),
                Rotation::None,
                crate::render::TileSpec::new(col, row),
            ),
        }
    }

    #[test]
    fn stores_and_reports_stats() {
        let cache = LruCache::new(10_000);
        assert!(cache.get(&key(0, 1.0, None)).is_none());

        cache.insert(key(0, 1.0, None), image(4000));
        let found = cache.get(&key(0, 1.0, None)).expect("应命中");
        assert_eq!(found.byte_len(), 4000);

        let stats = cache.stats();
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.bytes, 4000);
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(cache.used_bytes(), 4000);
    }

    #[test]
    fn evicts_least_recently_used() {
        let cache = LruCache::new(8_000);
        cache.insert(key(0, 1.0, None), image(4000));
        cache.insert(key(1, 1.0, None), image(4000));
        // 再插一张 → 必须淘汰最旧的（页 0）
        cache.insert(key(2, 1.0, None), image(4000));

        assert!(cache.get(&key(0, 1.0, None)).is_none(), "页 0 应被淘汰");
        assert!(cache.get(&key(1, 1.0, None)).is_some());
        assert!(cache.get(&key(2, 1.0, None)).is_some());
        assert!(cache.used_bytes() <= 8_000);
    }

    #[test]
    fn touching_on_get_protects_an_entry() {
        let cache = LruCache::new(8_000);
        cache.insert(key(0, 1.0, None), image(4000));
        cache.insert(key(1, 1.0, None), image(4000));

        // 读一下页 0 → 它变成"最新"，接下来被淘汰的应该是页 1
        assert!(cache.get(&key(0, 1.0, None)).is_some());
        cache.insert(key(2, 1.0, None), image(4000));

        assert!(cache.get(&key(1, 1.0, None)).is_none(), "页 1 应被淘汰");
        assert!(cache.get(&key(0, 1.0, None)).is_some(), "页 0 因刚被访问而保留");
    }

    #[test]
    fn remove_page_drops_every_scale_and_tile() {
        let cache = LruCache::new(1_000_000);
        cache.insert(key(7, 1.0, None), image(4000));
        cache.insert(key(7, 2.0, None), image(4000));
        cache.insert(key(7, 2.0, Some((0, 0))), image(4000));
        cache.insert(key(8, 1.0, None), image(4000));
        assert_eq!(cache.stats().entries, 4);

        cache.remove_page(PageIndex::new(7));

        let stats = cache.stats();
        assert_eq!(stats.entries, 1, "页 7 的所有缩放与瓦片都应清掉");
        assert_eq!(stats.bytes, 4000);
        assert!(cache.get(&key(8, 1.0, None)).is_some());
    }

    #[test]
    fn keeps_one_entry_even_when_it_exceeds_capacity() {
        let cache = LruCache::new(1000);
        cache.insert(key(0, 1.0, None), image(4000));
        // 单张超限：保留它（否则会陷入"每帧重新渲染"的抖动）
        assert_eq!(cache.stats().entries, 1);
        assert!(cache.get(&key(0, 1.0, None)).is_some());

        // 再插入一张时才开始真正淘汰
        cache.insert(key(1, 1.0, None), image(4000));
        assert_eq!(cache.stats().entries, 1, "始终至少保留一张");
        assert!(cache.get(&key(1, 1.0, None)).is_some());
        assert!(cache.get(&key(0, 1.0, None)).is_none());
    }

    #[test]
    fn clear_resets_everything_but_counters() {
        let cache = LruCache::new(1_000_000);
        cache.insert(key(0, 1.0, None), image(4000));
        let _ = cache.get(&key(0, 1.0, None));
        cache.clear();

        let stats = cache.stats();
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.bytes, 0);
        assert_eq!(stats.hits, 1, "命中计数是统计信息，清空缓存不应重置");
        assert_eq!(cache.used_bytes(), 0);
    }

    #[test]
    fn overwriting_the_same_key_does_not_double_count_bytes() {
        let cache = LruCache::new(1_000_000);
        cache.insert(key(0, 1.0, None), image(4000));
        cache.insert(key(0, 1.0, None), image(2000));
        let stats = cache.stats();
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.bytes, 2000, "同键覆盖必须替换字节数而不是累加");
    }
}
