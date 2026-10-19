# 渲染与分块（现状）

> 这是本项目最容易出错的一块。改动前请读完 §3–§8；每节末尾都列出守它的测试。
> 设计理由见 [`../decisions/rationale.md`](../decisions/rationale.md)，历史教训见
> [`../decisions/history.md`](../decisions/history.md)。

## 1. 三层比例

| 名称 | 含义 | 谁给 | 备注 |
| --- | --- | --- | --- |
| **显示比例** `placement.scale` | 内容空间像素 / point（`zoom`） | 布局 | 连续值，UI 缩放会小幅变动 |
| **渲染比例** `render.scale` | 位图实际使用的比例，= 所在波段的**上界** | `tiling::render_scale_for` | **离散**：与主视图同波段时完全相同 |
| `u` | `显示比例 / 渲染比例`，`(0, 1]` | 计算 | 只用于摆放与重采样（INV-7） |

```text
band_for(display, overscan)        = floor(ln(display) / ln(overscan))
render_scale_for_band(band, over)  = over^(band + 1)          // 波段上界
render_scale_for(display, over)    = render_scale_for_band(band_for(display))
```

`DEFAULT_OVERSCAN = 1.6`；`render_scale_for(display, overscan ≤ 1.0)` 表示**关闭超采样**
（渲染比例 = 显示比例，仅供测试与新旧对比）。

推论（INV-4）：同一波段内任意缩放 → 同一个渲染比例 → **同一个 `RenderKey`** → 不重渲染、
也不取消在飞任务。100%→800% 依次经过 5 个波段：`1.6 / 2.56 / 4.096 / 6.554 / 10.486`。

## 2. `TileGrid`：一页一套网格

`pdfcore::tiling::TileGrid`（`crates/pdfcore/src/tiling.rs`）是**唯一的块几何来源**：

```rust
TileGrid::new_bounded(旋转后的页尺寸(point), 渲染比例, 纹理单边上限)
  → side            每边块数（2 的幂，level 5 封顶 = 32×32）
  → edges_x/edges_y 共享整数边界 edges[i] = round(i * total / side)
  → render_px()     整页渲染像素 = 最后一条边界
  → tile_px(col,row)             块在该页位图里的整数像素矩形
  → display_rect(页矩形, col,row, 显示比例, 渲染比例) → 块在内容空间的位置/尺寸（×u）
  → tile_page_rect(col,row, 页尺寸, 旋转)             → 块的 PDF 页面空间矩形
  → tiles_in_view(页矩形, 视口, 显示, 渲染)           → 视口覆盖的块范围
  → tiles_for_page_rect(页面矩形, 页尺寸, 旋转)        → 覆盖某页面矩形的块范围（预览用）
```

**分块判据是两条，任一成立就分**（`TilingPolicy::requires_tiling_bounded`）：

1. 渲染像素**面积** > 4 MP（性能/内存取舍）；
2. 渲染像素**单边** > 纹理上限（显卡硬约束，例如 16384）。**这条优先于策略** ——
   即使策略是 `Never`，单边超限也必须分（不然那张纹理根本创建不出来）。

要点：

* 边界 `round(i·total/side)` **共享**：相邻块 `x_i + w_i == x_{i+1}` 严格成立（f32 相等）；
* `side` 先由"再细分一级后块边长仍 ≥ `TARGET_TILE_PX = 512`"决定，再**保证块边长 ≤ 纹理上限**
  （必要时继续细分，最多 32×32）；
* 同一渲染比例下网格固定 ⇒ `(页, 渲染比例, 旋转, 块坐标)` 唯一确定一块区域（拱卫 `RenderKey`）。

守它的测试：`tiling::tests::*`（边界单调/覆盖整页、相邻块相接、视口探测==全量扫描、
`tiles_for_page_rect`==全量扫描、`render_px`==最后一条边界、越界块为空）。

## 3. 请求与键

```rust
RenderKey { page, scale_milli /*渲染比例×1000*/, rotation, tile: Option<TileSpec> }
RenderRequest { page, scale /*渲染比例*/, rotation, tile, region: Option<RenderRegion>,
                priority, cancel }
RenderRegion { page_rect: Rect /*PDF 页面空间*/, pixel_size: (u32, u32) }
```

* `scale_milli = round(scale × 1000)`，**不做 1/8 量化**（量化会让键里的比例 ≠ 真正渲染的比例，
  预览重建网格时会差几像素）；
* `tile = None` 表示整页（`region = None`，尺寸由后端推导）；`tile = Some` 用
  `RenderRequest::tiled(..., region)`，**region 必填**；
* `region.pixel_size` 必须等于 `TileGrid::tile_px` 的尺寸；`region.page_rect` 必须等于
  `TileGrid::tile_page_rect`。

## 4. 后端渲染路径（`backend-pdfium/src/raster.rs`）

```text
整页在渲染比例下的像素 page_px = round(旋转后页尺寸 × 渲染比例)   // 与 render_px 同一表达式
page_rect --Rotation::map_point--> 显示坐标 --×渲染比例--> 设备像素 → 取整得到 (left, top, size)
set_fixed_size(page_px) + set_origin(-left, -top) + rotate(rotation)  → 只渲染该区域
```

**三条硬校验**（不符返回 `CoreError::Internal`，绝不静默拉伸）：

1. 映射后的 `left/top/w/h` 必须落在整像素上（容差 0.01px）；
2. `size == region.pixel_size`；
3. 区域必须在页内、块必须落在整页位图内。

真实 PDF 断言：`backend-pdfium/tests/real_pdf.rs::tiles_match_the_crop_of_the_whole_page_render`
（612×792 边界整数 / 610×790 边界要取整 / `/Rotate 90°`），以及
`wrong_expected_tile_size_is_rejected_not_stretched`。

## 5. 会话侧（`pdfcore/src/session.rs`）

| 方法 | 用途 |
| --- | --- |
| `request_page(page, priority)` | 分块 → 只提交**视口覆盖的块**；否则整页 |
| `request_tiles` | 每个块带 `RenderRegion`（页面矩形 + 期望尺寸），按 `RenderKey` 去重 |
| `request_region(page, page_rect, 渲染比例)` | **非视口用途**（预览截图）：按给定渲染比例渲染一块区域，记在 `region_jobs`，**不被 `cancel_stale_jobs` 取消** |
| `region_tiles(...)` | 该区域全部就绪 → `Some(块列表)`；缺一块 → `None`（调用方继续等） |
| `tiles_for(page)` / `image_for(page)` | 当前波段的已缓存块 / 整页图（分块时 `image_for` 返回 `None`） |
| `tile_grid(page)` / `render_scale(page)` | UI 与核心**共用**同一套网格与比例 |
| `visible_tiles(page)` | 视口覆盖的块范围 `(col0,row0,col1,row1)` |
| `cancel_stale_jobs` | 取消"滚出视口 / 键已过时"的在飞任务；**跳过 `region_jobs`** |

`pending()` 同时计入区域任务，因此有预览在渲染时 UI 会继续重绘。

## 6. 摆放（UI 侧）

```rust
// app/view.rs
let content = grid.display_rect(placement.rect, col, row, 显示比例, 渲染比例);  // ×u
let view    = content.translate(scroll);                                       // 减滚动
let screen  = canvas.min + view;
```

即 `x = 页.x + 块像素_x · u`、`w = 块像素_w · u`（INV-7）。`u == 1`（渲染比例 = 显示比例）
时退化为"像素偏移直接相加"——那是历史不变量 D-17 的特例，**不是**通用规则。

守它的测试：`view::tests::tile_view_rect_uses_the_display_to_render_ratio`、
`tile_placement_agrees_with_layout_coordinates`（块摆放与布局换算重合，选区/高亮才不会错位）。

## 7. 缓存与保留

### 7.1 CPU 位图层（`LruCache`，`RenderCache` 的默认实现）

* 512 MiB 上限，按字节 LRU；**不按波段裁剪**——没超限就一直留着（缩回旧波段可秒回）。
* 单张超限时保留最后一张（避免"存进去立刻被丢"的抖动）。

### 7.2 GPU 纹理层（`app::view::KeyedStore`）

* 容量 `TEXTURE_CAPACITY = 128` 条目（LRU 兜底）；
* 每帧 `retain_preview_scales(pages, 当前渲染比例)` 裁剪，每页**至多三档**：

| 档 | 选法 | 用途 |
| --- | --- | --- |
| 当前波段 | `scale == 当前渲染比例` | 主图层 |
| 最近的其它波段 | 与当前比例差最小的非当前比例 | 放大方向的预览底料 |
| 当前以下最近的整页波段 | `scale < 当前` 且 `tile == None` 的最大者 | **缩小方向的整页底料** |

旋转必须匹配（按页取 `placement.rotation`），否则旧图会画成横躺的。

## 8. 画法决策（`app/view.rs::plan_page`）

每页每帧先算一组输入，再决定"这一帧画什么"：

```rust
DrawInput { tiled, has_whole_current, ready, total, preview, waited_ms }
PageDraw  = Whole | Stretch(scale) | TilePreview(scale) | Tiles | Blank
```

判据顺序（**顺序即语义**）：

| # | 条件 | 结果 |
| --- | --- | --- |
| 1 | 当前波段的整页纹理在 | `Whole` |
| 2 | 该页当前不分块 | 有整页旧图 → `Stretch`；只有旧块 → `TilePreview`；都没有 → `Blank` |
| 3 | **该页在本波段上一帧已经是 `Tiles`**（粘性，D-104） | `Tiles`（缺的块由底料补，已清晰的部分保持清晰） |
| 4 | `total > 0 && ready >= total` | `Tiles`（可见范围一块不缺才进门） |
| 5 | `ready > 0` 且 `waited_ms >= READY_TIMEOUT_MS(400)` | `Tiles`（**兜底**：某块渲染失败时不永远停在糊图） |
| 6 | 有旧内容 | `Stretch` / `TilePreview` |
| 7 | 都没有 | `Tiles`（首次打开，白不可避免） |

第 3 条（D-104）解决的是"波段内滚动"：新露出的块没就绪时，若退回预览层，
**整页**会短暂变糊（实测 40–100ms）；粘性规则让页面留在 `Tiles`，
缺失处由底料（D-98）补上 —— 即"已清晰的部分保持清晰，新块先给低分辨率"。
波段一变，上一帧的波段号不等于当前波段号，粘性自然失效，仍走第 4/5 条的进门门槛
（D-97 的"不许提前切"因此不被绕过）。

配套三个纯函数：

| 函数 | 规则 |
| --- | --- |
| `ready_tiles_in_view` | 与绘制循环**同一段** `grid.contains` 枚举，数出 `ready/total`（否则会"永远差一块"） |
| `preview_source` | 整屏预览取谁：缩小方向优先整页图（旧块只覆盖缩之前那一片）；放大方向取更接近目标的；等距优先整页图 |
| `underlay_source` | **覆盖优先**：有整页图就用整页图，否则旧块（打底用，必须能盖满） |
| `needs_underlay` | `Stretch`/`TilePreview` 总是要底料；`Tiles` 只在 `ready < total` 时要（粘性模式下这就是常态，D-104） |

绘制顺序（每页）：**纸张 → 底料 → 主图层（预览或当前波段的块）**。
缺块处露出底料（糊的旧图），**不补白**。

> 关键约束（INV-8）：降级/预览一律在**整页层面**，绝不"缺哪块就用旧块补哪块"——
> 那种做法会让相邻块各持一个比例，接缝变成几何错位（D-87 回退、D-88 结论）。
> 底料是一张整页图（单一比例、单一映射），与块都对齐到同一页面矩形，因此只有**清晰度差**。

守它的测试：`view::tests::plan_page_*`（含"17/18 不翻页、18/18 才翻页"）、
`preview_source_prefers_coverage_when_zooming_out`、
`preview_layers_always_need_an_underlay`、
`zooming_out_keeps_a_whole_page_preview_even_after_bitmaps_are_evicted`（真实会话 + 清空位图缓存）。

## 8.5 纹理单边上限（D-108）

`max_texture_side` 从 egui/后端传进 `DocumentSession`（`set_max_texture_side`），导致两个行为：

| 位置 | 行为 |
| --- | --- |
| 分块判据 | 单边超限 → 强制分块（见 §2） |
| 预览图重建 | 记录的分辨率若让区域超过上限，按 1.6 步长**降档**（`view::preview_render_scale`，最多降 4 档） |

## 9. 预览图（截图）的重建

见 [`persistence.md`](persistence.md) §4：按存档里记录的 **`scale`（截图时的显示比例）**
→ 吸到波段 → `request_region` / `region_tiles` → `compose_region_snapshot` 在**渲染像素**里
1:1 拼图。输出尺寸 = 区域 × 渲染比例，**与当前缩放无关**。

## 10. 已知限制

* `TileGrid` 的层级最大 32×32；超大页在高倍下块边长会小于 512px（可接受）。
* 预览重建用 `scale_milli/1000` 近似旧渲染比例重建网格：理论误差 < 0.1%，块数在极端页码尺寸下
  可能差一级；旧块预览对越界块直接跳过（不用错误矩形贴图）。
* `tiles_for(page)` 遍历整个网格的缓存查询（≤1024 次），每帧每页一次。
* 整页位图的渲染像素面积永远 < 4 MP（否则走分块），因此单张整页位图 ≤ 16 MB。
* **极端长宽比**（如 100×15000pt 的条幅页）会按"最长边"决定网格层级，于是被切成很多
  极窄的块（实测 32×32 = 1024 块，可见范围一次 64 块）。正确但目前浪费；要改需要**非正方网格**
  （每轴独立层级），暂列待办。
