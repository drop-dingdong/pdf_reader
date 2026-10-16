# trait 接口设计（**评审稿 v1**）

> 配套文档：[architecture.md](architecture.md)（分层、并发、坐标系、决策）
> 阅读方式：每个 trait 先看 Rust 声明里的 `///` 注释（说明**为什么需要它**），再看紧随其后的"评审要点"。
> 状态：**待你评审**，未写实现代码。

## 全局约定

所有 trait 遵守以下 5 条，后面不再重复：

1. **对象安全**：方法不含泛型参数、不返回 `impl Trait`、不用 `async fn`，因此可以写成 `Arc<dyn Trait>` / `Box<dyn Trait>` —— 这是"运行时替换零件"的前提。
2. **接收者一律 `&self`**：需要内部可变性的实现自己用 `Mutex`/`RwLock`/原子量。好处是 `Arc<dyn T>` 可在 UI 与后台线程间自由共享，调用方无需区分 `&mut`。
3. **跨线程契约显式**：需要被后台线程调用的 trait 声明 `Send + Sync`；只属于 UI 线程的（如 `TextSelection`）只要求 `Send`（允许把会话整体搬到别的线程，但不并发访问）。
4. **返回自有数据**：不得把借用自内部状态的数据（`&str`、`&[T]`）交给调用方，一律返回 `String`/`Vec<T>`/`Arc<T>`，避免生命周期传染到 UI。
5. **能力缺失不是错误**：可选能力用 `Option`（零件不存在）或默认返回 `CoreError::Unsupported` 表达，而不是让实现返回假数据。

---

## 1. `PdfBackend` —— 后端工厂，可替换性的根节点

```rust
/// **后端工厂**：本接口层中唯一"知道某个具体 PDF 库存在"的抽象。
///
/// ## 这个 trait 存在的目的
/// 它是可替换性的根节点：UI 只持有它产出的 [`OpenedDocument`]，因此
/// 把 pdfium 后端换成 hayro 后端、或在测试里换成内存后端，都不需要改动 UI 与会话代码。
/// 换句话说，整个项目"用什么 PDF 库"这个决定，被压缩到了这一个 trait 的实现里。
///
/// ## 实现者义务
/// * 遇到加密文档且未提供密码时返回 [`CoreError::PasswordRequired`]；
///   密码错误返回 [`CoreError::IncorrectPassword`]。两者都是**非故障**分支，
///   UI 据此弹密码框后重试，而不是弹错误提示。
/// * 不得在 `open` 之后继续持有文档所有权：文档生命周期由返回值的 `Arc` 决定，
///   因此"关闭文档"就是 drop，不需要额外的 `close()` 方法（少一个可能忘记调用的 API）。
/// * 必须提供 [`PageRaster`]；[`TextLayer`] 可缺省，但缺省时必须把
///   [`Capabilities::text_layer`] 声明为 `false`，否则 UI 会显示无效入口。
pub trait PdfBackend: Send + Sync {
    /// 后端名称，用于"关于"对话框与日志（如 `"pdfium"` / `"hayro"` / `"memory"`）。
    fn name(&self) -> &'static str;

    /// 声明能力，UI 据此决定显示哪些功能入口（搜索框、复制按钮、密码框…）。
    fn capabilities(&self) -> Capabilities;

    /// 打开文档。同一个 backend 实例可并发打开多个文档。
    fn open(&self, source: Source<'_>, password: Option<&str>) -> Result<OpenedDocument>;
}
```

**评审要点**

- 一个 `PdfBackend` 实例对应"一个后端库"，而不是"一个文档"；多标签/多窗口就是多次 `open`。
- `Source<'a>` 目前只有 `Path` / `Bytes` 两个变体。是否需要第三种"已解密的读取器"（`&mut dyn Read`）？我倾向于**暂不加入**：它会让实现者难以做 mmap/增量加载，且真实需求（网络流）可用 `Bytes` 覆盖。
- `name()` 返回 `&'static str` 而不是 `String`：名称是编译期常量，避免无意义的分配。

---

## 2. `Document` —— 只读文档结构

```rust
/// **文档结构**：只读、无状态、可并发访问的文档信息。
///
/// ## 这个 trait 存在的目的
/// 把"文档结构查询"从具体后端中抽出，使布局引擎与会话可以在不知道 PDFium 存在的情况下工作，
/// 也让单测能构造一个"3 页、尺寸写死"的假文档。
/// 它只覆盖**不变的部分**：页数、页尺寸、`/Rotate`、元数据、大纲、链接；
/// 页面**像素**归 [`PageRaster`]，页面**文字**归 [`TextLayer`]。
///
/// ## 实现者义务
/// * `info()` 必须 O(1)：页数与元数据在打开时已解析完毕，不要每次重新解析。
/// * `page_size` / `page_rotation` 返回**未旋转**的原始值 —— 旋转由布局叠加。
/// * 大纲与链接属可选能力：不支持时返回空 `Vec`，只有真正的解析失败才返回错误。
pub trait Document: Send + Sync {
    /// 元数据与页数。
    fn info(&self) -> &DocumentInfo;

    /// 某页的原始尺寸（PDF point，未旋转）。
    fn page_size(&self, page: PageIndex) -> Result<Size>;

    /// 某页自带的 `/Rotate`。默认不旋转。
    ///
    /// 注意它是**逐页**的：一篇文档里不同页可以有不同的旋转值。
    fn page_rotation(&self, page: PageIndex) -> Result<Rotation> { /* 默认 Ok(Rotation::None) */ }

    /// 大纲（书签）树。默认返回空列表。
    fn outline(&self) -> Result<Vec<OutlineNode>> { /* 默认 Ok(Vec::new()) */ }

    /// 某页的超链接热区。默认返回空列表。
    fn links(&self, page: PageIndex) -> Result<Vec<Link>> { /* 默认 Ok(Vec::new()) */ }
}
```

**评审要点**

- `page_size` 被调用 N 次（N = 页数）来建立布局，每页一次跨 FFI 调用；对数千页文档是可以接受的（只在打开/旋转/缩放时重建），但如果你希望更快，可以增加一个批量方法 `page_sizes(&self) -> Result<Vec<Size>>`。**要加吗？**
- 没有 `close()`：靠 `Arc` 的 drop。这是有意的——避免"忘了关"导致的 PDFium 句柄泄漏。
- `info()` 返回 `&DocumentInfo` 而非克隆：元数据不变，避免每帧克隆字符串。

---

## 3. `PageRaster` —— 光栅化，最核心的替换缝

```rust
/// **光栅化能力**：把一页按请求渲染成位图。整个项目里最重要的一个替换缝。
///
/// ## 这个 trait 存在的目的
/// 把"PDF 后端"压缩成**一个方法**，从而：
/// * PDFium 适配层只需实现它，不必关心缓存、优先级、线程（那些都在上层）；
/// * 测试可以塞一个"按页号画纯色方块"的假实现，完全不碰真实 PDF；
/// * 未来换 hayro / 自研渲染器时，改动范围仅限于此 trait 的一个实现。
///
/// ## 实现者义务
/// * 支持并发调用（`Send + Sync`），但**并发上限由 [`Capabilities::max_concurrency`] 声明**，
///   调度器会遵守；PDFium 必须声明 1。
/// * 渲染前若 [`CancelToken::is_cancelled`] 为真，应立即返回 [`CoreError::Cancelled`]。
///   （PDFium 无法中断进行中的渲染，所以取消只在任务开始前有效。）
/// * 返回位图的尺寸应为 `页尺寸(旋转后) × scale`，允许 ±1 像素取整误差。
/// * 输出格式如实填写 [`PixelFormat`]：PDFium 原生是 BGRA，不要假装成 RGBA。
pub trait PageRaster: Send + Sync {
    /// 渲染一页。
    fn render(&self, request: &RenderRequest) -> Result<Arc<RasterImage>>;
}
```

**评审要点**

- **没有缩略图方法**（有意）：缩略图 = 小 `Scale` 的普通渲染请求，由 `scale_for_target_width()` 从页尺寸换算得到，因此缩略图与主视图共享同一份缓存。→ 少一个 trait、少一条重复渲染路径。
- **没有分块（tile）渲染**：MVP 以"整页一张位图 + 缩放量化"实现。超大页面（如海报）在高缩放下会吃内存，M4 若需要可分块，届时在 `RenderRequest` 里加 `tile: Option<TileSpec>` 即可，`PageRaster` 签名不变。**是否接受先不分块？**
- 返回 `Arc<RasterImage>` 而不是 `RasterImage`：缓存与 UI 纹理上传需要共享同一份像素，避免拷贝。

---

## 4. `RenderCache` —— 位图缓存

```rust
/// **位图缓存**：`RenderKey` → 位图。
///
/// ## 这个 trait 存在的目的
/// 把"缓存策略"从渲染与 UI 中独立出来，使得：
/// * MVP 可以用几十行的 `HashMap` 先跑通；
/// * 之后换成 LRU / 按内存上限淘汰 / 落磁盘的实现，**不改动任何调用方**；
/// * 单测里可以用"永不命中"或"只保留一页"的缓存来验证调度与重渲染逻辑。
///
/// ## 实现者义务
/// * `get` 会被 UI 线程每帧调用多次，必须 O(1) 且不长时间持锁。
/// * 不允许返回尺寸/格式与键不匹配的位图。
pub trait RenderCache: Send + Sync {
    /// 查询；未命中返回 `None`。
    fn get(&self, key: &RenderKey) -> Option<Arc<RasterImage>>;

    /// 写入（同键覆盖）。
    fn insert(&self, key: RenderKey, image: Arc<RasterImage>);

    /// 丢弃某一页的所有缩放版本（页面内容变化、文档重载时调用）。
    fn remove_page(&self, page: PageIndex);

    /// 清空。
    fn clear(&self);

    /// 统计信息（状态栏展示与调优）；默认返回空统计。
    fn stats(&self) -> CacheStats { /* 默认 CacheStats::default() */ }
}
```

**评审要点**

- 缓存键是 `{ page, 量化缩放, 旋转 }`：**旋转必须进键**，否则"旋转后画面不变"会成为必然 bug。
- `stats()` 给了默认实现，因此一个最小缓存只需实现 4 个方法。
- 是否需要在接口层预留"内存上限"概念？我的意见是**不预留**——上限属于实现细节（构造参数），不是接口契约。

---

## 5. `RenderScheduler` —— 调度，UI 面向的唯一渲染入口

```rust
/// **渲染调度**：UI 面向的唯一渲染入口。
///
/// ## 这个 trait 存在的目的
/// 让 UI 完全不必知道"谁在渲染、有几个线程、PDFium 是否线程安全"：
/// * `submit` 非阻塞，UI 每帧都可以放心调用；
/// * 结果通过 `drain_events` 轮询取回，与 egui 的"每帧拉取"模型天然契合
///   （不必把 UI 状态塞进回调，也不会出现回调跨帧持有 UI 引用的问题）；
/// * 真实实现是"后台线程 + 优先级队列"，测试实现可以**同步渲染**，
///   于是「UI → 会话 → 调度 → 后端 → 缓存 → UI」整条链路能在单测里确定性地跑通。
///
/// ## 实现者义务
/// * `submit` 必须立即返回，不得同步等待渲染完成（同步实现仅用于测试）。
/// * 必须自行去重：同一 [`RenderKey`] 已在队列或已在飞行中时，重复提交应被忽略或合并。
/// * 必须遵守 [`Capabilities::max_concurrency`]；对 PDFium 只能是 1，因此
///   实现通常是**单工作线程**，而不是线程池。
pub trait RenderScheduler: Send + Sync {
    /// 提交任务；返回任务号用于取消。
    fn submit(&self, request: RenderRequest) -> JobId;

    /// 取消指定任务。
    fn cancel(&self, job: JobId);

    /// 取消某一页的所有任务（该页滚出视口时调用）。
    fn cancel_page(&self, page: PageIndex);

    /// 取消全部任务（关闭文档、缩放/旋转导致缓存键整体失效时调用）。
    fn cancel_all(&self);

    /// 取出至多 `max` 条已完成事件（非阻塞）。UI 每帧调用一次。
    fn drain_events(&self, max: usize) -> Vec<RenderEvent>;

    /// 排队中 + 正在执行的任务数（状态栏与背压判断）。
    fn pending(&self) -> usize;
}
```

**评审要点**

- 为什么用"轮询事件"而不是"完成回调"？因为 egui 每帧执行，回调会跨越帧边界持有 UI 状态，容易写出生命周期与重入问题；轮询把并发收敛到一个确定的调用点。
- `drain_events(max)` 带上限：防止一帧内处理过多事件导致掉帧；未取完的留到下一帧。
- **是否需要 `set_concurrency(n)`？** 我倾向不加：并发上限来自后端能力（`max_concurrency`），运行期调整既无必要也容易破坏 PDFium 的串行假设。

---

## 6. `TextLayer` —— 文本层与搜索（**可选零件**）

```rust
/// **文本层能力**：字符级文本提取与全文搜索。
///
/// ## 这个 trait 存在的目的
/// * 让"选中 / 复制 / 搜索"与具体 PDF 库解耦：PDFium 用现成的 `FPDFText_*`，
///   未来自研后端可以从头实现而不影响 UI；
/// * 让能力可缺省：只做渲染的后端（如 hayro）直接让
///   [`OpenedDocument::text`] 为 `None`，UI 据此隐藏搜索与复制入口 ——
///   不需要任何 `if backend == "hayro"` 之类的分支。
///
/// ## 实现者义务
/// * 必须保证 [`PageText`] 的"字符一一对应"不变量：
///   `text.chars().count() == chars.len()`（缺位置的字符也要带零包围盒占位），
///   否则选区与高亮会整体错位。
/// * 包围盒必须在 **PDF 页面空间**（左下原点、point）。
/// * 文本提取可能很慢，实现应自带缓存，或依赖会话层的页文本缓存。
pub trait TextLayer: Send + Sync {
    /// 取某页文本层。
    fn page_text(&self, page: PageIndex) -> Result<Arc<PageText>>;

    /// 全文搜索。
    ///
    /// 默认实现返回 [`CoreError::Unsupported`]：这使"只做文本提取、不做搜索"的后端
    /// 只需实现一个方法；UI 通过 [`Capabilities::search`] 提前知道不要展示搜索框。
    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>> { /* 默认 Err(Unsupported) */ }
}
```

**评审要点**

- 搜索是**同步**的：对几百页文档，全文搜索可能耗时数百毫秒到数秒。MVP 里由 UI 放到自己的后台任务里跑（或者接受短暂卡顿）。**如果你希望接口层统一管这件事，我可以增加一个 `SearchJob` 风格的异步接口** —— 但那会让本 trait 复杂化，我建议先同步、M4 再优化。
- 是否要把 `TextLayer` 拆成 `TextExtractor` 与 `SearchEngine` 两个 trait？我倾向**不拆**：搜索依赖文本提取，拆开后每个后端都要成对实现，且 `Capabilities` 已能表达"有文本但无搜索"。
- 是否需要"整篇文本一次性取出"（用于导出/朗读）？MVP 不需要，可用 `page_text` 循环。

---

## 7. `TextSelection` —— 选区模型（不含文档内容）

```rust
/// **选区模型**：鼠标交互 → 选中区间 → 高亮矩形 / 可复制文本。
///
/// ## 这个 trait 存在的目的
/// 把"选中状态机"从 UI 事件处理与文档内容中剥离出来：
/// * 它**不含文档内容**，只记两个字符位置，因此可被 100% 单测覆盖
///   （含反向拖拽、跨页选择、全选）；
/// * UI 只负责把鼠标坐标交给 [`PageText::hit_test`]，再把结果交给本 trait；
/// * `highlight` 与 `selected_text` 有默认实现（基于 `selection()` + 调用方提供的
///   "取页文本"闭包），因此实现者最少只需写 5 个方法。
///
/// ## 实现者义务
/// * `begin` 重置选择，`extend` 更新焦点；两者收到的 `position` 应已由调用方按
///   [`SelectionGranularity`] 展开（见 [`PageText::range_at`]）—— 也就是说
///   "什么叫一个单词"不属于本 trait 的职责。
/// * 只需 `Send`（选区只属于 UI 线程，但会话可能整体被移动到其它线程）。
pub trait TextSelection: Send {
    /// 当前粒度。
    fn granularity(&self) -> SelectionGranularity;

    /// 设置粒度（单击/双击/三击时由 UI 调用）。
    fn set_granularity(&mut self, granularity: SelectionGranularity);

    /// 开始新选择。
    fn begin(&mut self, position: TextPosition);

    /// 扩展到新位置（拖拽中反复调用）。
    fn extend(&mut self, position: TextPosition);

    /// 清空选择（单击空白、Esc）。
    fn clear(&mut self);

    /// 当前选择；无选择时为 `None`。
    fn selection(&self) -> Option<Selection>;

    /// 是否没有有效选择；默认由 `selection()` 推导。
    fn is_empty(&self) -> bool { /* 默认实现 */ }

    /// 计算高亮矩形；默认实现按页取区间再调 [`PageText::rects_for_range`]。
    /// 尚未加载文本的页会被跳过（UI 可先请求该页文本再重算）。
    fn highlight(&self, text_of: &dyn Fn(PageIndex) -> Option<Arc<PageText>>) -> Vec<Highlight> { /* 默认实现 */ }

    /// 取选中文本用于复制；跨页以换行连接。默认实现。
    fn selected_text(&self, text_of: &dyn Fn(PageIndex) -> Option<Arc<PageText>>) -> String { /* 默认实现 */ }
}
```

**评审要点**

- 这里出现了 `&mut self`（`begin`/`extend`/`clear`/`set_granularity`）——与"全局约定 2"的 `&self` 不一致。**这是有意的例外**：选区是纯 UI 状态，不允许并发访问；用 `&mut self` 能让实现方避免无谓的锁，也把"只有 UI 线程能改选区"写在类型上。**你认可这个例外吗？**
- `highlight`/`selected_text` 用 `&dyn Fn(...)` 而不是 `&dyn TextLayer`：这样选区模型完全不知道"文本从哪来"，测试里传一个闭包即可，无需构造后端。
- 默认实现覆盖了高亮与复制，`RangeSelection` 只需 ~30 行。将来若要"以词为单位的增量拖拽""选择记忆/多选区"，写第二个实现即可，UI 不变。

---

## 8. `PageLayout` —— 布局引擎（纯函数）

```rust
/// **布局引擎**：页几何 → 内容空间排布。
///
/// ## 这个 trait 存在的目的
/// * 布局是**纯函数**：输入页几何与参数，输出每页矩形；不碰 PDF、不碰 GPU、不碰 UI 状态，
///   因此可被完整单测 —— 而"滚动错位""跳页偏了""高亮上下颠倒"绝大多数出在这一层；
/// * 允许替换排布方式（纵向连续 / 单页翻页 / 双页对开 / 横向），
///   会话与 UI 完全不需要改动；
/// * 通过**默认方法**提供滚动相关的高频操作（可见页、命中、跳页、夹取滚动），
///   实现者最少只需实现 `compute` 一个方法。
///
/// ## 实现者义务
/// * `compute` 必须是纯函数：相同输入必须得到相同输出。
/// * `placements` 的顺序必须与页序一致，且覆盖所有页（缺页会导致该页永远渲染不出来）。
/// * 必须把 `params.user_rotation` 与每页自带的 `rotation` 叠加后写进
///   [`PagePlacement::rotation`] —— 渲染请求会直接采用它。
pub trait PageLayout: Send + Sync {
    /// 计算整篇文档的布局。**唯一必须实现的方法。**
    fn compute(&self, pages: &[PageGeometry], params: &LayoutParams) -> DocumentLayout;

    /// 取视口内可见的页。
    ///
    /// 用 `out: &mut Vec` 而不是返回 `Vec`：UI 每帧都要调用，复用缓冲可避免
    /// 每秒 60 次的分配。
    fn visible_pages(&self, layout: &DocumentLayout, viewport: &Viewport, out: &mut Vec<PageIndex>) { /* 默认实现 */ }

    /// 命中测试：内容空间的点落在哪一页上。
    fn page_at(&self, layout: &DocumentLayout, content_point: Point) -> Option<PageIndex> { /* 默认实现 */ }

    /// 跳转到某页时应该设置的滚动偏移（默认把该页居中，并夹取到合法范围）。
    fn scroll_to_page(&self, layout: &DocumentLayout, page: PageIndex, viewport_size: Size) -> Vec2 { /* 默认实现 */ }

    /// 最大滚动偏移。
    fn max_scroll(&self, layout: &DocumentLayout, viewport_size: Size) -> Vec2 { /* 默认实现 */ }

    /// 把滚动偏移夹取到合法范围（避免滚出内容区出现空白页）。
    fn clamp_scroll(&self, layout: &DocumentLayout, viewport_size: Size, scroll: Vec2) -> Vec2 { /* 默认实现 */ }
}
```

**评审要点**

- `compute` 接收 `&[PageGeometry]`（每页尺寸 + 自带旋转）而不是 `&[Size]`：因为 `/Rotate` 是**逐页**的。**这是对"全局一个旋转值"写法的有意否决。**
- `visible_pages` 目前是"遍历所有 placement 做矩形相交"：数千页时每帧 O(N)，实测在微秒级，可接受；十万页文档再换二分查找（`DocumentLayout` 内部可加按 y 排序的索引，接口不变）。
- 默认实现的 `scroll_to_page` 是"页面居中"；如果你希望"页面顶端对齐"或"保留旧滚动比例"，告诉我，改默认实现即可，不影响任何调用方。

---

## 9. `KeyValueStore` —— 偏好与最近文件持久化（可选零件）

```rust
/// 极简键值存储。
///
/// ## 这个 trait 存在的目的
/// 把"设置存哪里、用什么格式"从会话与 UI 中隔离出去：桌面实现写配置文件，
/// 测试实现用 `HashMap`，从而让"设置读写"也能被单测覆盖；同时避免接口层
/// 引入 serde/toml/directories 等依赖。
///
/// ## 实现者义务
/// * `get` 对不存在的键返回 `None`（不要报错）。
/// * `set` 失败（磁盘只读等）返回错误，由 UI 决定是提示还是忽略。
pub trait KeyValueStore: Send + Sync {
    /// 读取。
    fn get(&self, key: &str) -> Option<String>;

    /// 写入（覆盖）。
    fn set(&self, key: &str, value: &str) -> Result<()>;

    /// 删除。
    fn remove(&self, key: &str);
}
```

**评审要点**

- 只有 3 个方法、值统一是 `String`：够 MVP 用（缩放、布局模式、最近文件列表）。若将来偏好变复杂，可在 `pdfcore` 之上加一个"序列化层"，而不必改这个 trait。
- `AppSettings` / `RecentFiles` 是**普通结构体**（不是 trait），负责"字段 ↔ 键值"的映射；损坏的设置退化为默认值，不应阻止应用启动。

---

## 10. 为什么**不**给这些概念建 trait

| 概念 | 决定 | 理由 |
| --- | --- | --- |
| 缩略图 | 不建 trait | 复用 `PageRaster` + `scale_for_target_width()`，与主视图共享缓存 |
| 剪贴板 | 不建 trait | `SessionEvent::CopyToClipboard(String)` 交回 UI，接口层保持无平台依赖 |
| 文件对话框 | 不建 trait | 属于 UI 职责；会话只接受"已打开的文档" |
| 缓存淘汰策略 | 不建 trait | 是 `RenderCache` 实现的构造参数，不是接口契约 |
| 事件总线/观察者 | 不建 trait | 会话已用"命令进、事件出"覆盖；再加一层会重复 |
| 线程池/任务执行器 | 不建 trait | 由 `RenderScheduler` 实现内部决定；暴露出来只会泄漏实现细节 |

---

## 11. 枚举清单（逐变体语义）

| 枚举 | 变体 | 语义 / 边界 |
| --- | --- | --- |
| `CoreError` | `Io` / `PasswordRequired` / `IncorrectPassword` / `CorruptDocument` / `PageOutOfRange` / `Unsupported(&'static str)` / `Cancelled` / `Backend` / `Internal` | 前 4 个是用户可见故障；`PasswordRequired`/`IncorrectPassword` 触发密码框；`Unsupported` 触发能力降级；`Cancelled` **不得**弹提示 |
| `PixelFormat` | `Rgba8` / `Bgra8` / `Gray8` | PDFium 原生 BGRA、egui 要 RGBA：格式显式建模后，转换只发生在 `RasterImage::to_rgba8()` |
| `Rotation` | `None` / `Clockwise90` / `Clockwise180` / `Clockwise270` | 页面 `/Rotate` ⊕ 用户旋转；提供 `compose`/`inverse`/`map_point`/`apply_to_size`。**0° 与 180° 也必须做 y 轴翻转** |
| `RenderPriority` | `Interactive` / `Visible` / `Prefetch` / `Background` | `rank()` 决定出队顺序；`Interactive` 用于翻页后的目标页 |
| `PageLayoutMode` | `Continuous` / `SinglePage` | MVP 共用纵向布局，差别在滚动是否按页对齐；"只渲染当前页"的裁剪属 M4 |
| `ReadingDirection` | `LeftToRight` / `RightToLeft` | MVP 不生效，为对开/RTL 预留 |
| `SelectionGranularity` | `Char` / `Word` / `Line` | 单击/双击/三击；展开逻辑在 `PageText::range_at` |
| `SidebarTab` | `None` / `Outline` / `Thumbnails` / `Search` | 纯 UI 状态 |
| `Source<'a>` | `Path` / `Bytes` | 打开来源；单测用 `Bytes` 可避免产生临时文件 |
| `Destination` | `Page{page, offset}` / `Named` / `Uri` | 大纲与链接共用；`Named` 需后端二次解析 |
| `RenderEvent` | `Rendered{key, image}` / `Failed{page, error}` / `Cancelled{page}` | 调度器 → 会话；`Rendered` 携带位图，会话直接写缓存 |
| `SessionEvent` | `ViewportChanged` / `LayoutChanged` / `ScrollChanged` / `CurrentPageChanged` / `SelectionChanged` / `SidebarChanged` / `PageRendered` / `PageRenderFailed` / `PageTextLoaded` / `CopyToClipboard` / `Error` | 会话 → UI：UI 只依赖这个枚举驱动界面刷新 |
| `ViewerCommand` | 视口/滚动/翻页/缩放/旋转/侧边栏/粒度/选择/复制/请求可见页 | UI → 会话；每个命令互斥且幂等（可安全重复发送） |

**关于枚举的三条设计取向**

1. **不用 `bool` 参数**：`Capabilities` 用命名字段而非位标志，调用处可读（`capabilities.text_layer` 优于 `caps & TEXT`）。
2. **不用 `String` 表达状态**：所有状态都是枚举，便于 `match` 穷尽检查，将来加变体时编译器会指出所有需要处理的地方。
3. **可选能力用 `Option`，不用"空实现"**：`OpenedDocument::text: Option<Arc<dyn TextLayer>>` 让"没有文本层"在类型上可见，而不是返回空 `PageText` 让 UI 误判。

---

## 12. 请你重点批复的 5 个接口问题

1. **`Document` 是否增加批量 `page_sizes()`**（§2）？影响打开大文档的耗时。
2. **`PageRaster` 是否先不做分块渲染**（§3）？影响超大页面的内存上限。
3. **搜索是否保持同步接口**（§6）？影响全文搜索时的 UI 卡顿。
4. **`TextSelection` 用 `&mut self` 的例外是否接受**（§7）？
5. **`scroll_to_page` 默认"页面居中"是否合适**（§8）？

批复后我会据此定稿 `traits.md`，再进入实现（建议先实现 `pdfcore` + 内存假后端，用单测锁定 §5 的坐标不变量与布局行为）。
