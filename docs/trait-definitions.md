# trait 具体定义（**定稿候选 v3**）

> 配套：[architecture.md](architecture.md)、[traits.md](traits.md)、[data-flow.md](data-flow.md)、[decision-log.md](decision-log.md)
> **v3 并入你批复的 8 条决定**（批量页几何 / 分块预渲染 / 搜索同步 / `&mut self` 例外 / 滚动锚点 / 白底占位 / 导航索引预解析）。文中 `▲v3` 标记本次变更处，便于复核。
> 全文遵循：`&self` 接收者、对象安全（可用 `Arc<dyn T>`）、返回自有数据、能力缺失用 `Option`/`Unsupported` 表达。

---

## 0. 先回答你的示例：为什么不是 `type Document`

你的示例：

```rust
trait PdfBackend {
    type Document;
    fn open(path: Path) -> Option<Document>;
}
```

其中 4 个点我做了不同选择：

| 你的写法 | 我的写法 | 原因 |
| --- | --- | --- |
| `type Document;`（关联类型） | 返回 `Arc<dyn Document>` | 关联类型让 trait **无法作为 trait object 使用**（除非写 `dyn PdfBackend<Document = X>`），"运行期替换后端"的泛型会传染整条调用链 |
| `fn open(...)`（无 `&self`） | `fn open(&self, ...)` | 静态方法无法承载实例状态（PDFium 句柄、字体配置、内存上限），也放不进 `Vec<Box<dyn PdfBackend>>` |
| `-> Option<Document>` | `-> Result<OpenedDocument>` | `Option` 丢失失败原因：**加密需密码**、密码错误、文件损坏、IO 失败在 UI 上处理完全不同 |
| `path: Path` | `source: Source<'_>` | `Path` 是 unsized，按值传参编译不过；枚举后能同时支持路径与内存字节 |

**关联类型 vs trait object**：

```rust
// A. 关联类型风格：静态分发、零成本，但泛型向上传染
trait PdfBackendA { type Document; type Raster; type Text;
    fn open(&self, path: &Path) -> Result<(Self::Document, Self::Raster, Self::Text)>; }
struct App<B: PdfBackendA> { backend: B, /* …整棵 UI 都带上 B… */ }
// ✗ 无法写 Vec<Box<dyn PdfBackendA>>；✗ 单测替身会改变 App 的类型

// B. dyn 风格（本设计）：运行期可替换，签名稳定
trait PdfBackendB { fn open(&self, source: Source<'_>, password: Option<&str>) -> Result<OpenedDocument>; }
struct App { backend: Arc<dyn PdfBackendB>, /* 与具体后端无关 */ }
// ✓ Vec<Arc<dyn PdfBackendB>> 可用；✓ 测试注入内存后端，App 类型不变
```

**什么时候用关联类型**：只有一个确定实现、且要内联/零虚调用时。本项目明确要"多后端 + 测试替身 + 将来可能加 hayro"，故选 dyn。代价是每次调用一次虚表跳转（对毫秒级渲染可忽略）。

---

## 1. 类型词典

### 1.1 几何与标识

```rust
/// 二维点（含义由所在坐标系决定，见 architecture.md §5）。
pub struct Point { pub x: f32, pub y: f32 }        // const ORIGIN; new; translate; distance
/// 二维向量 / 偏移。
pub struct Vec2  { pub x: f32, pub y: f32 }        // const ZERO; new; length; scale
/// 尺寸。
pub struct Size  { pub width: f32, pub height: f32 }  // const ZERO; area; is_empty; scale; rotated(Rotation)
/// 轴对齐矩形（左闭右开）。
pub struct Rect  { pub x: f32, pub y: f32, pub width: f32, pub height: f32 }
//   min/max/center/right/bottom/size/is_empty/contains/intersects/intersection/union/translate/inset
/// 四边留白。
pub struct Insets { pub top: f32, pub right: f32, pub bottom: f32, pub left: f32 }  // ZERO; all; new; horizontal; vertical

/// 页面旋转（PDF 的 /Rotate 与用户旋转共用）。
pub enum Rotation { None, Clockwise90, Clockwise180, Clockwise270 }
//   degrees() / from_degrees(i32) / swaps_dimensions() / compose(other) / inverse()
//   apply_to_size(Size)
//   map_point(pdf_pt, 未旋转页尺寸) -> 旋转后页内坐标   ← 含 y 轴翻转（0°/180° 也要翻！）
//   unmap_point(...) 为其逆运算

/// 页索引（0-based；Display 输出 1-based）。
pub struct PageIndex(pub u32);   // FIRST; new; get; display_number; next; prev
/// 渲染任务号。
pub struct JobId(pub u64);
```

### 1.2 文档、能力与导航索引 ▲v3

```rust
/// 打开来源。
pub enum Source<'a> { Path(&'a Path), Bytes(&'a [u8]) }

/// 权限（PDF 加密字典；未加密文档全为 true）。
pub struct Permissions { pub print: bool, pub copy_text: bool, pub modify: bool, pub annotate: bool }

/// 文档元数据与统计。
pub struct DocumentInfo {
    pub page_count: u32,
    pub title: Option<String>, pub author: Option<String>, pub subject: Option<String>,
    pub keywords: Option<String>, pub producer: Option<String>,
    pub encrypted: bool,
    pub permissions: Permissions,
    pub file_size_bytes: Option<u64>,
}

/// ▲v3 跳转目标（大纲与链接共用）—— **总是已解析**：
/// 后端在 open 时把 PDF 的命名目标（`/Named`、`/Dests` 字典里的名称）解析成具体页，
/// UI 永远不会拿到"待解析"的目标，因此点击链接时不需要再访问后端。
/// （v2 里的 `Named(String)` 变体已删除，见 decision-log D-08。）
pub enum Destination {
    Page { page: PageIndex, offset: Option<Point> },  // 页内落点，PDF 页面空间
    Uri(String),                                      // 外部链接
}

/// 页内超链接热区（PDF 页面空间）。
pub struct Link { pub rect: Rect, pub destination: Destination }

/// 大纲节点（递归树；destination 同样已解析）。
pub struct OutlineNode {
    pub title: String,
    pub destination: Option<Destination>,
    pub children: Vec<OutlineNode>,
}

/// ▲v3 导航索引：open 时**一次性解析并合并存储**的全部导航数据。
///
/// 为什么合并成一个结构：
/// * 点击链接/大纲时不再访问后端（纯内存查找，O(1)）；
/// * 页内链接可以随页面一起预取，缩略图侧边栏也能一次拿到；
/// * 命名目标解析失败时不会静默变成"点了没反应"，而是被记进 `unresolved` 供诊断。
pub struct NavigationIndex {
    /// 大纲树（destination 已解析）。
    pub outline: Vec<OutlineNode>,
    /// 逐页链接热区；下标 = 页号，越界即该页无链接。
    pub links_by_page: Vec<Vec<Link>>,
    /// 名称 → 目标（PDF 交叉引用、命名目标），保留供"命名的目录项"与调试使用。
    pub named_destinations: HashMap<String, Destination>,
    /// 无法解析的名称；仅用于诊断（UI 可忽略）。
    pub unresolved: Vec<String>,
}

/// 后端能力声明：UI 据此决定显示哪些入口。
pub struct Capabilities {
    pub text_layer: bool,
    pub search: bool,
    pub encryption: bool,
    pub outline: bool,
    pub links: bool,
    pub max_concurrency: u8,   // PDFium 必须为 1
}
//  const minimal(); const full(); Default = minimal()

/// 一次 open 的成果：装配包。
pub struct OpenedDocument {
    pub document: Arc<dyn Document>,
    pub raster:   Arc<dyn PageRaster>,
    pub text:     Option<Arc<dyn TextLayer>>,   // 可选零件：None = 该后端没有文本层
    pub capabilities: Capabilities,
}
//  page_count(); has_text_layer(); text_layer() -> Option<&Arc<dyn TextLayer>>
```

### 1.3 渲染（含分块）▲v3

```rust
/// 缩放：设备像素 / point。
pub struct Scale(pub f32);
//  const IDENTITY(1.0); from_zoom(f32); from_dpi(f32); zoom(); dpi(); is_valid()
//  quantize() -> 量化到 1/8 步长（缓存键用，避免滚轮缩放摧毁缓存）

/// ▲v3 瓦片边长（设备像素）。做成常量而非配置：它是缓存键的一部分，
/// 变动会导致全部瓦片失效，没有"运行期可调"的必要。
pub const TILE_SIZE: u16 = 512;

/// ▲v3 瓦片坐标（列、行），网格由该页的**像素尺寸**决定。
pub struct TileSpec { pub col: u16, pub row: u16 }
//  grid_for(displayed_size: Size) -> (u16, u16)          ⌈宽/512⌉ × ⌈高/512⌉
//  pixel_rect(displayed_size: Size) -> Rect              该瓦片在"旋转后页位图"中的像素矩形
//  page_rect(page_size: Size, rotation: Rotation) -> Rect 该瓦片在 PDF 页面空间的裁剪矩形
//                                                         （渲染请求携带它，避免后端自己推算）

/// 缓存键：页 + 量化缩放 + 旋转 + 瓦片坐标（整页渲染时为 None）。
pub struct RenderKey {
    pub page: PageIndex,
    pub scale_milli: u32,      // 量化缩放 ×1000（f32 不能做 Hash/Eq）
    pub rotation: Rotation,
    pub tile: Option<TileSpec>,   // ▲v3
}
//  new(page, scale, rotation)（整页）; tiled(page, scale, rotation, tile); scale() -> Scale

/// 渲染优先级（rank() 越小越优先）。
pub enum RenderPriority { Interactive, Visible, Prefetch, Background }

/// 取消令牌（可克隆，共享同一标志）。
pub struct CancelToken(Arc<AtomicBool>);   // new(); cancel(); is_cancelled()

/// 一次渲染请求。
pub struct RenderRequest {
    pub page: PageIndex,
    pub scale: Scale,
    pub rotation: Rotation,        // 必须是布局给出的最终旋转
    pub tile: Option<TileSpec>,    // ▲v3 None = 整页；Some = 只渲染该瓦片
    pub priority: RenderPriority,
    pub cancel: CancelToken,
}
//  new(page, scale, rotation)                     → 整页（默认 Visible）
//  tiled(page, scale, rotation, tile)             → 单瓦片
//  with_priority(p); key() -> RenderKey

/// ▲v3 何时分块：按"页的像素面积"阈值决定，避免小页被碎片化。
pub enum TilingPolicy {
    /// 从不分块（整页一张位图）。
    Never,
    /// 页的像素面积超过阈值时才分块（默认 4 MP，见 D-15；v3 设计时为 16 MP）。
    AbovePixels(u32),
    /// 总是分块。
    Always,
}
//  Default = AbovePixels(16_000_000)

/// 位图格式（PDFium 原生 BGRA；egui 要 RGBA）。
pub enum PixelFormat { Rgba8, Bgra8, Gray8 }   // bytes_per_pixel(); has_alpha()

/// 一页/一块瓦片的渲染结果（不可变，像素在 Arc<[u8]> 中共享）。
pub struct RasterImage { pub width: u32, pub height: u32, pub format: PixelFormat, data: Arc<[u8]> }
//  new(..) -> Option<Self>（长度不符返回 None）; expected_len(); stride(); size(); data(); byte_len()
//  to_rgba8() -> Vec<u8>    ← 全项目唯一的格式转换点

/// 缓存统计。
pub struct CacheStats { pub entries: usize, pub bytes: u64, pub hits: u64, pub misses: u64 }

/// 调度器 → 会话的事件。
pub enum RenderEvent {
    Rendered  { key: RenderKey, image: Arc<RasterImage> },
    Failed    { page: PageIndex, error: CoreError },
    Cancelled { page: PageIndex },
}
```

### 1.4 文本

```rust
/// 页内字符区间，半开 [start, end)，单位是**字符**（不是字节）。
pub struct TextRange { pub start: u32, pub end: u32 }
//  new(start, end)（自动归一化）; empty(); len(); is_empty(); contains(i); union(other)

/// 文档中的字符位置。
pub struct TextPosition { pub page: PageIndex, pub index: u32 }

/// 选区（anchor 可晚于 focus，允许反向拖拽）。
pub struct Selection { pub anchor: TextPosition, pub focus: TextPosition }
//  normalized() -> (TextPosition, TextPosition); is_empty(); page_ranges() -> Vec<(PageIndex, TextRange)>

/// 选择粒度。
pub enum SelectionGranularity { Char, Word, Line }

/// 一个字符及其位置（包围盒在 PDF 页面空间）。
pub struct TextChar { pub ch: char, pub bbox: Rect, pub origin: Point, pub span: u32 }

/// 样式分段。
pub struct TextSpan { pub range: TextRange, pub font_size: f32 }

/// 一页文本层。不变量：text.chars().count() == chars.len()。
pub struct PageText { pub page: PageIndex, pub text: String, pub chars: Vec<TextChar>, pub spans: Vec<TextSpan> }
//  len(); is_empty(); chars_in(range); text_for_range(range); rects_for_range(range)（按行合并）
//  range_for_rect(rect); hit_test(pdf_point); word_range_at(i); line_range_at(i)
//  range_at(i, granularity)   ← 唯一负责"什么叫一个单词"的地方

/// 高亮类型（选区与搜索共用）。
pub enum HighlightKind { Selection, SearchMatch, ActiveSearchMatch }

/// 一块高亮。
pub struct Highlight { pub page: PageIndex, pub rects: Vec<Rect>, pub kind: HighlightKind }

/// 搜索条件。
pub struct SearchQuery {
    pub needle: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub start_page: Option<PageIndex>,
    pub limit: Option<usize>,
}

/// 一条命中。
pub struct SearchHit { pub page: PageIndex, pub range: TextRange, pub rects: Vec<Rect> }
```

### 1.5 布局与滚动锚点 ▲v3

```rust
/// 每页几何：原始尺寸 + 自带 /Rotate（**逐页**，不能全局一个值）。
pub struct PageGeometry { pub size: Size, pub rotation: Rotation }
//  effective_rotation(user_rotation); displayed_size(user_rotation)

/// 排布模式。
pub enum PageLayoutMode { Continuous, SinglePage }
/// 阅读方向（MVP 不生效，为对开/RTL 预留）。
pub enum ReadingDirection { LeftToRight, RightToLeft }

/// 布局参数。
pub struct LayoutParams {
    pub mode: PageLayoutMode,
    pub direction: ReadingDirection,
    pub scale: Scale,
    pub page_gap: f32,
    pub margin: Insets,
    pub user_rotation: Rotation,   // 叠加在每页 /Rotate 之上
}

/// 视口。
pub struct Viewport { pub size: Size, pub scroll: Vec2 }   // scroll = 视口左上角在内容空间中的坐标
//  content_rect() -> Rect
//  ▲v3 center_content_point() -> Point   视口中心在内容空间中的坐标（用于捕获锚点）

/// ▲v3 滚动锚点：视口中心对应的"页内位置"（PDF 页面空间）。
/// 布局重算（缩放、旋转、切换排布、窗口缩放）前后用它把视野中心钉回原处。
pub struct ScrollAnchor { pub page: PageIndex, pub page_point: Point }

/// 某一页的摆放结果 —— **坐标系换算的唯一入口**。
pub struct PagePlacement {
    pub page: PageIndex,
    pub rect: Rect,          // 内容空间
    pub scale: Scale,        // 渲染该页应使用的缩放
    pub rotation: Rotation,  // 最终旋转 = 页 /Rotate ⊕ 用户旋转
    pub page_size: Size,     // 未旋转的原始尺寸（换算需要）
}
//  displayed_size()
//  page_to_content(pdf_point) -> Point       PDF 页面空间 → 内容空间
//  content_to_page(content_point) -> Point   内容空间 → PDF 页面空间
//  rect_to_content(pdf_rect) -> Rect         四角取外接矩形（对 90°/270° 安全）
//  pixel_size() -> Size                      旋转后尺寸 × scale，取整为像素（分块网格要用）

/// 整篇文档的布局结果。
pub struct DocumentLayout { pub content_size: Size, pub placements: Vec<PagePlacement> }
//  placement(page) -> Option<&PagePlacement>; page_count()
```

### 1.6 错误与 UI 状态 / 事件

```rust
/// 统一错误（含"非故障"分支）。
pub enum CoreError {
    Io(String),
    PasswordRequired,        // 非故障：请 UI 弹密码框后重试
    IncorrectPassword,       // 非故障：提示重试
    CorruptDocument(String),
    PageOutOfRange { page: PageIndex, page_count: u32 },
    Unsupported(&'static str),  // 非故障：能力缺失，UI 静默降级
    Cancelled,               // 非故障：不得弹提示
    Backend(String),
    Internal(String),
}
pub type Result<T> = std::result::Result<T, CoreError>;

/// UI 状态（纯数据，不含文档内容）。
pub struct ViewerState {
    pub zoom: f32,
    pub layout_mode: PageLayoutMode,
    pub reading_direction: ReadingDirection,
    pub current_page: PageIndex,
    pub user_rotation: Rotation,
    pub sidebar: SidebarTab,
    pub selection_granularity: SelectionGranularity,
    pub page_gap: f32,
    pub margin: Insets,
}

pub enum SidebarTab { None, Outline, Thumbnails, Search }

/// UI → 会话。
pub enum ViewerCommand {
    ResizeViewport(Size), ScrollBy(Vec2), SetScroll(Vec2),
    GoToPage(PageIndex), NextPage, PrevPage,
    SetZoom(f32), ZoomIn, ZoomOut, FitWidth,
    SetLayoutMode(PageLayoutMode), SetUserRotation(Rotation), RotateClockwise,
    SetSidebar(SidebarTab), SetSelectionGranularity(SelectionGranularity),
    SelectBegin(TextPosition), SelectExtend(TextPosition), SelectClear, SelectAll, CopySelection,
    RequestVisible,
}

/// 会话 → UI。
pub enum SessionEvent {
    ViewportChanged, LayoutChanged, ScrollChanged(Vec2), CurrentPageChanged(PageIndex),
    SelectionChanged, SidebarChanged(SidebarTab),
    PageRendered(RenderKey),
    PageRenderFailed { page: PageIndex, error: CoreError },
    PageTextLoaded(PageIndex),
    CopyToClipboard(String),   // 平台能力交回 UI
    Error(CoreError),
}
```

---

## 2. 九个 trait 的完整定义

### 2.1 `PdfBackend` —— 后端工厂

**数据形态**：`Source<'_>` + 可选密码 → `OpenedDocument`（三个 `Arc<dyn …>` + 能力声明）。

```rust
/// **后端工厂**：本接口层中唯一"知道某个具体 PDF 库存在"的抽象。
///
/// ## 目的
/// 可替换性的根节点。UI 只持有它产出的 [`OpenedDocument`]，因此换后端（pdfium → hayro）、
/// 或换成测试用的内存后端，都不需要改动 UI 与会话代码。
///
/// ## 实现者义务
/// * 加密且未给密码 → [`CoreError::PasswordRequired`]；密码错误 → [`CoreError::IncorrectPassword`]。
///   两者都是**非故障**分支：UI 弹密码框后重试，而不是弹错误提示。
/// * 不在 `open` 之后继续持有文档所有权：文档生命周期由返回值的 `Arc` 决定，
///   因此"关闭文档"= drop，不需要 `close()`。
/// * 必须提供 [`PageRaster`]；[`TextLayer`] 可缺省，此时必须把
///   [`Capabilities::text_layer`] 声明为 `false`，否则 UI 会显示无效入口。
/// * ▲v3 **打开时预解析**：页几何表与 [`NavigationIndex`] 应在 `open` 内一次性备好
///   （命名目标、逐页链接、页尺寸与 `/Rotate`），使运行期查询是纯内存操作。
pub trait PdfBackend: Send + Sync {
    /// 后端名称（"pdfium" / "hayro" / "memory"），用于"关于"与日志。
    fn name(&self) -> &'static str;

    /// 声明能力，UI 据此决定显示哪些功能入口。
    fn capabilities(&self) -> Capabilities;

    /// 打开文档。同一实例可并发打开多个文档（多标签/多窗口）。
    fn open(&self, source: Source<'_>, password: Option<&str>) -> Result<OpenedDocument>;
}
```

**谁实现**：`PdfiumBackend`、`MemoryBackend`（testkit）、未来 `HayroBackend`。

---

### 2.2 `Document` —— 只读文档结构 ▲v3

**数据形态**：无输入 → 页几何表 / 导航索引（两者都是 O(1) 的 `Arc` 克隆）。

```rust
/// **文档结构**：只读、可并发访问的文档信息。
///
/// ## 目的
/// 只覆盖 PDF 里**不变的部分**（页数、页几何、元数据、导航索引），使布局与会话无需知道
/// PDFium 存在；单测也能构造"3 页、尺寸写死"的假文档。页面**像素**归 [`PageRaster`]，
/// 页面**文字**归 [`TextLayer`] —— 三者刻意分开，因为它们的性能特征与可用性完全不同。
///
/// ## 实现者义务
/// * ▲v3 `page_geometries()` 与 `navigation()` 必须在 `open` 时备好并**缓存**，
///   调用只做 `Arc` 克隆（O(1)），因此 UI 每帧调用也不会有跨 FFI 开销；
///   惰性实现请用 `OnceLock`，不要在每次调用时重新解析。
/// * 返回的 `PageGeometry::size` / `rotation` 是**未旋转**的原始值；旋转由布局叠加。
/// * 导航索引不含"未解析"的目标：解析不了的名称归入
///   [`NavigationIndex::unresolved`]，不要泄漏给 UI 当作可点击项。
pub trait Document: Send + Sync {
    /// 元数据与页数。
    fn info(&self) -> &DocumentInfo;

    /// ▲v3 **批量**页几何（尺寸 + 逐页 `/Rotate`）。打开时已备好，O(1) 返回。
    ///
    /// 之所以是批量而不是"逐页查询"：布局要一次性拿到全部页尺寸才能算出内容总高度与
    /// 滚动范围；逐页跨 FFI 查询会把打开大文档变成 N 次调用。
    fn page_geometries(&self) -> Result<Arc<[PageGeometry]>>;

    /// 单页尺寸；默认实现查批量表（越界返回 `PageOutOfRange`）。
    /// 频繁路径请直接用 `page_geometries()`，避免重复的 `Arc` 克隆与边界检查。
    fn page_size(&self, page: PageIndex) -> Result<Size> {
        let geometries = self.page_geometries()?;
        geometries
            .get(page.get() as usize)
            .map(|geometry| geometry.size)
            .ok_or(CoreError::PageOutOfRange { page, page_count: geometries.len() as u32 })
    }

    /// 单页自带的 `/Rotate`；默认实现查批量表，缺省为不旋转。
    fn page_rotation(&self, page: PageIndex) -> Result<Rotation> {
        let geometries = self.page_geometries()?;
        Ok(geometries.get(page.get() as usize).map(|geometry| geometry.rotation).unwrap_or(Rotation::None))
    }

    /// ▲v3 导航索引：大纲 + 逐页链接 + 命名目标，**打开时一次性解析并合并存储**。O(1) 返回。
    ///
    /// 取代了 v2 的 `outline()` 与 `links(page)`：点击链接/大纲时不再访问后端。
    fn navigation(&self) -> Result<Arc<NavigationIndex>>;
}
```

**谁实现**：`PdfiumDocument`、`MemoryDocument`。

---

### 2.3 `PageRaster` —— 光栅化（最核心的替换缝）▲v3

**数据形态**：`&RenderRequest`（页 + 缩放 + 旋转 + **可选瓦片** + 优先级 + 取消令牌）→ `Arc<RasterImage>`。

```rust
/// **光栅化能力**：把一页（或一页中的一块瓦片）按请求渲染成位图。
///
/// ## 目的
/// 把"PDF 后端"压缩成**一个方法**：
/// * PDFium 适配层只需实现它，缓存/优先级/线程都在上层；
/// * 测试可塞一个"按页号画纯色"的假实现，完全不碰真实 PDF；
/// * 换 hayro 或自研渲染器时，改动范围仅限此 trait 的一个实现。
///
/// ## 实现者义务
/// * `Send + Sync`，但**并发上限由 [`Capabilities::max_concurrency`] 声明**，调度器会遵守；
///   PDFium 必须声明 1。
/// * 渲染前若 [`CancelToken::is_cancelled`] 为真，立即返回 [`CoreError::Cancelled`]
///   （PDFium 无法中断进行中的渲染，取消只在任务开始前有效）。
/// * 整页渲染（`tile == None`）：位图尺寸 = "旋转后页尺寸 × scale"，允许 ±1 像素取整误差。
/// * ▲v3 瓦片渲染（`tile == Some(spec)`）：**只渲染该瓦片**，位图尺寸 =
///   `spec.pixel_rect(displayed_size)` 的尺寸（最后一列/行不足 [`TILE_SIZE`]）。
///   必须用 `spec.page_rect(page_size, rotation)` 作为裁剪矩形**直接渲染该区域**，
///   **不得先整页渲染再裁剪** —— 否则分块就失去了省内存的意义。
///   （pdfium 适配层用位图尺寸 + 渲染原点即可实现：`PdfRenderConfig::set_origin()`。）
/// * 瓦片必须对齐整数像素边界，拼接时不得重叠或留缝。
/// * 如实填写 [`PixelFormat`]（PDFium 原生 BGRA，不要假装 RGBA）。
pub trait PageRaster: Send + Sync {
    /// 渲染一页或一页中的一块瓦片。
    fn render(&self, request: &RenderRequest) -> Result<Arc<RasterImage>>;
}
```

**刻意的缺失**：没有 `render_thumbnail()`（缩略图 = 小 `Scale` 的整页请求，与主视图共享缓存）。

**谁实现**：`PdfiumRaster`、`MemoryRaster`。

---

### 2.4 `RenderCache` —— 位图缓存

**数据形态**：`&RenderKey` → `Option<Arc<RasterImage>>`；写入 `RenderKey + Arc<RasterImage>`。

```rust
/// **位图缓存**：`RenderKey` → 位图（瓦片也是位图，因此同一套缓存同时服务整页与瓦片）。
///
/// ## 目的
/// 把"缓存策略"独立出来：MVP 用几十行 `HashMap` 先跑通，之后换 LRU / 内存上限 / 落盘，
/// **不改任何调用方**；单测里还能用"永不命中"的缓存验证重渲染逻辑。
///
/// ## 实现者义务
/// * `get` 被 UI 线程每帧调用多次，必须 O(1) 且不长时间持锁。
/// * 不得返回尺寸/格式与键不匹配的位图。
/// * ▲v3 瓦片键与整页键可能同时存在；`remove_page` 必须把该页的**两种键**都清掉。
pub trait RenderCache: Send + Sync {
    /// 查询；未命中返回 `None`。
    fn get(&self, key: &RenderKey) -> Option<Arc<RasterImage>>;

    /// 写入（同键覆盖）。`image` 为 `Arc`，与 UI 共享像素，不产生拷贝。
    fn insert(&self, key: RenderKey, image: Arc<RasterImage>);

    /// 丢弃某一页的所有缩放版本与所有瓦片。
    fn remove_page(&self, page: PageIndex);

    /// 清空。
    fn clear(&self);

    /// 统计信息（状态栏展示与调优）；默认空统计。
    fn stats(&self) -> CacheStats {
        CacheStats::default()
    }
}
```

**谁实现**：`LruCache`（pdfcore 自带，LRU + 内存上限）、`MemoryCache`（testkit，无淘汰）。

---

### 2.5 `RenderScheduler` —— 调度（UI 面向的唯一渲染入口）

**数据形态**：`RenderRequest` → `JobId`；`drain_events(max)` → `Vec<RenderEvent>`。

```rust
/// **渲染调度**：UI 面向的唯一渲染入口。
///
/// ## 目的
/// 让 UI 完全不必知道"谁在渲染、几个线程、PDFium 是否线程安全"：
/// * `submit` 非阻塞，UI 每帧都可放心调用；
/// * 结果用 `drain_events` 轮询取回，与 egui "每帧拉取"模型天然契合；
/// * 真实实现 = 后台线程 + 优先级队列；测试实现可**同步渲染**，
///   于是「UI → 会话 → 调度 → 后端 → 缓存 → UI」能确定性地在单测里跑通。
///
/// ## 实现者义务
/// * `submit` 立即返回，不得同步等待（同步实现仅用于测试）。
/// * 必须自行去重：同一 [`RenderKey`]（含瓦片坐标）已在队列或飞行中时忽略重复提交。
/// * 必须遵守 [`Capabilities::max_concurrency`]；对 PDFium 只能是 1（即单工作线程）。
/// * ▲v3 分块后同页会出现多个任务，优先级相同者按"距视口中心由近到远"或
///   "行优先"顺序出队均可，但**不得**让整页任务插到瓦片任务之前（二者互斥，不会同时存在）。
pub trait RenderScheduler: Send + Sync {
    /// 提交任务；返回任务号用于取消。
    fn submit(&self, request: RenderRequest) -> JobId;

    /// 取消指定任务。
    fn cancel(&self, job: JobId);

    /// 取消某一页的所有任务（含该页所有瓦片）。
    fn cancel_page(&self, page: PageIndex);

    /// 取消全部任务（关闭文档、缩放/旋转导致缓存键整体失效）。
    fn cancel_all(&self);

    /// 取出至多 `max` 条已完成事件（非阻塞）。UI 每帧调用一次。
    fn drain_events(&self, max: usize) -> Vec<RenderEvent>;

    /// 排队中 + 正在执行的任务数。
    fn pending(&self) -> usize;
}
```

**谁实现**：`WorkerScheduler`（pdfcore 自带：后台线程 + 优先级队列 + 去重 + 补发取消事件）、
`ImmediateScheduler`（testkit，同步，用于确定性测试）。

---

### 2.6 `TextLayer` —— 文本层与搜索（可选零件）

**数据形态**：`PageIndex` → `Arc<PageText>`；`&SearchQuery` → `Vec<SearchHit>`。

```rust
/// **文本层能力**：字符级提取与全文搜索。
///
/// ## 目的
/// 1. 让"选中 / 复制 / 搜索"与具体 PDF 库解耦；
/// 2. **让能力可缺省**：只做渲染的后端（如 hayro）直接让 `OpenedDocument::text` 为 `None`，
///    UI 据此隐藏搜索与复制入口 —— 不需要任何 `if backend == "hayro"` 分支。
///
/// ## 实现者义务
/// * 必须保证 [`PageText`] 的"字符一一对应"不变量：
///   `text.chars().count() == chars.len()`（缺位置的字符也要带零包围盒占位）。
/// * 包围盒必须在 **PDF 页面空间**（左下原点、point）。
/// * 文本提取可能很慢，实现应自带缓存或依赖会话层的页文本缓存。
/// * ▲v3 `search` 是**同步**方法（已定稿，不做异步版本）：
///   耗时由调用方承担 —— MVP 里 UI 若担心卡顿，可自行放到自己的后台任务里跑，
///   接口层不为此引入异步复杂度。
pub trait TextLayer: Send + Sync {
    /// 取某页文本层。
    fn page_text(&self, page: PageIndex) -> Result<Arc<PageText>>;

    /// 全文搜索。
    ///
    /// 默认返回 [`CoreError::Unsupported`]：使"只提取、不搜索"的后端只需实现一个方法；
    /// UI 通过 [`Capabilities::search`] 提前知道不要展示搜索框。
    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>> {
        Err(CoreError::Unsupported("全文搜索"))
    }
}
```

**谁实现**：`PdfiumTextLayer`、`MemoryTextLayer`。

---

### 2.7 `TextSelection` —— 选区模型（不含文档内容）

**数据形态**：`TextPosition` → 内部两个位置；查询时用"取页文本"闭包换出 `Vec<Highlight>` / `String`。

```rust
/// **选区模型**：鼠标交互 → 选中区间 → 高亮矩形 / 可复制文本。
///
/// ## 目的
/// 把"选中状态机"从 UI 事件与文档内容中剥离：
/// * 它**不含文档内容**，只记两个字符位置，因此可 100% 单测（含反向拖拽、跨页、全选）；
/// * UI 只负责把鼠标坐标交给 [`PageText::hit_test`]，再把结果交给本 trait；
/// * `highlight` / `selected_text` 有默认实现，实现者最少只需写 5 个方法。
///
/// ## 实现者义务
/// * `begin` 重置选择、`extend` 更新焦点；收到的 `position` 应已由调用方按
///   [`SelectionGranularity`] 展开（见 [`PageText::range_at`]）。
/// * 只需 `Send`（选区属于 UI 线程，但会话可整体被移动到其它线程）。
///
/// ## 关于 `&mut self`（已确认保留）
/// 改选区的 4 个方法用 `&mut self`，是本项目对"全局 `&self` 约定"的**唯一例外**：
/// 选区是纯 UI 状态、不允许并发访问，`&mut self` 让实现方免锁，并把
/// "只有 UI 线程能改选区"写进类型。你已确认接受此例外（decision-log D-04）。
pub trait TextSelection: Send {
    /// 当前粒度。
    fn granularity(&self) -> SelectionGranularity;

    /// 设置粒度（单击/双击/三击时由 UI 调用）。
    fn set_granularity(&mut self, granularity: SelectionGranularity);

    /// 开始新选择。
    fn begin(&mut self, position: TextPosition);

    /// 扩展到新位置（拖拽中反复调用）。
    fn extend(&mut self, position: TextPosition);

    /// 清空选择。
    fn clear(&mut self);

    /// 当前选择；无选择为 `None`。
    fn selection(&self) -> Option<Selection>;

    /// 是否没有有效选择；默认由 `selection()` 推导。
    fn is_empty(&self) -> bool {
        self.selection().map_or(true, |selection| selection.is_empty())
    }

    /// 计算高亮矩形；默认实现按页取区间再调 [`PageText::rects_for_range`]。
    fn highlight(&self, text_of: &dyn Fn(PageIndex) -> Option<Arc<PageText>>) -> Vec<Highlight>;

    /// 取选中文本用于复制；跨页以换行连接。默认实现。
    fn selected_text(&self, text_of: &dyn Fn(PageIndex) -> Option<Arc<PageText>>) -> String;
}
```

**谁实现**：`RangeSelection`（pdfcore 自带）、将来可能的 `MultiSelection`。

---

### 2.8 `PageLayout` —— 布局引擎（纯函数）▲v3

**数据形态**：`&[PageGeometry]` + `&LayoutParams` → `DocumentLayout`；查询时用 `DocumentLayout` + `Viewport` → 页集合 / 坐标。

```rust
/// **布局引擎**：页几何 → 内容空间排布。
///
/// ## 目的
/// * 布局是**纯函数**，不碰 PDF/GPU/UI 状态，可被完整单测 ——
///   "滚动错位""跳页偏了""高亮上下颠倒"绝大多数出在这一层；
/// * 可替换排布方式（纵向连续 / 单页 / 对开 / 横向），会话与 UI 不动；
/// * 用**默认方法**提供滚动高频操作，实现者最少只需实现 `compute` 一个方法。
///
/// ## 实现者义务
/// * `compute` 必须是纯函数（相同输入 → 相同输出）。
/// * `placements` 顺序必须与页序一致且覆盖所有页。
/// * 必须把 `params.user_rotation` 与每页自带 `rotation` 叠加后写进
///   [`PagePlacement::rotation`] —— 渲染请求会直接采用它。
pub trait PageLayout: Send + Sync {
    /// 计算整篇文档的布局。**唯一必须实现的方法。**
    fn compute(&self, pages: &[PageGeometry], params: &LayoutParams) -> DocumentLayout;

    /// 取视口内可见的页。
    ///
    /// 用 `out: &mut Vec` 而非返回 `Vec`：UI 每帧调用，复用缓冲避免每秒 60 次分配。
    fn visible_pages(&self, layout: &DocumentLayout, viewport: &Viewport, out: &mut Vec<PageIndex>);

    /// 命中测试：内容空间的点落在哪一页上。
    fn page_at(&self, layout: &DocumentLayout, content_point: Point) -> Option<PageIndex>;

    /// 跳转到某页时应设置的滚动偏移（默认：页面居中，并夹取到合法范围）。
    fn scroll_to_page(&self, layout: &DocumentLayout, page: PageIndex, viewport_size: Size) -> Vec2;

    /// ▲v3 **保持视野中心不动**：把某个"页内位置"放到视口中心，返回应设置的滚动偏移。
    ///
    /// 默认实现：`placement.page_to_content(page_point)` → 减去视口一半 → 夹取。
    /// 由会话在**任何重算布局**的操作前后调用（缩放、旋转、切换排布、窗口缩放），
    /// 使用户的阅读位置不因布局变化而漂移（decision-log D-05）。
    fn scroll_to_anchor(
        &self,
        layout: &DocumentLayout,
        page: PageIndex,
        page_point: Point,
        viewport_size: Size,
    ) -> Vec2;

    /// 最大滚动偏移。
    fn max_scroll(&self, layout: &DocumentLayout, viewport_size: Size) -> Vec2;

    /// 把滚动偏移夹取到合法范围（避免滚出内容区出现空白）。
    fn clamp_scroll(&self, layout: &DocumentLayout, viewport_size: Size, scroll: Vec2) -> Vec2;
}
```

**谁实现**：`VerticalLayout`（pdfcore 自带：纵向排列 + 水平居中）、将来 `FacingLayout`（对开）。

---

### 2.9 `KeyValueStore` —— 持久化（可选零件）

**数据形态**：`&str` key → `Option<String>`；`&str` key + `&str` value → `Result<()>`。

```rust
/// 极简键值存储。
///
/// ## 目的
/// 把"设置存哪里、什么格式"从会话与 UI 中隔离：桌面实现写配置文件，测试实现用
/// `HashMap`，于是"设置读写"也能单测；同时避免接口层引入 serde/toml/directories 依赖。
///
/// ## 实现者义务
/// * `get` 对不存在的键返回 `None`（不报错）。
/// * `set` 失败（磁盘只读等）返回错误，由 UI 决定提示还是忽略。
pub trait KeyValueStore: Send + Sync {
    /// 读取。
    fn get(&self, key: &str) -> Option<String>;

    /// 写入（覆盖）。
    fn set(&self, key: &str, value: &str) -> Result<()>;

    /// 删除。
    fn remove(&self, key: &str);
}
```

**谁实现**：`FileStore`（app 侧）、`MemoryStore`（testkit）。

---

## 3. 会话对外 API（装配面，v3 增补）

`DocumentSession` 不是 trait，但它是"把上面 9 个零件拼起来"后 UI 唯一持有的对象。v3 因分块与锚点新增了 4 个方法：

```rust
impl DocumentSession {
    // 装配
    pub fn new(
        opened: OpenedDocument,
        cache: Arc<dyn RenderCache>,
        scheduler: Arc<dyn RenderScheduler>,
        layout_engine: Box<dyn PageLayout>,
        selection: Box<dyn TextSelection>,
    ) -> Result<Self>;

    // 每帧
    pub fn handle(&mut self, command: ViewerCommand, events: &mut Vec<SessionEvent>);
    pub fn pump(&mut self, events: &mut Vec<SessionEvent>);

    // 只读访问（UI 绘制用）
    pub fn page_count(&self) -> u32;
    pub fn state(&self) -> &ViewerState;
    pub fn layout(&self) -> &DocumentLayout;
    pub fn viewport(&self) -> &Viewport;
    pub fn placement(&self, page: PageIndex) -> Option<&PagePlacement>;
    pub fn capabilities(&self) -> Capabilities;

    /// ▲v3 单页几何（来自后端批量表的本地副本，O(1)）。
    pub fn page_geometry(&self, page: PageIndex) -> Option<PageGeometry>;

    /// 整页位图（未分块时使用）；分块时返回 `None`。
    pub fn image_for(&self, page: PageIndex) -> Option<Arc<RasterImage>>;

    /// ▲v3 该页当前缩放下的全部**已在缓存中**的瓦片（分块时使用），供 UI 拼接绘制。
    pub fn tiles_for(&self, page: PageIndex) -> Vec<(TileSpec, Arc<RasterImage>)>;

    /// ▲v3 当前分块策略（UI 可据此决定走 `image_for` 还是 `tiles_for`）。
    pub fn tiling_policy(&self) -> TilingPolicy;

    /// ▲v3 切换分块策略（会作废在飞任务；缓存键含义随之变化）。
    pub fn set_tiling_policy(&mut self, policy: TilingPolicy);

    /// ▲v3 捕获/恢复视野中心锚点（会话在重算布局的操作内部自动使用）。
    pub fn capture_anchor(&self) -> Option<ScrollAnchor>;
    pub fn restore_anchor(&mut self, anchor: ScrollAnchor);

    // 文本与导航
    pub fn page_text(&mut self, page: PageIndex) -> Option<Arc<PageText>>;
    pub fn cached_page_text(&self, page: PageIndex) -> Option<Arc<PageText>>;
    pub fn text_position_at(&mut self, content_point: Point) -> Option<TextPosition>;
    pub fn highlight_selection(&self) -> Vec<Highlight>;
    /// 复制语义要求完整：这里**会按需加载**选区覆盖的页文本，因此需要 `&mut self`。
    pub fn selected_text(&mut self) -> String;
    pub fn highlights_for_search(&self, hits: &[SearchHit], active: Option<usize>) -> Vec<Highlight>;

    /// ▲v3 导航索引（O(1) 克隆）；取代 v2 的 `outline()` 与 `links(page)`。
    pub fn navigation(&self) -> Result<Arc<NavigationIndex>>;
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>>;
}
```

---

## 4. 接线总图：谁产出谁消费

```text
                        ┌──────────────────────────────────────────┐
   Source + password ──▶│ PdfBackend::open                         │
                        └───────────────┬──────────────────────────┘
                                        │ Result<OpenedDocument>
                    ┌───────────────────┴────────────────────┐
                    ▼                    ▼                       ▼
          Arc<dyn Document>     Arc<dyn PageRaster>    Option<Arc<dyn TextLayer>>
          ├ page_geometries()          │（像素）                │（文字/搜索）
          └ navigation()               │                       │
             ▲v3 打开时备好            ▼                       │
             │                ┌──────────────────┐             │
             │                │ RenderScheduler  │◀─ RenderRequest（会话按布局产出，
             │                │  submit/cancel   │   ▲v3 分块时每页多个瓦片请求）
             │                │  drain_events    │──▶ RenderEvent
             │                └────────┬─────────┘             │
             │                         │ render()              │
             │                         ▼                       │
             │                  Arc<RasterImage> ──▶ RenderCache（键含 tile）
             │                                                 │
   UI：ViewerCommand ──▶┌──────────────────────────────────────┐ │
                        │ DocumentSession（装配点）             │ │
   UI：SessionEvent  ◀──│  PageLayout / TextSelection 注入      │◀┘ page_text()/search()
                        └──────────────────────────────────────┘
                                        ▲
                                        │ compute() / visible_pages() / page_at()
                                        │ ▲v3 scroll_to_anchor()
                                Arc<dyn PageLayout>
```

线程归属：

| 组件 | 线程 |
| --- | --- |
| `DocumentSession`、`TextSelection`、`PageLayout`、`Document`、`TextLayer` | UI 线程（`TextLayer` 也可被后台搜索任务调用，故要求 `Send + Sync`） |
| `RenderScheduler` 工作线程 → `PageRaster` | 后台（数量 = `Capabilities::max_concurrency`，PDFium = 1） |
| `RenderCache` | 两侧都访问（内部加锁） |

---

## 5. 对象安全自检

| trait | 泛型方法 | `impl Trait` 返回 | `async fn` | 可作为 `dyn` |
| --- | --- | --- | --- | --- |
| `PdfBackend` | 无 | 无 | 无 | ✅ `Arc<dyn PdfBackend>` |
| `Document` | 无 | 无 | 无 | ✅ `Arc<dyn Document>` |
| `PageRaster` | 无 | 无 | 无 | ✅ `Arc<dyn PageRaster>` |
| `RenderCache` | 无 | 无 | 无 | ✅ `Arc<dyn RenderCache>` |
| `RenderScheduler` | 无 | 无 | 无 | ✅ `Arc<dyn RenderScheduler>` |
| `TextLayer` | 无 | 无 | 无 | ✅ `Arc<dyn TextLayer>` |
| `TextSelection` | 无 | 无 | 无 | ✅ `Box<dyn TextSelection>`（`&mut self` 不影响对象安全） |
| `PageLayout` | 无 | 无 | 无 | ✅ `Box<dyn PageLayout>` |
| `KeyValueStore` | 无 | 无 | 无 | ✅ `Arc<dyn KeyValueStore>` |

---

## 6. 版本差异（便于复核）

### v2 → v3（并入 8 条批复）

| # | 位置 | 变更 | 对应决定 |
| --- | --- | --- | --- |
| 1 | `Document` | 新增 `page_geometries() -> Arc<[PageGeometry]>`（批量、O(1)）；`page_size`/`page_rotation` 降级为默认方法 | D-01 |
| 2 | `RenderRequest` / `RenderKey` | 新增 `tile: Option<TileSpec>`；新增 `TILE_SIZE`、`TileSpec`、`TilingPolicy`；`PageRaster` 契约明确"只渲染瓦片、不得整页后裁剪" | D-02 |
| 3 | `TextLayer::search` | 注释明确"同步、不做异步版本" | D-03 |
| 4 | `TextSelection` | 明确 `&mut self` 为例外并记录已确认 | D-04 |
| 5 | `Viewport` / `ScrollAnchor` / `PageLayout` | 新增 `center_content_point()`、`ScrollAnchor`、`scroll_to_anchor()`；`scroll_to_page` 保留居中语义（**v4/D-11 起改为顶部对齐 + 横向居中**，并新增 `ViewerCommand::CenterOnPagePoint` 供搜索跳转居中） | D-05 / D-11 |
| 6 | `Destination` / `NavigationIndex` | 删除 `Destination::Named`；新增 `NavigationIndex`；`Document::outline()/links()` 合并为 `navigation()` | D-08 |
| 7 | `PagePlacement` | 新增 `pixel_size()`（分块网格要用） | D-02 |
| 8 | 会话 API | 新增 `page_geometry` / `tiles_for` / `tiling_policy` / `capture_anchor` / `restore_anchor` / `navigation` | D-01/02/05/08 |

### v1 → v2

| 位置 | v1 | v2 | 原因 |
| --- | --- | --- | --- |
| `PageLayout::compute` | `&[Size]` | `&[PageGeometry]` | `/Rotate` 是逐页的，`Size` 表达不了 |
| `LayoutParams.rotation` | 全局 `rotation` | `user_rotation`（与页自带旋转叠加） | 同上 |
| `ViewerCommand` | 无粒度命令 | 增加 `SetSelectionGranularity` | 双击选词需要 UI 先告知粒度 |
| `SelectBegin` 语义 | 只放锚点 | 按粒度展开成完整区间 | 否则双击后选区为空 |
| `OpenedDocument` | 不可克隆 | `Clone` | 便于"同一文档换一套能力声明"测降级路径 |
