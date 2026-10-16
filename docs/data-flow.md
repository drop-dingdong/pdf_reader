# 数据流推演（**评审稿 v3**）

> 配套：[trait-definitions.md](trait-definitions.md)（签名）、[architecture.md](architecture.md)、[decision-log.md](decision-log.md)
> **v3 并入你批复的 8 条决定**；`▲v3` 标记本次变更处。
> 读法：每个场景 **触发 → 步骤 → 形态变化表 → 关键点**。文中数字都可直接验算。

## 图例

| 标记 | 含义 |
| --- | --- |
| `[UI]` | UI 线程（egui 每帧执行） |
| `[BG]` | 后台渲染线程（数量 = `Capabilities::max_concurrency`，PDFium = 1） |
| `[ANY]` | 两侧都可能访问（内部加锁） |
| ★ | 该步骤是设计上的关键约束，改动需同步改文档 |
| ▲v3 | v3 新增或修改的内容 |

---

## 场景 1：打开文件（`path` → `Document`）▲v3

### 触发
用户点"打开"，UI 用 `rfd` 拿到 `PathBuf("/home/u/paper.pdf")`。

### 步骤

1. `[UI]` 读设置取上次目录 → 弹文件对话框 → 得到 `path: PathBuf`。
2. `[UI]` 调 `backend.open(Source::Path(&path), None)` ★（传**引用**，零拷贝）。
3. `[BG]`（PDFium 内部）：
   1. `FPDF_LoadDocument(path)` → 句柄（空则读 `FPDF_GetLastError()`）；
   2. ▲v3 **一次性预解析并缓存**三份数据：
      - `Arc<[PageGeometry]>`：逐页 `FPDF_GetPageWidth/Height` + `/Rotate` ★（**唯一一次**全量读页尺寸）；
      - `Arc<NavigationIndex>`：遍历大纲树、每页 `/Annots` 链接、`/Dests` 命名目标，
        把命名目标**全部解析成具体页**后合并存储；
      - `DocumentInfo`：页数、元数据、加密标志与权限。
   3. 构造三个对象：`PdfiumDocument`、`PdfiumRaster`、`PdfiumTextLayer`（各 `Arc`）；
   4. 填 `Capabilities { text_layer: true, search: true, encryption: true, outline: true, links: true, max_concurrency: 1 }` ★。
4. `[BG]` 返回 `Ok(OpenedDocument { document, raster, text: Some(..), capabilities })`。
5. `[UI]` 调 `DocumentSession::new(opened, cache, scheduler, Box::new(VerticalLayout), Box::new(RangeSelection::new()))?`：
   1. ▲v3 `document.page_geometries()` → **`Arc` 克隆（O(1)）**，不再循环 N 次跨 FFI 查询；
      会话保留一份本地副本 `Vec<PageGeometry>`（数 KB，便于 `&[PageGeometry]` 传给布局）；
   2. `layout_engine.compute(&pages, &LayoutParams { .. })` → `DocumentLayout`。
6. `[UI]` 进入每帧循环（场景 2）。

### 形态变化

| 阶段 | 数据形态 |
| --- | --- |
| 用户操作 | `PathBuf` |
| 传给后端 | `Source::Path(&Path)`（借用，零拷贝） |
| 后端返回 | `OpenedDocument { Arc<dyn Document>, Arc<dyn PageRaster>, Option<Arc<dyn TextLayer>>, Capabilities }` |
| ▲v3 后端预解析 | `Arc<[PageGeometry]>`（页数 × {尺寸, /Rotate}）、`Arc<NavigationIndex>`（大纲 + 逐页链接 + 命名目标） |
| 会话内部 | `Vec<PageGeometry>`（本地副本）+ `DocumentLayout { content_size, Vec<PagePlacement> }` |

### 分支：加密文档

```text
open(path, None) ──▶ Err(CoreError::PasswordRequired)      // 句柄为空且错误码 = PASSWORD
   [UI] 弹密码框，用户输入 "1234"
open(path, Some("1234")) ──▶ Err(CoreError::IncorrectPassword)  // 重新弹框，显示"密码错误"
open(path, Some("s3cr3t")) ──▶ Ok(OpenedDocument)               // 成功
```

**关键点**：`PasswordRequired` / `IncorrectPassword` 是 `CoreError` 里的**非故障**分支（`is_auth_related()` 为真），UI 据此决定"弹输入框"而不是"弹错误提示"。用 `Option` 返回值无法区分二者。

### 关键点
- ★ `open` 传引用而非所有权：文档生命周期由返回的 `Arc` 决定，"关闭 = drop"。
- ▲v3 ★ **预解析的代价与收益**：打开时间略增（遍历大纲与链接），换来运行期"点击链接/大纲 0 次后端调用"。
  超大文档（数万页且链接极多）可能让打开变慢 —— 若实测有影响，可把 `navigation()` 改成
  `OnceLock` 惰性构建（首次调用时解析一次），**接口签名不变**。→ 见 decision-log D-08 的备注。
- ★ `max_concurrency = 1` 会被 `WorkerScheduler` 读取，决定它只起 **1** 个渲染线程。

---

## 场景 2：首帧渲染（空视口 → 屏幕上出现第一页）▲v3

### 触发
窗口刚显示，`viewport.size = (900, 1000)`，`zoom = 1.5`，单页文档 `612 × 792 pt`。

### 步骤

1. `[UI]` `handle(ResizeViewport(Size{900,1000}))` → 记下视口尺寸并夹取滚动 → 事件 `ViewportChanged`。
2. `[UI]` `handle(RequestVisible)`：
   1. `layout_engine.visible_pages(&layout, &viewport, &mut scratch)` ★（出参复用缓冲，每帧零分配）；
   2. 对每个可见页调内部 `request_page(page, Visible)`：
      1. 取 `placement`（`rect = (12, 12, 918, 1188)`，`scale = 1.5`，`rotation = None`）；
      2. ▲v3 **决定是否分块**（见场景 2b）：`placement.pixel_size() = (918, 1188)` →
         面积 `1,090,584 px ≈ 1.09 MP` < 4 MP → **整页渲染**，`tile = None`；
      3. 算 `RenderKey::new(0, Scale(1.5), None)` → 量化 `1.5 / 0.125 = 12` → `scale_milli = 1500`，`tile = None`；
      4. 查 `cache.get(&key)` → `None`；查在飞集合 → 空；
      5. 组 `RenderRequest { page: 0, scale: 1.5, rotation: None, tile: None, priority: Visible, cancel: CancelToken::new() }`；
      6. `scheduler.submit(request)` → `JobId(1)`；把 key 记入"在飞"。
   3. 预取：可见页首尾 ±1（此处无）。
3. `[BG]` 工作线程取出任务 → 检查取消（否）→ `raster.render(&request)`
   → PDFium 渲染 918 × 1188 BGRA → `Arc<RasterImage>`（≈4.2 MiB）。
4. `[BG]` 推入 `RenderEvent::Rendered { key, image }`。
5. `[UI]` **下一帧** `pump(&mut events)`：`drain_events(64)` → `cache.insert(key, image.clone())`（`Arc` 克隆，零拷贝）→ 事件 `PageRendered(key)`。
6. `[UI]` 绘制：`session.image_for(PageIndex(0))` → `Arc<RasterImage>` →
   `image.to_rgba8()` ★（唯一的 BGRA→RGBA 转换点）→ `egui::ColorImage` → `TextureHandle` →
   画在视图矩形 `placement.rect.translate(-viewport.scroll)`。
   **位图尚未就绪时**：先用**上一档缩放的位图拉伸占位**；连旧图都没有才画 ★**白底页框**（`placement.rect` + 1px 灰边）（decision-log D-12，补充 D-06）。

### 形态变化

| 阶段 | 数据形态 |
| --- | --- |
| 视口 | `Size { 900, 1000 }` + `Vec2 { 0, 0 }` |
| 可见页 | `Vec<PageIndex> = [0]` |
| 请求 | `RenderRequest { page: 0, scale: 1.5, rotation: None, tile: None, priority: Visible, cancel }` |
| 键 | `RenderKey { page: 0, scale_milli: 1500, rotation: None, tile: None }` ▲v3 |
| 后端产物 | `Arc<RasterImage> { width: 918, height: 1188, format: Bgra8 }` |
| 缓存 | `RenderKey → Arc<RasterImage>` |
| 上屏 | `Vec<u8>` RGBA → `egui::ColorImage` → GPU 纹理 |

### 关键点
- ★ **渲染结果下一帧才可见**（异步）：因此缺图时**先用上一档缩放的旧图拉伸占位**（D-12），
  连旧图都没有才画白底页框；滚动/翻页时都不会闪。
- ★ 请求里的 `scale`/`rotation` **原样取自 `PagePlacement`**，会话不自己算。
- `Arc` 贯穿全程：缓存与 UI 共享同一份像素，只有转 RGBA 时有一次必然的拷贝。

---

## 场景 2b ▲v3：高倍缩放下的分块预渲染

### 触发
同一页（`612 × 792 pt`）用户放大到 `zoom = 8.0`。

### 判断

```text
placement.pixel_size() = 612×8, 792×8 = (4896, 6336) px
面积 = 4896 × 6336 = 31,021,056 px ≈ 31.0 MP > TilingPolicy::AbovePixels(4 MP)
→ 分块（tile = Some(TileSpec { col, row }))

对比：整页一张位图 = 31,021,056 × 4 B = 124,084,224 B ≈ 124 MB（单页！）★
网格 = ⌈4896/512⌉ × ⌈6336/512⌉ = 10 × 13 = 130 块，每块 512×512×4 B = 1 MiB
视口 900×1000 px 覆盖约 3×3 = 最多 9 块 ≈ 6–9 MiB（边缘块更小）
```

### 步骤

1. `[UI]` `visible_pages` → `[0]`；`tiling_policy` 判定需分块 →
   `TileSpec::grid_for((4896, 6336))` → `(10, 13)`。
2. `[UI]` 只对**视口覆盖到的瓦片**提交请求（按距离视口中心由近到远排序）：
   `scheduler.submit(RenderRequest::tiled(0, Scale::from_zoom(8.0), None, TileSpec { col, row }))`；
   其余 121 块交给 `Prefetch`（或在滚近时再提交）。
3. `[BG]` 逐块渲染：后端按 `tile.page_rect(page_size, rotation)` 得到的 **PDF 页面空间裁剪矩形**
   **直接渲染该区域**（`PdfRenderConfig::set_origin()` + 目标位图尺寸 = 该瓦片像素尺寸）★
   —— 绝不"先渲染 124 MB 整页再裁剪"。
4. `[UI]` `pump` → 每块各自按 `RenderKey { .., tile: Some(spec) }` 入缓存 → 事件 `PageRendered(key)`。
5. `[UI]` 绘制：`session.tiles_for(PageIndex(0))` 返回 `Vec<(TileSpec, Arc<RasterImage>)>`，
   按 `spec.pixel_rect(displayed_size)` 拼接（整数像素边界，**不重叠、不留缝**）；
   未就绪的瓦片同样先画白底小框。

### 关键点
- ★ 分块解决的是**内存**（124 MB → 6–9 MiB），不是速度：渲染总量不变，甚至更多次调用。
  因此只在高倍缩放（面积超阈值）时启用，避免小页被碎片化。
- ★ 缓存键必须含 `tile`，否则不同瓦片会互相覆盖（表现为"页面上出现了别处的图"）。
- ★ 阈值 `TilingPolicy::AbovePixels(4_000_000)` 是**内存/调用次数**的唯一调节旋钮（D-15）：
  4 MP ≈ 单页 16 MB；实测一块 512×512 瓦片约 9ms，因此超过 4 MP 后分块比整页更快也更省内存。
- 缩略图（scale ≈0.2）永远走整页路径：面积远低于阈值。

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
2. `[UI]` `set_scroll(target = (0, 400))` → `clamp_scroll` → `(0, 400)`（未越界）。
3. `[UI]` 事件 `ScrollChanged((0,400))`；`sync_current_page()`：探针 `(350, 401)` 落在页 0（`12..804`）→ 当前页仍为 0，不发事件。
4. `[UI]` 绘制（位图已在缓存，直接命中）：页 0 画在 `y = 12 − 400 = −388`（大部分被裁剪），页 1 画在 `y = 816 − 400 = 416`。
5. `[UI]` `handle(RequestVisible)` → `visible_pages` → 窗口 `(0,400,700,800)`：
   - 页 0：`12 < 1200 ∧ 400 < 804` → 相交 ✅
   - 页 1：`816 < 1200 ∧ 400 < 1608` → 相交 ✅
   - 页 2：`1620 < 1200` ✗
   → `visible = [0, 1]`；页 0 命中缓存跳过；页 1 提交 `Visible`；预取页 2（`Prefetch`）。
6. `[BG]`→`[UI]` 下一帧 `pump()` 收到页 1 位图 → 入缓存 → 事件 `PageRendered`。
7. `[UI]` 继续滚到 `scroll = (0, 1700)` → 夹取为 `(0, 1624)` → 可见 `[2]`；
   探针 `(350, 1625)` 落在页 2 → 事件 `CurrentPageChanged(PageIndex(2))`；
   页 0/1 若仍在队列 → `scheduler.cancel_page(0)` / `cancel_page(1)`。

### 关键点
- ★ "当前页"取**视口中心**所在的页（取不到时退回"视口顶部 + 1px"探针）。
  只认顶部探针会有一个实测缺陷：滚动被夹取到最大偏移后，较短的一页（带 `/Rotate` 的横向页、
  或文档最后一页）可能永远到不了视口顶部，于是状态栏页号卡在前一页。
- ★ 滚出视口的页要 `cancel_page`：PDFium 只能串行渲染，队列里堆着看不见的页会让用户看的那页变慢。
- 页间距处（`804..816`）探针不命中任何页 → **保留**上一次的当前页，不清空。
- ▲v3 纯滚动**不涉及锚点**：锚点只在"重算布局"的操作（缩放/旋转/窗口缩放）前后使用。

---

## 场景 4：缩放 —— 保持视野中心不动 ▲v3

### 步骤（以场景 2 的状态为起点：`zoom = 1.5`、`scroll = (0,0)`、`viewport = (900, 1000)`）

1. `[UI]` `ZoomIn` → 目标 `zoom = 1.5 × 1.25 = 1.875`。
2. `[UI]` ★ **捕获锚点**（在任何布局变化之前）：
   - `viewport.center_content_point()` → 内容空间 `(450.00, 500.00)`；
   - `layout_engine.page_at(layout, (450, 500))` → 页 0；
   - `placement.content_to_page((450, 500))`：
     `display = ((450−12)/1.5, (500−12)/1.5) = (292.00, 325.33)`；
     `unmap(None)` 翻 y → PDF `(292.00, 792 − 325.33) = (292.00, 466.67)`；
   - → `ScrollAnchor { page: 0, page_point: (292.00, 466.67) }`。
3. `[UI]` `invalidate_render()`：`cancel_all()` + 清空"在飞"集合（缩放后旧键全部失效）。
4. `[UI]` `recompute_layout()`：页 0 矩形 `(12, 12, 1147.5, 1485)`，内容尺寸 `(1171.5, 1509)`。
5. `[UI]` ★ **恢复锚点**：`layout_engine.scroll_to_anchor(layout, 0, (292.00, 466.67), (900, 1000))`：
   - `placement.page_to_content((292.00, 466.67))` → `display = (292.00, 792−466.67 = 325.33)` →
     内容 `(12 + 292.00×1.875, 12 + 325.33×1.875) = (559.50, 622.00)`；
   - 减去视口一半 → `(559.50 − 450, 622.00 − 500) = (109.50, 122.00)`；
   - 夹取：`max_scroll = (1171.5−900, 1509−1000) = (271.5, 509)` → 不越界 → `scroll = (109.50, 122.00)`。
   - 验算：`(559.50 − 109.50, 622.00 − 122.00) = (450.00, 500.00)` = 视口中心 ✅ **视野中心回到原处**。
6. `[UI]` 事件 `LayoutChanged` + `ScrollChanged((109.5, 122))`；当前页仍为 0（探针落点未跨页）。
7. 用户微调到 `zoom = 1.90`：量化 `1.90 / 0.125 = 15.2 → 15 → 1.875` ★
   → 键仍是 `scale_milli = 1875` → **缓存命中**，直接拉伸 1.875 的位图（+1.3% 误差肉眼不可见）。

### 量化边界示例

| zoom | `/0.125` | 取整 | 量化后 | scale_milli | 结果 |
| --- | --- | --- | --- | --- | --- |
| 1.50 | 12.00 | 12 | 1.500 | 1500 | 与 1.50 同档 |
| 1.52 | 12.16 | 12 | 1.500 | 1500 | **命中** 1.50 的位图 |
| 1.56 | 12.48 | 12 | 1.500 | 1500 | **命中** |
| 1.57 | 12.56 | 13 | 1.625 | 1625 | **换档**，重新渲染 |
| 1.875 | 15.00 | 15 | 1.875 | 1875 | 与 1.875 同档 |

### 关键点
- ▲v3 ★ **锚点捕获必须在布局变化之前**，恢复在之后 —— 顺序反了就会"以旧布局的点去新布局里找位置"，产生漂移。
- ▲v3 ★ 适用场景：`ZoomIn/Out/SetZoom/FitWidth`、`RotateClockwise/SetUserRotation`、
  `SetLayoutMode`、`ResizeViewport`。纯 `ScrollBy/SetScroll/GoToPage` **不用**锚点。
- ▲v3 锚点落在页间距或页外（`page_at` 返回 `None`）时：**退化为"当前页顶部对齐"**（即 `scroll_to_page`；D-11 起该方法是顶部对齐），不报错。
- ★ `invalidate_render` 必须同时清"在飞"集合，否则旧键会被判定"已在渲染"而永不重提交。
- `scroll_to_page`（**顶部对齐 + 横向居中**；D-11 修正了 D-05 的"居中"）**保留**：用于跳页与锚点退化路径（decision-log D-05/D-11）。

---

## 场景 5：翻页（NextPage）

1. `[UI]` `handle(NextPage)` → 检查 `current_page + 1 < page_count`（越界 → 事件 `Error(PageOutOfRange)`，不 panic）。
2. `[UI]` `state.current_page = 1`；`scroll = layout_engine.scroll_to_page(layout, 1, viewport.size)`
   → 页 1 矩形 `(12, 816, 612, 792)`，视口高 800 → `y = 816 + 396 − 400 = 812` → 夹取后仍为 `812`。
3. `[UI]` 事件 `CurrentPageChanged(1)` + `ScrollChanged((0, 812))`。
4. `[UI]` `request_page(1, RenderPriority::Interactive)` ★ —— 翻页是"用户此刻就要看"的动作，
   优先级最高，插到所有 `Visible`/`Prefetch` 之前。
5. `[BG]` 队列按 `RenderPriority::rank()` 出队：`Interactive(0) < Visible(1) < Prefetch(2) < Background(3)`。
6. `[UI]` 下一帧 `pump()` → 位图入缓存 → 绘制页 1。

> ▲v3 若目标页触发分块（高倍缩放），第 4 步提交的是**该页视口覆盖的若干瓦片**，
> 且都带 `Interactive` 优先级；`cancel_page` 也会一次性清掉该页所有瓦片任务。

**关键点**：`scroll_to_page` 是 `PageLayout` 的默认方法（顶部对齐 + 横向居中 + 夹取，D-11），换成对开布局时跳页行为自动适配。

---

## 场景 6：双击选词 → 高亮 → 复制（含逐步坐标数值）

### 环境
单页 `612 × 792 pt`，`/Rotate = None`，`user_rotation = None`，`zoom = 1.5`，留白 12，`scroll = (0, 0)`：

```text
placement = PagePlacement { page: 0, rect: (12, 12, 918, 1188), scale: 1.5, rotation: None, page_size: (612, 792) }
```

一页 12pt 文本的 `PageText`（PDF 页面空间，左下原点 y 向上，基线 y = 686）：

```text
chars[0] 'H' bbox = (72.00, 686, 6.03, 12)     chars[3] 'l' bbox = (90.09, 686, 6.03, 12)
chars[1] 'e' bbox = (78.03, 686, 6.03, 12)     chars[4] 'o' bbox = (96.12, 686, 6.03, 12)
chars[2] 'l' bbox = (84.06, 686, 6.03, 12)     text = "Hello world"
```

### 步骤

1. `[UI]` 鼠标双击 → 先发 `handle(SetSelectionGranularity(Word))`。
2. `[UI]` 视图坐标 `view = (150.0, 160.0)` → `content = view + scroll = (150.0, 160.0)`。
3. `[UI]` `session.text_position_at(Point{150, 160})`：
   1. `page_at(layout, (150,160))` → 页 0（矩形 `12..930 × 12..1200`）；
   2. `placement.content_to_page((150,160))`：
      `display = ((150−12)/1.5, (160−12)/1.5) = (92.00, 98.67)`；
      `unmap(None)` ★（**含 y 轴翻转**）→ PDF `(92.00, 792 − 98.67) = (92.00, 693.33)`；
   3. `page_text(0)` → 首次触发 `TextLayer::page_text(0)` → `Arc<PageText>` 存入会话缓存；
   4. `PageText::hit_test((92.00, 693.33))` → `chars[3]` 的 bbox `x 90.09..96.12, y 686..698` 命中 → `index = 3`。
4. `[UI]` `handle(SelectBegin(position))`：按粒度展开 `range_at(3, Word)` → `word_range_at(3)` →
   `TextRange { 0, 5 }`（"Hello"）→ `selection.begin((0,0))` + `selection.extend((0,5))` ★。
5. `[UI]` `handle(SelectExtend(..))`（按住拖动时反复调用）→ 只移动焦点，锚点不变。
6. `[UI]` 绘制高亮 `session.highlight_selection()`：
   1. `Selection::page_ranges()` → `[(PageIndex(0), TextRange{0,5})]`；
   2. `PageText::rects_for_range(0..5)` → 按行合并 → PDF 空间 `Rect { 72.00, 686, 30.15, 12 }`；
   3. `PagePlacement::rect_to_content(..)`（四角取外接矩形）：
      `x: 12 + 72.00×1.5 = 120.00` → `12 + 102.15×1.5 = 165.23`；
      `y`: PDF 698 → display 94 → `12 + 141 = 153`；PDF 686 → display 106 → `12 + 159 = 171`；
      → `Rect { 120.00, 153.00, 45.23, 18.00 }`；
   4. 视图矩形 = 内容矩形 − `scroll`（此处相同）→ `Highlight { page: 0, rects: [..], kind: Selection }`。
7. `[UI]` 用半透明蓝填充该矩形（`kind` 决定配色）。
8. `[UI]` `handle(CopySelection)` → `selected_text()` → `"Hello"` →
   事件 `SessionEvent::CopyToClipboard("Hello")` → ★ **UI 自己写系统剪贴板**（会话不依赖平台 API）。

### 坐标换算链

| 步骤 | 输入 | 变换 | 输出 |
| --- | --- | --- | --- |
| 1 | 视图 `(150, 160)` | `+ scroll` | 内容 `(150, 160)` |
| 2 | 内容 | `PagePlacement::content_to_page` | PDF `(92.00, 693.33)` |
| 3 | PDF 点 | `PageText::hit_test` | 字符下标 `3` |
| 4 | 下标 + 粒度 | `PageText::range_at` | `TextRange { 0, 5 }` |
| 5 | 区间 | `rects_for_range`（PDF 空间） | `Rect { 72, 686, 30.15, 12 }` |
| 6 | PDF 矩形 | `PagePlacement::rect_to_content` | 内容 `Rect { 120, 153, 45.23, 18 }` |
| 7 | 内容矩形 | `− scroll` | 视图矩形（供绘制） |

### 关键点
- ★ 后端字符包围盒**永远是 PDF 页面空间**；只有 `PagePlacement` 做 page ↔ content 换算。
- ★ `Rotation::None` 也必须翻 y（上表第 2 步的 `792 − 98.67`）；这是最常见的 bug 点。
- ★ 旋转先于缩放：`content_to_page` 先减偏移除缩放，再 `unmap_point`。
- ★ "什么叫一个单词"只在 `PageText::range_at` 一处定义；选区模型只做区间记账 → 可单测。

---

## 场景 7：全文搜索 → 跳转到命中 ▲v3

1. `[UI]` 用户输入 "world"，发 `SetSidebar(Search)`。
2. `[UI]` 调 `session.search(&SearchQuery { needle: "world", .. })`：
   - `opened.text` 为 `Some` → 转调 `TextLayer::search(..)`；
   - 为 `None` → 立即返回 `Err(Unsupported("全文搜索"))`（UI 早已按 `Capabilities::search == false` 隐藏入口）★。
3. ▲v3 ★ **同步执行，不做异步**（decision-log D-03）：MVP 允许短暂卡顿；
   若实测大文档卡顿，由 **UI 自己**把这次调用放进 `std::thread` 或任务队列，
   接口层不引入异步/取消令牌 —— 代价是搜索期间无法取消。
4. `[UI]` `session.highlights_for_search(&hits, Some(active))` → `Vec<Highlight>`（当前命中用 `ActiveSearchMatch` 配色）。
5. `[UI]` 用户点第 2 条命中（`hit.page = 3`）→ `handle(GoToPage(PageIndex(3)))`；
   若要精确到行：`placement.page_to_content(hit.rects[0].min())` 把滚动位置对准命中行（`SetScroll`）。

---

## 场景 8：缩略图（复用同一渲染路径，共享同一缓存）

1. `[UI]` 打开侧边栏 `Thumbnails` → 需要 160px 宽的缩略图。
2. `[UI]` 对每页：`geometry = session.page_geometry(page)` ▲v3（O(1)，来自后端批量表）
   → `Scale = scale_for_target_width(geometry.size, geometry.rotation, 160.0)`（纯函数）。
3. `[UI]` 组 `RenderRequest::new(page, scale, rotation)`（`tile = None`）+
   `priority: Background` → `scheduler.submit(..)`（优先级最低，绝不抢当前页）★。
4. `[BG]`→`[UI]` 与场景 2 完全相同的路径 → `pump` 入缓存。
5. `[UI]` `cache.get(&RenderKey::new(page, Scale(0.2), rotation))` → 画成 160px 宽的小图。

**关键点**：缩略图**没有**独立 trait，也不是分块路径（面积远低于阈值）：
- 与主视图共享同一份缓存实现（同一页在 0.2 倍率下只渲染一次）；
- 侧边栏虚拟化与主视图用同一套 `visible_pages` 思路。

---

## 场景 9：关闭 / 切换文档

1. `[UI]` 关窗口或打开新文件：`scheduler.cancel_all()`（同时清掉 `Interactive/Visible/Prefetch` 与全部瓦片任务），
   随后 **drop `DocumentSession`**。
2. drop 顺序：`DocumentSession` → `OpenedDocument` → `Arc<dyn Document>` / `Arc<dyn PageRaster>` /
   `Option<Arc<dyn TextLayer>>` 计数归零 → 后端对象 `Drop`。
3. `[BG]` PDFium 适配层 `Drop` 里执行 `FPDF_CloseDocument(handle)` ★（由最后一个 `Arc` 持有者释放）。
4. `[ANY]` ▲v3 `RenderCache` 由 UI 持有（可跨文档复用），此时应 `cache.clear()` ——
   它同时清掉整页键与瓦片键，避免旧文档的位图与瓦片占内存。

**关键点**：没有 `close()` API → 不可能"忘记关闭"。渲染线程若正持有 `Arc<dyn PageRaster>`，
句柄延后到该次渲染结束才释放，不会 use-after-free。

---

## 场景 10：无文本层后端（hayro 类）的降级路径

| 环节 | 有文本层（PDFium） | 无文本层（hayro 类） |
| --- | --- | --- |
| `open` 返回 | `text: Some(..)`，`Capabilities::text_layer = true` | `text: None`，`text_layer = false` |
| UI 初始布局 | 显示"搜索"按钮与侧边栏 Search 页签 | **不显示**搜索与复制入口（读 `Capabilities`）★ |
| `session.search(..)` | 走 `TextLayer::search` | `Err(Unsupported("全文搜索"))` |
| `text_position_at(..)` | 命中字符，可选中 | `None` → UI 不进入选择状态 |
| `selected_text()` | 返回文本 | 空串 |
| 渲染/滚动/缩放/分块/缩略图/导航 | 正常 | **完全正常**（只依赖 `PageRaster` + `PageLayout` + `Document`） |

**关键点**：降级靠**类型**（`Option`）与**能力声明**（`Capabilities`）双重表达；
UI 里不会出现 `if backend_name == "hayro"`，新后端只要如实声明能力即可复用全部 UI 代码。

---

## 场景 11：点击大纲 / 页内链接 —— 走预解析的导航索引 ▲v3

### 数据来源（打开时已备好）
```text
NavigationIndex {
  outline:            Vec<OutlineNode>       // destination 已解析成 Destination::Page / Uri
  links_by_page:      Vec<Vec<Link>>         // 下标 = 页号
  named_destinations: HashMap<String, Destination>
  unresolved:         Vec<String>            // 解析失败的名称，仅诊断用
}
```

### 步骤

1. `[UI]` 侧边栏点击大纲项 → `node.destination` 已是 `Destination::Page { page, offset }` →
   `handle(GoToPage(page))`，若 `offset` 存在再用 `placement.page_to_content(offset)` 精确对齐。
   **全程 0 次后端调用** ★。
2. `[UI]` 点击页面上的链接热区：
   1. `content_point` → `placement.content_to_page(..)` → PDF 点；
   2. 与 `navigation().links_by_page[page]` 里 `Link::rect` 做包含测试（两者同为 PDF 空间，可直接比较）；
   3. 按 `Destination` 分支：

| 变体 | 处理 | 数据流 |
| --- | --- | --- |
| `Page { page, offset }` | `handle(GoToPage(page))`（必要时 `SetScroll`） | 纯会话内 |
| `Uri(s)` | 交回 UI 用系统浏览器打开 | 会话不接触平台 API |

3. ▲v3 解析失败的链接（`unresolved` 非空）：**不产生可点击项**，UI 可仅在"文档信息/诊断"里显示数量。
   好处是"点了没反应"这类问题在打开时就能被发现，而不是用户点击时才暴露。

### 关键点
- ▲v3 ★ 命名目标（`/Named`、`/Dests`）在 `open` 时统一解析，因此 `Destination` **不再有 `Named` 变体**：
  UI 不可能遇到"未解析目标"这种中间状态（decision-log D-08）。
- ★ 链接热区与字符包围盒同为 PDF 页面空间 → 命中测试无需任何坐标系转换，只需一次 `contains`。
- 代价：打开时遍历全部页的 `/Annots`。若实测拖慢打开，改成 `OnceLock` 惰性构建，签名不变。

---

## 附：一帧内的完整时序（1080p 60fps 场景）

| 阶段 | 线程 | 典型耗时 | 说明 |
| --- | --- | --- | --- |
| 采集输入 → `handle(命令)` | UI | < 0.1 ms | 只改状态、算布局，不渲染 |
| `capture_anchor` / `recompute_layout` / `restore_anchor`（仅缩放/旋转变动时） | UI | ~0.1–1 ms | 数千页时线性遍历；锚点额外 2 次坐标换算 |
| `visible_pages` + `request_page`（含分块网格计算） | UI | < 0.3 ms | 出参缓冲，零分配 |
| `pump` → `drain_events` → 入缓存 | UI | ~0.05 ms | `Arc` 交接 |
| `to_rgba8` + 纹理上传（有页/瓦片就绪时） | UI | 1–5 ms/页；0.3–1 ms/瓦片 | 918×1188 ≈ 4.2 MiB；512² 瓦片 = 1 MiB |
| 绘制（egui 三角形） | UI | 1–2 ms | 只画可见页（+ 拼接可见瓦片） |
| `PageRaster::render` | BG | **10–200 ms/页**（瓦片按面积等比缩小） | 真实瓶颈；靠优先级 + 缓存 + 虚拟化 + 分块保证 UI 不等它 |

结论：UI 侧每帧工作量恒定（与文档页数和页面积无关），后端渲染的抖动通过"白底占位 + 下一帧替换"吸收。

---

## 批复状态（v3 已全部确认）

| 问题 | 你的决定 | 落地位置 |
| --- | --- | --- |
| 缺图占位策略 | **先用上一档缩放的位图拉伸**，无旧图才白底页框（D-12） | 场景 2 步骤 6、场景 2b 步骤 5 |
| 搜索执行位置 | **同步接口，不做异步** | 场景 7 步骤 3、`TextLayer` 契约 |
| `Destination::Named` | **打开时统一解析并合并存储** | 场景 11、`NavigationIndex` |

> 8 条批复的完整清单与影响范围见 [decision-log.md](decision-log.md)。
