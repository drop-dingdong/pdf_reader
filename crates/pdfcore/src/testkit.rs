//! 内存假后端与测试替身。
//!
//! ## 这个模块存在的意义
//! 它是"trait 拆分是否真的可替换"的**可执行证据**：在既没有 PDFium、也没有 PDF 文件、
//! 更没有 GPU 的环境里，用这里的几百行内存实现就能把整条链路
//! （后端 → 调度 → 缓存 → 布局 → 选区 → 会话）跑起来并写断言。
//!
//! 由此得到两个直接收益：
//! * 布局、滚动、选择、调度这些**与 PDF 无关**的逻辑可以在毫秒级单测里验证，不需要集成测试；
//! * PDFium 后端接入后，如果某个测试失败，可以先用假后端复现，从而判断问题出在
//!   "上层逻辑"还是"后端适配"。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::document::{
    Capabilities, Document, DocumentInfo, Link, OpenedDocument, OutlineNode, PdfBackend, Source,
};
use crate::error::{CoreError, Result};
use crate::geometry::{Point, Rect, Rotation, Size};
use crate::ids::{JobId, PageIndex};
use crate::image::{PixelFormat, RasterImage};
use crate::render::{
    CacheStats, PageRaster, RenderCache, RenderEvent, RenderKey, RenderRequest, RenderScheduler,
};
use crate::store::KeyValueStore;
use crate::text::{PageText, SearchHit, SearchQuery, TextChar, TextLayer, TextRange, TextSpan};

// ------------------------------------------------------------------ 存储替身

/// 内存键值存储。
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<String, String>>,
}

impl MemoryStore {
    /// 新建空存储。
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyValueStore for MemoryStore {
    fn get(&self, key: &str) -> Option<String> {
        self.entries.lock().unwrap().get(key).cloned()
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.entries.lock().unwrap().insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn remove(&self, key: &str) {
        self.entries.lock().unwrap().remove(key);
    }
}

// ------------------------------------------------------------------ 缓存替身

/// 内存位图缓存（无淘汰策略，够 MVP 与测试用）。
#[derive(Debug, Default)]
pub struct MemoryCache {
    entries: Mutex<HashMap<RenderKey, Arc<RasterImage>>>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl MemoryCache {
    /// 新建空缓存。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前条目数。
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl RenderCache for MemoryCache {
    fn get(&self, key: &RenderKey) -> Option<Arc<RasterImage>> {
        let found = self.entries.lock().unwrap().get(key).cloned();
        if found.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        found
    }

    fn insert(&self, key: RenderKey, image: Arc<RasterImage>) {
        self.entries.lock().unwrap().insert(key, image);
    }

    fn remove_page(&self, page: PageIndex) {
        self.entries.lock().unwrap().retain(|key, _| key.page != page);
    }

    fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }

    fn stats(&self) -> CacheStats {
        let entries = self.entries.lock().unwrap();
        CacheStats {
            entries: entries.len(),
            bytes: entries.values().map(|image| image.byte_len() as u64).sum(),
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }
}

// ------------------------------------------------------------------ 调度替身

/// **同步**调度器：`submit` 立刻调用 [`PageRaster::render`]，把结果放进事件队列。
///
/// 它的价值在于让"提交 → 完成 → 入缓存"这条异步链路在单测里变成确定性的：
/// 不需要等线程，也不会有竞态导致的偶发失败。
/// 真实实现（后台线程 + 优先级队列）与它在 [`RenderScheduler`] 契约上等价。
pub struct ImmediateScheduler {
    raster: Arc<dyn PageRaster>,
    events: Mutex<VecDeque<RenderEvent>>,
    next_job: AtomicU64,
}

impl ImmediateScheduler {
    /// 用给定的光栅器构造。
    pub fn new(raster: Arc<dyn PageRaster>) -> Self {
        Self { raster, events: Mutex::new(VecDeque::new()), next_job: AtomicU64::new(1) }
    }
}

impl std::fmt::Debug for ImmediateScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImmediateScheduler")
            .field("pending_events", &self.events.lock().unwrap().len())
            .finish()
    }
}

impl RenderScheduler for ImmediateScheduler {
    fn submit(&self, request: RenderRequest) -> JobId {
        let job = JobId(self.next_job.fetch_add(1, Ordering::Relaxed));
        let event = if request.cancel.is_cancelled() {
            RenderEvent::Cancelled { page: request.page }
        } else {
            let key = request.key();
            match self.raster.render(&request) {
                Ok(image) => RenderEvent::Rendered { key, image },
                Err(error) => RenderEvent::Failed { page: request.page, error },
            }
        };
        self.events.lock().unwrap().push_back(event);
        job
    }

    fn cancel(&self, _job: JobId) {
        // 同步实现没有队列，任务在 submit 时已经完成。
    }

    fn cancel_page(&self, page: PageIndex) {
        self.events.lock().unwrap().retain(|event| event_page(event) != Some(page));
    }

    fn cancel_all(&self) {
        self.events.lock().unwrap().clear();
    }

    fn drain_events(&self, max: usize) -> Vec<RenderEvent> {
        let mut queue = self.events.lock().unwrap();
        let take = max.min(queue.len());
        queue.drain(..take).collect()
    }

    fn pending(&self) -> usize {
        self.events.lock().unwrap().len()
    }
}

/// 事件所属的页（用于取消）。
fn event_page(event: &RenderEvent) -> Option<PageIndex> {
    match event {
        RenderEvent::Rendered { key, .. } => Some(key.page),
        RenderEvent::Failed { page, .. } => Some(*page),
        RenderEvent::Cancelled { page } => Some(*page),
    }
}

// ------------------------------------------------------------------ 假文档

/// 一页假文档：尺寸、自带旋转、文本行。
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryPage {
    /// 未旋转的页面尺寸。
    pub size: Size,
    /// 页面自带的 `/Rotate`。
    pub rotation: Rotation,
    /// 文本行（每行会渲染成一个字符序列）。
    pub lines: Vec<String>,
}

impl MemoryPage {
    /// 构造。
    pub fn new(size: Size, rotation: Rotation, lines: Vec<String>) -> Self {
        Self { size, rotation, lines }
    }
}

/// [`Document`] 的内存实现。
#[derive(Debug)]
pub struct MemoryDocument {
    info: DocumentInfo,
    pages: Arc<Vec<MemoryPage>>,
}

impl Document for MemoryDocument {
    fn info(&self) -> &DocumentInfo {
        &self.info
    }

    fn page_size(&self, page: PageIndex) -> Result<Size> {
        self.pages
            .get(page.get() as usize)
            .map(|page| page.size)
            .ok_or(CoreError::PageOutOfRange { page, page_count: self.pages.len() as u32 })
    }

    fn page_rotation(&self, page: PageIndex) -> Result<Rotation> {
        self.pages
            .get(page.get() as usize)
            .map(|page| page.rotation)
            .ok_or(CoreError::PageOutOfRange { page, page_count: self.pages.len() as u32 })
    }

    fn outline(&self) -> Result<Vec<OutlineNode>> {
        // 假后端演示"有目录但能力声明为 false"之外的另一种情况：直接返回一个两级大纲。
        Ok(self
            .pages
            .iter()
            .enumerate()
            .map(|(index, page)| OutlineNode {
                title: page.lines.first().cloned().unwrap_or_else(|| format!("Page {}", index + 1)),
                destination: Some(crate::document::Destination::Page {
                    page: PageIndex::new(index as u32),
                    offset: None,
                }),
                children: Vec::new(),
            })
            .collect())
    }

    fn links(&self, _page: PageIndex) -> Result<Vec<Link>> {
        Ok(Vec::new())
    }
}

/// [`PageRaster`] 的内存实现：按页号填纯色，尺寸严格按请求计算。
#[derive(Debug)]
pub struct MemoryRaster {
    pages: Arc<Vec<MemoryPage>>,
}

impl MemoryRaster {
    /// 构造。
    pub fn new(pages: Arc<Vec<MemoryPage>>) -> Self {
        Self { pages }
    }

    /// 每页一个可区分的颜色（测试可据此断言"确实是这一页的位图"）。
    pub fn color_for(page: PageIndex) -> [u8; 4] {
        match page.get() % 3 {
            0 => [255, 0, 0, 255],
            1 => [0, 255, 0, 255],
            _ => [0, 0, 255, 255],
        }
    }
}

impl PageRaster for MemoryRaster {
    fn render(&self, request: &RenderRequest) -> Result<Arc<RasterImage>> {
        if request.cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let page = self.pages.get(request.page.get() as usize).ok_or(
            CoreError::PageOutOfRange {
                page: request.page,
                page_count: self.pages.len() as u32,
            },
        )?;
        let displayed = page.size.rotated(request.rotation);
        let width = (displayed.width * request.scale.0).round().max(1.0) as u32;
        let height = (displayed.height * request.scale.0).round().max(1.0) as u32;
        let color = Self::color_for(request.page);
        let mut data = Vec::with_capacity(RasterImage::expected_len(width, height, PixelFormat::Rgba8));
        for _ in 0..(width as usize * height as usize) {
            data.extend_from_slice(&color);
        }
        let data: Arc<[u8]> = Arc::from(data.into_boxed_slice());
        let image = RasterImage::new(width, height, PixelFormat::Rgba8, data)
            .ok_or_else(|| CoreError::Internal("假光栅器生成的位图长度不匹配".to_string()))?;
        Ok(Arc::new(image))
    }
}

/// [`TextLayer`] 的内存实现，含一个朴素但可用的全文搜索。
#[derive(Debug)]
pub struct MemoryTextLayer {
    pages: Arc<Vec<MemoryPage>>,
}

impl MemoryTextLayer {
    /// 构造。
    pub fn new(pages: Arc<Vec<MemoryPage>>) -> Self {
        Self { pages }
    }
}

impl TextLayer for MemoryTextLayer {
    fn page_text(&self, page: PageIndex) -> Result<Arc<PageText>> {
        let target = self
            .pages
            .get(page.get() as usize)
            .ok_or(CoreError::PageOutOfRange { page, page_count: self.pages.len() as u32 })?;
        Ok(Arc::new(build_page_text(page, target.size, &target.lines)))
    }

    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>> {
        if query.needle.is_empty() {
            return Ok(Vec::new());
        }
        let needle: Vec<char> = if query.case_sensitive {
            query.needle.chars().collect()
        } else {
            query.needle.to_lowercase().chars().collect()
        };
        let mut hits = Vec::new();
        let start_page = query.start_page.unwrap_or(PageIndex::FIRST).get() as usize;
        for index in start_page..self.pages.len() {
            let page = PageIndex::new(index as u32);
            let text = self.page_text(page)?;
            let haystack: Vec<char> = if query.case_sensitive {
                text.text.chars().collect()
            } else {
                text.text.to_lowercase().chars().collect()
            };
            if needle.len() > haystack.len() {
                continue;
            }
            for start in 0..=(haystack.len() - needle.len()) {
                if haystack[start..start + needle.len()] != needle[..] {
                    continue;
                }
                if query.whole_word {
                    let before_ok = start == 0 || !haystack[start - 1].is_alphanumeric();
                    let after = start + needle.len();
                    let after_ok = after >= haystack.len() || !haystack[after].is_alphanumeric();
                    if !before_ok || !after_ok {
                        continue;
                    }
                }
                let range = TextRange::new(start as u32, (start + needle.len()) as u32);
                hits.push(SearchHit { page, range, rects: text.rects_for_range(range) });
                if let Some(limit) = query.limit {
                    if hits.len() >= limit {
                        return Ok(hits);
                    }
                }
            }
        }
        Ok(hits)
    }
}

/// 按固定字距、行距构造一页文本层（与真实后端一样，换行符带空包围盒）。
pub fn build_page_text(page: PageIndex, page_size: Size, lines: &[String]) -> PageText {
    const CHAR_WIDTH: f32 = 6.0;
    const CHAR_HEIGHT: f32 = 10.0;
    const LINE_HEIGHT: f32 = 14.0;
    const LEFT: f32 = 36.0;

    let mut text = String::new();
    let mut chars = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        if line_index > 0 {
            text.push('\n');
            chars.push(TextChar { ch: '\n', bbox: Rect::ZERO, origin: Point::ORIGIN, span: 0 });
        }
        // PDF 页面空间原点在左下角，因此第一行最靠上（y 最大）。
        let baseline = page_size.height - (line_index as f32 + 1.0) * LINE_HEIGHT;
        for (char_index, ch) in line.chars().enumerate() {
            text.push(ch);
            let x = LEFT + char_index as f32 * CHAR_WIDTH;
            chars.push(TextChar {
                ch,
                bbox: Rect::new(x, baseline, CHAR_WIDTH, CHAR_HEIGHT),
                origin: Point::new(x, baseline),
                span: 0,
            });
        }
    }
    let len = chars.len() as u32;
    PageText { page, text, chars, spans: vec![TextSpan { range: TextRange::new(0, len), font_size: 12.0 }] }
}

// ------------------------------------------------------------------ 假后端

/// [`PdfBackend`] 的内存实现。
#[derive(Debug)]
pub struct MemoryBackend {
    pages: Arc<Vec<MemoryPage>>,
    password: Option<&'static str>,
}

impl MemoryBackend {
    /// 用给定页面构造。
    pub fn new(pages: Vec<MemoryPage>) -> Self {
        Self { pages: Arc::new(pages), password: None }
    }

    /// 三页演示文档：前两页正常、第三页带 `/Rotate 90`，每页都有文本。
    pub fn demo() -> Self {
        let size = Size::new(612.0, 792.0);
        Self::new(vec![
            MemoryPage::new(
                size,
                Rotation::None,
                vec!["Hello world".to_string(), "Second line".to_string()],
            ),
            MemoryPage::new(
                size,
                Rotation::None,
                vec!["Page two".to_string(), "has text too".to_string()],
            ),
            MemoryPage::new(size, Rotation::Clockwise90, vec!["Rotated page".to_string()]),
        ])
    }

    /// 模拟加密文档：不带密码打开会得到 [`CoreError::PasswordRequired`]。
    pub fn with_password(mut self, password: &'static str) -> Self {
        self.password = Some(password);
        self
    }

    /// 页面清单。
    pub fn pages(&self) -> &[MemoryPage] {
        &self.pages
    }
}

impl PdfBackend for MemoryBackend {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            text_layer: true,
            search: true,
            encryption: self.password.is_some(),
            outline: true,
            links: false,
            max_concurrency: 1,
        }
    }

    fn open(&self, source: Source<'_>, password: Option<&str>) -> Result<OpenedDocument> {
        match source {
            Source::Bytes(bytes) if bytes.is_empty() => {
                return Err(CoreError::CorruptDocument("空的字节流".to_string()))
            }
            Source::Path(path) if path.as_os_str().is_empty() => {
                return Err(CoreError::Io("空路径".to_string()))
            }
            _ => {}
        }

        if let Some(expected) = self.password {
            match password {
                None => return Err(CoreError::PasswordRequired),
                Some(given) if given != expected => return Err(CoreError::IncorrectPassword),
                Some(_) => {}
            }
        }

        let info = DocumentInfo {
            page_count: self.pages.len() as u32,
            title: Some("内存演示文档".to_string()),
            author: Some("pdfcore::testkit".to_string()),
            encrypted: self.password.is_some(),
            ..DocumentInfo::default()
        };

        Ok(OpenedDocument::new(
            Arc::new(MemoryDocument { info, pages: self.pages.clone() }),
            Arc::new(MemoryRaster::new(self.pages.clone())),
            Some(Arc::new(MemoryTextLayer::new(self.pages.clone()))),
            self.capabilities(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{RenderPriority, Scale};

    #[test]
    fn raster_respects_scale_and_rotation() {
        let backend = MemoryBackend::demo();
        let opened = backend.open(Source::Bytes(b"%PDF-fake"), None).unwrap();
        // 第 3 页带 /Rotate 90：612x792 旋转后是 792x612
        let request = RenderRequest::new(
            PageIndex::new(2),
            Scale::from_zoom(0.5),
            Rotation::Clockwise90,
        )
        .with_priority(RenderPriority::Visible);
        let image = opened.raster.render(&request).unwrap();
        assert_eq!((image.width, image.height), (396, 306));
    }

    #[test]
    fn raster_reports_out_of_range() {
        let backend = MemoryBackend::demo();
        let opened = backend.open(Source::Bytes(b"%PDF-fake"), None).unwrap();
        let request = RenderRequest::new(PageIndex::new(99), Scale::IDENTITY, Rotation::None);
        assert!(matches!(
            opened.raster.render(&request),
            Err(CoreError::PageOutOfRange { .. })
        ));
    }

    #[test]
    fn backend_signals_password_requirement() {
        let backend = MemoryBackend::demo().with_password("secret");
        assert!(matches!(
            backend.open(Source::Bytes(b"%PDF-fake"), None),
            Err(CoreError::PasswordRequired)
        ));
        assert!(matches!(
            backend.open(Source::Bytes(b"%PDF-fake"), Some("nope")),
            Err(CoreError::IncorrectPassword)
        ));
        assert!(backend.open(Source::Bytes(b"%PDF-fake"), Some("secret")).is_ok());
    }

    #[test]
    fn text_layer_supports_search_and_ranges() {
        let backend = MemoryBackend::demo();
        let opened = backend.open(Source::Bytes(b"%PDF-fake"), None).unwrap();
        let text = opened.text.as_ref().unwrap();
        let page = text.page_text(PageIndex::new(0)).unwrap();
        assert_eq!(page.text, "Hello world\nSecond line");
        assert_eq!(page.word_range_at(7), TextRange::new(6, 11));

        let hits = text.search(&SearchQuery::new("page")).unwrap();
        assert_eq!(hits.len(), 2, "第 2、3 页各命中一次（默认不区分大小写）");
        assert_eq!(hits[0].page, PageIndex::new(1));
        assert!(!hits[0].rects.is_empty());

        let limited =
            text.search(&SearchQuery { limit: Some(1), ..SearchQuery::new("page") }).unwrap();
        assert_eq!(limited.len(), 1, "limit 必须生效");
    }
}
