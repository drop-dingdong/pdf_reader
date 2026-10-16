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

> ✅ **全部完成**：上述 6 项改动连同测试已落地，`cargo test` 结果为
> **50 单元 + 14 集成 + 1 doctest = 65 passed, 0 failed, 0 warnings**。
> 相关提交：工具链验证检查点 `4f466fd`、D-01 `c1bfc77`、D-02 `82aed7f`、D-05 `f834648`、D-08（本次）。

| # | 文件 | 需要的改动 | 必须新增/更新的测试 |
| --- | --- | --- | --- |
| 1 | `document.rs` | `Document` 加 `page_geometries()`；`page_size`/`page_rotation` 改默认方法；删 `outline()`/`links()`；加 `navigation()`；删 `Destination::Named`；加 `NavigationIndex` | 假后端：批量表长度 = 页数；越界返回 `PageOutOfRange`；`navigation()` 返回已解析目标（不含 `Named`） |
| 2 | `render.rs` | 加 `TILE_SIZE`、`TileSpec`（`grid_for`/`pixel_rect`/`page_rect`）、`TilingPolicy`；`RenderKey`/`RenderRequest` 加 `tile` | 网格计算（含"最后一列不足一块"）；瓦片键与整页键互不覆盖；`pixel_rect` 拼接无重叠无缝隙 |
| 3 | `layout.rs` | `PagePlacement::pixel_size()`；`PageLayout::scroll_to_anchor` 默认实现；`Viewport::center_content_point()`；`ScrollAnchor` | 锚点数值往返（data-flow 场景 4 的 `(292.00, 466.67)` 用例）；跨页/旋转/页间距退化路径 |
| 4 | `session.rs` | `Document` 改动适配；`capture_anchor`/`restore_anchor` 并在 4 类命令中调用；分块决策 + `tiles_for()`/`tiling_policy()`；`navigation()` 转发 | 缩放后视野中心不变；分块阈值上下界（阈值下整页、阈值上分块）；`cancel_page` 清掉该页全部瓦片 |
| 5 | `testkit.rs` | 内存后端实现批量表与 `NavigationIndex`；`MemoryRaster` 支持按瓦片渲染（便于断言"只渲染了该区域"） | 瓦片尺寸断言（最后一列/行更小）；`NavigationIndex` 内容断言 |
| 6 | `tests/assemble.rs` | 适配新 API；新增锚点与分块两条端到端链路 | 见上 |

## 4. 下一步（M1：接入真实后端）

### ✅ 已完成：`crates/backend-pdfium`（2026-10）

`PdfBackend` / `Document` / `PageRaster` / `TextLayer` 四个 trait 的真实实现已落地并测试通过：

* **D-01**：`open` 时一次性读出全部页几何（含逐页 `/Rotate`）；
* **D-02**：瓦片走 PDFium 原生的部分渲染（`set_origin`），**只渲染被请求的区域**；
  真实 PDF 的测试断言"包含文字的那块瓦片有墨迹、页面底部空白瓦片零墨迹"；
* **D-08**：大纲与逐页链接在打开时采集完毕；PDFium 自身已把命名目标解析成具体页；
* 测试用 PDF **由 Rust 现场生成**（2 页、1 页带 `/Rotate 90`、含大纲与链接），仓库无二进制夹具；
* 找不到动态库时测试**跳过并打印指引**（`scripts/fetch-pdfium.sh`）。

顺带修掉一个真实缺陷：`DocumentSession::selected_text` 原先只读缓存，导致"全选 + 复制"
会静默漏掉未加载的页 —— 现改为**按需加载**选区覆盖的页（`&mut self`），
而 `highlight_selection` 保持"尽力而为"（只画已加载的页），避免为高亮拉起整本文本。

**测试总数：81 passed, 0 failed, 0 warnings**（pdfcore 65 + backend-pdfium 16）。

### ✅ 已完成：运行期零件（`WorkerScheduler` + `LruCache`）

放在 `pdfcore`（而不是 app）里的理由：它们是**纯 std** 实现，且只有放在这里才能被
`cargo test` 完整覆盖（放 app 侧就只能通过 UI 验证，违背 agent.md 规则 2 的精神）。

* `pdfcore::scheduler::WorkerScheduler`：后台线程 + 优先级队列（`Interactive < Visible < Prefetch
  < Background`，同优先级 FIFO）、按 `RenderKey` 去重、并发数取 `Capabilities::max_concurrency`；
  **取消不静默丢弃** —— 被取消的任务会被补发 `RenderEvent::Cancelled`，
  否则会话的"在飞集合"永远清不掉、该页再也不会被重新请求（这是设计调度器时发现的关键契约）。
* `pdfcore::cache::LruCache`：LRU + 字节上限（默认 512 MiB）；淘汰以 `RenderKey` 为单位，
  因此分块时淘汰的是单块瓦片；单张超限时保留最后一张，避免"存进去立刻被丢"的抖动。
* 会话新增"滚出视口即取消"：`cancel_stale_jobs()` 按页取消不可见的在飞任务
  （用 `cancel_page` 而非逐个 `cancel(job)`，两种调度器实现都支持且都会补发事件）。

顺带修掉第二个真实缺陷：**"当前页"原先只认视口顶部探针**，导致滚动到文档末尾时
较短的一页（带 `/Rotate` 的横向页 / 最后一页）永远当不上"当前页"，页号卡在前一页。
现改为优先取视口中心，取不到再退回顶部探针。

**测试总数：101 passed, 0 failed, 0 warnings**（pdfcore 85 + backend-pdfium 16）。

### ✅ 已完成：`crates/app`（egui/eframe 桌面壳）

* 打开方式：命令行参数 / 拖放文件；加密文档弹**模态密码框**（`PasswordRequired` 非故障分支的落地）；
* 界面：工具栏（翻页/缩放/适应宽度/旋转/目录开关/搜索）、左侧目录、底部状态栏、中央画布；
* 画布：整页纹理或**瓦片拼接**（分块时）、白底页框占位（D-06）、
  选区与搜索命中高亮（走 `PagePlacement::rect_to_content` 这条唯一换算链）；
* 交互：滚轮/`Ctrl`+滚轮缩放、拖动平移、拖选文字、双击选词、快捷键、拖放打开；
* 运行期零件直接注入：`LruCache`（512 MiB 上限）+ `WorkerScheduler`（并发数取 `Capabilities`）。

> **egui 0.37 的 API 与旧版本差异很大**（`App::ui(ui, frame)` 取代 `update(ctx, frame)`、
> 侧边/上下栏统一为 `egui::Panel`、`Id::new` 弃用、`smooth_scroll_delta` 取代 `raw_scroll_delta`、
> 拖放文件是 `Arc<dyn DroppedFile>`）。
> 纯逻辑已抽到 `crates/app/src/view.rs` 并有 7 个单测；**其余部分必须真实窗口才能验证**，
> 验收清单见 [crates/app/README.md](../crates/app/README.md)（agent.md 规则 2）。

### ⏭ 剩余

1. **人工验收**：按 app README 的 13 项清单检查（滚动跟手性、旋转页朝向、选择与复制、密码框…）；
2. 打包（M3）：三平台 CI、动态库随包 + rpath 校验、macOS 签名公证、AppImage；
3. 真实 PDF 采样集冒烟（CJK、加密、超大页、扫描件）；
4. M2 增强：原生文件对话框（`rfd`）、最近文件、缩略图侧边栏、大文档搜索放到后台。

> 按 [agent.md](agent.md)：逻辑层测试随代码提交；**需要真实窗口/GPU 的验收项交给你**，
> 我会在交付时说明"看什么、怎么看"。
