# 架构现状

> 只描述"现在是什么样"。为什么这样设计见 [`../decisions/rationale.md`](../decisions/rationale.md)，
> 怎么变来的见 [`../decisions/history.md`](../decisions/history.md)。

## 1. 目标与边界

一个**本地单文件 PDF 阅读器**桌面应用：连续滚动、缩放、选字复制、搜索、目录、链接、
自定义标注（浮窗 / 截图预览）、横向浏览、全屏。不做云、不做编辑、不做注释写入。

技术栈：Rust + egui/eframe 0.37 + PDFium（`pdfium-render` 0.9，运行期加载动态库）。

## 2. 三个 crate 与依赖方向

```text
        app（egui 外壳：胶水 + 纯逻辑）
          │  依赖
          ▼
       pdfcore（trait + 领域模型，**零依赖**）
          ▲
          │  实现
   backend-pdfium（pdfium-render 适配）
```

| crate | 行数 | 角色 |
| --- | --- | --- |
| `pdfcore` | ~7.3k | 接口层：9 个 trait、领域类型、会话装配点 `DocumentSession`、纯逻辑模块（几何/布局/分块/波段/缓存/调度/文本）、测试替身 `testkit` |
| `backend-pdfium` | ~1.0k | 真实后端：文档几何表、导航索引、整页/块渲染、字符级文本 |
| `app` | ~8.0k | egui 外壳：`view`（纯逻辑，可测）+ `app`（胶水，需真机）+ `persist`（存档 JSON） |

依赖只允许指向 `pdfcore`；`pdfcore` 不依赖任何第三方 crate（`#![forbid(unsafe_code)]`）。

## 3. `pdfcore` 模块地图

| 模块 | 职责 | 关键类型 |
| --- | --- | --- |
| `geometry` | 几何类型 + **坐标约定**（见 §5） | `Point` `Rect` `Size` `Vec2` `Rotation` `Insets` |
| `ids` | 标识符 | `PageIndex` `JobId` |
| `error` | 统一错误（含"非故障"分支） | `CoreError`（`Unsupported` / `PasswordRequired`） |
| `image` | 位图载体（`Arc<[u8]>`、不可变） | `RasterImage` `PixelFormat` |
| `document` | 后端入口与只读结构 | `PdfBackend` `Document` `OpenedDocument` `Capabilities` `NavigationIndex` `Destination` |
| `render` | 渲染契约 + 键 + 优先级 | `PageRaster` `RenderCache` `RenderScheduler` `RenderKey` `RenderRequest` `RenderRegion` `Scale` `TilingPolicy` `TileSpec` |
| `tiling` | **块几何与波段**（纯函数） | `TileGrid` `DEFAULT_OVERSCAN` `band_for` `render_scale_for` |
| `scheduler` | 后台线程 + 优先级队列 | `WorkerScheduler` |
| `cache` | 按字节的 LRU 位图缓存 | `LruCache` `DEFAULT_CAPACITY_BYTES = 512 MiB` |
| `layout` | 页摆放 + 滚动/锚点 | `PageLayout` `VerticalLayout` `HorizontalLayout` `PagePlacement` `Viewport` `ScrollAnchor` |
| `selection`/`text` | 字符级文本、选区、搜索 | `TextLayer` `TextSelection` `PageText` `RangeSelection` `SearchQuery` |
| `session` | 装配点：命令进、事件出 | `DocumentSession` `ViewerCommand` `SessionEvent` |
| `store` | 少量持久化接口 | `KeyValueStore` `RecentFiles` `AppSettings` |
| `testkit` | 内存假后端与替身 | `MemoryBackend` `MemoryRaster` `ImmediateScheduler` `MemoryCache` |

## 4. 数据流

### 4.1 打开文档

```text
app::open → backend.open(Source) → OpenedDocument{ document, raster, text, capabilities }
          → DocumentSession::new(opened, cache, scheduler, layout, selection)
              · document.page_geometries()  ← 一次性批量表（打开后 0 次跨 FFI）
              · recompute_layout()
```

### 4.2 每帧（UI 线程）

```text
collect_commands → session.handle(命令…)      // 视角状态：滚动/缩放/翻页/选择…
apply_and_pump   → scheduler.drain_events → cache.insert → SessionEvent
ensure_textures  → 上传整页/块纹理 + 按波段裁剪纹理
draw_*           → 用布局矩形 + TileGrid 摆放绘制
RequestVisible   → 提交可见页（及预取页）的渲染任务
```

详见 [`app-ui.md`](app-ui.md) §2。

### 4.3 一次渲染

```text
DocumentSession::request_page / request_tiles / request_region
  → RenderRequest{ page, scale=渲染比例, rotation, tile, region, priority }
  → WorkerScheduler: 优先级队列 → 工作线程（PDFium 串行，1 个）
  → PdfiumRaster::render: set_fixed_size(整页px) + set_origin(-x,-y) + rotate
  → RenderEvent::Rendered{ key, image } → pump → RenderCache.insert
```

## 5. 坐标系（唯一换算链）

| 空间 | 原点 | y 轴 | 单位 | 谁负责换算 |
| --- | --- | --- | --- | --- |
| PDF 页面空间 | 页面左下角 | **向上** | point（1/72 英寸） | 后端只给原始值 |
| 内容空间 | 整篇文档左上角 | 向下 | 设备无关像素 | `PageLayout` / `PagePlacement` |
| 视图空间 | 视口左上角 | 向下 | 设备无关像素 | UI（减去滚动量） |
| 渲染像素（设备） | 该页位图左上角 | 向下 | 像素 | `TileGrid`（块与整页位图） |

换算链：

```text
PDF 页面坐标 --Rotation::map_point--> 旋转后页内坐标 --×Scale + 页在内容空间的偏移--> 内容坐标
            --−滚动偏移--> 视图坐标
            --×渲染比例（只用于块/位图）--> 设备像素
```

约束：

* **只有 `PagePlacement` 做 page ↔ content 换算**（`page_to_content` / `content_to_page` / `rect_to_content`）。
* 旋转先于缩放；`Rotation::map_point` 在 0°/180° 时也包含 y 翻转。
* 页面坐标先减去 `PageGeometry.origin`（渲染盒左下角）再参与换算（D-27）。

## 6. 线程模型

| 谁 | 跑什么 | 约束 |
| --- | --- | --- |
| UI 线程（egui） | `DocumentSession::handle/pump`、纹理上传、绘制 | 只碰 `DocumentSession`；不直接碰后端 |
| 工作线程 ×N | `PageRaster::render` | `N = Capabilities::max_concurrency`，PDFium 为 **1**；全局去重按 `RenderKey` |

取消契约：PDFium 无法中断正在渲染的任务，取消只在出队前生效；**取消必须补发
`RenderEvent::Cancelled`**，否则会话的在飞集合永远清不掉、该页再也不会被请求。

## 7. 不变量清单（改代码前先看这里）

| # | 不变量 | 落点 | 谁来守 |
| --- | --- | --- | --- |
| INV-1 | 只有 `PagePlacement` 做 page ↔ content 换算 | `layout.rs` | 单测（`layout` 的换算与锚点往返） |
| INV-2 | 后端只产出 PDF 页面空间坐标；文本/链接减去渲染盒原点 | `backend-pdfium/document.rs`、`text.rs` | `real_pdf.rs`（`geometry_text_and_links_are_normalized_*`） |
| INV-3 | **一页一套网格**：边界整数、共享、相邻块严格相接 | `tiling.rs` `TileGrid` | `tiling::tests::adjacent_tiles_abut_exactly` |
| INV-4 | 渲染键里的比例 = **真正用于渲染的比例**（波段上界），同波段内任意缩放同键 | `render.rs` `RenderKey` | `zooming_inside_a_band_does_not_render_again`（假后端 + 真实 PDFium） |
| INV-5 | 位图尺寸只有一个来源：`TileGrid::render_px` / `tile_px`，两侧不各算一次 | `tiling.rs`、`backend-pdfium/raster.rs` | `tiling_policy…` / `tiles_match_the_crop_…` |
| INV-6 | 块渲染 == 整页裁剪（逐像素）；尺寸/整像素不符即报错 | `backend-pdfium/raster.rs` | `tiles_match_the_crop_of_the_whole_page_render`（含奇数页宽、`/Rotate 90°`） |
| INV-7 | `u = 显示/渲染` 必须作用在**摆放**上：`x = 页.x + 块像素·u` | `tiling.rs::display_rect` | `tile_view_rect_uses_the_display_to_render_ratio` |
| INV-8 | 画法**整页级**降级：不做"逐块混用不同比例" | `app/view.rs` `plan_page` | `view::tests::plan_page_*`、`preview_layers_always_need_an_underlay` |
| INV-9 | 预览图不存像素，只存元数据 + 分辨率；按记录分辨率重建 | `persist.rs`、`app.rs::preview_texture` | `persist::tests::v*`、`region_render_feeds_the_preview_at_the_recorded_scale` |
| INV-10 | UI 线程不直接调后端；渲染只经 `RenderScheduler` | `session.rs` | 结构约束（后端只以 `Arc<dyn …>` 出现在会话里） |
| INV-11 | 取消必须补发 `Cancelled` 事件 | `scheduler.rs`、`testkit.rs` | `scheduler::tests::cancelling_*` |

## 8. 资源与性能要点

* **两级缓存**：CPU 位图 `LruCache`（512 MiB，按字节 LRU）与 GPU 纹理 `view::KeyedStore`
  （128 条目 + 每页三档保留）。详见 [`rendering.md`](rendering.md) §7。
* **渲染预算**：`TilingPolicy::AbovePixels(4 MP)`（按**渲染**像素面积）决定整页还是分块；
  一块约 512px 的目标边长（`TARGET_TILE_PX`）。
* **超采样**：`DEFAULT_OVERSCAN = 1.6`，渲染比例取波段上界，波段内缩放不重渲染。
* **每帧限量**：`MAX_UPLOADS_PER_FRAME = 2`（按页计）；拖动期间 `allow_upload = false`。
* **预览窗口**：缩放/旋转变更后 160ms 内不提交渲染（`ZOOM_PREVIEW_WINDOW`）。

## 9. 已知限制（现状，不是 bug 清单）

* 纹理层按**条目数**限额，没有显存字节预算；块大小随页面尺寸变化。
* 上传/像素转换没有字节预算：一页一次可能上传几十块。
* 阈值（4 MP / 512 / 1.6 / 400ms / 128）来自历史实测，没有自动化的性能基线。
* 存档不保留未知字段（新版本写的字段，旧版本保存会丢）。
* 决策→绘制的集成没有自动化测试（见 [`testing.md`](testing.md) §5）。
