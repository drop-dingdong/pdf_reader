# trait 具体定义（**定稿候选 v2**）

> 配套：[architecture.md](architecture.md)（分层与决策）、[traits.md](traits.md)（每个 trait 的设计意图）、[data-flow.md](data-flow.md)（场景化数据流）
> 本稿是**签名级**定义：与你批复的架构一致，批准后可直接落到 `crates/pdfcore`（去掉注释即为源码）。
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

其中 4 个点我做了不同选择，逐条说明（这也是本设计最需要你确认的取舍）：

| 你的写法 | 我的写法 | 原因 |
| --- | --- | --- |
| `type Document;`（关联类型） | 返回 `Arc<dyn Document>` | 关联类型会让 trait **无法作为 trait object 使用**（除非写 `dyn PdfBackend<Document = X>`），于是"运行期替换后端"就要把泛型传染给整条调用链 |
| `fn open(...)`（无 `&self`） | `fn open(&self, ...)` | 静态方法无法承载实例状态（PDFium 初始化句柄、字体配置、内存上限），也无法把后端放进 `Vec<Box<dyn PdfBackend>>` 做多后端回退 |
| `-> Option<Document>` | `-> Result<OpenedDocument>` | `Option` 丢失失败原因：是**加密需要密码**、密码错误、文件损坏，还是 IO 失败？这四种在 UI 上的处理完全不同 |
| `path: Path` | `source: Source<'_>` | `Path` 是 unsized 类型，按值传参编译不过（须 `&Path`/`PathBuf`）；另外把"来源"建模成枚举后可同时支持路径与内存字节 |

**关联类型 vs trait object 的代价对比**（这是唯一的实质分歧）：

```rust
// A. 关联类型风格：静态分发、零成本，但泛型会向上传染
trait PdfBackendA {
    type Document; type Raster; type Text;
    fn open(&self, path: &Path) -> Result<(Self::Document, Self::Raster, Self::Text)>;
}
fn build_session<B: PdfBackendA>(backend: &B, ...) { ... }   // 泛型从这里开始传染
struct App<B: PdfBackendA> { backend: B, /* …整棵 UI 都带上 B… */ }
// ✗ 无法写 Vec<Box<dyn PdfBackendA>>；✗ 单测替身会改变 App 的类型

// B. dyn 风格（本设计）：运行期可替换，签名稳定
trait PdfBackendB {
    fn open(&self, source: Source<'_>, password: Option<&str>) -> Result<OpenedDocument>;
}
struct App { backend: Arc<dyn PdfBackendB>, /* 与具体后端无关 */ }
// ✓ Vec<Arc<dyn PdfBackendB>> 可用；✓ 测试注入内存后端，App 类型不变
```

**什么时候该用关联类型**：只有一个确定实现、且希望内联/零虚函数开销时（例如库内部把 `PdfiumDocument` 作为唯一后端）。本项目明确要"多后端 + 测试替身 + 将来可能加 hayro"，所以选 dyn 风格。
**代价**：每次调用有一次虚表跳转（对渲染这种毫秒级操作完全可忽略）。

---

## 1. 类型词典

trait 签名里出现的全部类型，先列在这里，后面 §2 的 trait 定义直接引用。

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

### 1.2 文档与能力

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

/// 跳转目标（大纲与链接共用）。
pub enum Destination {
    Page { page: PageIndex, offset: Option<Point> },  // 页内落点，PDF 页面空间
    Named(String),                                     // PDF 命名目标，需后端二次解析
    Uri(String),
}

/// 页内超链接热区（PDF 页面空间）。
pub struct Link { pub rect: Rect, pub destination: Destination }

/// 大纲节点（递归树）。
pub struct OutlineNode {
    pub title: String,
    pub destination: Option<Destination>,
    pub children: Vec<OutlineNode>,
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

### 1.3 渲染

```rust
/// 缩放：设备像素 / point。
pub struct Scale(pub f32);
//  const IDENTITY(1.0); from_zoom(f32); from_dpi(f32); zoom(); dpi(); is_valid()
//  quantize() -> 量化到 1/8 步长（缓存键用，避免滚轮缩放摧毁缓存）

/// 缓存键：页 + 量化缩放（千分之一，整数以便 Hash/Eq）+ 旋转。
pub struct RenderKey { pub page: PageIndex, pub scale_milli: u32, pub rotation: Rotation }
//  new(page, scale, rotation); scale() -> Scale

/// 渲染优先级（rank() 越小越优先）。
pub enum RenderPriority { Interactive, Visible, Prefetch, Background }

/// 取消令牌（可克隆，共享同一标志）。
pub struct CancelToken(Arc<AtomicBool>);   // new(); cancel(); is_cancelled()

/// 一次渲染请求。
pub struct RenderRequest {
    pub page: PageIndex,
    pub scale: Scale,
    pub rotation: Rotation,        // 必须是布局给出的最终旋转
    pub priority: RenderPriority,
    pub cancel: CancelToken,
}
//  new(page, scale, rotation)（默认 Visible）; with_priority(p); key() -> RenderKey

/// 位图格式（PDFium 原生 BGRA；egui 要 RGBA）。
pub enum PixelFormat { Rgba8, Bgra8, Gray8 }   // bytes_per_pixel(); has_alpha()

/// 一页渲染结果（不可变，像素在 Arc<[u8]> 中共享）。
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

/// 一页文本层。
/// 不变量：text.chars().count() == chars.len()。
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

### 1.5 布局

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
///   因此"关闭文档"= drop，不需要 `close()`（少一个可能忘记调用的 API）。
/// * 必须提供 [`PageRaster`]；[`TextLayer`] 可缺省，此时必须把
///   [`Capabilities::text_layer`] 声明为 `false`，否则 UI 会显示无效入口。
pub trait PdfBackend: Send + Sync {
    /// 后端名称（"pdfium" / "hayro" / "memory"），用于"关于"与日志。
    fn name(&self) -> &'static str;

    /// 声明能力，UI 据此决定显示哪些功能入口。
    fn capabilities(&self) -> Capabilities;

    /// 打开文档。同一实例可并发打开多个文档（多标签/多窗口）。
    fn open(&self, source: Source<'_>, password: Option<&str>) -> Result<OpenedDocument>;
}
```

**谁实现**：`PdfiumBackend`（真实）、`MemoryBackend`（testkit）、未来 `HayroBackend`。

---

### 2.2 `Document` —— 只读文档结构

**数据形态**：`PageIndex` → 页尺寸 / `/Rotate`；无输入 → 元数据 / 大纲 / 链接。

```rust
/// **文档结构**：只读、可并发访问的文档信息。
///
/// ## 目的
/// 只覆盖 PDF 里**不变的部分**（页数、页尺寸、`/Rotate`、元数据、大纲、链接），
/// 使布局与会话无需知道 PDFium 存在；单测也能构造"3 页、尺寸写死"的假文档。
/// 页面**像素**归 [`PageRaster`]，页面**文字**归 [`TextLayer`]——三者刻意分开，
/// 因为它们的性能特征与可用性完全不同。
///
/// ## 实现者义务
/// * `info()` 必须 O(1)（打开时已解析完毕）。
/// * `page_size` / `page_rotation` 返回**未旋转**的原始值；旋转由布局叠加。
/// * 大纲与链接属可选能力：不支持时返回空 `Vec`，只有真正解析失败才返回错误。
pub trait Document: Send + Sync {
    /// 元数据与页数。
    fn info(&self) -> &DocumentInfo;

    /// 某页的原始尺寸（PDF point，未旋转）。
    fn page_size(&self, page: PageIndex) -> Result<Size>;

    /// 某页自带的 `/Rotate`。默认不旋转。
    ///
    /// 注意是**逐页**的：同一文档不同页可有不同旋转值。
    fn page_rotation(&self, page: PageIndex) -> Result<Rotation> {
        Ok(Rotation::None)
    }

    /// 大纲（书签）树。默认空列表。
    fn outline(&self) -> Result<Vec<OutlineNode>> {
        Ok(Vec::new())
    }

    /// 某页的超链接热区。默认空列表。
    fn links(&self, page: PageIndex) -> Result<Vec<Link>> {
        Ok(Vec::new())
    }
}
```

**谁实现**：`PdfiumDocument`、`MemoryDocument`。

---

### 2.3 `PageRaster` —— 光栅化（最核心的替换缝）

**数据形态**：`&RenderRequest`（页 + 缩放 + 旋转 + 优先级 + 取消令牌）→ `Arc<RasterImage>`。

```rust
/// **光栅化能力**：把一页按请求渲染成位图。
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
/// * 返回位图尺寸应等于"旋转后页尺寸 × scale"，允许 ±1 像素取整误差。
/// * 如实填写 [`PixelFormat`]（PDFium 原生 BGRA，不要假装 RGBA）。
pub trait PageRaster: Send + Sync {
    /// 渲染一页。
    fn render(&self, request: &RenderRequest) -> Result<Arc<RasterImage>>;
}
```

**刻意的缺失**：没有 `render_thumbnail()`（缩略图 = 小 `Scale` 的普通请求，与主视图共享缓存）、没有分块参数（M4 若需要，在 `RenderRequest` 加 `tile` 字段，签名不变）。

**谁实现**：`PdfiumRaster`、`MemoryRaster`。

---

### 2.4 `RenderCache` —— 位图缓存

**数据形态**：`&RenderKey` → `Option<Arc<RasterImage>>`；写入 `RenderKey + Arc<RasterImage>`。

```rust
/// **位图缓存**：`RenderKey` → 位图。
///
/// ## 目的
/// 把"缓存策略"独立出来：MVP 用几十行 `HashMap` 先跑通，之后换 LRU / 内存上限 / 落盘，
/// **不改任何调用方**；单测里还能用"永不命中"或"只留一页"的缓存验证重渲染逻辑。
///
/// ## 实现者义务
/// * `get` 被 UI 线程每帧调用多次，必须 O(1) 且不长时间持锁。
/// * 不得返回尺寸/格式与键不匹配的位图。
pub trait RenderCache: Send + Sync {
    /// 查询；未命中返回 `None`。
    fn get(&self, key: &RenderKey) -> Option<Arc<RasterImage>>;

    /// 写入（同键覆盖）。`image` 为 `Arc`，与 UI 共享像素，不产生拷贝。
    fn insert(&self, key: RenderKey, image: Arc<RasterImage>);

    /// 丢弃某一页的所有缩放版本。
    fn remove_page(&self, page: PageIndex);

    /// 清空。
    fn clear(&self);

    /// 统计信息（状态栏展示与调优）；默认空统计。
    fn stats(&self) -> CacheStats {
        CacheStats::default()
    }
}
```

**谁实现**：`LruCache`（app 侧）、`MemoryCache`（testkit）、测试用的 `NeverHitCache`。

---

### 2.5 `RenderScheduler` —— 调度（UI 面向的唯一渲染入口）

**数据形态**：`RenderRequest` → `JobId`；`drain_events(max)` → `Vec<RenderEvent>`。

```rust
/// **渲染调度**：UI 面向的唯一渲染入口。
///
/// ## 目的
/// 让 UI 完全不必知道"谁在渲染、几个线程、PDFium 是否线程安全"：
/// * `submit` 非阻塞，UI 每帧都可放心调用；
/// * 结果用 `drain_events` 轮询取回，与 egui "每帧拉取"模型天然契合
///   （不需要把 UI 状态塞进回调，也不会出现回调跨帧持有 UI 引用）；
/// * 真实实现 = 后台线程 + 优先级队列；测试实现可**同步渲染**，
///   于是「UI → 会话 → 调度 → 后端 → 缓存 → UI」能确定性地在单测里跑通。
///
/// ## 实现者义务
/// * `submit` 立即返回，不得同步等待（同步实现仅用于测试）。
/// * 必须自行去重：同一 [`RenderKey`] 已在队列或飞行中时忽略重复提交。
/// * 必须遵守 [`Capabilities::max_concurrency`]；对 PDFium 只能是 1（即单工作线程）。
pub trait RenderScheduler: Send + Sync {
    /// 提交任务；返回任务号用于取消。
    fn submit(&self, request: RenderRequest) -> JobId;

    /// 取消指定任务。
    fn cancel(&self, job: JobId);

    /// 取消某一页的所有任务（该页滚出视口）。
    fn cancel_page(&self, page: PageIndex);

    /// 取消全部任务（关闭文档、缩放/旋转导致缓存键整体失效）。
    fn cancel_all(&self);

    /// 取出至多 `max` 条已完成事件（非阻塞）。UI 每帧调用一次。
    fn drain_events(&self, max: usize) -> Vec<RenderEvent>;

    /// 排队中 + 正在执行的任务数。
    fn pending(&self) -> usize;
}
```

**谁实现**：`WorkerScheduler`（后台线程 + 优先级队列，app 侧）、`ImmediateScheduler`（testkit，同步）。

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
///   `text.chars().count() == chars.len()`（缺位置的字符也要带零包围盒占位），
///   否则高亮会整体错位。
/// * 包围盒必须在 **PDF 页面空间**（左下原点、point）。
/// * 文本提取可能很慢，实现应自带缓存或依赖会话层的页文本缓存。
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
/// * `highlight` / `selected_text` 有默认实现（基于 `selection()` + 调用方给的取文本闭包），
///   因此实现者最少只需写 5 个方法。
///
/// ## 实现者义务
/// * `begin` 重置选择、`extend` 更新焦点；收到的 `position` 应已由调用方按
///   [`SelectionGranularity`] 展开（见 [`PageText::range_at`]）——"什么叫一个单词"
///   不是本 trait 的职责。
/// * 只需 `Send`（选区属于 UI 线程，但会话可整体被移动到其它线程）。
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
    /// 文本尚未加载的页会被跳过（UI 可先请求该页文本再重算）。
    fn highlight(&self, text_of: &dyn Fn(PageIndex) -> Option<Arc<PageText>>) -> Vec<Highlight>;

    /// 取选中文本用于复制；跨页以换行连接。默认实现。
    fn selected_text(&self, text_of: &dyn Fn(PageIndex) -> Option<Arc<PageText>>) -> String;
}
```

> **与"全局 `&self`"约定的唯一例外**：改选区的 4 个方法用 `&mut self`。选区是纯 UI 状态、不允许并发，用 `&mut self` 能让实现方免锁，并把"只有 UI 线程能改选区"写进类型。若你更希望完全统一，我可以把它们也改成 `&self` + 内部 `Mutex`（代价是每次拖拽都加锁）。

**谁实现**：`RangeSelection`（pdfcore 自带，约 30 行）、将来可能的 `MultiSelection`。

---

### 2.8 `PageLayout` —— 布局引擎（纯函数）

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
/// * `placements` 顺序必须与页序一致且覆盖所有页（缺页 = 该页永远渲染不出来）。
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

## 3. 接线总图：谁产出谁消费

```text
                        ┌──────────────────────────────────────────┐
   Source + password ──▶│ PdfBackend::open                         │
                        └───────────────┬──────────────────────────┘
                                        │ Result<OpenedDocument>
                    ┌───────────────────┴────────────────────┐
                    ▼                    ▼                       ▼
          Arc<dyn Document>     Arc<dyn PageRaster>    Option<Arc<dyn TextLayer>>
          （结构/大纲/链接）      （像素）                （文字/搜索）
                    │                    │                       │
                    │                    ▼                       │
                    │          ┌──────────────────┐              │
                    │          │ RenderScheduler  │◀── RenderRequest（由会话按布局产出）
                    │          │  submit/cancel   │
                    │          │  drain_events    │──▶ RenderEvent
                    │          └────────┬─────────┘              │
                    │                   │ 调用 render()          │
                    │                   ▼                        │
                    │            Arc<RasterImage> ──▶ RenderCache（键=页+量化缩放+旋转）
                    │                                            │
   UI：ViewerCommand ──▶┌──────────────────────────────────────┐  │
                        │ DocumentSession（装配点）             │  │
   UI：SessionEvent  ◀──│  PageLayout / TextSelection 注入      │◀─┘ page_text()/search()
                        └──────────────────────────────────────┘
                                        ▲
                                        │ compute() / visible_pages() / page_at()
                                Arc<dyn PageLayout>
```

线程归属：

| 组件 | 线程 |
| --- | --- |
| `DocumentSession`、`TextSelection`、`PageLayout`、`Document`、`TextLayer` | UI 线程（`TextLayer` 也可被后台搜索任务调用，故要求 `Send + Sync`） |
| `RenderScheduler` 的工作线程 → `PageRaster` | 后台（数量 = `Capabilities::max_concurrency`，PDFium = 1） |
| `RenderCache` | 两侧都访问（内部加锁） |

---

## 4. 对象安全自检

| trait | 泛型方法 | `impl Trait` 返回 | `async fn` | 可作为 `dyn` |
| --- | --- | --- | --- | --- |
| `PdfBackend` | 无 | 无 | 无 | ✅ `Arc<dyn PdfBackend>` |
| `Document` | 无 | 无 | 无 | ✅ `Arc<dyn Document>` |
| `PageRaster` | 无 | 无 | 无 | ✅ `Arc<dyn PageRaster>` |
| `RenderCache` | 无 | 无 | 无 | ✅ `Arc<dyn RenderCache>` |
| `RenderScheduler` | 无 | 无 | 无 | ✅ `Arc<dyn RenderScheduler>` |
| `TextLayer` | 无 | 无 | 无 | ✅ `Arc<dyn TextLayer>` |
| `TextSelection` | 无 | 无 | 无 | ✅ `Box<dyn TextSelection>`（注意 `&mut self` 方法不影响对象安全） |
| `PageLayout` | 无 | 无 | 无 | ✅ `Box<dyn PageLayout>` |
| `KeyValueStore` | 无 | 无 | 无 | ✅ `Arc<dyn KeyValueStore>` |

> 唯一需要留意的是 `TextSelection::highlight(&self, text_of: &dyn Fn(...))`：`dyn Fn` 参数是对象安全的（它本身是 trait object），不影响本 trait 作为 `dyn` 使用。

---

## 5. 与 v1 评审稿的差异（便于你复核）

| 位置 | v1 | v2（本稿） | 原因 |
| --- | --- | --- | --- |
| `PageLayout::compute` | `&[Size]` | **`&[PageGeometry]`** | `/Rotate` 是逐页的，`Size` 表达不了 |
| `LayoutParams.rotation` | 全局 `rotation` | **`user_rotation`**（与页自带旋转叠加） | 同上 |
| `ViewerCommand` | 无粒度命令 | 增加 `SetSelectionGranularity` | 双击选词需要 UI 先告知粒度 |
| `SelectBegin` 语义 | 只放锚点 | **按粒度展开成完整区间** | 否则双击后选区为空，需再发一次 extend 才生效 |
| `OpenedDocument` | 不可克隆 | **`Clone`** | 便于"同一文档换一套能力声明"来测降级路径 |
