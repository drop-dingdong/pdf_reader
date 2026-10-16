# 决策记录（Decision Log）

> 记录已确认的设计决定、其影响范围，以及**代码同步清单**（因为 `crates/pdfcore` 目前落后于设计）。
> 关联文档：[architecture.md](architecture.md)、[traits.md](traits.md)、[trait-definitions.md](trait-definitions.md)、[data-flow.md](data-flow.md)

## 1. 本轮批复（v3）

| ID | 来源问题 | 你的决定 | 设计影响 |
| --- | --- | --- | --- |
| **D-01** | traits §12-1：批量页尺寸 | **增加批量，提前知道每一页大小** | `Document::page_geometries() -> Result<Arc<[PageGeometry]>>` 成为**必需**方法（open 时备好）；`page_size` / `page_rotation` 降为默认方法（查表）。会话 `new()` 从"N 次跨 FFI"变成 1 次 `Arc` 克隆 |
| **D-02** | traits §12-2：分块渲染 | **可做分块预渲染** | 新增 `TILE_SIZE = 512`、`TileSpec`、`TilingPolicy::AbovePixels(16 MP)`；`RenderRequest.tile: Option<TileSpec>`、`RenderKey.tile: Option<TileSpec>`；`PageRaster` 契约新增"瓦片只渲染该区域、不得整页后裁剪"；会话新增 `tiles_for()` / `tiling_policy()`；`PagePlacement` 新增 `pixel_size()` |
| **D-03** | traits §12-3：搜索是否同步 | **保持同步接口，不做异步** | `TextLayer::search` 保持同步；契约注释写明"耗时由调用方承担"。接口层不引入 runtime、不引入搜索取消 |
| **D-04** | traits §12-4：`&mut self` 例外 | **接受** | `TextSelection` 的 `set_granularity/begin/extend/clear` 保留 `&mut self`，作为全局 `&self` 约定的**唯一例外**并写入文档注释 |
| **D-05** | traits §12-5：`scroll_to_page` 语义 | **居中是合理的**；补充要求：**scroll 前后保持视野中心对应的页面位置不动** | 保留 `scroll_to_page`（居中+夹取）；新增 `Viewport::center_content_point()`、`ScrollAnchor { page, page_point }`、`PageLayout::scroll_to_anchor(...)`；会话在**任何重算布局**的操作（缩放/旋转/切换排布/窗口缩放）前后自动捕获与恢复锚点；锚点落在页间距时退化为"当前页居中" |
| **D-06** | data-flow 问题 1：缺图占位 | **白底页框** | 纯 UI 层约定：位图未就绪时画 `placement.rect` + 1px 灰边，**不保留**上一档拉伸图。无接口改动 |
| **D-07** | data-flow 问题 2：搜索执行位置 | **同步，不需要异步实现**（与 D-03 同一结论，合并执行） | 同 D-03；MVP 接受大文档搜索期间短暂卡顿，若实测不可接受，由 UI 自行放到后台线程，接口不变 |
| **D-08** | data-flow 问题 3：`Destination::Named` | **打开时对所有链接/目标做预解析，并把 destination 合并存储**，不做实时解析 | 删除 `Destination::Named` 变体；新增 `NavigationIndex { outline, links_by_page, named_destinations, unresolved }`；`Document` 的 `outline()` + `links(page)` 合并为 `navigation() -> Result<Arc<NavigationIndex>>`；会话新增 `navigation()`。点击链接/大纲时 0 次后端调用 |

### D-08 的代价与回退方案
预解析要在 `open` 时遍历大纲树、每页 `/Annots` 与 `/Dests` 字典。对"页数多 + 链接密"的文档会拉长打开时间。
若实测不可接受，回退方式**不改签名**：把 `navigation()` 实现成 `OnceLock` 惰性构建（首次调用时解析一次），
或仅在 `Capabilities` 里增加 `navigation: bool` 让 UI 按需触发。**签名层无需再评审。**

## 2. 更早确认的决定（沿用）

| ID | 决定 | 出处 |
| --- | --- | --- |
| D-00a | 技术栈：Rust + egui/eframe + PDFium（动态库随包分发） | [framework-selection.md](framework-selection.md) |
| D-00b | 接口层独立成**零依赖** crate `pdfcore`，依赖单向 | architecture §2 |
| D-00c | 9 个 trait 的划分（含"文本层为可选零件"、不建缩略图/剪贴板 trait） | architecture §3.1、traits §10 |
| D-00d | `PageLayout::compute` 收 `&[PageGeometry]`（逐页 `/Rotate`），`LayoutParams.user_rotation` 叠加 | architecture §6.3 |
| D-00e | 坐标系：后端只给 PDF 页面空间坐标；`PagePlacement` 独占换算；旋转先于缩放 | architecture §5 |
| D-00f | 缩放量化 1/8 作为缓存键；`invalidate_render` 同时清"在飞"集合 | architecture §7、data-flow 场景 4 |

## 3. 代码同步清单（`crates/pdfcore` 目前落后于 v3）

> 按 `agent.md` 规则 1，下列每一项都必须**带测试**落地；规则 3 要求提交前跑通全部测试。
> 当前状态：代码与 v1/v2 对齐，**未编译验证**（本机无工具链），因此第 4 节是下一轮的第一件事。

| # | 文件 | 需要的改动 | 必须新增/更新的测试 |
| --- | --- | --- | --- |
| 1 | `document.rs` | `Document` 加 `page_geometries()`；`page_size`/`page_rotation` 改默认方法；删 `outline()`/`links()`；加 `navigation()`；删 `Destination::Named`；加 `NavigationIndex` | 假后端：批量表长度 = 页数；越界返回 `PageOutOfRange`；`navigation()` 返回已解析目标（不含 `Named`） |
| 2 | `render.rs` | 加 `TILE_SIZE`、`TileSpec`（`grid_for`/`pixel_rect`/`page_rect`）、`TilingPolicy`；`RenderKey`/`RenderRequest` 加 `tile` | 网格计算（含"最后一列不足一块"）；瓦片键与整页键互不覆盖；`pixel_rect` 拼接无重叠无缝隙 |
| 3 | `layout.rs` | `PagePlacement::pixel_size()`；`PageLayout::scroll_to_anchor` 默认实现；`Viewport::center_content_point()`；`ScrollAnchor` | 锚点数值往返（data-flow 场景 4 的 `(292.00, 466.67)` 用例）；跨页/旋转/页间距退化路径 |
| 4 | `session.rs` | `Document` 改动适配；`capture_anchor`/`restore_anchor` 并在 4 类命令中调用；分块决策 + `tiles_for()`/`tiling_policy()`；`navigation()` 转发 | 缩放后视野中心不变；分块阈值上下界（阈值下整页、阈值上分块）；`cancel_page` 清掉该页全部瓦片 |
| 5 | `testkit.rs` | 内存后端实现批量表与 `NavigationIndex`；`MemoryRaster` 支持按瓦片渲染（便于断言"只渲染了该区域"） | 瓦片尺寸断言（最后一列/行更小）；`NavigationIndex` 内容断言 |
| 6 | `tests/assemble.rs` | 适配新 API；新增锚点与分块两条端到端链路 | 见上 |

## 4. 下一轮开工顺序（建议）

1. 装/续装 Rust 工具链（`/tmp` 内，不污染仓库）→ `cargo test` 跑通**现有**测试（先证明 v2 代码自洽）；
2. 按第 3 节清单逐项改造为 v3，每项带测试；
3. 全绿后 `cargo clippy`（可选）+ 提交；
4. 之后再进入 `backend-pdfium` 适配层（M1）。
