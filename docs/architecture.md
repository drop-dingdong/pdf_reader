# MVP PDF 阅读器 —— 接口层设计文档（**v3**）

> 配套文档：[trait-definitions.md](trait-definitions.md)（签名级定义，v3）、[traits.md](traits.md)（设计意图）、[data-flow.md](data-flow.md)（数据流推演，v3）、[decision-log.md](decision-log.md)（决策记录）
> 技术栈已定：Rust + egui/eframe + PDFium（见 [framework-selection.md](framework-selection.md)）
> 状态：**架构与 8 项批复已确认**。`▲v3` 标记本次变更处。

---

## 0. 批复状态（v3）

你已确认"trait 架构合理"，并逐条批复了 5 个接口问题与 3 个数据流细节 —— 完整记录见 [decision-log.md](decision-log.md)。

| # | 事项 | 结论 |
| --- | --- | --- |
| 1 | MVP 功能边界 | 沿用：打开/滚动/缩放/翻页/选择复制/搜索/大纲/缩略图/链接跳转；不做批注、表单、打印、双页对开、OCR。**（此项只是随架构整体认可，若需增删请直接指出）** |
| 2 | 并发模型 | 确认：单后台渲染线程 + 优先级队列 + UI 每帧轮询事件（D-00c） |
| 3 | trait 的 `&self` 约定 | 确认；唯一例外是 `TextSelection` 的 4 个改状态方法（**D-04**） |
| 4 | 文本层作为"可选零件" | 确认：`Option<Arc<dyn TextLayer>>` + `Capabilities` |
| 5 | 坐标系与单位 | 确认；**新增第 5 条不变量**：分块拼接必须整数像素对齐（见 §5） |
| 6 | 缓存键量化 | 确认：1/8 步长；**瓦片坐标并入缓存键**（**D-02**） |
| 7 | 会话层不接触平台 API | 确认 |
| 8 | 拆分方式 | 确认：独立零依赖 crate `pdfcore`；**导航索引上移到接口层**（**D-08**） |

本轮四项签名级变更：**D-01** 批量页几何、**D-02** 分块预渲染、**D-05** 滚动锚点、**D-08** 导航索引预解析。

---

## 1. 目标与非目标

### 1.1 MVP 必须有

| 能力 | 说明 |
| --- | --- |
| 打开文档 | 路径或内存字节；加密文档先用 `PasswordRequired` 通知 UI 弹密码框，再带密码重试 |
| 页面渲染 | 只渲染视口内可见页 ±1 页（虚拟化），支持取消与优先级 |
| 位图缓存 | 键 = (页号, 量化缩放, 旋转)，多缩放版本共存 |
| 滚动与缩放 | 连续纵向滚动、10%–800%、适应宽度、每页可带 `/Rotate`、用户可再旋转 |
| 翻页与跳页 | 上/下一页、跳指定页、当前页跟随视口 |
| 文本层 | 字符级包围盒；单击选字符、双击选词、三击选行、拖动扩展、全选、复制 |
| 搜索 | 全文搜索、命中高亮、跳到命中（后端支持时） |
| 导航 | 大纲（书签）树、缩略图、页内链接跳转 |

### 1.2 明确不做（留 M4）

批注与高亮编辑、表单填写、数字签名校验、打印、双页/对开模式、横向滚动模式、OCR（扫描件取字）、PDF 编辑与导出。

> 判断标准：**只读且不改文件**的功能才进 MVP；任何需要回写 PDF 的功能都推迟。

---

## 2. 分层与依赖方向

```text
        ┌─────────────────────────┐
        │  app（egui 壳）          │  窗口/工具栏/侧边栏/快捷键/剪贴板/文件对话框
        └───────────┬─────────────┘
                    │ 只认 ViewerCommand / SessionEvent / DocumentSession
        ┌───────────▼─────────────┐        ┌──────────────────────────┐
        │  pdfcore（接口层）        │◀───────│  backend-pdfium          │
        │  trait + 领域模型 + 会话  │        │  pdfium-render 适配      │
        └───────────▲─────────────┘        └──────────────────────────┘
                    │                      ┌──────────────────────────┐
                    └──────────────────────│  testkit（内存假后端）    │
                                           └──────────────────────────┘
```

规则：

1. **依赖单向**：两个实现 crate 依赖 `pdfcore`，`pdfcore` 不依赖任何一方。
2. **`pdfcore` 零外部依赖**（纯 std）。带来三个直接收益：接口层可被 CLI/移动端/WASM 复用；单测不需要 PDFium 与 GPU；CI 离线可跑。
3. **UI 不直接调用后端**：`egui` 里不出现 `pdfium_render::*`，只出现 `DocumentSession` 与上面两个枚举。这样换 UI 框架（iced/Slint）不影响后端，换后端（hayro）不影响 UI。
4. **后端不依赖 UI 类型**：后端返回的位图是自有类型 `RasterImage`（RGBA/BGRA 字节），不出现 `egui::ColorImage`。

---

## 3. 零件与装配方式

### 3.1 零件清单（共 9 个 trait）

| # | trait | 角色 | 必需？ | 可替换实现 |
| --- | --- | --- | --- | --- |
| 1 | `PdfBackend` | 工厂：打开文档、声明能力 | 必需 | pdfium / hayro / 内存 |
| 2 | `Document` | 只读结构：页数、页尺寸、`/Rotate`、元数据、大纲、链接 | 必需 | 同上 |
| 3 | `PageRaster` | 光栅化一页 | 必需 | pdfium / 假渲染器 |
| 4 | `RenderCache` | 位图缓存（键 → 位图） | 必需 | `LruCache`（pdfcore 自带，带内存上限）/ `MemoryCache`（testkit） |
| 5 | `RenderScheduler` | 排队、优先级、取消、事件回传 | 必需 | `WorkerScheduler`（pdfcore 自带，后台线程 + 优先级队列）/ `ImmediateScheduler`（testkit，同步） |
| 6 | `TextLayer` | 文本提取 + 搜索 | 可选 | pdfium / 假文本层 |
| 7 | `TextSelection` | 选区记账（不含文档内容） | 必需（有默认实现） | `RangeSelection` |
| 8 | `PageLayout` | 页几何 → 内容空间排布（纯函数） | 必需（有默认实现） | `VerticalLayout` / 未来对开 |
| 9 | `KeyValueStore` | 偏好与最近文件的持久化 | 可选 | 文件 / 内存 |

**刻意不建的 trait**（避免过度抽象）：

| 概念 | 为什么不建 trait |
| --- | --- |
| 缩略图 | 页尺寸来自 `Document`，目标宽度可换算成普通 `Scale`，直接复用 `PageRaster::render`，因此缩略图与主视图**共享同一份缓存**，不会重复渲染 |
| 剪贴板 | 通过 `SessionEvent::CopyToClipboard(String)` 交回 UI，会话保持无平台依赖 |
| 文件对话框 | 属于 UI 层职责；会话只接受"已打开的文档" |
| 缓存淘汰策略 | 作为 `RenderCache` 实现的一部分（内存上限、LRU、落盘），而不是单独 trait |

### 3.2 装配骨架（示意，仅结构与签名）

```text
// 1) 后端产出"已打开文档"：三个 Arc<dyn ...> + 能力声明
let opened: OpenedDocument = backend.open(Source::Path(&path), password)?;
//    OpenedDocument { document: Arc<dyn Document>,
//                     raster:   Arc<dyn PageRaster>,
//                     text:     Option<Arc<dyn TextLayer>>,   // ← 可选零件
//                     capabilities: Capabilities }

// 2) 注入其余零件，拼成会话（UI 唯一持有的对象）
let session = DocumentSession::new(
    opened,
    Arc::new(LruCache::new(512 << 20)),          // RenderCache
    Arc::new(WorkerScheduler::new(1)),           // RenderScheduler（PDFium 并发上限 = 1）
    Box::new(VerticalLayout),                    // PageLayout
    Box::new(RangeSelection::new()),             // TextSelection
)?;

// 3) UI 每帧只做两件事：发命令、取事件
session.handle(ViewerCommand::RequestVisible, &mut events);
session.pump(&mut events);
```

> 关键点：`DocumentSession` **不构造**任何零件，全部由外部注入。因此桌面端注入真实实现、测试注入假实现，走的是同一条代码路径。

---

## 4. 并发模型与数据流

### 4.1 为什么是"一个后台线程 + 事件轮询"

PDFium 不是线程安全的（官方建议"并行处理多个文档"而不是多线程共享一个文档），因此：

- `Capabilities::max_concurrency` 声明后端可承受的并发，**PDFium 必须填 1**；调度器据此决定工作线程数。
- 渲染是 CPU 密集的阻塞式调用，用 `std::thread` + 优先级队列比 async 更贴合（async 只解决 I/O 密集问题，还会引入 runtime 依赖并破坏 `dyn` 兼容性）。
- egui 是立即模式、每帧执行：让 UI 每帧 `drain_events()` 拉结果，天然契合，不需要把 UI 状态塞进回调。

```text
   UI 线程（每帧）                   渲染线程（1 个）
   ──────────────                   ─────────────────
   handle(命令) ──submit──────────────▶ 优先级队列
                                          │ 按优先级出队（Interactive > Visible > Prefetch > Background）
                                          │ 出队时检查 CancelToken
   pump() ◀────drain_events──────────      ▼
     │  位图写入 RenderCache          PageRaster::render()（PDFium，阻塞）
     ▼
   image_for(page) → 上传/复用 egui 纹理
```

### 4.2 每帧调用顺序（UI 侧契约）

1. 采集输入 → `session.handle(命令…, &mut events)`
2. `session.pump(&mut events)` —— 取回上一帧提交的渲染结果并写入缓存
3. 绘制：对每个可见页 `session.image_for(page)` → 上传纹理 → 画；再叠加 `highlight_selection()` / `highlights_for_search()`
4. `session.handle(ViewerCommand::RequestVisible, &mut events)` —— 为下一帧备好位图

### 4.3 取消的真实语义（重要）

**PDFium 无法中断已经在执行的渲染。** 因此 `CancelToken` 只在"任务出队前"生效。流畅度的真正保障是三件事，而不是取消：

1. **缩放量化**（1/8 步长）→ 缓存命中率高，滚轮缩放不会摧毁缓存；
2. **优先级队列** → 用户正在看的那一页永远排在预取页前面；
3. **只渲染可见页 ±1** → 无论文档多长，在飞任务数是常数级。

---

## 5. 单位与坐标系（最容易出 bug 的地方）

| 空间 | 原点 | y 轴 | 单位 | 谁产出 |
| --- | --- | --- | --- | --- |
| PDF 页面空间 | 左下角 | 向上 | point（1/72 英寸） | 后端（PDFium 原始值） |
| 内容空间 | 整篇文档左上角 | 向下 | 设备无关像素 | 布局引擎 |
| 视图空间 | 视口左上角 | 向下 | 设备无关像素 | UI |

换算链：

```text
PDF 页面坐标
  ──Rotation::map_point（先旋转；注意它同时负责 y 轴翻转）──▶ 旋转后页内坐标
  ──× Scale（缩放：设备像素 / point）────────────────────────▶ 像素坐标
  ──+ 页在内容空间的偏移（PagePlacement.rect.min）──────────▶ 内容空间坐标
  ──− 视口滚动偏移（Viewport.scroll）────────────────────────▶ 视图空间坐标
```

**五条必须守住的不变量**（将由单测锁定）：

1. 后端给出的字符包围盒**永远是 PDF 页面空间**（左下原点）。任何"顺手翻一下 y"的实现都会让高亮整体上下颠倒。
2. **只有 `PagePlacement` 负责 page ↔ content 换算**（`page_to_content` / `content_to_page` / `rect_to_content`）。UI 与后端都不得自己算。
3. **旋转必须先于缩放**，且 `Rotation::map_point` 在 **0° 与 180° 时同样包含 y 轴翻转**（这是实现中最常见的错误：0° 时直接返回原点）。
4. **渲染请求里的 `scale` 与 `rotation` 必须取自 `PagePlacement`**，不能各自计算 —— 否则会出现"布局转了、位图没转"或"高亮与文字错位"。
5. ▲v3 **分块（tile）必须整数像素对齐**：瓦片在缓存键里带 `(col, row)`，渲染时按 `TileSpec::page_rect()` 的 PDF 空间裁剪区**直接渲染**（不得整页后裁剪），拼接时以 `TileSpec::pixel_rect()` 的整数像素矩形为准 —— 否则会出现 1px 缝隙或重叠导致的模糊带。

---

## 6. 枚举与数据模型一览

### 6.1 值域/配置类枚举

| 枚举 | 取值 | 用途 |
| --- | --- | --- |
| `PixelFormat` | `Rgba8` / `Bgra8` / `Gray8` | PDFium 原生输出 BGRA、egui 要 RGBA；把格式显式建模，转换只在一处发生 |
| `Rotation` | `None` / `Clockwise90` / `Clockwise180` / `Clockwise270` | 页面 `/Rotate` 与用户旋转；提供 `compose`/`inverse`/`map_point`/`apply_to_size` |
| `PageLayoutMode` | `Continuous` / `SinglePage` | MVP 两者共用纵向布局，差别在滚动是否按页对齐 |
| `ReadingDirection` | `LeftToRight` / `RightToLeft` | MVP 不生效，为将来的对开/RTL 预留（代价小于将来改接口） |
| `RenderPriority` | `Interactive` / `Visible` / `Prefetch` / `Background` | 调度顺序 |
| `SelectionGranularity` | `Char` / `Word` / `Line` | 单击/双击/三击 |
| `SidebarTab` | `None` / `Outline` / `Thumbnails` / `Search` | UI 状态 |

### 6.2 事件/结果类枚举

| 枚举 | 用途 |
| --- | --- |
| `Source<'a>` | 打开来源：`Path(&Path)` / `Bytes(&[u8])`（网络下载、单测、剪贴板流） |
| `Destination` | ▲v3 跳转目标：`Page{page, offset}` / `Uri(String)`，大纲与链接共用；**命名目标已在打开时解析掉，故无 `Named` 变体**（D-08） |
| `RenderEvent` | 调度器 → 会话：`Rendered{key, image}` / `Failed{page, error}` / `Cancelled{page}` |
| `SessionEvent` | 会话 → UI：`PageRendered` / `CurrentPageChanged` / `SelectionChanged` / `CopyToClipboard(String)` / `Error(CoreError)` 等 |
| `ViewerCommand` | UI → 会话：`ResizeViewport` / `ScrollBy` / `GoToPage` / `SetZoom` / `FitWidth` / `SetSelectionGranularity` / `SelectBegin` / `SelectExtend` / `RequestVisible` 等 |
| `CoreError` | 统一错误，含**非故障**分支：`PasswordRequired`（请 UI 弹密码框）、`Unsupported(&'static str)`（能力缺失，UI 静默降级）、`Cancelled`（不提示） |
| `Capabilities` | 结构体（非枚举）：`text_layer` / `search` / `encryption` / `outline` / `links` / `max_concurrency` —— UI 据此决定"要不要显示搜索框" |

### 6.3 关键数据结构

| 类型 | 作用 |
| --- | --- |
| `OpenedDocument` | 装配包：`Arc<dyn Document>` + `Arc<dyn PageRaster>` + `Option<Arc<dyn TextLayer>>` + `Capabilities` |
| `PageGeometry` | `{ size, rotation }` —— 每页尺寸与自带旋转（**旋转必须逐页**）；▲v3 由 `Document::page_geometries()` 批量返回 |
| `NavigationIndex` | ▲v3 `{ outline, links_by_page, named_destinations, unresolved }` —— 打开时预解析并合并存储的全部导航数据（D-08） |
| `PagePlacement` | `{ page, rect, scale, rotation, page_size }` —— 布局结果，也是坐标系换算的唯一入口；▲v3 加 `pixel_size()` |
| `DocumentLayout` | `{ content_size, placements }` —— 内容总尺寸（算滚动范围）+ 每页位置 |
| `Viewport` | `{ size, scroll }` —— 视口尺寸与滚动偏移；▲v3 加 `center_content_point()` |
| `ScrollAnchor` | ▲v3 `{ page, page_point }` —— 视野中心锚点，布局重算前后保持阅读位置不漂移（D-05） |
| `Scale` | newtype(f32)：设备像素 / point；含 `quantize()`（缓存键量化）与 `dpi()` |
| `RenderKey` | `{ page, scale_milli, rotation, tile }` ▲v3 —— 缓存键（f32 不能做 `Hash`，故存千分之一整数；瓦片坐标并入键） |
| `TileSpec` / `TilingPolicy` | ▲v3 瓦片坐标（`grid_for` / `pixel_rect` / `page_rect`）与分块策略（默认 `AbovePixels(4 MP)`，见 D-15）（D-02） |
| `RasterImage` | 不可变位图，像素在 `Arc<[u8]>` 中，缓存与 UI 共享零拷贝；`to_rgba8()` 是唯一格式转换点 |
| `PageText` / `TextChar` / `TextSpan` | 页文本 + 逐字符包围盒 + 样式分段；不变量：`text.chars().count() == chars.len()` |
| `TextRange` / `TextPosition` / `Selection` | 字符区间（半开、字符下标而非字节）、文档位置、选区（anchor/focus 可反向） |
| `Highlight` | `{ page, rects, kind }` —— 选区与搜索命中共用，UI 只写一次绘制逻辑 |
| `ViewerState` | UI 状态：zoom、layout_mode、current_page、user_rotation、sidebar、granularity |
| `AppSettings` / `RecentFiles` | 设置与最近文件（纯数据，读写经 `KeyValueStore`） |

---

## 7. 关键设计决策与备选方案

| 决策 | 理由 | 被否方案 / 代价 |
| --- | --- | --- |
| 接口层独立成零依赖 crate | 可测试、可移植、依赖单向 | 直接在主 crate 里定义会迫使测试也链接 PDFium |
| trait 方法全同步 | PDF 渲染本就是阻塞 CPU 任务；`async fn in trait` 会带来 `dyn` 兼容与 runtime 依赖问题 | async/await：收益小、复杂度高 |
| 全部 `&self` + 内部 `Mutex` | 实现者不必区分 `&mut`；`Arc<dyn T>` 可自由共享 | `&mut self`：无法放进 `Arc`，UI 与调度器抢借用 |
| 文本层建模为 `Option<Arc<dyn TextLayer>>` | 一个 UI 同时适配功能完整的 PDFium 与只有渲染能力的后端 | 单一"全能后端" trait：hayro 之类的实现会被迫返回假数据 |
| 能力同时用 `Capabilities` 与 `Option` | `Option` 表示"零件不存在"，`Capabilities` 让 UI 提前决定界面（隐藏搜索框）而不是靠试错 | 只靠 `Result::Unsupported` 事后判断：界面会闪烁 |
| 选区模型不含文档内容 | 状态机可 100% 单测（含反向拖拽、跨页）；粒度展开由调用方用 `PageText::range_at` 完成 | 把文本塞进选区模型：难测且与后端耦合 |
| 布局是纯函数 trait，只强制 `compute` | 其余（可见页、命中、跳页居中、滚动夹取）用默认实现，新布局只需写一个方法 | 一次性大接口：实现负担重，易与需求脱节 |
| `visible_pages` 用 `&mut Vec` 出参 | 每帧调用，复用缓冲避免 60 次/秒的分配 | 返回 `Vec`：简单但每帧分配 |
| 缩放量化 1/8 做缓存键 | 12.5% 内共用位图，滚轮缩放不摧毁缓存 | 精确键：缓存命中率趋近 0 |
| 剪贴板/文件对话框不进接口层 | 接口层保持无平台依赖，测试无需 mock 系统 API | 建 `Clipboard` trait：为单一平台调用引入抽象 |
| ▲v3 `Document::page_geometries()` 批量返回页几何 | 布局需一次拿到全部页尺寸才能算内容总高与滚动范围；把 N 次跨 FFI 变成 1 次 `Arc` 克隆 | 逐页查询：打开大文档变慢（D-01） |
| ▲v3 分块渲染，阈值 16 MP → **4 MP**（D-15）| 高倍缩放下单页位图可达 124 MB；分块后一块瓦片仅 ~9ms、视口内 6–9 MB | 始终整页：内存爆炸且高倍时每次缩放都要重画几十 MP；始终分块：小页被碎片化、请求数暴增（D-02/D-15） |
| ▲v3 用滚动锚点保持视野中心（`ScrollAnchor`） | 缩放/旋转/改排布后阅读位置不漂移；`scroll_to_page` 仍负责跳页与锚点退化路径 | 每次重算布局都回到页首：用户迷失位置（D-05） |
| ▲v3 导航索引在打开时预解析并合并存储 | 点击链接/大纲 0 次后端调用；解析失败在打开时即暴露 | 实时解析：点击有延迟、UI 需处理"未解析目标"分支（D-08） |
| ▲v3 搜索保持同步、不引入异步 | 文本提取本就是阻塞任务；异步会带来 runtime 依赖与搜索取消的复杂度 | async 搜索：收益小、接口复杂化（D-03/D-07） |

---

## 8. 测试策略

| 层次 | 手段 | 覆盖对象 |
| --- | --- | --- |
| 纯函数单测 | 直接断言 | 旋转换算（含 0°/180° 的 y 翻转、往返一致性）、布局（摆放/可见页/命中/夹取）、文本区间与选区、缓存键量化 |
| 装配端到端 | 内存假后端 + 同步调度器 | 打开 → 请求可见页 → 位图入缓存 → 翻页 → 选词/复制 → 搜索/大纲；以及"无文本层后端"的降级路径 |
| 冒烟测试 | 真实 PDF 采样集 | pdfium 适配层；重点看旋转页、CJK、加密文档 |
| 不测 | — | egui 绘制、GPU 上屏（人工验收） |

关键收益：**布局、滚动、选择、调度这些与 PDF 无关的逻辑，可在毫秒级单测里验证**。适配层出问题时，可先用假后端复现，从而判断故障在上层逻辑还是后端适配。

---

## 9. 与交付路线图的对应

| 阶段 | 需要哪些零件就位 |
| --- | --- |
| **M1 走通** ✅ | `PdfBackend`、`Document`、`PageRaster`、`RenderCache`(`LruCache`)、`RenderScheduler`(`WorkerScheduler`)、`PageLayout`(VerticalLayout)、`DocumentSession` + `crates/app`（egui 壳）|
| **M2 可用阅读器** | 加 `TextLayer`、`TextSelection`（已完成基础版）；原生文件对话框、最近文件、缩略图侧边栏、搜索转后台 |
| **M3 可发布** | 加 `KeyValueStore`、`AppSettings`（设置与最近文件）；打包与 CI；可选静态化 PDFium |
| **M4 增强** | 新增 `PageLayout` 实现（对开）、批注相关 trait（届时再设计） |

---

## 10. 复核要点与下一步

三处重点已确认（MVP 功能边界、并发模型、坐标系约定）。本轮批复后**唯一待你确认的**是 §0 第 1 行：

- **MVP 功能边界是否要增删？** —— 少一项会让 M2 返工，多一项会拖慢首发。

下一步（**v3 接口层与内存假后端已完成并测试通过**，见 [decision-log.md](decision-log.md) §3/§4）：

1. ✅ 工具链就绪，`cargo test` 全绿：50 单元 + 14 集成 + 1 doctest = **65 passed, 0 failed, 0 warnings**；
2. ✅ **D-01 / D-02 / D-05 / D-08** 已逐项改造完成，每项都带测试；
3. ⏭ 进入 M1：新建 `crates/backend-pdfium`（真实后端），再做 `app`（egui 壳）与打包。
