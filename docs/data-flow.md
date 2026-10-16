# 数据流推演（**评审稿 v1**）

> 配套：[trait-definitions.md](trait-definitions.md)（签名）、[architecture.md](architecture.md)（分层与决策）
> 目的：把"某个具体动作发生时，数据以什么形态、经过哪个 trait、变成什么"逐步写清楚。
> 读法：每个场景 **触发 → 步骤 → 形态变化表 → 关键点**。文中数字都是**可直接验算**的具体值。

## 图例

| 标记 | 含义 |
| --- | --- |
| `[UI]` | UI 线程（egui 每帧执行） |
| `[BG]` | 后台渲染线程（数量 = `Capabilities::max_concurrency`，PDFium = 1） |
| `[ANY]` | 两侧都可能访问（内部加锁） |
| ★ | 该步骤是设计上的关键约束，改动需同步改文档 |

---

## 场景 1：打开文件（`path` → `Document`）

### 触发
用户点"打开"，UI 用 `rfd` 拿到 `PathBuf("/home/u/paper.pdf")`。

### 步骤

1. `[UI]` 读取设置取上次目录 → 弹文件对话框 → 得到 `path: PathBuf`。
2. `[UI]` 调 `backend.open(Source::Path(&path), None)` ★ —— 注意传的是**引用**，`Source<'a>` 借用 `path` 的生命周期，不产生拷贝。
3. `[BG]`（PDFium 内部）：
   1. `FPDF_LoadDocument(path)` → `FPDF_DOCUMENT` 句柄（空则读 `FPDF_GetLastError()`）；
   2. 读页数、元数据、加密标志与权限；
   3. 构造三个对象：`PdfiumDocument`（结构）、`PdfiumRaster`（像素）、`PdfiumTextLayer`（文字），各自 `Arc`；
   4. 填 `Capabilities { text_layer: true, search: true, encryption: true, outline: true, links: true, max_concurrency: 1 }` ★。
4. `[BG]` 返回 `Ok(OpenedDocument { document, raster, text: Some(..), capabilities })`。
5. `[UI]` 调 `DocumentSession::new(opened, cache, scheduler, Box::new(VerticalLayout), Box::new(RangeSelection::new()))?`：
   1. 循环 `page_count` 次调 `document.page_size(i)` 与 `document.page_rotation(i)` ★
      → 得到 `Vec<PageGeometry>`（这是**唯一一次**全量读页尺寸；数千页文档也只在打开时做一遍）；
   2. `layout_engine.compute(&pages, &LayoutParams { .. })` → `DocumentLayout`。
6. `[UI]` 进入每帧循环（见场景 2）。

### 形态变化

| 阶段 | 数据形态 |
| --- | --- |
| 用户操作 | `PathBuf` |
| 传给后端 | `Source::Path(&Path)`（借用，零拷贝） |
| 后端返回 | `OpenedDocument { Arc<dyn Document>, Arc<dyn PageRaster>, Option<Arc<dyn TextLayer>>, Capabilities }` |
| 会话内部 | `Vec<PageGeometry>`（页数 × {尺寸, /Rotate}） |
| 布局产物 | `DocumentLayout { content_size, Vec<PagePlacement> }` |

### 分支：加密文档

```text
open(path, None) ──▶ Err(CoreError::PasswordRequired)      // 句柄为空且错误码 = PASSWORD
   [UI] 弹密码框，用户输入 "1234"
open(path, Some("1234")) ──▶ Err(CoreError::IncorrectPassword)  // 重新弹框，显示"密码错误"
open(path, Some("s3cr3t")) ──▶ Ok(OpenedDocument)               // 成功
```

**关键点**：`PasswordRequired` / `IncorrectPassword` 是 `CoreError` 里的**非故障**分支（`is_auth_related()` 为真），UI 据此决定"弹输入框"而不是"弹错误提示"。用 `Option` 返回值就无法区分这两种情况。

### 关键点
- ★ `open` 传引用而非所有权：文档生命周期由返回的 `Arc` 决定，"关闭 = drop"。
- ★ `max_concurrency = 1` 会被 `WorkerScheduler` 读取，决定它只起 **1** 个渲染线程。
- 会话在 `new` 里就把 `Vec<PageGeometry>` 固化，后续缩放/旋转**不再**访问 `Document`。

---

## 场景 2：首帧渲染（空视口 → 屏幕上出现第一页）

### 触发
窗口刚显示，`viewport.size = (900, 1000)`，`zoom = 1.5`，单页文档 `612 × 792 pt`。

### 步骤

1. `[UI]` `handle(ResizeViewport(Size{900,1000}))` → 会话记下视口尺寸并夹取滚动 → 事件 `ViewportChanged`。
2. `[UI]` `handle(RequestVisible)`：
   1. `layout_engine.visible_pages(&layout, &viewport, &mut scratch)` ★（用出参复用缓冲，每帧零分配）；
   2. 对每个可见页调内部 `request_page(page, Visible)`：
      1. 取 `placement`；（此处 `rect = (12, 12, 918, 1188)`，`scale = 1.5`，`rotation = None`）
      2. 算 `RenderKey::new(0, Scale(1.5), None)` → 量化 `1.5 / 0.125 = 12` → `scale_milli = 1500`；
      3. 查 `cache.get(&key)` → `None`；查在飞集合 → 空；
      4. 组 `RenderRequest { page: 0, scale: 1.5, rotation: None, priority: Visible, cancel: CancelToken::new() }`；
      5. `scheduler.submit(request)` → `JobId(1)`；把 key 记入"在飞"。
   3. 预取：可见页首尾 ±1（此处无）。
3. `[BG]` 工作线程从优先级队列取出任务 → 检查 `cancel.is_cancelled()`（否）→
   `raster.render(&request)` → PDFium 渲染 918 × 1188 BGRA → `Arc<RasterImage>`（像素在 `Arc<[u8]>`，≈4.4 MB）。
4. `[BG]` 把 `RenderEvent::Rendered { key, image }` 推入事件队列。
5. `[UI]` **下一帧** `pump(&mut events)`：
   1. `scheduler.drain_events(64)` → 取到 `Rendered`；
   2. `cache.insert(key, image.clone())`（`Arc` 克隆，零拷贝）→ 从"在飞"移除；
   3. 事件 `PageRendered(key)`。
6. `[UI]` 绘制：`session.image_for(PageIndex(0))` → 从缓存按同一 key 取回 `Arc<RasterImage>` →
   `image.to_rgba8()` ★（唯一的 BGRA→RGBA 转换点）→ `egui::ColorImage` → `TextureHandle` →
   画在视图矩形 `placement.rect.translate(-viewport.scroll)`。

### 形态变化

| 阶段 | 数据形态 |
| --- | --- |
| 视口 | `Size { 900, 1000 }` + `Vec2 { 0, 0 }` |
| 可见页 | `Vec<PageIndex> = [0]` |
| 请求 | `RenderRequest { page: 0, scale: 1.5, rotation: None, priority: Visible, cancel }` |
| 键 | `RenderKey { page: 0, scale_milli: 1500, rotation: None }` |
| 后端产物 | `Arc<RasterImage> { width: 918, height: 1188, format: Bgra8 }` |
| 缓存 | `RenderKey → Arc<RasterImage>` |
| 上屏 | `Vec<u8>` RGBA → `egui::ColorImage` → GPU 纹理 |

### 关键点
- ★ **渲染结果下一帧才可见**（异步）。因此缺图时应画"白色页框"占位，而不是让页面消失（否则滚动时闪烁）。
- ★ 请求里的 `scale`/`rotation` **原样取自 `PagePlacement`**，会话不自己算 —— 这是"布局与像素永不打架"的保证。
- `Arc` 贯穿全程：缓存与 UI 共享同一份像素，只有转 RGBA 时有一次必然的拷贝（PDFium 的 BGRA 无法直接喂给 egui）。

---

## 场景 3：一帧滚动（滚轮下滑 400px）

三页文档（`612 × 792 pt`，缩放 1.0，留白 12，页间距 12），`viewport = (700, 800)`：

```text
页 0 置放矩形 = (12,   12, 612, 792)
页 1 置放矩形 = (12,  816, 612, 792)
页 2 置放矩形 = (12, 1620, 612, 792)
内容尺寸 = (12 + 612 + 12, 12 + 792×3 + 12×2 + 12) = (636, 2424)
最大滚动 = (max(636−700, 0), max(2424−800, 0)) = (0, 1624)
```

### 步骤

1. `[UI]` 滚轮 → `handle(ScrollBy(Vec2{0, 400}))`。
2. `[UI]` `set_scroll(target = (0, 0+400))` → `layout_engine.clamp_scroll(..)` → `(0, 400)`（未越界）。
3. `[UI]` 事件 `ScrollChanged((0,400))`；`sync_current_page()`：探针 `(350, 401)` 落在页 0（`12..804`）→ 当前页仍为 0，不发事件。
4. `[UI]` 绘制可见页（此时位图已在缓存，直接命中）：页 0 画在 `y = 12−400 = −388`（大部分被裁剪），页 1 画在 `y = 816−400 = 416`。
5. `[UI]` `handle(RequestVisible)` → `visible_pages` → 窗口 `(0,400,700,800)`：
   - 页 0：`12 < 1200 ∧ 400 < 804` → 相交 ✅
   - 页 1：`816 < 1200 ∧ 400 < 1608` → 相交 ✅
   - 页 2：`1620 < 1200` ✗
   → `visible = [0, 1]`；页 0 命中缓存跳过；页 1 提交 `Visible`；预取页 2（`Prefetch`）。
6. `[BG]`→`[UI]` 下一帧 `pump()` 收到页 1 位图 → 入缓存 → 事件 `PageRendered`。
7. `[UI]` 继续滚动到 `scroll = (0, 1700)` → 夹取为 `(0, 1624)` → 可见 `[2]`；
   探针 `(350, 1625)` 落在页 2 → 事件 `CurrentPageChanged(PageIndex(2))`（状态栏与缩略图高亮更新）。
   与此同时，页 0/1 若还在队列里 → `scheduler.cancel_page(0)` / `cancel_page(1)`。

### 关键点
- ★ "当前页"由**视口顶部探针**决定（`scroll.y + 1`，水平取中央），而不是"最后一个可见页" —— 后者在快速滚动时会来回跳。
- ★ 滚出视口的页要 `cancel_page`：PDFium 只能串行渲染，队列里堆着看不见的页会让用户看到的那页变慢。
- 页间距处（`804..816`）探针不命中任何页，此时**保留**上一次的当前页，而不是清空。

---

## 场景 4：缩放与缓存键量化

### 步骤（以场景 2 的状态为起点）

1. `[UI]` `ZoomIn` → `zoom = 1.5 × 1.25 = 1.875`。
2. `[UI]` `invalidate_render()`：`scheduler.cancel_all()` + 清空"在飞"集合 ★
   （缩放后旧键全部失效，继续渲染旧缩放的位图是纯浪费）。
3. `[UI]` `recompute_layout()` → 新 `placements`（页 0 矩形变为 `(12, 12, 1147.5, 1485)`）。
4. `[UI]` 滚动重新锚定到当前页居中 → 事件 `LayoutChanged` + `ScrollChanged`。
5. `[UI]` 下一帧 `RequestVisible` → `RenderKey { page: 0, scale_milli: 1875, rotation: None }` → 未命中 → 渲染。
6. 用户继续微调到 `zoom = 1.90`：量化 `1.90 / 0.125 = 15.2 → 15 → 1.875` ★ → 键仍是 `1875` → **缓存命中**，直接拉伸 1.875 倍率的位图（`+1.3%` 误差肉眼不可见）。

### 量化边界示例

| zoom | `/0.125` | 取整 | 量化后 | scale_milli | 结果 |
| --- | --- | --- | --- | --- | --- |
| 1.50 | 12.00 | 12 | 1.500 | 1500 | 与 1.50 同档 |
| 1.52 | 12.16 | 12 | 1.500 | 1500 | **命中** 1.50 的位图 |
| 1.56 | 12.48 | 12 | 1.500 | 1500 | **命中** |
| 1.57 | 12.56 | 13 | 1.625 | 1625 | **换档**，重新渲染 |
| 1.875 | 15.00 | 15 | 1.875 | 1875 | 与 1.875 同档 |

### 关键点
- ★ 量化步长 1/8（12.5%）是"清晰度 vs 缓存命中率"的折中；档位最多 `log(8)` 数量级，不会爆缓存。
- ★ `invalidate_render` 必须同时清"在飞"集合，否则旧键会被判定为"已在渲染"而永不重提交。
- 缩放锚点目前是"当前页居中"（最可预测）；若要"以光标为锚点"，只需在 `set_zoom` 里保留光标的内容坐标 —— 接口不变。

---

## 场景 5：翻页（NextPage）

1. `[UI]` `handle(NextPage)` → 会话检查 `current_page + 1 < page_count`（越界则发 `Error(PageOutOfRange)` 而不 panic）。
2. `[UI]` `state.current_page = 1`；`scroll = layout_engine.scroll_to_page(layout, 1, viewport.size)`
   → 页 1 矩形 `(12, 816, 612, 792)`，视口高 800 → 目标 `y = 816 + 396 − 400 = 812` → 夹取后仍为 `812`。
3. `[UI]` 事件 `CurrentPageChanged(1)` + `ScrollChanged((0, 812))`。
4. `[UI]` `request_page(1, RenderPriority::Interactive)` ★ —— 翻页是"用户此刻就要看"的动作，
   优先级最高，会插到所有 `Visible`/`Prefetch` 任务之前。
5. `[BG]` 队列按 `RenderPriority::rank()` 出队：`Interactive(0) < Visible(1) < Prefetch(2) < Background(3)`。
6. `[UI]` 下一帧 `pump()` → 位图入缓存 → 绘制页 1。

**关键点**：`scroll_to_page` 是 `PageLayout` 的默认方法（页面居中 + 夹取），因此换成"对开布局"时跳页行为自动适配。

---

## 场景 6：双击选词 → 高亮 → 复制（含逐步坐标数值）

这是全流程里最容易出错的一段，因此把每一步的**具体数字**都写出来。

### 环境
单页 `612 × 792 pt`，`/Rotate = None`，`user_rotation = None`，`zoom = 1.5`（`Scale(1.5)`），
留白 12，`scroll = (0, 0)`：

```text
placement = PagePlacement { page: 0, rect: (12, 12, 918, 1188), scale: 1.5, rotation: None, page_size: (612, 792) }
```

页面上有一行 12pt 文本，`PageText`（PDF 页面空间，左下原点 y 向上，基线 y = 686）：

```text
chars[0] 'H' bbox = (72.00, 686, 6.03, 12)     chars[3] 'l' bbox = (90.09, 686, 6.03, 12)
chars[1] 'e' bbox = (78.03, 686, 6.03, 12)     chars[4] 'o' bbox = (96.12, 686, 6.03, 12)
chars[2] 'l' bbox = (84.06, 686, 6.03, 12)     text = "Hello world"
```

### 步骤

1. `[UI]` 鼠标双击 → 先发 `handle(SetSelectionGranularity(Word))`。
2. `[UI]` 视图坐标 `view = (150.0, 160.0)` → 转内容坐标：`content = view + scroll = (150.0, 160.0)`。
3. `[UI]` `handle(SelectBegin(TextPosition{..}))` 之前先要"鼠标 → 字符"：
   `session.text_position_at(Point{150, 160})`：
   1. `layout_engine.page_at(layout, (150,160))` → 页 0（矩形 `12..930 × 12..1200` 包含该点）；
   2. `placement.content_to_page((150,160))`：
      - 去偏移去缩放：`display = ((150−12)/1.5, (160−12)/1.5) = (92.00, 98.67)`；
      - `Rotation::None::unmap_point` ★（**含 y 轴翻转**）：`pdf = (92.00, 792 − 98.67) = (92.00, 693.33)`；
   3. `session.page_text(0)` → 首次调用触发 `TextLayer::page_text(0)` → `Arc<PageText>` → 存入会话缓存；
   4. `PageText::hit_test((92.00, 693.33))` → `chars[3]` 的包围盒 `x 90.09..96.12, y 686..698` 包含该点 → `index = 3`。
   → `TextPosition { page: 0, index: 3 }`。
4. `[UI]` `handle(SelectBegin(position))`：会话按粒度展开
   `PageText::range_at(3, Word)` → `word_range_at(3)` → `TextRange { start: 0, end: 5 }`（"Hello"）
   → `selection.begin((0,0))` + `selection.extend((0,5))` ★。
5. `[UI]` `handle(SelectExtend(..))`（按住拖动时反复调用）→ 每次只把焦点移到新位置；锚点不变。
6. `[UI]` 绘制高亮：`session.highlight_selection()`：
   1. `Selection::page_ranges()` → `[(PageIndex(0), TextRange{0,5})]`；
   2. `PageText::rects_for_range(0..5)` → 按行合并 → PDF 空间 `Rect { x: 72.00, y: 686, w: 30.15, h: 12 }`；
   3. `PagePlacement::rect_to_content(..)`（四角取外接矩形）：
      - `x: 12 + 72.00×1.5 = 120.00` → `12 + 102.15×1.5 = 165.23`
      - `y`: PDF 698 → display 94 → `12 + 141 = 153`；PDF 686 → display 106 → `12 + 159 = 171`
      → `Rect { x: 120.00, y: 153.00, w: 45.23, h: 18.00 }`；
   4. 视图矩形 = 内容矩形 − `scroll` = 原值；`Highlight { page: 0, rects: [..], kind: Selection }`。
7. `[UI]` 用半透明蓝填充该矩形（`kind` 决定配色）。
8. `[UI]` `handle(CopySelection)` → `selection.selected_text(&lookup)` →
   `chars_in(0..5)` → `"Hello"` → 事件 `SessionEvent::CopyToClipboard("Hello")`
   → ★ **UI 自己去写系统剪贴板**（会话不依赖任何平台 API）。

### 坐标换算链（单一职责的验证）

| 步骤 | 输入 | 变换 | 输出 |
| --- | --- | --- | --- |
| 1 | 视图 `(150, 160)` | `+ scroll` | 内容 `(150, 160)` |
| 2 | 内容 `(150, 160)` | `PagePlacement::content_to_page` | PDF `(92.00, 693.33)` |
| 3 | PDF 点 | `PageText::hit_test` | 字符下标 `3` |
| 4 | 下标 + 粒度 | `PageText::range_at` | `TextRange { 0, 5 }` |
| 5 | 区间 | `rects_for_range`（PDF 空间） | `Rect { 72, 686, 30.15, 12 }` |
| 6 | PDF 矩形 | `PagePlacement::rect_to_content` | 内容 `Rect { 120, 153, 45.23, 18 }` |
| 7 | 内容矩形 | `− scroll` | 视图矩形（供绘制） |

### 关键点
- ★ **后端的字符包围盒永远是 PDF 页面空间**；只有 `PagePlacement` 负责 page ↔ content。
  若某处"顺手翻一下 y"，高亮就会整体上下颠倒。
- ★ `Rotation::None` 也必须翻 y（PDF 左下原点 → 视图左上原点）；
  这是实现中最常见的 bug（0° 时直接 return 原值）。
- ★ 旋转发生在缩放**之前**，所以 `content_to_page` 里先减偏移除缩放、再 `unmap_point`。
- "什么叫一个单词"只在 `PageText::range_at` 一处定义；选区模型只做区间记账 → 可单测。

---

## 场景 7：全文搜索 → 跳转到命中

1. `[UI]` 用户输入 "world"，发 `SetSidebar(Search)`。
2. `[UI]` 调 `session.search(&SearchQuery { needle: "world", case_sensitive: false, whole_word: false, start_page: None, limit: None })`：
   - 会话检查 `opened.text`：为 `Some` → 转调 `TextLayer::search(..)`；
   - 为 `None` → 立即返回 `Err(Unsupported("全文搜索"))`，UI 展示"该后端不支持搜索"（或干脆不显示搜索入口，因为 `Capabilities::search == false`）★。
3. `[BG]`（若 UI 把搜索放在自己的后台任务里）→ 后端逐页 `page_text()` + 匹配 → `Vec<SearchHit>`。
4. `[UI]` `session.highlights_for_search(&hits, Some(active))` → `Vec<Highlight>`（当前命中用 `ActiveSearchMatch` 配色）。
5. `[UI]` 用户点第 2 条命中（`hit.page = 3`）→
   `handle(GoToPage(PageIndex(3)))` → 跳到该页；若需要精确到行，可用 `Placement::page_to_content(hit.rects[0].min())` 把滚动位置对准命中行。

**关键点**：搜索是**同步接口**（见 traits.md §6 评审项）；MVP 里由 UI 决定放在主线程还是自己的后台任务。会话不引入异步，避免把复杂度扩散到接口层。

---

## 场景 8：缩略图（复用同一渲染路径，共享同一缓存）

1. `[UI]` 打开侧边栏 `Thumbnails` → 需要 160px 宽的缩略图。
2. `[UI]` 对每一页：`size = document.page_size(page)`；`rotation = document.page_rotation(page)`
   → `Scale = scale_for_target_width(size, rotation, 160.0)`（纯函数，`160 / 旋转后宽度`）。
3. `[UI]` 组 `RenderRequest { page, scale, rotation, priority: Background, cancel }`
   → `scheduler.submit(..)`（优先级最低，绝不抢用户当前页的带宽）★。
4. `[BG]`→`[UI]` 与场景 2 完全相同的路径：`PageRaster::render` → `RenderEvent::Rendered` → `pump` 入缓存。
5. `[UI]` `cache.get(&RenderKey::new(page, Scale(0.2), rotation))` → 画成 160px 宽的小图。

**关键点**：缩略图**没有**独立 trait —— 它就是"小 scale 的普通渲染"，因此：
- 与主视图共享同一份缓存实现（同一页在 0.2 倍率下只渲染一次）；
- 侧边栏虚拟化（只请求可见的缩略图）与主视图用同一套 `visible_pages` 思路。

---

## 场景 9：关闭 / 切换文档

1. `[UI]` 关窗口或打开新文件 → `handle(cancel-all)` 语义：
   `scheduler.cancel_all()` 清空队列；随后 **drop `DocumentSession`**。
2. drop 顺序：`DocumentSession` → `OpenedDocument` → `Arc<dyn Document>` / `Arc<dyn PageRaster>` /
   `Option<Arc<dyn TextLayer>>` 计数归零 → 后端对象的 `Drop` 被调用。
3. `[BG]` PDFium 适配层的 `Drop` 里执行 `FPDF_CloseDocument(handle)` ★（句柄由 `Arc` 的最后一个持有者释放）。
4. `[ANY]` `RenderCache` 由 UI 持有（可跨文档复用），此时应 `cache.clear()`，避免旧文档的位图占内存。

**关键点**：没有 `close()` API → 不可能"忘记关闭"。渲染线程若正持有 `Arc<dyn PageRaster>`，
句柄会延后到该次渲染结束才释放，不会出现 use-after-free。

---

## 场景 10：无文本层后端（hayro 类）的降级路径

| 环节 | 有文本层（PDFium） | 无文本层（hayro 类） |
| --- | --- | --- |
| `open` 返回 | `text: Some(..)`，`Capabilities::text_layer = true` | `text: None`，`text_layer = false` |
| UI 初始布局 | 显示"搜索"按钮与侧边栏 Search 页签 | **不显示**搜索与复制入口（读 `Capabilities`）★ |
| `session.search(..)` | 走 `TextLayer::search` | `Err(Unsupported("全文搜索"))` |
| `text_position_at(..)` | 命中字符，可选中 | `None` → UI 不进入选择状态 |
| `selected_text()` | 返回文本 | 空串 |
| 渲染/滚动/缩放/缩略图 | 正常 | **完全正常**（这些只依赖 `PageRaster` + `PageLayout`） |

**关键点**：降级靠**类型**（`Option`）与**能力声明**（`Capabilities`）双重表达，UI 里不会出现
`if backend_name == "hayro"` 这类分支；新后端只要如实声明能力即可复用全部 UI 代码。

---

## 场景 11：点击大纲 / 页内链接（`Destination` 三个分支）

1. `[UI]` 侧边栏点击大纲项，或点击页面上的链接热区：
   - 链接热区命中：`content_point` → `placement.content_to_page(..)` → PDF 点 →
     与 `document.links(page)` 返回的 `Link::rect` 做包含测试（两者同为 PDF 空间，可直接比较）。
2. `[UI]` 按 `Destination` 分支处理：

| 变体 | 处理 | 数据流 |
| --- | --- | --- |
| `Page { page, offset }` | `handle(GoToPage(page))`；若 `offset` 存在，用 `placement.page_to_content(offset)` 精确对齐 | 纯会话内，无需后端 |
| `Named(name)` | 交回后端解析成具体页（`Document` 需增加 `resolve_named_destination`，**当前未定义**） | 需要后端二次查询 |
| `Uri(s)` | 交回 UI 用系统浏览器打开 | 会话不接触平台 API |

**待你确认**：`Named` 目标当前没有对应方法。两个选择：① 在 `Document` 增加
`fn resolve_destination(&self, name: &str) -> Result<Destination>`（推荐，注意不要返回 `Named` 造成递归）；
② MVP 直接忽略 `Named`（点击无反应）。我倾向 ①，因为它同样服务于"PDF 内部交叉引用跳转"。

---

## 附：一帧内的完整时序（1080p 60fps 场景）

| 阶段 | 线程 | 典型耗时 | 说明 |
| --- | --- | --- | --- |
| 采集输入 → `handle(命令)` | UI | < 0.1 ms | 命令处理只改状态、算布局，不渲染 |
| `recompute_layout`（仅缩放/旋转时） | UI | ~0.1–1 ms | 数千页时线性遍历 |
| `visible_pages` + `request_page` | UI | < 0.2 ms | 出参缓冲，零分配 |
| `pump` → `drain_events` → 入缓存 | UI | ~0.05 ms | `Arc` 交接 |
| `to_rgba8` + 纹理上传（有首页就绪时） | UI | 1–5 ms/页 | 918×1188 ≈ 4.4 MB；已有纹理则跳过 |
| 绘制（egui 三角形） | UI | 1–2 ms | 只画可见页 |
| `PageRaster::render` | BG | **10–200 ms/页** | 真实瓶颈；靠优先级 + 缓存 + 虚拟化保证 UI 不等它 |

结论：UI 侧每帧工作量恒定（与文档页数无关），后端渲染的抖动通过"缺图占位 + 下一帧替换"吸收。

---

## 需要你确认的 3 个数据流细节

1. **场景 2 的"缺图占位"策略**：先画白底页框（推荐），还是画上一档缩放的拉伸图（更流畅但要保留旧纹理）？
2. **场景 7 的搜索执行位置**：MVP 接受主线程同步搜索（大文档可能卡顿），还是从一开始就把搜索放进 UI 自己的后台任务？
3. **场景 11 的 `Named` 目标**：给 `Document` 增加 `resolve_destination()`，还是 MVP 先忽略？
