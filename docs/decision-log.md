# 决策记录（Decision Log）

> 记录已确认的设计决定、其影响范围，以及**代码同步清单**（因为 `crates/pdfcore` 目前落后于设计）。
> 关联文档：[architecture.md](architecture.md)、[traits.md](traits.md)、[trait-definitions.md](trait-definitions.md)、[data-flow.md](data-flow.md)

## 1. 本轮批复（v3）

| ID | 来源问题 | 你的决定 | 设计影响 |
| --- | --- | --- | --- |
| **D-01** | traits §12-1：批量页尺寸 | **增加批量，提前知道每一页大小** | `Document::page_geometries() -> Result<Arc<[PageGeometry]>>` 成为**必需**方法（open 时备好）；`page_size` / `page_rotation` 降为默认方法（查表）。会话 `new()` 从"N 次跨 FFI"变成 1 次 `Arc` 克隆 |
| **D-02** | traits §12-2：分块渲染 | **可做分块预渲染** | 新增 `TILE_SIZE = 512`、`TileSpec`、`TilingPolicy::AbovePixels(16 MP)`（**阈值后由 D-15 改为 4 MP**）；`RenderRequest.tile: Option<TileSpec>`、`RenderKey.tile: Option<TileSpec>`；`PageRaster` 契约新增"瓦片只渲染该区域、不得整页后裁剪"；会话新增 `tiles_for()` / `tiling_policy()`；`PagePlacement` 新增 `pixel_size()` |
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
| **D-09** | 滚动规整**逐轴**判断：内容比视口小的方向上让内容**居中**（偏移可为负），否则夹取到 `[0, max]` | 首轮人工验收"缩小时页面中心往左上漂"（[feedback-round-1.md](feedback-round-1.md) §2.3）；落地 `layout::center_or_clamp` |
| **D-10** | UI 缩放与文档缩放**不得共用快捷键**：关闭 egui 内建 `Options::zoom_with_keyboard`；`Ctrl`+`+`/`-`/`0` 与 `Ctrl`+滚轮统一解释为**文档**缩放 | 同上 §2.1/§2.2：原先 `Ctrl`+`+` 会同时放大界面与页面，而 `Ctrl`+滚轮被 egui 当成 UI 缩放、文档完全没反应 |
| **D-11** | **跳转语义统一**：跳页（上一页/下一页/目录）= 目标页**顶部对齐**视口顶部；搜索命中 = 命中位置**居中**；并新增"键入页码 + 回车"跳转 | 第二轮验收 §12。落地 `PageLayout::scroll_to_page`（顶部对齐）与新增 `ViewerCommand::CenterOnPagePoint`（复用 D-05 锚点机制）。**取代 D-05 中"跳页 = 居中"的部分**（锚点机制本身不变） |
| **D-12** | 缺图占位改为：**先用上一档缩放的位图拉伸占位**，连旧图都没有才画白底页框；分块渲染下只要有任一块真瓦片就画瓦片（旧图不许盖住已就绪内容） | 第二轮验收 §3（连续缩放出现大片白填充）与 §15（800% 视野全黑）。**补充 D-06**：白底页框降级为"什么都没有时"的最终兜底 |
| **D-13** | 拖拽工具只保留 **平移 / 选字** 两档（移除"自动"），按住 `Shift` 临时反选；`PageText::hit_test` 增加容差（字高的一半） | 第二轮验收 §7/§10。"自动"失效的根因正是 `hit_test` 无距离上限：点页面任何位置都会命中"最近的那个字"，于是任何拖拽都变成选字、无法平移 |
| **D-14** | 预取半径 1 → **2**；纹理上传**每帧限量**（≤2 张）且提前上传相邻页 | 第二轮验收 §2（翻页边界卡顿）：把"新页进入视口那一刻"的渲染与上传开销提前摊掉 |
| **D-15** | 分块阈值 16 MP → **4 MP**（`TilingPolicy::AbovePixels(4_000_000)`）| 第三轮验收"缩放总有顿感"。实测（真实 PDFium）一块 512×512 瓦片约 **9ms**，而 4 MP 以上的整页渲染要几十到几百毫秒、内存随面积线性增长（31 MP = 124 MB）。超过 4 MP 后分块**既更快也更省内存** |
| **D-16** | 缩放/旋转后必须**取消"键已过时"的在飞任务**（按页取消，两种调度器实现都生效）；新增 `PDF_READER_DIAGNOSE=1` 运行时诊断输出 | 同上。不取消的话 PDFium（串行）会先把旧比例画完再画新的，用户每次都感到停顿。诊断输出用于定位**只能在真实窗口/GPU 上出现**的问题（agent.md 规则 2：这类现象必须由用户侧提供实测数据） |
| **D-17** | **瓦片视图矩形不变量**：内容空间单位在当前缩放下**就等于该页位图像素**（`pixel_size()` 就是取整后的 `rect.size()`），因此"位图像素偏移"换算到视图空间时**直接相加，绝不能再乘/除缩放** | 第三轮验收的实测诊断是决定性证据：`画法=瓦片 6/6`（6 块都就绪、都画了）却全黑。v1 写成 `pixels / scale`，748% 时 6 块瓦片被压到页面左上角约 1/7 的小方格里，而视口落在页面中部 → 全黑；滚到页面左上角就能看到图（与"滚动时有图片显示"完全吻合）。回归测试 `view::tile_view_rect_is_in_content_units_not_scaled` 直接使用当时的诊断数字 |
| **D-18** | 缩放/旋转后进入 **160ms 预览窗口**：窗口内**不提交渲染**，所有可见页统一显示旧内容（有整页旧图就拉伸；高倍下没有整页图，就用**旧瓦片按比例拼回整页**）；停手后再渲染最终比例 | 第三轮验收：连续缩放会产生一串中间比例（100→120→140→150），逐个渲染既浪费又每步掉帧；且旧图拉伸原先只对"有整页旧图"的页生效，高倍分块时其它页没有预览。落地 `ZOOM_PREVIEW_WINDOW`、`Plan::TilePreview`、`view::tile_view_rect_scaled`、`KeyedStore::{best_tile_scale, tiles_at}` |
| **D-19** | 纹理保留策略：保留**可见页 ±1 + 当前页**，且每页保留「当前比例 + **最接近它的一个旧比例**」（旋转必须匹配）；缩放过场优先用旧内容顶替（当前比例真瓦片 → 旧整页图 → 旧瓦片按比例拼回 → 白纸） | 第四轮验收：`LayoutChanged` 时写了一句 `retain(|key| key.page == current)`，**每次缩放都把非当前页的纹理丢掉** → 中心页有旧图可顶替而不闪，4/6 页只能闪白。这正是用户描述的"只闪非中心页"。同时不能全留：一次连续缩放会产生十几档比例，显存会无限增长。落地 `KeyedStore::retain_preview_scales`（有测试）。**保留判定按页取旋转**：同一份文档各页 `/Rotate` 可能不同，用"当前页"的旋转去筛会把别的页误删（空页） |
| **D-20** | 侧栏宽度**由 app 自己持有并处理拖拽**（`exact_size` + 画布左侧自绘拖拽条），不使用 egui 面板的内建 `resizable` | 第三、四轮两次反馈"空有动作没有结果"。根因：面板状态以 `parent_ui.scope_id().with_salt(id)` 为键持久化，而 eframe 每帧传入的是**新的根 `Ui`**，拖拽结果写进去下一帧读不回来（第二轮曾试图"读回宽度再回填 default_size"，仍无效） |
| **D-21** | 拖拽条与画布都用**显式矩形 + `Ui::interact`**（`view::canvas_rect_for`），**不参与布局流**；画布高度必须保持满高 | 第五轮验收"分隔条能拖，但没有任何图像"：上一版用 `allocate_painter(5px × 整高)` 画拖拽条，纵向布局把整高全部分给它，画布高度变成 0。回归测试 `canvas_rect_keeps_full_height` 钉住"高度不得被吃掉" |
| **D-22** | **预渲染阶梯落地**（设计见 [preview-ladder-design.md](preview-ladder-design.md)）：新增 `RenderQuality{Full,Preview}` + `ViewerCommand::SetRenderQuality`；预览档 = 目标 ÷ 4 **向下量化到 2 的幂**；**只在完整档需要分块（> 4 MP）时启用**；手势中若缓存已有精度不低于预览档的旧内容则**什么都不渲染**（用户补充）；预取固定用完整档 | 手势期间画面持续跟随、新区域不再白纸，而键/缓存/后端**完全不动**（质量档就是"更小的 scale"，`RenderKey` 早已按 `scale_milli` 分键）。切到预览时取消完整档的在飞任务，避免 PDFium 先画昂贵那张。测试：`tests/preview_ladder.rs` 6 项 + app 2 项。偏差说明见设计文档 §10.2（"差距≥2倍"判据恒成立，改用分块阈值判据） |

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
