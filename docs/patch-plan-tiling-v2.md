# `feat/tiling-v2`：联动改造计划（尚未实施）

> 纯逻辑层已完成并通过测试（D-90/91/92）。下面是**必须一次性完成**的三处联动改动。
> 任何一处单独落地都会造成"键按波段、贴图按显示比例"的错配，因此合并为一次提交，
> 并在 `main` 上保持可回退（一次 `git revert`）。

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

- 请求形状：块的**页面空间矩形** + **期望像素尺寸**（`t_i`）。
- 渲染后校验 `输出宽高 == 期望`，不符则返回 `CoreError::Internal`（**不再静默拉伸**）。
  若 PDFium 的 `set_fixed_size + set_origin` 无法满足，则改为"按块矩形裁剪渲染"。

## 5. 验收（只能真机）

1. 连续缩放（跨多个波段）后停止：同一页不应再有"同行上下错位/重影"；
2. 缩放过程中不再出现成片白块（未就绪处补白属预期，但不能整页白）；
3. 横向布局 + 大缩放 100% 下向右滚动：`scroll` 单调、无跳变；
4. `PDF_READER_DIAGNOSE=1`：同一页的 `[贴图]` 应只出现**一个** `页面px`（同波段）；
5. 缩放到同一波段内的另一个比例：**不应产生新的渲染请求**（日志无新 `[瓦片]` 行）。

## 6. 风险与回退

- 主要风险：`u ≠ 1` 后所有像素级断言（`tile_view_rect` 的测试、D-17）都需重写；
- 回退：整次改动一个提交，`git revert <sha>` 即可回到 512 网格行为；
- 顺序建议：先 1+2+3（不含 4，后端继续按整页 `set_fixed_size`），确认画面正确后再做 4。
