//! # pdfcore —— 阅读器接口层（MVP）
//!
//! 本 crate 只包含 **trait、枚举与领域数据**，不含任何 PDF 解析、渲染或 UI 代码。
//! 它定义了"一个 PDF 阅读器由哪些可替换的零件拼成"，因此：
//!
//! * **后端可替换**：PDFium（`pdfium-render`）、hayro、纯测试用内存后端都实现同一组 trait；
//! * **UI 可替换**：egui 之外换成 iced / Slint / 甚至 CLI，只要能调用 [`DocumentSession`]；
//! * **可测试**：不依赖真实 PDF 文件与 GPU，`cargo test` 即可验证布局、选择、调度逻辑。
//!
//! ## 分层与依赖方向
//!
//! ```text
//!   ui（egui）──▶ pdfcore（trait + 领域模型）◀── backend-pdfium（pdfium-render）
//!                        ▲
//!                        └── testkit（内存假后端，仅测试/演示用）
//! ```
//!
//! 依赖只允许指向 `pdfcore`，`pdfcore` 不依赖任何一方。
//!
//! ## 零件清单（把阅读器"拼"起来的顺序）
//!
//! 1. [`PdfBackend`]：拿到一个已打开文档 [`OpenedDocument`]（含文档、光栅器、可选的文本层）；
//! 2. [`Document`]：页数 / 页尺寸 / 大纲 / 链接等只读结构信息；
//! 3. [`PageRaster`]：把某一页按给定缩放渲染成位图；
//! 4. [`TextLayer`]：文本层与搜索（后端不支持时为 `None`，UI 据此隐藏搜索框）；
//! 5. [`RenderCache`] + [`RenderScheduler`]：把渲染异步化、可取消、可缓存；
//! 6. [`PageLayout`]：把页尺寸算成带位置的内容布局（纯函数，可单测）；
//! 7. [`TextSelection`]：选区记账（不含文档内容，可单测）；
//! 8. [`DocumentSession`]：上面 7 个零件的装配点，UI 只需要跟它打交道。
//!
//! ## 最小装配示例（见 `crates/pdfcore/tests/assemble.rs`）
//!
//! ```no_run
//! use pdfcore::prelude::*;
//! # use std::sync::Arc;
//! # use pdfcore::testkit::{ImmediateScheduler, MemoryBackend, MemoryCache};
//! # fn demo() -> Result<()> {
//! // 1) 后端：真实场景是 pdfium 后端，这里用内存假后端（零依赖、零文件）
//! let backend = MemoryBackend::demo();
//! let opened = backend.open(Source::Bytes(b"%PDF-fake"), None)?;
//!
//! // 2) 调度与缓存：真实场景是后台线程 + LRU，这里同步执行便于测试
//! let cache = Arc::new(MemoryCache::new());
//! let scheduler = Arc::new(ImmediateScheduler::new(opened.raster.clone()));
//!
//! // 3) 装配：布局引擎 + 选区模型 + 会话
//! let mut session = DocumentSession::new(
//!     opened,
//!     cache,
//!     scheduler,
//!     Box::new(VerticalLayout),
//!     Box::new(RangeSelection::new()),
//! )?;
//!
//! // 4) UI 每帧要做的事：发命令 → 取结果
//! let mut events = Vec::new();
//! session.handle(ViewerCommand::ResizeViewport(Size::new(800.0, 900.0)), &mut events);
//! session.handle(ViewerCommand::RequestVisible, &mut events);
//! session.pump(&mut events);
//! # Ok(())
//! # }
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cache;
pub mod document;
pub mod error;
pub mod geometry;
pub mod ids;
pub mod image;
pub mod layout;
pub mod render;
pub mod tiling;
pub mod scheduler;
pub mod session;
pub mod store;
pub mod testkit;
pub mod text;

/// 常用类型的一次性导入。
pub mod prelude {
    // 运行期"电池"：默认的调度器与缓存实现（纯 std，可直接用于桌面 app）
    pub use crate::cache::{LruCache, DEFAULT_CAPACITY_BYTES};
    pub use crate::scheduler::WorkerScheduler;
    pub use crate::document::{
        Capabilities, Destination, Document, DocumentInfo, Link, NavigationIndex, OpenedDocument,
        OutlineNode, PdfBackend, Permissions, Source,
    };
    pub use crate::error::{CoreError, Result};
    pub use crate::geometry::{Insets, Point, Rect, Rotation, Size, Vec2};
    pub use crate::ids::{JobId, PageIndex};
    pub use crate::image::{PixelFormat, RasterImage};
    pub use crate::layout::{
        DocumentLayout, LayoutParams, PageGeometry, PageLayout, PageLayoutMode, PagePlacement,
        ReadingDirection, ScrollAnchor, VerticalLayout, Viewport,
    };
    pub use crate::render::{
        CancelToken, CacheStats, PageRaster, RenderCache, RenderEvent, RenderKey, RenderPriority,
        RenderRegion, RenderRequest, RenderScheduler, Scale, TileSpec, TilingPolicy,
    };
    pub use crate::tiling::{
        render_scale_for, band_for, reuse_is_enough, render_scale_for_band, TileGrid,
        DEFAULT_OVERSCAN, TARGET_TILE_PX,
    };
    pub use crate::session::{DocumentSession, SessionEvent, SidebarTab, ViewerCommand, ViewerState};
    pub use crate::store::{AppSettings, KeyValueStore, RecentFiles};
    pub use crate::text::{
        Highlight, HighlightKind, PageText, RangeSelection, SearchHit, SearchQuery, Selection,
        SelectionGranularity, TextChar, TextLayer, TextPosition, TextRange, TextSelection, TextSpan,
    };
}
