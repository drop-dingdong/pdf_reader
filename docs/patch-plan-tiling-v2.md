# `feat/tiling-v2`：联动改造计划（已实施）

> 纯逻辑层已完成并通过测试（D-90/91/92）。下面是**必须一次性完成**的三处联动改动。
> 任何一处单独落地都会造成"键按波段、贴图按显示比例"的错配，因此合并为一次提交，
> 并在 `main` 上保持可回退（一次 `git revert`）。
>
> **状态**
> * §1–§3（`render` + `session` + `app` 联动）：**已完成**，见 D-94，一次提交、可整体 `git revert`；
> * §4（后端"块矩形 + 期望像素尺寸 + 尺寸校验"）：**已完成**，见 D-95（单独一次提交）；
> * §5 的 5 条判据**只能真机验收**，仍未验收（见下）；
> * 顺带改动：D-89 的瓦片红线改为**默认关闭**（`PDF_READER_TILE_BORDERS=1` 才开）；
>   截图拼合改为"位图铺满内容矩形再重采样"（`u ≠ 1` 后必需，见 D-94 ⑥）。

## 1. `pdfcore::render` —— 请求与键

| 现状 | 目标 |
| --- | --- |
| `RenderRequest { page, scale, rotation, tile }`，`scale` = 显示比例 | `scale` = **波段渲染比例**（`tiling::render_scale_for(placement.scale, overscan)`）|
| `RenderKey { page, scale_milli, rotation, tile }`，按精确 milli 区分 | `scale_milli` = 渲染比例的 milli（因此同波段内**自动相同**）|
| `TileSpec::pixel_rect / grid_for`、`tiles_covering`（512 固定网格）| 由 `tiling::TileGrid` 取代：`edges` 共享整数边界、`tile_px`、`tiles_in_view` |

## 2. `pdfcore::session` —— 请求哪些块

- `visible_tiles(page)`：改为 `TileGrid::tiles_in_view(page_rect, viewport, display, render)`，
  返回 `(col0,row0,col1,row1)`；`TileGrid::new(page_size, render_scale)`。
- `request_page`：对范围内每个（非空）块发请求，`scroll`/`scale` 一并带上渲染比例；
  在飞任务按 `RenderKey` 去重（键里已是渲染比例 → 同波段天然复用）。
- `cancel_stale_jobs`：比较键里的 `scale_milli`（现在是波段比例）→ 同波段不再取消。
- 淘汰：保留"当前波段 + 一个上一波段"，其余丢弃（避免多波段长期共存）。

## 3. `app` —— 怎么摆

| 现状 | 目标 |
| --- | --- |
| `tile_view_rect(page_rect, pixels)`：像素偏移直接相加（隐含 `u = 1`）| `TileGrid::display_rect(page_rect, col, row, display, render)`：`x = page.x + c_i·u`、`w = t_i·u` |
| `upload_page` 的键来自 `placement.scale` | 键来自 `render_scale_for(placement.scale)`（与 session 一致）|
| 未就绪 → 补白 | 保持（D-88：整页层面降级，不逐块混用）|
| 上一档整页/瓦片拉伸预览 | 保持不变（只是"上一档"现在是"上一波段"）|

## 4. 后端（`backend-pdfium`）

- 请求形状：块的**页面空间矩形** + **期望像素尺寸**（`t_i`）——落地为 `RenderRegion`，
  由 `TileGrid::tile_page_rect` / `tile_px` 给出（D-95）。
- 渲染后校验 `输出宽高 == 期望`，不符则返回 `CoreError::Internal`（**不再静默拉伸**）。
  实际还多校验两条：映射必须落在整像素上、区域必须在页内。
- 结论：**`set_fixed_size + set_origin` 满足要求**，不需要改成"按块矩形裁剪渲染"。
  证据：`tiles_match_the_crop_of_the_whole_page_render` 在真实 PDFium 下逐像素比较
  "块渲染"与"整页裁剪"（含奇数页宽与 `/Rotate 90°`）。

## 5. 验收（只能真机）

> 启动：`PDF_READER_DIAGNOSE=1 PDF_READER_TILE_BORDERS=1 cargo run -p app -- <某个大页 PDF>`
> （红线默认已关；需要看块边界时再开 `PDF_READER_TILE_BORDERS=1`。）
> 判据 4/5 只看 stderr 的 `[瓦片]` / `[贴图]` 行；判据 1–3 只看画面。

1. 连续缩放（跨多个波段）后停止：同一页不应再有"同行上下错位/重影"；
   —— 看同一行小字是否在块接缝处上下错开；开红线时看线两侧是否连续。
2. 缩放过程中不再出现成片白块（未就绪处补白属预期，但不能整页白）；
   —— 波段内缩放**应当完全无重渲染**（画面直接跟手缩放，不闪）。
3. 横向布局 + 大缩放 100% 下向右滚动：`scroll` 单调、无跳变；
4. `PDF_READER_DIAGNOSE=1`：同一页的 `[贴图]` 应只出现**一个** `页面px`（同波段），
   且 `渲染比例=` 也只有一个值；
5. 缩放到同一波段内的另一个比例（`DEFAULT_OVERSCAN = 1.6`，例如 100% ↔ 150%）：
   **不应产生新的渲染请求**（日志无新 `[瓦片]` 行）。

## 6. 风险与回退

- 主要风险：`u ≠ 1` 后所有像素级断言（`tile_view_rect` 的测试、D-17）都需重写；
- 回退：两处改动各一个提交 ——
  * `D-94`（1+2+3 联动）：`git revert` 回到 512 网格行为；
  * `D-95`（4 后端块矩形）：单独 `git revert` 即可退回"后端自己按网格推算块像素矩形"；
- 顺序（已按此执行）：先 1+2+3（后端继续用 `set_fixed_size + set_origin`），
  确认逻辑与几何（真实 PDF 的逐像素裁剪一致性测试）通过后再做 4。
