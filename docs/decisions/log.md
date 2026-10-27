# 决策索引

> 每条一行：**决定了什么**。完整理由见 git 历史里的旧 `docs/decision-log.md`
> （`git show 778dc60:docs/decision-log.md`），以及本目录的
> [`rationale.md`](rationale.md)（主题化的"为什么"）与 [`history.md`](history.md)（演进史）。
>
> 新决策从 **D-102** 起追加在文末，格式见 [`README.md`](README.md) §3。

## 一、接口与领域模型（v1–v3）

| ID | 决定 |
| --- | --- |
| D-00a | 技术栈：Rust + egui/eframe + PDFium（动态库随包分发） |
| D-00b | 接口层独立成**零依赖** crate `pdfcore`，依赖单向（app → pdfcore ← backend） |
| D-00c | 9 个 trait 的划分（文本层可选；不建缩略图/剪贴板 trait） |
| D-00d | `PageLayout::compute` 收 `&[PageGeometry]`（逐页 `/Rotate`），`user_rotation` 叠加 |
| D-00e | 坐标系：后端只给 PDF 页面空间；`PagePlacement` 独占换算；旋转先于缩放 |
| D-00f | 缩放量化 1/8 作缓存键、`invalidate_render` 清在飞集合（**量化部分已被 D-94 废除**） |
| D-01 | 批量页几何 `Document::page_geometries()` 在 open 时备好；单页查询降为默认方法 |
| D-02 | 分块渲染：`TileSpec`/`TilingPolicy`/请求与键带 `tile`（**512 固定网格已被 D-94 的 `TileGrid` 取代**） |
| D-03 | 搜索保持同步接口，不做异步 |
| D-04 | `TextSelection` 的 `&mut self` 作为全局 `&self` 约定的唯一例外 |
| D-05 | 缩放/旋转/重排前后保持"视野中心对应的页面位置"（`ScrollAnchor` / `scroll_to_anchor`） |
| D-06 | 缺图占位画白底页框（D-12 起降级为最终兜底） |
| D-07 | 搜索同步执行（与 D-03 合并） |
| D-08 | 打开时预解析所有链接/命名目标，合并为 `NavigationIndex` |
| D-09 | 滚动**逐轴**判断：内容小于视口时居中，否则夹取（`center_or_clamp`） |
| D-10 | UI 缩放与文档缩放分离：关闭 egui `zoom_with_keyboard`；`Ctrl`+滚轮/`+`/`-`/`0` 只缩放文档 |
| D-11 | 跳转语义：跳页=顶部对齐；搜索命中=居中（新增 `CenterOnPagePoint`） |
| D-12 | 缺图先用上一档位图拉伸占位，什么都没有才白底（D-96 起改为整页级预览 + 底料） |
| D-13 | 拖拽只保留平移/选字；`PageText::hit_test` 加半字高容差 |
| D-14 | 预取半径 1→2；纹理上传每帧限量且提前上传相邻页 |
| D-15 | 分块阈值 16→**4 MP**（实测 512×512 块约 9ms） |
| D-16 | 缩放后取消"键已过时"的在飞任务；引入 `PDF_READER_DIAGNOSE` |
| D-17 | 瓦片视图矩形不变量：内容单位 == 位图像素时直接相加，**绝不除以显示比例**（D-94 起推广为 `x = 页.x + 块像素·u`） |
| D-18 | 缩放后 160ms 预览窗口：窗口内不提交渲染，用旧内容顶住 |
| D-19 | 纹理保留：可见页 ±1 + 当前页，每页"当前比例 + 最近的一个旧比例"（D-97 起为三档） |
| D-20 | 侧栏宽度由 app 自己持有并处理拖拽（不用 egui 内建 `resizable`） |
| D-21 | 拖拽条与画布用**显式矩形 + `interact`**，画布保持满高 |
| D-23 | 回退"预渲染阶梯"实现（自激振荡 + 取消在飞任务），保留设计与安全形状 |
| D-24 | 链接热区命中放核心层；有落点则居中；新增「链接框」开关（低缩放不画） |
| D-25 | 拖动期间持续 `request_repaint`（否则松手才动） |
| D-26 | 拖动/滚动**当帧生效**（不再落后一帧） |
| D-27 | 页面坐标归一化：`PageGeometry.origin`（渲染盒左下角），文本/链接热区减它 |
| D-28 | 诊断加拖动时序（相对进程启动时间） |
| D-29 | 诊断加每帧耗时分解（查知"不跟手"是帧率问题） |
| D-30 | 纹理上传与裁剪共用同一页集合（`view::texture_pages`） |
| D-31 | 目录侧栏虚拟化行渲染（`flatten_outline` + `show_rows`） |
| D-32 | 位图→`ColorImage` 一遍完成；拖动期间暂缓转换/上传、松手补齐；诊断拆"转换/上传" |
| D-33 | 空块只铺白、不画边框；页框只属于页面整体 |
| D-34 | 滚轮作用域：只在指针位于画布时作用于文档；读取时机移到面板绘制之后 |
| D-35 | 不为像素转换做并行化（跟手优先于瓦片即时性） |
| D-36 | 视图内浮窗：可移动/改大小/关闭；后加入者在上层 |
| D-37 | 引用点标注：按 0.5pt 去重、与链接框同阈值、独立开关 |
| D-38 | 截图模式：主视图不响应、`Enter` 输出区域信息、`Esc` 静默退出 |
| D-39 | 浮窗关闭按钮加可见记号（圆底 + ✕ + 悬停提示） |
| D-40 | 浮窗内容分文字/图片；快照走**内存快照**（`compose_snapshot`），图片保持长宽比 |
| D-41 | 截图区域必须先把屏幕坐标换算到内容空间（`screen_rect_to_content`） |
| D-42 | 被引用点升级为可编辑条目（`DestMark`），rect 可重选并回写截图 |
| D-43 | 悬停预览：截图 `None` 时懒加载一次；点击转固定 |
| D-44 | 链接→被引用点打开时建索引（`link_marks`）；跳转优先到矩形中心 |
| D-45 | 悬停预览两道闸门：滚动静默 250ms、指针在浮窗上不出现 |
| D-46 | 已有固定浮窗时只高亮置顶，不再弹临时预览 |
| D-47 | 所有固定浮窗"点击置顶"；临时预览最后绘制（最上层） |
| D-48 | 浮窗交互补齐：标题栏点击置顶、正文区拖动浮窗、临时预览拖动即固定 |
| D-49 | 自定义 preview（Pin）：工具栏进入、点页取点、保存页号+页内坐标 |
| D-50 | Pin 退出条件**只看预览模式切换**（平移/选字/截图） |
| D-51 | 文字浮窗：题目只在标题栏，正文只放内容 |
| D-52 | 浮窗文本统一 `overflow_layout`：正文换行、标题单行截断、正文裁剪 |
| D-53 | 侧栏改造：标签页（目录/自定义preview）、Pin 行操作、行高亮 |
| D-54 | 修正 D-53：只显示 `(delete)` 副本；侧栏行"占位→绘制→注册点击" |
| D-55 | 侧栏条目浮窗按行定位；修正 `click_highlight` 从未生效 |
| D-56 | 有正式浮窗时悬浮只高亮置顶（与 D-46 一致） |
| D-57 | 新增「Pin悬浮响应」开关（只挡悬浮这一路） |
| D-58 | 单击延迟一个双击窗口（**已被 D-59 取代**） |
| D-59 | 去掉双击与单击延迟，行内新增 `→` 按钮跳转；单击立即生成/唤出浮窗 |
| D-60 | 浮窗正文尺寸 `body_size` 作为权威尺寸；`float_size_for_body`/`body_size_of` 互逆 |
| D-61 | 任何"保存数据"的动作都**不**再自动生成浮窗 |
| D-62 | 普通截图跨页拼合用 `pages_intersecting` |
| D-63 | 自定义图片 preview 用"单页 + 最大交叠"语义（跨页框选只取重叠多的那页） |
| D-64 | 自定义数据存档（JSON，手写序列化）；**图片像素不入档** |
| D-65 | 自定义图片 preview 懒合成（存档恢复后按需重建） |
| D-66 | 三处修正：悬浮也合成、限制为本 preview 所在页、浮窗内容每帧刷新 |
| D-67 | Pin 行加 ✎（改题目）/❎（删除）；文字正文原位编辑 |
| D-68 | 浮窗每帧同步标题；切换文档清空自定义数据 |
| D-69 | 修正 D-68：清理必须在恢复**之前** |
| D-70 | 文本输入获得焦点时不采集视图键盘命令 |
| D-71 | 文字正文原位编辑（三个 pass：跳过/检测/绘制） |
| D-72 | 未保存修改标记 `data_dirty` 的置位规则 |
| D-73 | 尺寸比较带 0.5px 容差（**已被 D-74 取代**） |
| D-74 | 浮窗自带权威尺寸 `Float.body`，尺寸不从矩形反推 |
| D-75 | 顶栏重构为三行；页码/缩放双击原位编辑；新增逆时针旋转；隐藏截图/测试浮窗入口 |
| D-76 | 页码/缩放双击**当帧请求焦点**；搜索栏移入中央区域底部；预览按钮改符号 |
| D-77 | 三处焦点/诊断修正（正文编辑与搜索框当帧聚焦；图片预览合成失败诊断） |
| D-78 | 选字模式拖动不再平移；全屏模式（顶栏悬浮、不占布局） |
| D-79 | 修全屏顶栏宽度（用窗口视口宽度）；`F11` 切换全屏 |
| D-80 | 横向浏览：`HorizontalLayout` + `DocumentSession::set_layout` + 横向滚动开关 |
| D-81 | 修切文件后横/纵状态错位；布局菜单每次点击都提交 |
| D-82 | 再次隐藏截图按钮；滚轮/拖动加 `[滚动]` 诊断 |
| D-83 | 锚点缺失时**不再跳回页顶**，改为保持位置并夹取 |

## 二、分块与波段（tiling v1 → v2）

| ID | 决定 |
| --- | --- |
| D-84 | 瓦片拼合错位：先加后端逐瓦片诊断（不盲改） |
| D-85 | 再补贴图侧逐瓦片诊断（区分"内容"与"摆放"） |
| D-86 | 定位根因：同页多比例瓦片混画、旧比例参与绘制、过激淘汰导致白洞 |
| D-87 | 逐瓦片用旧比例同块拉伸顶上（**已回退**，见 D-88） |
| D-88 | **降级/预览必须在整页层面**，绝不逐块混用（当前 INV-8 的来源） |
| D-89 | 临时诊断：瓦片描 2px 红线（D-97 起默认关闭） |
| D-90 | `pdfcore::tiling`：等比分块纯几何（共享整数边界、探测、排列） |
| D-91 | 波段（超采样）纯逻辑：`band_for`/`render_scale_for_band`/`reuse_is_enough`；`DEFAULT_OVERSCAN = 1.6` |
| D-92 | `render_scale_for(display, overscan)`：显示比例 → 波段渲染比例的**唯一入口** |
| D-93 | 写下 tiling-v2 联动改造计划（本轮不改代码） |
| D-94 | **渲染键/请求改用波段渲染比例**；删 512 固定网格改 `TileGrid`；session/app 同步；`scale_milli` 去量化 |
| D-95 | 后端请求改为"块**页面空间矩形** + 期望像素尺寸"，三条硬校验；`set_fixed_size + set_origin` 已足够 |

## 三、白块修复与预览/存档（tiling v3 → 存档 v2）

| ID | 决定 |
| --- | --- |
| D-96 | 写下 tiling-v3 计划：整页级就绪门槛 + 多留一档整页图（本轮不改代码） |
| D-97 | 落地 D-96：`plan_page` 六条判据、`ready_tiles_in_view`、三档保留、`TEXTURE_CAPACITY = 128` |
| D-98 | 缺块处用**整页底料**补齐，不再补白（拖动方向） |
| D-99 | **预览层也压在底料之上**（分层而非二选一）：`needs_underlay` |
| D-100 | 存档加版本号（v1→v2），记录截图分辨率 `scale`；迁移置脏 |
| D-101 | 预览图按记录分辨率**请求渲染**（`request_region` / `region_tiles` / `compose_region_snapshot`） |

## 四、新决策（D-102 起）

### D-102 文档重构：现状与决策分离

- **决定**：`docs/` 只保留两类文档 —— `current/`（现状：分层、机制、开关、测试）与
  `decisions/`（决策索引 / 架构理由 / 演进史）。此前的 17 个设计/历史文档从工作区删除，
  内容吸收进新文档，原文只留在 git 历史（最后共存于提交 `778dc60`）。
- **理由**：旧文档 4.0k 行里混着设计意图、五轮验收反馈、两版改造计划和已被推翻的做法，
  新人无法判断哪句还成立（本轮开发中多次出现"代码与文档不一致"）。分成"现在是什么"与
  "为什么/怎么变来的"之后，前者可以逐条对照代码，后者只追加。
- **影响**：`docs/README.md` 成为入口；`current/rendering.md` 集中了最容易出错的分块/画法
  机制；`architecture.md` 给出 11 条不变量；决策索引保留 D-00a…D-101 的一行摘要以支撑代码里
  的 `D-xx` 引用。
- **验证**：代码里不再有指向已删文档的链接（`grep docs/ crates/` 只命中新目录）；
  `cargo test --workspace` 保持全绿（纯文档改动）。
- **回退**：单提交 `git revert`。
- **状态**：有效。

### D-103 回放器：脚本驱动真实 `App::ui`，四条断言进回归网（第一阶段）

- **决定**：新增**无窗口回放器**（`app::replay`）：用 JSON 脚本（打开/缩放/滚动/跳页/悬停链接）
  驱动**真实的 `App::ui`**（egui headless：`begin_pass`/`end_pass`），逐帧采集画法快照、真实绘制的
  纹理四边形、帧耗时、上传字节与诊断文本，检查四条断言：
  ① 已清晰区域不得变白（页坐标 16×16 格子，清晰层只增不减）；
  ② 一个波段切换只翻一次（按 `(页, 波段, 可见范围)` 分段）；
  ③ 预览尺寸 == 区域 × 分辨率（与 `compose_region_snapshot` 共用 `region_device_rect`）；
  ④ 不得提前切到瓦片（D-97 的就绪门槛，除超时兜底）。
  配套四处**可测试化**改造（均不改行为）：`new_headless` 构造、可注入 `Clock`（18 处行为性
  截止时间改读它，测量仍用真实时间）、`Diagnostics` 去处（stderr / 内存缓冲）、
  `KeyedStore::iter()`（`TextureId` → `RenderKey`）；`last_plan` 改为无条件的结构化 `PlanStamp`。
- **理由**：最近四轮回归（白块 D-97/98/99、预览 D-101）全在"绘制决策 → 实际绘制"这条缝上，
  纯函数单测覆盖不到。回放器让这条缝**可自动检查**，并顺带产出可复现的日志（解决"拖动时看不了日志"）。
- **影响**：`app` 新增 `clock.rs` / `diag.rs` / `app/replay.rs`；`main` 增加 `--replay` / `--sample`；
  测试夹具上移到 `backend-pdfium::fixtures`（app 回放与后端测试共用，仓库仍无二进制夹具）；
  正常运行时行为不变（默认 `Clock::system` + `PDF_READER_DIAGNOSE` 决定诊断）。
- **验证**：`cargo run -p app -- --replay scripts/replay/tiling_800.json` → 8 个动作、85 帧、0 违规；
  **牙齿**：把 D-97 门槛改回 `ready > 0`，同一脚本立刻报 16 条"提前切到瓦片"；
  `replay.rs` 的 10 个单测覆盖四个断言的判定逻辑。
- **回退**：单提交 `git revert`（回放器与可测试化在同一提交里，改了可再单独拆）。
- **状态**：有效（第二阶段：性能基线 + 本机硬件信息；第三阶段：脚本集与门禁）。

### D-104 波段内"瓦片模式"粘住，滚出新块不再退回整页预览

- **决定**：`plan_page` 新增输入 `showing_tiles`（该页**当前波段**上一帧是否已是 `Tiles`）。
  为真时直接返回 `Tiles`，不再因为"新的可见范围没齐"退回 `Stretch`/`TilePreview`；
  缺失块由整页底料补（D-98）。波段一变粘性自然失效，仍走 D-97 的"全就绪才进门 + 400ms 兜底"。
- **理由**：回放实测（D-103）发现，高倍下**滚动**露出新块时，画法会 `Tiles → 旧瓦片预览 → Tiles`
  闪一下（40–100ms），整页包括已清晰的部分一起变糊。这与用户一开始的要求不符 ——
  他要的是"需要重渲某块时不留白，用全页低分辨率补**那一块**"，而不是把整页退回去。
  之前的实现只在"该波段已等满 400ms"后才这么做，波段开始后的 400ms 内仍是整页降级。
- **影响**：`view::DrawInput` 增加字段、`plan_page` 增加一条判据（顺序在"全就绪"之前）；
  `PlanStamp` 的 `fallback` 改名 `partial`（粘性模式下"部分块"是常态，不再专指兜底），
  诊断标签从"（兜底）"改为"（缺 n/total 块）"；回放器把"一次翻页"改为按**连续逗留波段**分段，
  断言 ④ 只在翻页那一刻检查。
- **验证**：`view::tests::tiles_are_sticky_inside_a_band`、
  `a_new_band_still_waits_for_the_whole_range`、`stickiness_does_not_override_earlier_rules`；
  回放 `tiling_800` 的 `drag` 动作从 `瓦片→旧瓦片预览(4/6)→瓦片` 变为 `瓦片(4/6，底料补)→瓦片(6/6)`，0 违规。
- **回退**：单提交 `git revert`（判据是纯函数，回退只影响绘制选择）。
- **状态**：有效（真机手感待用户验收：高倍拖动时新区域应"先糊后清"，页面整体不闪）。

### D-105 回放脚本集与样本集入库，断言 ③ 接上真实预览（第二阶段）

- **决定**：① 回放器新增动作 `open_preview`（唤出第 index 条自定义图片 preview 的浮窗，
  由 app 自己的 `ensure_pending_custom_snapshots` 每帧重试生成 —— 不绕过真实路径）；
  ② 样本统一到 `app::replay::samples`（`a4 / rotated / large / many / preview` 五种，
  `preview` 会一并写 sidecar），命令行 `--sample` 与测试共用；
  ③ `scripts/replay/` 收录 **9 个脚本**，`cargo test -p app --test replay_scripts` 逐个跑并断言 0 违规；
  ④ `ActionReport.previews` 记录每次动作后预览纹理的实际尺寸，写进 JSONL，供"跨缩放一致性"断言。
- **理由**：D-103 的断言 ③ 当时是"潜伏"的（没有脚本会创建预览）；把"存档 → 按记录分辨率请求
  渲染 → 拼图"接进回放后，预览尺寸第一次被端到端验证。样本现场生成（不提交二进制）、
  脚本用数据文件（不改代码就能加场景），两者都让回归网可以持续长大。
- **影响**：`app::replay::samples` 新模块；`--sample` 的种类由样本表决定；测试新增
  `crates/app/tests/replay_scripts.rs`（3 个用例，含"9 个脚本 0 违规"与"预览尺寸与缩放无关"）。
- **验证**：`preview_100` / `preview_800` 的 `[预览]` 行显示两者都是
  `记录分辨率=800% 渲染比例=10.486 尺寸=1573x839`；`replay_scripts` 全部通过（234 passed）。
- **回退**：单提交 `git revert`（新增文件为主，不改运行时行为）。
- **状态**：有效（第三阶段：`timing` profile + 本机硬件信息 + 基线比较）。

### D-106 性能基线：结构指标硬门禁、时序指标同机对比，硬件信息随基线入库（第三阶段）

- **决定**：回放增加 **timing** profile（真实时钟，`--timing`），并在 CLI 上提供
  `--record <基线.json>` / `--compare <基线.json>`：
  * 基线文件带 **machine** 块（CPU 型号 / 核数 / 内存 / OS / 架构 / debug|release），作为
    "附带测试条件"；**每台机器自己记**，`perf/` 不入库；
  * **硬门禁**（硬件无关）：动作序列、`submitted`（渲染请求数，`DocumentSession::submitted_jobs`）、
    `textures_peak`、`preview` 尺寸、`upload_bytes`（±10%）；
  * **只报告**（硬件相关）：`cpu_ms` 的 p50/p95/p99（+30% 告警）、RSS 峰值（2 倍告警）、
    机器条件不一致（先提示"时序只作参考"）；`max` 只记录不比较（同机两次可差 50%）。
  * 退出码：结构不符或回放违规 → 1；只有提示 → 0。
- **理由**：绝对帧耗时依赖机器，跨机器门禁会立刻红，最后一定被注释掉；而"提交了多少次渲染、
  传了多少字节、纹理留了几档"由代码与脚本决定，换机器也一样 —— 那才是能守的回归线。
  用户的要求也正是这两分法（"非硬件依赖的可以直接处理，硬件依赖的依本机器情况处理，
  并写入本机硬件信息作为附带测试条件"）。
- **影响**：`pdfcore` 新增 `submitted_jobs` 计数（诊断/基线用，不改行为，同一波段改缩放不新增有断言）；
  `app::replay::baseline` 新模块（Machine / Baseline / compare / Finding）；CLI 三个开关；
  `FrameStat` 采集 `submitted` 与 `rss_kb`。
- **验证**：`baseline` 6 个单测（往返、同输入零结论、多渲染为门禁、字节容差、换机器只提示、换脚本门禁）；
  集成用例 `a_freshly_recorded_baseline_has_no_gate_findings`（记录→落盘→读回→比较，无门禁结论）；
  手工实测：把基线里 `drag` 的 `submitted` +3 → 立刻 `[门禁] 动作 drag 的渲染请求数 0 ≠ 基线 3`，
  退出码 1。
- **回退**：单提交 `git revert`。
- **状态**：有效（本机实测基线：AMD Ryzen 7 7730U / 16 核 / 15329 MB / linux-x86_64 / debug，
  `tiling_800` 的 p99 ≈ 50ms、RSS 峰值 ≈ 84MB —— debug 构建，仅供同机前后对比）。

#### D-110 后续：为什么要"整页图全留"（实测）

最初的理由是"缩略图不能被裁掉"，但当时没验证。实测（`whole_pages` 场景：100% → 160% → 300% → 回落）：

| 保留策略 | `at_300` 上传 | 整段上传 | 纹理峰值 |
| --- | --- | --- | --- |
| 只留"当前以下最大的一张" | **136.6 MB** | 185.5 MB | 34.7 MB |
| 整页图全留（≤ 当前比例） | 8.4 MB | **47.0 MB** | 44.9 MB |

原因不是"缺底料"，而是**上传与保留打架**：`upload_page` 发现"缓存里有、纹理层没有"的整页图就补传，
而旧规则下一帧又把它裁掉 —— 于是每帧重传 5.1 MB（14 帧 ≈ 128 MB，`[贴图]` 里看不到，因为
当时只有瓦片打日志）。

因此：
* 保留"整页图全留（≤ 当前比例）"；数量天然有限（每页 ≤3 张），多花约 5–10 MB 峰值；
* 把"**上传集合 ⊆ 保留集合**"当成硬约束，并加回归脚本 `whole_pages.json`（基线会在
  `upload_bytes` 上把 8 MB → 136 MB 这种退化直接拦下）；
* 整页/底料的上传也补上 `[贴图]` 诊断行（此前只有瓦片有，正是这次没被发现的直接原因）。

#### D-106 后续修正（真机第一次录制暴露）

用户在 release 下录制基线时：`cpu_ms.p50 = 0.075ms`、5 个动作全部"240 帧内没稳定"，
最后仍写出一份 `submitted` 全 0 的基线。根因是**帧预算按帧数给**：release 下一帧 0.075ms，
240 帧合计 18ms，还没跨过 160ms 的缩放预览窗口 —— 窗口内不提交渲染，于是画面原地不动、
动作永远不"稳定"，而录基线发生在"检查违规"之前，假基线照样落盘。

修正：
* 帧预算改为**帧数 + 墙钟双上限**（20000 帧 / 5s），"没稳定"的报错里标出 `预览窗口=是/否`
  与墙钟耗时；
* 违规检查提前到录制之前：**跑坏了就不写基线**，并明确提示；
* 录制/比较都拒绝"零渲染"基线（`--record` 直接退出 1；`--compare` 判 `[门禁]`）；
* 摘要与逐动作输出加 `提交=n`（一眼看出"到底渲染了没有"）；
* 回归测试：`timing_mode_settles_and_actually_renders`、`a_baseline_without_any_render_requests_is_rejected`。

#### D-106 后续修正二（"忙帧"口径，基线格式升到 v2）

修好帧预算后真机再测：`提交=21`、动作都稳定，但 `cpu_ms.p50` 仍旧是 **0.08ms** —— 因为
timing 模式全速转，160ms 预览窗口里空转约 2000 帧（每帧 0.08ms），把"所有帧"的分位数
稀释成了空转成本。

修正：
* `FrameStat` 增加 `busy`（这一帧转换/上传了像素，或画面内容变了）；指标改为
  **忙帧分位数 + 忙帧 CPU 合计 + 动作墙钟**（墙钟含等预览窗口，最接近手感）；
* 基线格式 `version: 2`：逐动作加 `busy_frames / cpu_ms_total / wall_ms`，顶层加
  `cpu_ms_total / wall_ms_total / busy_frames`；比较时这三项也进 `[提示]` 档；
* 输出摘要与逐动作行都带上"忙 N / 墙钟 / CPU"，一眼能看出是不是空转。

### D-107 存档只做**向后兼容**：新版本存档一律拒绝读取且禁止覆盖

- **决定**：`persist::load` 成为载入的唯一入口：解析 → **版本闸门** → 迁移。
  `version > CURRENT_VERSION` 时返回 `LoadError::NewerVersion`，应用侧（`Sidecar::Locked`）
  当作"没有自定义数据"，并且：
  * 「保存自定义数据」按钮**置灰**（悬停说明原因），`save_custom_data` 也直接拒绝；
  * 状态栏提示"同目录存档是 v9，本程序只支持到 v2：不加载、也不会覆盖它"；
  * stderr 打一行 `[存档] … 由更新版本写入（v9 > v2）：不加载，也不会覆盖它`。
  同时删除 `migrate` 里"更新版本只给提示"的分支（那条路径不再存在）。
- **理由**（用户提出，我同意）：版本兼容的真实需求是**新程序读旧文件**；反过来用旧程序读新文件，
  必然只认识一部分字段，读进来再保存就是"用残缺数据覆盖完整数据" —— 比什么都不做更糟。
  而"读进来但不许保存"会让用户的编辑被静默丢弃，同样不可取。
  拒绝 + 禁止覆盖是唯一没有信息损失的处置；代价是降级使用时看不到那份存档（需要手动挪走文件）。
  副作用是**不再需要**"原样回写未知字段"（原 v4 计划作废）。
- **影响**：`persist` 新增 `LoadError` / `Loaded` / `load`；`migrate` 去掉 newer 分支；
  `PdfReaderApp` 增加 `sidecar_locked: Option<(u32, u32)>`，打开/保存/按钮三处联动；
  数据不加载，`data_dirty` 保持不变。
- **验证**：`persist` 单测 `newer_sidecar_is_refused`（解析成功但 load 拒绝）、
  `current_version_loads_without_migration`、`missing_version_is_treated_as_v1_and_migrated`；
  端到端实测：把 v9 存档放在样本旁边跑 `--replay`（含 `open_preview`）→ 打印拒绝行、
  预览未生成（无数据）、**存档文件 md5 不变**。
- **回退**：单提交 `git revert`。
- **状态**：有效。

### D-108 分块判据加入"纹理单边上限"，预览图超限时降档

- **决定**：分块不再只看"渲染像素面积 > 4 MP"，而是**两条任一成立就分**：
  ① 面积超阈值；② **单边**超过纹理上限 `max_texture_side`（由 app 每帧从 egui 后端传入
  `DocumentSession::set_max_texture_side`，0 = 不限制）。第②条**优先于策略**：即使
  `TilingPolicy::Never` 也必须分（那不是取舍，是纹理根本创建不出来）。
  网格构造改为 `TileGrid::new_bounded(页尺寸, 渲染比例, 上限)`：在"512 目标"给出的层级之上，
  继续细分直到块边长 ≤ 上限（仍受 32×32 封顶）。
  另外预览图重建（`preview_texture`）若按记录分辨率会超过上限，按 1.6 步长降档
  （`view::preview_render_scale`，最多 4 档），回放器的断言③用同一个助手计算期望尺寸。
- **理由**（用户提出）：显卡对单张纹理的**边长**有硬限制，面积小但很长的页
  （例如 100×15000pt 的条幅）在 160% 下是 160×24000 px —— 面积只有 3.84 MP、远低于 4 MP，
  却根本画不出来。只按面积判会把"显存够不够"当成唯一条件，漏掉了"单张装不装得下"。
- **影响**：`TilingPolicy::requires_tiling_bounded`、`TileGrid::new_bounded`、
  `DocumentSession::max_texture_side/set_max_texture_side`（`grid_of` 与 `region_grid` 都用它）、
  `view::preview_render_scale`；app 在 `ensure_textures` 里把后端上限同步进会话（只在变化时）。
- **验证**：`render::tests::a_long_page_tiles_even_below_the_area_threshold`、
  `tiling::tests::bounded_grid_keeps_tiles_within_the_texture_side_limit`、
  `assemble::a_strip_page_tiles_because_of_the_texture_side_limit`（真实会话：不设上限→整页；
  设 16384→分块且每块都在上限内）、`view::tests::preview_scale_drops_when_the_texture_side_limit_would_be_exceeded`；
  回放新增样本 `strip`（100×15000pt）+ 脚本 `strip_page.json`（真实 PDFium 下 0 违规）。
- **回退**：单提交 `git revert`（改的是判据与网格构造，不涉及绘制顺序）。
- **状态**：有效。已知遗留：极端长宽比会切出很多极窄块（按最长边定层级），需要"每轴独立层级"才能优化，列待办。

#### 顺带修掉：JSONL 的字段错位

回放写 JSONL 时用位置参数拼格式串，插入新字段后静默错位（写出 `"settled":5` 这种非法 JSON，
`--out` 的文件无法被脚本解析）。改为"键值对列表"逐个拼接，并加单测 `jsonl_lines_are_valid_json_with_the_right_fields`
（写一行 → 用自家 JSON 解析 → 校验字段）。

### D-109 纹理层记账：按字节统计占用（先观测，不改行为）

- **决定**：`KeyedStore` 增加**字节账**（`bytes()` / `average_bytes()` / `refresh_bytes()`）；
  构造时给一个 `bytes_of: fn(&V) -> u64`，纹理层传 `view::texture_bytes`（`w×h×4`），
  测试替身默认 0 —— 这样测试代码一行都不用改。
  账目在 `insert`（含同键替换）、LRU 淘汰、`retain`、`clear` 之后都保持一致；`retain` 后**重算**
  而不是逐个扣减（摘除路径有好几条，漏扣一次就静默漂移；条目最多几十个，重算是常数活）。
  观测出口三处：`[诊断]` 行的 `纹理=n条/x MB`、回放的 `texture_bytes_peak`（每帧采样取峰值）、
  基线的 `texture_bytes_peak`（结构门禁，±10%）。**不改变任何行为**（没有上限、没有淘汰变化）。
- **理由**：要"按字节预算"必须先有可信的账。128 条目这个上限是 A4 时代的经验值 ——
  条目数相同的两张纹理面积可能差几十倍，靠条目数根本判断不出"占了多少显存"。
- **影响**：`view::KeyedStore`（字段 + 三个方法 + `with_bytes`）、`view::texture_bytes`、
  `app::ensure_textures` 诊断行、`replay::{FrameStat, ActionReport, Report}`、基线格式升到 **v3**。
- **验证**：`view::tests::keyed_store_tracks_bytes_of_its_values`（插入/替换/淘汰/retain/clear
  五种路径后账目都对）；回放实测 `tiling_800` 纹理峰值 **29.3 MB**、`preview_800` **27.8 MB**
  —— 远低于 128 条目可能代表的量级，说明真正需要管的是字节而不是条目。
- **回退**：单提交 `git revert`（纯观测）。
- **状态**：有效（下一步：开场底料；再下一步：按字节的分级淘汰）。

### D-110 开场底料：每页一张低分辨率整页图，杜绝"往回缩露白"

- **决定**：`DocumentSession::request_thumbnail(page)` 请求一张**整页**低分辨率位图，比例
  `min(1.6, sqrt(THUMBNAIL_MAX_PIXELS / 页面面积), 纹理上限 / 最长边)`（`THUMBNAIL_MAX_PIXELS = 1.5 MP`）。
  触发条件是"该页**一张整页图都没有**"（起步就在高倍、或跳到没去过的页），由 app 在
  `ensure_textures` 里对目标页检查并请求；任务记在 `thumbnail_jobs` 里，**不被"波段过时"的取消清掉**。
  保留策略同步放宽：**所有 ≤ 当前比例的整页图都留**（原先只留"当前以下最大的一张"）。
  只有 "≤ 当前比例" 是关键 —— 更大的整页图对缩小没有意义。
- **理由**：整页图是唯一能盖满页面的东西。如果用户一开始就在 800%（或跳到第 119 页再往回缩），
  缓存里没有任何整页图，缩小过程中页面会大片露白，直到新一档渲染完。
  一张 1.5 MP 的缩略图（几 MB、一次十几毫秒）就能把这段空白填掉，代价远低于"多留几档整页位图"。
- **影响**：`session::{request_thumbnail, thumbnail_scale, thumbnail_key, thumbnail_image,
  has_whole_page_image, THUMBNAIL_MAX_PIXELS}`；`view::retain_preview_scales` 的整页规则；
  app 在 `ensure_textures` 里请求、在 `upload_page` 里上传；回放新增 `start_zoom` 脚本字段与
  **断言⑤**（逐帧覆盖率 —— 只在"稳定后"检查会漏掉中间几帧的白，实测就是这样）。
- **验证**：
  * `assemble::thumbnail_is_requested_once_and_survives_band_changes`（幂等、换波段不被清）；
  * `assemble::thumbnail_scale_respects_both_area_and_side_limits`（A0 + 单边 1024）；
  * `view::tests::retain_keeps_current_nearest_and_all_whole_pages_below`（整页图全留）；
  * 回放 `start_high.json`（`start_zoom: 8.0` → 拖动 → 缩回 100%）：**0 违规**；
    **牙齿**：临时关掉底料请求，同一脚本报 2 条"露白"（`back_400`、`back_100`）。
- **回退**：单提交 `git revert`（新增能力，不改原有绘制判据）。
- **状态**：有效。

### D-111 纹理层按字节预算 + 三级淘汰（默认软上限 256 MiB）

- **决定**：`KeyedStore::retain_for_view(facts)` 取代 `retain_preview_scales`：每帧按三类保留，
  并在**字节预算**内让位（`set_max_bytes`，0 = 不限制）：
  * **A 必须** = 当前波段的可见内容（不分块时整页图；分块时**可见范围**内的块）→ **永不淘汰**（软上限）；
  * **B 底料** = 整页图（≤ 当前比例，含开场底料）→ 按字节留，底料优先；
  * **C 旧内容** = 最近一个旧波段 → 先让位，类内 LRU（最旧先丢）。
  预算默认 **256 MiB**（`PDF_READER_TEXTURE_BUDGET_MB` 覆盖）。A 类自身超预算时只打一行
  `[贴图] 纹理超出预算：当前视口需要 x MB > 预算 y MB（不淘汰当前视口，只提示）`。
  配套两条硬约束：
  1. **上传集合 ⊆ 保留集合**：上传只传 A 类（可见范围内的块 + 当前波段整页图）；
  2. **可再生的纹理上传前先问预算**（`KeyedStore::can_admit`）——底料放不下就不传。
- **理由**（用户提出）：固定"条目数"上限和"固定显存硬上限"都不对。极端情形（带鱼屏 + A0 在 800%）
  光是"当前视口"就要 68–206 MB，硬上限会把人正在看的东西裁掉；而多层（多缩放比/多页）留着的
  始终是**可再生**的纹理。所以：当前视口豁免（软上限），其余严格执行。
- **影响**：`view::{PageFacts, RetainReport, retain_for_view, apply_classes, can_admit,
  set_max_bytes}`；`retain_preview_scales` 删除（三个用例改写成 `retain_for_view`）；
  app 组装 `PageFacts`、诊断行改为 `纹理=n条/xMB(A../B../C..) 预算=yMB`；上传只取可见块。
- **验证**：`view::tests::retain_respects_the_byte_budget_by_class`（额度充足一张不丢 / 3 MB 时
  只保 A+底料 / 1 MB 时 A 全留并报软上限）；`replay_scripts::a_tiny_texture_budget_still_keeps_the_view_correct`
  （4 MB 预算跑 `page_walk`，0 违规）；12 个回放脚本实测峰值 9–52 MB（预算未触发裁剪），
  且多数脚本峰值**下降**（不再保"当前波段但不在视口"的块）：large_page 40.8→26.2、
  rotated 41.2→28.1、preview_800 27.8→19.6、  scroll_walk 32.7→26.7。
- **回退**：单提交 `git revert`。
- **状态**：有效。下一步（可选）：把"上传/转换按字节限流"与"开场底料按预算决定要不要"接上；
  目前"上传字节"仍无上限（用户已同意暂缓）。

### D-112 纹理层改成"一个纯函数 + 对账循环"

- **决定**：`view::desired_textures(facts, existing, budget, size_of) -> Desired` 成为**唯一**决定
  "这一帧 GPU 里应当有哪些纹理"的地方（纯函数，每帧算，几微秒）；`ensure_textures` 只做三件事：
  算 desired → `keep_only(desired.keep)` 删除 → 遍历 `desired.upload`（位图已到就传）。
  `upload_page`（"缓存里有位图就传"）与 `retain_for_view` 的历史逻辑一起删除。
  **不做缓存**：重算比一张瓦片的转换便宜约 1000 倍，而缓存需要维护七项输入的失效，
  漏一项就是静默的白块/重传（用户拍板："旧的 desired 应当没用"）。
- **理由**：原来是两个互不知情的策略（push 上传 / pull 保留），只靠"上传 ⊆ 保留"这个口头约定，
  一不一致就每帧传了又删（实测 8.4MB→136MB、350MB 两次）。收成一个纯函数后，
  "上传 ⊆ 保留"由构造保证，预算也只在同一处算，且**按几何算字节**（与"现在有没有"无关）。
- **影响**：`PageFacts` 改为 `PageShape`（Whole / Tiles(range) / TilesHidden）+ 声明式 `whole_pages`；
  `Desired` 带 keep/upload/三类字节数；`KeyedStore::{keep_only, keys_newest_first}`；
  `DocumentSession::{whole_page_scales, image_for_key}`；`MAX_UPLOADS_PER_FRAME`（按页）
  → `MAX_TEXTURE_UPLOADS_PER_FRAME = 40`（按张）。
- **验证**（258 passed，0 warnings）：12 个回放脚本 0 违规，且 `提交/上传/纹理峰值` 与重构前**逐项一致**
  （如 tiling_800 18/40.2/27.6、page_walk 6/71.7/52.3）—— 说明重构没有改变行为。
  新增断言⑥："动作稳定后可见页不能停在糊的预览上"，它是这次重构中两个真 bug 的唯一发现者：
  ① `upload` 漏了 A 类（瓦片从未上传，画面永远拉伸）——被脚本日志"从=旧图拉伸 到=旧图拉伸"暴露；
  ② 超预算时把 `upload` 整个清空（连 A 也传不上去，页面全白）。
- **回退**：两个提交分别回退（先 `desired` 纯函数，再对账循环）。
- **状态**：有效。

### D-113 工程门禁：fmt / clippy / 结构基线 / 文档链接

- **决定**：
  * 全仓 `cargo fmt`（一次性、单独提交，并把该提交记入 `.git-blame-ignore-revs`）；
  * clippy 清零并作为门禁：`cargo clippy --workspace --all-targets -- -D warnings`；
  * **结构基线入库**（`perf/baseline/*.json`，`profile: structural`）：只记渲染请求数、上传字节、
    纹理字节峰值、预览尺寸；时序与内存清零，因此跨机器可比，`compare` 见到它只比结构；
  * `scripts/check.sh` = 以上三项 + 全部测试 + 文档链接检查；`scripts/check-docs.sh` 检查
    相对链接可解析、且**代码注释**不再引用已移出的旧文档。
- **理由**：用户问"CI 具体该怎么做，是不是强制代码风格就够了"。风格是最不值钱的一项；
  真正能拦住回归的是"测试 + clippy + 结构基线"。而结构基线之所以能入库，是因为这些指标
  与硬件无关（这也是 D-106 把指标分两档的直接收益）。
- **影响**：`Baseline` 新增 `profile`（version 4）；CLI 新增 `--record-structural` / `--samples`；
  `perf/` 从忽略改为只忽略非 baseline 内容；新增两个脚本。
- **验证**：`./scripts/check.sh` 全绿（fmt、clippy 0、258 tests、4 份基线一致、文档 14 篇 0 失效链接）。
- **已知缺口**：缺 PDFium 时真实 PDF/回放用例会**静默跳过**，CI 里必须用
  `PDF_READER_REQUIRE_PDFIUM=1` 把它变成失败（`check.sh` 里已标 TODO）。
- **状态**：有效。

### D-114 验收辅助入口改为环境开关（默认不显示）

- **决定**：「截图」「测试浮窗」两个按钮默认**不在界面上**；设 `PDF_READER_DEV_ENTRY=1` 才出现。
  代码保留、可调用，既不加 `#[allow(dead_code)]`，也不从代码里删掉。
- **理由**：用户要求"界面上不显示，但保留代码"。用开关是最小代价：正式界面干净，
  手工验收（浮窗拖动/层级、截图流程）随时能用；而"删代码 + 靠 git 历史找回"会让这两个
  入口每次验收都要重新贴回来。
- **影响**：`app::dev_entries_enabled()` + 顶栏第二行的一层 `if`；`--selftest` 不变。
- **验证**：258 passed、clippy -D warnings 0、fmt 干净（默认路径下这两个 `ui.button` 不执行，
  也不产生 dead_code 警告，因为它们有调用者）。

### D-115 发布包自带 PDFium：`app` + `lib/libpdfium.so`

- **决定**：打包产物固定为
  `pdf_reader_<平台>/app` + `pdf_reader_<平台>/lib/libpdfium.so`（macOS `.dylib`、Windows `.dll`），
  外加一份 `BUILD-INFO.txt`（平台 / 二进制与库的 sha256 / 构建时间）。
  `scripts/package.sh <平台>` 负责构建、组装，并在临时目录里 `./app --selftest` 自检。
- **理由**：`backend-pdfium::library` 的查找顺序里本来就有"**可执行文件所在目录**及其 `lib/`"，
  且按绝对路径 `bind_to_library` —— 所以这个布局**不需要 rpath / LD_LIBRARY_PATH / 工作目录假设**。
  之所以值得显式约定：用户机器上往往装着别的 pdfium，`--selftest` + `BUILD-INFO.txt` 能把
  "到底加载了哪个库"变成一眼可见的事实。
- **影响**：新增 `--selftest`（加载 PDFium 并打印 `platform=…`，失败退出码 1）；`.gitignore` 加 `dist/`。
- **验证**：`./scripts/package.sh linux-x64` 产出 16 MB 目录，在临时目录里（无 `PDFIUM_PATH`、
  非仓库目录）`./app --selftest` 输出"自检通过：platform=linux-x64 pdfium=已加载"。
- **遗留**：macOS 分发仍需签名/公证；Windows 未在真机验证；三平台自动产出留给 CI。
- **修正（Windows 实测加载失败）**：三条原因都补上 ——
  ① `fetch-pdfium.sh` 在 Git Bash 下 `uname -s` 是 `MINGW64_NT-…`，原来直接报错退出
  （于是根本没有 `vendor/pdfium-win-*/lib/`）；现在按 `uname -m` 自动映射到 win-x64/win-x86/win-arm64；
  ② 新增 **PowerShell 版** `scripts/fetch-pdfium.ps1` / `scripts/package.ps1`（不依赖 bash，
  负责下载、把 `bin\pdfium.dll` 归位到 `lib\`、构建、打包、`--selftest`）；
  ③ `library.rs` 的加载失败信息现在标注**每个候选路径"文件存在/不存在"**
  —— 区分"没下载"与"位数不对/缺 VC 运行库"。
- **修正（只打包宿主平台）**：脚本调用的是宿主 `cargo build`，产出宿主格式的二进制
  —— 在 Linux 上 `package.sh win-x64` 只会得到一个 ELF 却命名为 Windows 包。
  因此脚本现在**拒绝**与当前平台不一致的请求（直接退出码 2 并说明正确做法）；
  Windows/macOS 产物在对应平台（或 CI 对应 runner）上构建，不做交叉编译。
- **修正（Windows 命名）**：上游 `pdfium-win-*` 包里，**运行库** `pdfium.dll` 在 `bin/` 下，
  而 `lib/pdfium.dll.lib` 是**导入库**（链接期用）。因此：
  `scripts/fetch-pdfium.sh` 装完把运行库归位到 `lib/` 并删除 `*.dll.lib`/`*.a`/`*.pdb`；
  `scripts/package.sh` 的 vendor 查找 `lib/` 优先、回退 `bin/`；
  `library.rs` 的 exe 候选目录增加 `bin/`；平台表补 `win-x86`（`ARCH=x86`）。
  单测 `candidate_directories_cover_exe_dir_lib_and_bin` 守住三种布局。
- **状态**：有效。

### D-116 链接框按"目的地类型"用两种颜色

- **决定**：内链的 `/Dest` **带页内位置**（`Destination::Page { offset: Some(_) }`）时，链接框画成
  **绿色** `(40,170,90)`；只给页码（`offset: None`）或外部 URI 时仍是**红色** `(220,40,40)`。
  判定集中在 `view::link_box_color`，单测覆盖三种目标。
- **理由**：两类目的地在 PDF 语义上不同（只页码 = 跳到页首；页码 + 位置 = 跳到页内某处），
  但界面原本同色，无法分辨（用户要求区分）。
- **影响**：`draw_canvas` 的链接框绘制改用该函数；`LINK_OVERLAY_COLOR` 仍用于引用点记号。
- **验证**：`link_box_color_distinguishes_page_only_and_page_with_position`。
- **状态**：有效。

### D-117 修复红蓝对调（`FPDF_REVERSE_BYTE_ORDER` 默认开启）

- **决定**：渲染配置显式 `.set_reverse_byte_order(false)`，让 `PdfBitmapFormat::BGRA` 真的输出
  B,G,R,A，与 `PixelFormat::Bgra8` / `view::color_image` 的交换保持一致。
- **理由**：`PdfRenderConfig::new()` 默认 `do_set_flag_reverse_byte_order = true`，PDFium 于是
  往 BGRA 位图写 **RGBA** 字节；我们再交换一次 → 红蓝对调。用户实测：黄色页显示成蓝色、
  蓝色字显示成红色（与"色相不动、只换 R/B"的症状完全一致）。
- **影响**：`backend-pdfium::raster` 一行配置 + 注释；`docs/current/rendering.md` 增加 §7.1.1。
- **验证**：新增样本 `fixtures::colored_page_pdf`（200×200 整页黄底 + 蓝字）与
  `real_pdf::yellow_page_stays_yellow_in_bgra_bytes`。去掉那一行时该测试失败并打印
  `(B=255,G=255,R=0)`（= 青色），加上后通过 —— 有牙。
- **状态**：有效。

### D-118 注释功能：已定决策与分阶段计划

**定位**：在阅读器上加"页面标注"（高亮 / 下划线 / 删除线 / 墨迹 / 选框 + 正文）。
本节记录**已定**的决策与阶段划分，实现按此推进；未实现前不改渲染管线。

#### 已定决策

1. **正文与图层分离**
   * 注释正文 → 复用 sidecar 的 `custom_texts` 条目（不新增 `SavedData` 字段），引用键用
     **哨兵标题** `#annot:<id>`（用户确认可以直接显示，不需要隐藏）；
   * 图层几何（高亮矩形 / 墨迹点 / 选框）→ 独立文件 `<pdf>.annot.json`（自带 `version`，
     新版本拒绝加载、不覆盖）；
   * 懒解析：点击注释时才按标题查正文；查不到 → 保留几何、正文显示"（正文已丢失）"，
     加载**不得**因为引用失效而失败。保存时对标题做唯一性检查。
2. **几何只存未旋转页面空间**（与 `Link` / `TextChar::bbox` 一致），绘制时才换算到内容/屏幕
   空间。注释**不进渲染管线**：不改 `desired_textures`、不改渲染键与瓦片网格。
3. **Ctrl+S** 保存所有脏文件：在 app 层处理（不进 `ViewerCommand`），先文本后 overlay，
   各自 tmp+rename 原子写；`sidecar_locked`（新版本拒绝）时拒写并提示；全干净时只报"无改动"。
4. **撤销只有一条栈**（时间序），带 redo 与**可视列表**（列表只是栈的视图；点击行只定位，
   不做状态跳转，避免全量快照把截图内存放大 N 倍）。每条命令带一句中文描述，撤销后给提示；
   目标不可见时给"显示"入口。截图类编辑的逆操作**把旧图移进栈**（不复制），按**字节**设上限。
5. **文本编辑的字符级撤销交给 egui**（`TextEdit` 自带 `TextEditUndoer`）；我们的栈只记录
   提交后的整体值（失焦提交）。全局 `Ctrl+Z` **在文本框有焦点时不拦截**。
6. **墨迹：一笔一命令**。拖拽中累积点、抬起时入栈；Esc / 指针移出窗口 / 切工具 → 丢弃该笔；
   相邻点 < 0.5px 抽稀。
7. **M4（写回 PDF）之前不推送本功能**，因此不追求"PDF + 两个 sidecar 一起搬迁"的便携性；
   但单文件原子写仍保留（不 push 不等于不会自伤）。

#### 进度（截至 `739b0bc`）

**S0–S3 已完成**，细节与交接见 [`../handoff-annotations.md`](../handoff-annotations.md)。
与原始计划的两处偏差：
1. "自定义 preview 重新截图"**不存在该入口**（截图入口只用于新建），故 S3 覆盖的是
   "标注增删 / 删除自定义 preview / 引用点重选"三类；
2. 正文不存进命令快照，而是删除那一刻进 `orphan_bodies`（否则"改正文 → 撤销 → 重做"会丢内容）。

#### 阶段

| 阶段 | 内容 | 验收要点 |
| --- | --- | --- |
| **S0** | 回放器补 `click` / `drag`（页面空间坐标）+ 断言字段 `page_after` / `scroll_after` | 现有 12 脚本结构基线不变；点链接能跳页、拖拽能平移 |
| **S1** | `pdfcore::annotation`（模型）+ 高亮/下划线/删除线 + 点击看正文 + 叠加绘制 | 1× 与 8× 下注释的 content 坐标误差 < 0.5px；旋转 90/180/270 位置一致 |
| **S2** | `<pdf>.annot.json` 落盘 + Ctrl+S | 保存→重开→注释与正文都在；引用缺失不崩；只读目录有可读错误 |
| **S3** | 单栈撤销/重做 + 可视列表（覆盖注释与"重选截图/引用点"） | 3 画 2 撤 1 重做后剩 2；重选截图撤销后位图字节一致 |
| **S4** | 墨迹 | 1000 条以内忙帧 p99 不明显退化 |
| **S5** | 写回 PDF（单独评估） | 写成功后删 overlay 与被引用的正文条目 |

#### 跨阶段不变式

* 几何只以 page space 存储；内容/屏幕坐标只出现在绘制与断言里；
* 注释不影响结构基线（渲染请求数、上传字节、纹理峰值）；
* 每个交互都必须能在无窗口回放下复现；
* 每阶段结束 `./scripts/check.sh` 全绿，新脚本的结构基线入库。

#### 风险（按严重度）

1. **旋转页坐标**（S1 必须有 90/180/270 断言，否则"看起来对、转个角度全歪"）；
2. **哨兵标题唯一性**（S2 的 debug 断言）；
3. **撤销的位图内存**（S3 的移动语义 + 字节上限）；
4. **截图不含注释**（`compose_region_snapshot` 只合成位图；需在 S2 后明确写入文档）；
5. **回放拖拽跨帧**（S0 先做，否则 S1 之后全靠人眼）。

### D-119 墨迹（S4a）：模型 + 回放动作 + 标注图层 v2

- **决定**（D-118 S4 的第一批，按"先回放、后模型、再工具"的顺序落地）：
  1. `AnnotationKind::Ink { points: Vec<Point>, width: f32 }`，**顶点与线宽都是页面空间**
     （存屏幕像素会让"画完再放大"变成一条细线）。`quads()` 对墨迹返回**空切片**：
     只处理行矩形的旧代码会自然跳过它，而不是把它当零面积矩形误判。
  2. `bounds()` 按点集外扩**半个线宽**（一条横线否则高度为 0，浮窗定位/截图会退化）；
     `contains()` 用"点到折线的最短距离 ≤ max(width/2, tolerance)"，
     **不能用包围盒**——L 形墨迹的包围盒里有大片空白。
  3. `<pdf>.annot.json` 版本 **1 → 2**（新增 `points` + `width`）。必须升版本：
     v1 的读取端会把 `"kind":"ink"` 当未知种类静默丢掉，用户再保存一次就真丢了；
     升版本让旧程序按既有规则**拒读且不覆盖**（D-107 / D-118 第 7 条）。v1 文件仍可读，
     不需要迁移（读得懂就不改数据）。
  4. 回放动作 `ink { page, points, width? }` 走"直接建标注"，与 `highlight` 共用新的
     `app.add_annotation` 唯一入口，因此自动获得 `apply_edit` 的撤销/重做与配对正文条目。
     **真实拖拽**（一笔一命令、Esc / 移出窗口 / 切工具丢弃、0.5px 抽稀）留给 S4 的工具层，
     那一层接上后改由手势驱动同一个 `annotate_ink`。
- **影响**：`pdfcore::annotation`（`Ink` / `is_valid` / `bounds` / `contains` / `ink()`）、
  `persist`（`ANNOT_VERSION = 2`、`SavedAnnotation::{points,width}`、
  `annotation_kind_from_saved`）、`app`（`add_annotation` / `annotate_ink`、墨迹绘制）、
  `replay`（`ink` 动作 + `DEFAULT_INK_WIDTH`）、`scripts/replay/ink.json` + 结构基线。
- **验证**：`annotation::tests::ink_*`（不变量 / 空 quads / 半线宽包围盒 / 折线命中而非包围盒）、
  `persist::annot_tests::ink_round_trips_through_json_with_points_and_width`、
  `a_version_1_overlay_still_loads`、`ink_with_too_few_points_is_skipped_at_the_model_layer`、
  回放 `ink_stroke_is_one_annotation_that_undo_and_redo_can_move`（一笔 = 1 条、撤销/重做、
  包围盒 48.5 / 688.5 / 233 / 43）、`annot_roundtrip` 增加墨迹（重开后几何与线宽原样回来，
  文件里 `"version": 2`），结构基线 `perf/baseline/ink.json` 与现有 8 条并列通过。
- **未做**：墨迹工具（手势状态机）、抽稀上限、Esc 丢弃、1000 条以内的忙帧 p99 验收；
  这些是 S4 的下一批。
- **状态**：S4a 生效；S4 未完成。

### D-120 墨迹工具（S4b）：拖拽中不建命令、抬笔才入库

- **决定**：
  1. **一笔一命令**落实在"抬笔那一帧"：拖动过程中采样点只存在 `InkStroke`（`ink.rs` 的纯逻辑），
     不进 `AnnotationStore`、不产生命令；`Esc` / 切走墨迹工具只要丢掉它即可，
     所以既不会留下半条，也不会在撤销栈里出现"半笔"。
  2. 起点用 egui 的 `press_origin`，并把**触发 `drag_started` 那一帧的当前位置**也采进来。
     只取位置会丢掉"按下 → 移过拖动阈值"之间的那一段（快速起笔时整笔少一截，实测可见）。
  3. 抽稀阈值 **0.5 页面空间单位**（不是屏幕像素）：同一笔画在 100% 与 800% 下必须得到
     密度相同的顶点，否则"几何只与页面空间有关"就破了；上限 2000 点/笔，非有限值丢弃。
  4. 丢弃条件：`Esc`、切走墨迹工具。指针移出窗口暂不特殊处理（egui 会继续给 `dragged`），
     列为已知边界而不是 bug。
  5. 墨迹默认样式独立：**不透明深色**（`INK_COLOR` / `INK_OPACITY`），文本类仍是半透明黄。
     35% 的黄笔画在页面上几乎看不见，会被当成"画了没反应"；默认值由 `Annotation::new`
     按种类选，存档里的颜色/不透明度优先。
  6. 回放 `ink` 动作改成**真实拖拽**（要求先 `set_tool ink`）：按下 → 逐点移动 → 抬起；
     另加 `set_tool` 动作，并给手势帧增加"同帧键盘事件"（`GestureFrame.keys`），
     让 `cancel: true` 能在抬起前按下 `Esc` —— 否则"拖到一半取消"只能靠人眼。
     `--out` 的 JSONL 顺带补上 `annotations` / `annotation_bounds` / `floats` / `previews_total`，
     以后排查"数量不对"不必再写临时测试。
  7. 工具栏的三种工具（平移/选字/墨迹）走同一个 `switch_tool`（工具栏与回放共用），
     保证"切走墨迹 = 丢弃正在画的一笔"只有一处实现。
- **影响**：`app/ink.rs`（新增）、`Tool::Ink` / `DragMode::Ink` / `switch_tool` /
  `handle_ink_pointer` / `page_at_content` / 墨迹绘制（含进行中的一笔）、
  `pdfcore::annotation` 的默认样式常量、`replay` 的 `set_tool` + 手势按键 + JSONL 字段、
  `scripts/replay/ink.json`（改为真实拖拽 + 1×/2× 对照 + Esc 取消）与结构基线。
- **验证**：`ink::tests`（抽稀 / 非有限值 / 2000 上限 / 单点不成笔）、
  回放 `ink_stroke_is_one_annotation_that_undo_and_redo_can_move`
  （一次拖拽 = 1 条、撤销/重做、`Esc` 后仍是 1 条且无多余正文、1× 与 2× 包围盒逐值相同）、
  `annot_roundtrip` 的墨迹保存/重开、9 条结构基线一致。
- **未做**：1000 条以内的忙帧 p99（S4c）、线宽/颜色 UI、指针移出窗口的丢弃、手写笔压力。
- **状态**：S4b 生效；S4 只剩性能验收与人工手感验收。

### D-121 S4c：绘制期间屏蔽链接悬停；历史列表虚拟化；1000 条墨迹门槛

- **决定**：
  1. **绘制期间不响应链接悬停**（用户实测：墨迹压住链接时弹出了目的地预览，打断绘制）。
     `hover_preview_allowed` 增加第三个闸门 `drawing`；`drawing` 的判定是
     "墨迹工具 &&（已成笔 || 主键按下）" —— 后者覆盖"按住但还没成笔"的那一小段
     （`ink` 要等指针移过拖动阈值才建立，只用 `ink.is_some()` 会留一个窗口）。
  2. S4b 写的"切走工具 → 丢弃正在画的一笔"**在 UI 上不可触达**（拖动时根本点不到工具栏）。
     代码保留为防御（回放 `set_tool` 能触发），但文档不再把它当可验收项。
  3. 压力脚本暴露了一个真实的 p99：1000 条标注本身画一帧只要 ~2ms，
     但**撤销后自动打开的历史窗口**一次排版 1001 行 = **323ms**。
     改成 `ScrollArea::show_rows` 虚拟化（行高 18、最大高 360），并给 `History`
     增加 `len` / `description(index)` 的按行访问 —— 同一帧降到 ~16ms。
  4. 压力脚本用新动作 `seed_ink`（模型级造 N 笔，走与绘制相同的 `annotate_ink`）。
     不用手势造 1000 笔：每笔 8 帧、上万帧，门禁会变慢且噪声大。
  5. 性能门槛分两类：
     * **结构门槛（强，与机器无关，进 check.sh）**：造标注时 `submitted == 0`、纹理层峰值不变
       —— 这是 D-118"注释不进渲染管线"的直接证据，基线 `perf/baseline/ink_many.json`；
     * **时间门槛（弱，debug 宽松上限，只在测试里）**：画 1000 条一帧 < 50ms、
       造 900 笔 < 100ms、撤销 + 历史窗口帧 < 100ms、100→1000 的帧时间 < 25 倍（反二次退化）。
       实测值随测试打印（造 100 笔 2.9ms / 造 1000 笔 4.0ms / 画 1000 笔一帧 3.1ms / 撤销 17ms）。
- **影响**：`view::hover_preview_allowed` 签名与测试、`app::update_hover_preview`、
  `app::draw_history_window`（虚拟化 + 两个常量）、`history::{len,is_empty,description}`（含单测）、
  `replay::Action::SeedInk` 与 `app::seed_ink_strokes`、`scripts/replay/ink_many.json` + 结构基线。
- **验证**：`view::tests::hover_preview_*` 三个闸门；`history::tests::descriptions_are_indexable_*`；
  回放 `a_thousand_ink_annotations_stay_out_of_the_render_pipeline_and_cheap`
  （结构：seed/settle 的 `submitted==0`、纹理峰值不变、1000 条后还能再画一笔；
  时间：四条上限），`ink_many.json` 结构基线逐项一致。
- **未做**：release 构建在固定机器上的时序门禁（仍是开放项）、线宽/颜色 UI、指针移出窗口的丢弃。
- **状态**：S4 完成（S4a `2fdc8bd` / S4b `c0350b0` / S4c 本条）。下一步是 S5 的**评估**（会改用户文件）。

### D-122 S5（写回 PDF）：先评估，不直接实现

- **决定**：S5 本轮**不写代码**，先产出评估
  [`evaluations/s5-writeback.md`](evaluations/s5-writeback.md)。评估结论：
  1. 技术可行，路径是"PDFium C API + 自带一层小 FFI + **子进程**执行"；
  2. **墨迹必须自己生成 `/AP` 外观流**（实测：无 AP 时 PDFium 只画出 1 个彩色像素，有 AP 时 162 个）；
  3. 文本类几何只能走 `FPDFAnnot_SetAttachmentPoints`（PDFium 文档原文称其“即 quadpoints”），
     且无 AP 时 PDFium 会自行合成高亮（实测黄色 3996 px、文字仍可读）—— 这条要用 spike 验证；
  4. 我们的程序**读不回** PDF 注释 ⇒ **不能**"写成功后删 overlay"，S5 的第一形态应是"导出副本"。
- **理由**（细节与证据见评估 §3）：
  * `pdfium-render 0.9.4` 没有注释/页面对象 API，`handle()`/`page_handle()` 都是 `pub(crate)`，
    保存 flags 在源码里写死为 0 → 写回必须自带 FFI；
  * 自带 FFI 会绕过该 crate 的全局锁（默认 `thread_safe`），与后台渲染线程竞争 → 子进程隔离；
  * 我们的几何是**未旋转页面空间**，与 `/InkList`、`/QuadPoints` 同坐标系，可直接落值；
  * 透明度 `/CA` 没有公开 setter → 文本类写回后透明度必然与界面的 35% 不一致（记录为已知差异）。
- **影响**：新增 `docs/decisions/evaluations/` 与一篇评估；**不改任何代码**。
  S5 的形态（导出/原地）、overlay 去留、范围、加密/签名策略、输出命名 5 个决策待用户确认（评估 §10）。
- **验证**：评估里的两个探针已实际运行（数字见 §3.3）；探针代码一次性、不入库，复现步骤在评估 §11。
- **回退**：纯文档，`git revert` 本条即可。
- **状态**：评估完成；**实现待确认**。

### D-123 S5 形态确认 + S5a spike：四种都能写，统一用 `SetAP` 生成外观

- **决定**（用户 2026-10-27 确认，全部按评估 §10 的建议）：
  1. **形态 = 导出副本** `<原名>.annotated.pdf`，不动原文件；
  2. **overlay 保留**（导出不删 `<pdf>.annot.json`）；
  3. **四种标注都写**；
  4. 加密 / 有数字签名 / `annotate == false` → **拒绝并说明原因**；
  5. 输出**不覆盖**（同名已存在则报错/加序号）。
- **S5a spike 结论**（可复现：`cargo run -p backend-pdfium --example s5_spike`）：
  * 墨迹：`CreateAnnot(Ink)` + `AddInkStroke`（返回 0 = 0 基索引，不是失败）+ 外观 →
    重开后注释数 1、渲染 220 红色像素，包围盒 (40,259)-(259,259)（页面坐标直写）；
  * 文本类：`AppendAttachmentPoints` 写的就是 `/QuadPoints`（高亮渲染包围盒精确到 (40,240)-(259,259)）；
  * **下划线/删除线没有 AP 就不可见**（下划线 0 像素；删除线 24 像素只覆盖文字，空页 0）；
    `AppendObject` 对下划线返回失败（文档也只支持 ink/stamp）；
  * **`FPDFAnnot_SetAP` 是通用解**：PDFium 建出 `/BBox` = `/Rect`、无 `/Matrix`、**无 `/Resources`** 的
    Form XObject，内容用页面坐标 → 下划线 440 像素、墨迹两条子路径 618 像素，位置都精确；
  * `/Contents` 用 UTF-16LE 往返正确（中文）；`flags=0` 与 `FPDF_NO_INCREMENTAL` 同尺寸且都能重开；
    真实样本写回后页数与既有链接都保留。
- **理由**：把"外观"这一层从 `AppendObject`（只支持 ink/stamp）换成 `SetAP`（任意子类型），
  方案从"部分可写"变成"四种全可写"，且不需要引入 lopdf（离线不可得）。
- **影响**：新增 `crates/backend-pdfium/examples/s5_spike.rs`（一次性探针，非产品代码）与
  `libloading` 的开发依赖；评估文档新增 §12 并修正 §1/§3.3/§6/§10。**产品代码未改**。
- **验证**：example 的 7 组断言/输出（见评估 §12）；文档检查通过。
- **未做 / 下一步**：S5b（子进程 `--writeback` + 导出副本 + 写后校验）未开始；
  **旋转页上的注释位置**是 S5b 的必测项（spike 的渲染调用传的是 `Rotation::None`）。
- **状态**：有效（S5b 待启动）。

### D-124 S5b-1：写回落地；"关闭 PDFium 再写"留给原地覆盖再议

- **决定**（回应用户关于"写回期间关闭 PDFium、写完重载"的提问）：
  * **S5b（导出副本）不采用**：子进程已经隔离了 PDFium 的全局状态与线程安全；
    关闭会话反而要付 UX 成本（撤销栈、纹理缓存、选区、滚动恢复、重渲染），
    而且解决不了"半写文件"（仍要 temp + rename + 校验）；
  * **S5c（原地覆盖）技术上可行**：`WorkerScheduler::Drop` 会 join 工作线程，
    `DocumentSession` 释放后 PDFium 文档句柄归零；Windows 上替换原文件本来也**必须**先放开。
    但必须满足四条：①写入期间"任何线程都进不了 PDFium"要有**唯一闸门**（不是"尽量停"）；
    ②temp + rename + 写后校验 + 失败回滚；③写失败要重新打开并恢复视图；④FFI 崩了会带走整个阅读器
    （子进程只死子进程）。→ S5c 时在"关-写-重载"与"子进程 + 关闭父文档后 rename"之间再评估。
- **S5b-1 实现决定**：
  1. 新增 `backend-pdfium::writeback`：18 个 FFI 符号（`libloading` 转入常驻依赖），
     `write(spec)` 创建注释 + 生成外观 + `FPDF_NO_INCREMENTAL` 保存 + **重开自检**；
  2. 子进程入口 `app --writeback <job.json>`（新增 `app::writeback` 解析 job）；
  3. 四种标注：`AddInkStroke` / `AppendAttachmentPoints` 写语义字段，`SetAP` 生成外观
     （**高亮不写 AP**，否则不透明填充盖住文字）；文本类线宽固定 2pt（页面空间）；
  4. 闸门：加密（`FPDF_GetSecurityHandlerRevision >= 0`）与数字签名（`FPDF_GetSignatureCount > 0`）
     一律拒绝；`annotate == false` 只出现在加密文档，被加密闸门覆盖；
  5. **拒绝 `source == output`**：原子替换永远由调用方做；
  6. 留存文档：`docs/current/writeback.md`（函数表、ABI、坐标系、AP 规则、闸门、排查手册）。
- **旋转页的实测发现**：PDFium 的 `FPDF_RenderPageBitmap`（pdfium-render 走它）在页面自带 `/Rotate`
  时对注释应用了带纵横比缩放的错误矩阵 —— 用**原始 PDF**（不经过写回）同样复现：
  `/Rotate 90` 下页面空间 (50,100)-(150,120) 的高亮被渲染成 (50,100)-(59,299)。
  → 自动测试只断言"`/QuadPoints` 保持页面空间 + 注释数正确"，真实位置列为**人工验收**（Chrome/Acrobat）；
  这也解释了为什么不能拿本仓库的渲染器给写回位置背书。
- **影响**：`backend-pdfium/src/{writeback,library}.rs`、`app/src/{writeback,main,lib}.rs`、
  `backend-pdfium/tests/writeback.rs`、`app/tests/writeback_cli.rs`、新文档与索引；
  `libloading` 从开发依赖转为常驻依赖；S5a 探针仍保留为 example。
- **验证**：`writeback` 单测 5（内容流/QuadPoints 顺序/数字格式/拒覆盖）、
  端到端 4（四种标注位置与计数、链接保留、旋转页几何、退化几何）、
  app CLI 3（job 相对路径、未知 kind、单点墨迹）；`check.sh` 全绿。
- **未做 / 下一步**：S5b-2（主进程起子进程、写后校验、temp/rename、UI「导出带标注的 PDF…」、
  拒绝原因提示）；加密/签名夹具；旋转页人工验收。
- **状态**：S5b-1 有效；S5b-2 未开始。

### D-125 S5b-2：导出编排与 UI（不覆盖、temp→rename、冒烟校验）

- **决定**：
  1. **命名**：`<stem>.annotated.pdf`（`book.pdf` → `book.annotated.pdf`），与源同目录；
  2. **不覆盖**：目标已存在 → 直接拒绝并提示，不做任何写入；
  3. **落盘**：子进程写 `<目标>.tmp-<pid>-<nanos>`，父进程冒烟校验通过后 `fs::rename`（同目录、原子）；
     失败删临时文件，源文件永远不动；
  4. **父进程校验只做冒烟**：重开 + 页数一致 + 渲染一页（`Scale(0.5)`）。**不比较像素** ——
     PDFium 整页渲染在旋转页上的注释位置有已知偏差（见 D-124），像素断言会误伤；
  5. **子进程失败**：把 stderr 原样放进状态栏消息（可读原因来自统一错误类型）；
  6. **UI**：工具栏第 0 行「导出标注 PDF」，未打开 PDF 或没有标注时禁用并说明；
     加密文档在父进程先预检（即时提示），签名/其余闸门在子进程里拒绝；
  7. **job 正文**：从 `custom_previews` 按 `#annot:<id>` 取；空正文不写 `/Contents`。
- **理由**：把"危险动作"关在子进程里（D-124），父进程只做可回滚的文件操作；
  这版不追求进度条/取消，先把正确性与可排查性做对。
- **影响**：`app/src/export.rs`（新增，5+1 个单测）、`app/src/app.rs`（工具栏按钮 + `export_annotated_pdf`）、
  `docs/current/{writeback,app-ui}.md`。
- **验证**：`export::tests`（路径、job JSON 可解析与正文转义、按页分组、拒覆盖、空列表、`finalize` 用真实写出的 PDF
  做"校验→改名→删临时文件"）；`writeback_cli` 3 条；`check.sh` 全绿（319 tests）。
- **未做 / 下一步**：旋转页与四种标注的**人工验收**（Chrome/Acrobat）、导出进度/取消、
  加密/签名夹具、S5c（原地替换，依赖 S6）。
- **状态**：有效（S5b 完成）。

### D-126 修正：`DocumentInfo.encrypted` 恒真；页面 `/Rotate` 被应用两次

- **问题（用户实测）**：
  1. 导出任何普通 PDF 都提示“加密文档暂不支持导出”，但 Okular 能正常批注保存；
  2. `/Rotate 90` 的页在本程序里显示成 **180° + 被压扁**（其他阅读器正常）。
- **根因与修复**：
  1. **`encrypted` 判据错**：`pdfium-render` 对**未加密**文档返回 `Ok(PdfSecurityHandlerRevision::Unprotected)`
     （PDFium 的 -1），而旧代码写的是 `.is_ok()` —— 于是**每个**文档都被判成加密，导出被父进程一律拒绝。
     改为：`Ok(Unprotected) => false`；`Ok(Revision2/3/4) => true`；`Err(未知修订，如 AES-256 的 5/6) => true`（保守）。
  2. **旋转被应用两次**：`FPDF_RenderPageBitmap` 自己会应用页面自带的 `/Rotate`
     （pdfium-render 的 display transform 已包含 intrinsic rotation），而适配层把
     “页面 `/Rotate` ⊕ 用户旋转”的**最终**旋转又传给 `.rotate()`。`/Rotate 90` 的页于是转 180°，
     位图按 90° 的尺寸、内容按 180° 画 → 压扁。修复：后端把 intrinsic 减掉，
     只把 `request.rotation ⊖ geometry.rotation` 交给 PDFium。
- **连带纠正**：D-124 / 评估 §12 里把“旋转页注释错位”归因为 PDFium 的 bug（L3）是**误判** ——
  那是同一个双重旋转问题（原始 PDF 也能复现是因为渲染参数错，不是 PDFium 错）。
  修正后旋转页的注释位置已能用像素断言覆盖。
- **影响**：`backend-pdfium/src/document.rs`（encrypted）、`backend-pdfium/src/raster.rs`（旋转）、
  `docs/current/{writeback,architecture}.md`（L3 改写、INV-12/13）、评估 §12。
- **验证**：
  * `intrinsic_rotation_is_applied_exactly_once`：200×400 `/Rotate 90` 页上画已知位置黑矩形，
    分别按“最终旋转 90”和“最终旋转 0”渲染，断言像素矩形落在 `Rotation::map_point` 给出的位置
    （修复前会落到 180° 的象限）；
  * `writeback::a_highlight_on_a_rotated_page_lands_where_the_rotation_says`：导出的高亮在
    `/Rotate 90` 页上位置正确（x 100..120、y 50..150）；
  * `unencrypted_documents_are_not_reported_as_encrypted`：普通 PDF 不得被判为加密且 `annotate == true`；
  * `check.sh` 全绿（321 tests）。
- **未做**：加密文档的**写回**仍拒绝（没有加密夹具，PDFium 保存需要 owner 密码）；
  旋转页在 Chrome/Acrobat 里的人工抽查仍建议做。
- **状态**：修复生效。

### D-127 修正：导出 job 里的路径没有绝对化（子进程解析到 /tmp）

- **问题（用户实测）**：`cargo run -p app -- scripts/replay/rotated_2p.pdf` 后点导出，报
  `源文件不存在：/tmp/scripts/replay/rotated_2p.pdf`。
- **根因**：`document_path` 是命令行给的**相对路径**；`export::job_json` 原样写进 job，
  而 job 落在系统临时目录，子进程按“相对 job 文件所在目录”解析 → `/tmp/scripts/...`。
- **修复**：父进程在 `export_with` 开头把 `pdf`/`output` 绝对化（`current_dir().join(path)`；
  MSRV 1.75 下不能用 `std::path::absolute`）。临时文件与 rename 也用绝对路径；
  子进程的相对路径解析保留作防御。
- **影响**：`app/src/export.rs`（绝对化 + 一个回归测试）；`docs/current/writeback.md` §5/§15。
- **验证**：`job_paths_are_absolutized_before_the_child_resolves_them` —— 用相对路径
  `Cargo.toml` 当源、假子进程把 job 回显到 stderr，断言 `"pdf"` 是 `cwd/Cargo.toml`
  且不含相对形式；`check.sh` 全绿（322 tests）。
- **状态**：修复生效。

### D-128 旋转契约写清；墨迹按页面裁剪 + 折角补点

- **背景（用户提问）**：
  1. `pdfcore` 是否假设"后端会按给定的（叠加后的）rotation 绘制"？
  2. 画墨迹跨到下一页时，墨迹会在那一页也出现；但保存后一条注释只属于一页 —— 绘制时是否也该裁掉？
  3. 直角拐弯处有一条裂缝。
- **决定**：
  1. **契约成立，且已经写清**：`RenderRequest.rotation` = "页面 `/Rotate` ⊕ 用户旋转"的**最终**旋转；
     `pdfcore` 的布局/网格/摆放都假设后端产出的位图方向**恰好**等于它。PDFium 会自己应用
     intrinsic `/Rotate`，所以适配层必须先减掉（INV-12）—— D-126 的 bug 就是适配层违约，
     不是 pdfcore 的假设有错。文档：`rendering.md` §4、`RenderRequest::rotation` 的注释。
  2. **墨迹绘制按页面矩形裁剪**：采样点仍按起笔页坐标系保存（几何忠实，PDF 阅读器本来就按页面裁剪），
     但**绘制**时用 `painter.with_clip_rect(页面屏幕矩形)` 裁剪 —— 编辑时看到的与导出后一致，
     跨页拖拽不会把墨迹画到相邻页的空白上。
  3. **折角补圆点**：egui 的折线是逐段四边形，直角/锐角拐角会漏一条缝；在内部顶点补一个
     半径 = 半线宽的圆（`paint_ink`）。导出 PDF 的 stroke join 是 miter，本身没有缝，
     所以这是**界面侧**的观感修正。
- **影响**：`app.rs` 新增 `paint_ink`（已保存墨迹 + 进行中的一笔共用）、`render.rs` 的字段文档、
  `rendering.md`、`app-ui.md`。
- **验证**：`check.sh` 全绿（322 tests）；两处是纯绘制行为，按 `agent.md` 交人工验收：
  跨页拖拽时下一页不出现墨迹、直角拐弯无裂缝、导出位置仍正确。
- **状态**：生效。

### D-129 修正导出的墨迹在拐角有裂缝：一条笔画一个子路径 + 圆角连接

- **问题（用户实测）**：导出的 PDF 里，直角折线在拐点外侧有一道缺口（界面叠加层没有）。
- **根因**：AP 内容流对**每一段**都写 `m … l` —— 每段成了独立子路径，各自带平头端帽；
  直角外侧两块端帽之间留下一个菱形缺口。界面用的是 egui 折线（miter 连接），所以只在导出文件里出现。
- **修复**：`appearance_content` 对墨迹只写**一个 `m`**（首点）+ 后续 `l`，并显式 `1 J 1 j`
  （圆头端帽、圆角连接）—— 任意夹角都不会漏。
- **影响**：`backend-pdfium/src/writeback.rs`；`docs/current/writeback.md` §9/§15。
- **验证**：
  * 单测 `ink_content_is_one_subpath_with_round_caps_and_joins`（恰好一个 `m`、含 `1 J 1 j`）；
  * 端到端 `a_thick_right_angle_has_no_gap_at_the_joint`：线宽 10 的直角墨迹导出后按 2× 渲染，
    断言拐角内侧页面 (97,762)（距顶点 3.6pt < 半径 5）是实色、顶点实心、笔画外仍是白底。
    **已确认这条测试在旧实现下失败、新实现下通过**（有牙）。
  * `check.sh` 全绿（323 tests）。
- **状态**：修复生效。

### D-130 S5c：原地写入（按 `/NM` 标记同步，保留 overlay）

- **决定**：
  1. **原地写入**取代"导出副本"作为第二个入口：工具栏「写入 PDF」→ 确认框 → 备份 → 原子替换；
  2. **不删 overlay**：读回 PDF 注释（S6）之前，删掉 `.annot.json` 就等于让标注在本程序里消失、
     不可编辑；因此 S5c 只做"把 PDF 同步成 overlay 的样子"，不做迁移；
  3. **幂等靠 `/NM` 标记**：每条写回的注释设 `/NM = "pd-fr:<id>"`。写回时逐页枚举：
     标记 id 仍在 overlay → 跳过；标记 id 已不在 → `FPDFPage_RemoveAnnot` 删除；
     非目标页上的标记 → 删除；**没有标记的注释（链接、其它程序加的）一律不碰**；
  4. 执行顺序：保存脏 sidecar（重开会从磁盘加载 overlay）→ 子进程写临时文件（同步）→
     冒烟校验 → **关闭会话**（释放 PDFium 文件句柄，Windows 才能替换）→
     备份 `<文件名>.bak-<纳秒>` → `fs::rename` → 重开并恢复 页/缩放/滚动。
- **理由**：没有读回能力时，"覆盖原文件 + 删 overlay"会让功能在界面上倒退；而"覆盖 + 保留 overlay"
  必须解决重复写回叠加的问题 —— 标记同步是最小且可验证的方案。标记同时写进导出副本，
  所以导出文件之后再做原地写入也能识别。
- **影响**：`backend-pdfium/src/writeback.rs`（`id` 字段、`/NM` 写入/读取、同步段、期望计数校验）、
  `app/src/{export,writeback,app}.rs`（`stage_in_place`、备份路径、确认框与执行流程）、
  `docs/current/{writeback,app-ui}.md`。
- **验证**：
  * `writing_twice_is_idempotent_and_drops_stale_marked_annotations`：第一次写 3 条；第二次报告
    **新增 0**、总数不变；删掉 overlay 里两条后再写 → PDF 里对应标记被删、链接保留；
  * `running_the_same_job_twice_does_not_duplicate_annotations`（job 层，含 `id` 解析）；
  * `check.sh` 全绿（325 tests）。
- **已知边界**：不更新已存在标记注释的几何/颜色（overlay 没有这类编辑）；写完后 undo/redo 只改 overlay，
  PDF 等下次写入同步；备份不自动清理；加密/签名文档仍拒绝；原地写入的"关闭→替换→重开"是 UI 层流程，
  只能人工验收。
- **状态**：有效（S5c 完成；S6 未开始）。

### D-131 修正：写回不更新已有注释的正文；重开后滚动被夹紧

- **问题（用户实测）**：
  1. 删掉/修改标注正文（`custom_texts` 的 `#annot:<id>` 条目）后再写回，其它阅读器里仍是**第一次写进去的旧文本**；
  2. 原地写入重开后，缩放正确但**滚动偏了一点**。
- **根因**：
  1. 同步段对"已存在的标记注释"只做了 `present.insert`（跳过添加），没有更新 `/Contents` ——
     第一次写入后 PDF 里的正文就冻结了。撤销/重做只改 overlay，所以两边看起来"不一致"。
  2. 重开后立刻下发 `GoToPage/SetZoom/SetScroll`；此时新会话**还没收到真实视口尺寸**，
     `SetScroll` 被按旧/默认视口夹紧，随后视口变大也不会回滚。
- **修复**：
  1. 同步段改成 **"先删掉所有 `/NM` 标记的注释，再按 overlay 重建"**（取代 D-130 的"跳过已存在"）：
     PDFium 的 `FPDFAnnot_SetColor` 对**已存在**的墨迹注释返回失败（新建可以，实测），
     `SetStringValue` 也不能保证；重建最简单，而且正文/颜色/几何统一以 overlay 为准。
     代价：其它程序对这些标记注释的修改会被覆盖（其它程序自己新增的注释不受影响）。
  2. `pending_view_restore`：重开后**延迟两帧**再下发恢复命令，确保视口尺寸已生效。
- **影响**：`backend-pdfium/src/writeback.rs`、`app/src/app.rs`（字段与 `apply_pending_view_restore`）、
  `docs/current/{writeback,app-ui}.md`。
- **验证**：
  * `rewriting_updates_contents_of_existing_marked_annotations`：写 "OLD" → 再写 "NEW"（同 id）→
    输出里含 `(NEW)`、不含 `(OLD)`；再把正文设为 `None` → 输出里不含 `(NEW)`；
  * 滚动恢复是 UI 行为，按 `agent.md` 交人工验收；
  * `check.sh` 全绿（326 tests）。
- **设计说明（回答"这是否符合设计"）**：
  * 正文条目（preview 列表里那条 `#annot:<id>`）与标注是**两条记录**：删除正文条目只删正文，不删标注；
    点击标注在正文缺失时显示"（正文已丢失）"，这是 S1 就定下的容错。
  * 撤销/重做只改**overlay**；PDF 是"下一次写回才同步"的副本。现在写回会同步正文，所以
    "撤销删除正文 → 写回"与"重做删除正文 → 写回"会得到不同的 PDF（符合 overlay 当前状态）。
  * 反向的"PDF 改了什么 → overlay 跟着变"要等 S6（读回），不在本阶段。
- **状态**：修复生效。

## 五、开放项（尚未决策，等实测/评审）

| # | 事项 | 触发条件 |
| --- | --- | --- |
| 1 | 纹理层改为**字节预算**（替代 128 条目）；上传/转换按字节限流（实测：release 下切波段那一帧 p99 ≈ 22ms，因为一帧连转 12 块） | **暂缓**（用户判断：会让状态机更复杂，而 p99 降到 6–8ms 对体验差别不大）；等到真有"切波段卡顿"反馈、或做 GPU 异步上传时再做 |
| 7 | ~~先做字节账观测~~（D-109）；~~纹理层按字节 + 分级保留~~（D-111）：软上限（当前视口所需的那一类永不淘汰，超出只告警），其余按"旧波段 → 非当前页底料"淘汰，类内 LRU；CPU 位图预算与 GPU 纹理预算分开定 | 用户已认可方向；等有空做（详见 §五 之后的讨论纪要） |
| 8 | ~~开场底料~~（已完成，D-110） | — |
| 2 | `app.rs` 拆分为多文件（绘制/侧栏/浮窗/截图/存档胶水） | 下一次在 `app.rs` 出现跨职责改动 |
| 3 | 统一诊断模块（级别 + 环形缓冲 + 一次手势轨迹）—— **部分已做**：D-103 有了 `Diagnostics` 去处（stderr / 内存缓冲），级别与环形缓冲仍未做 | 再出现"拖动时看不了日志" |
| 4 | 存档 v3：记录 `overscan` 快照与 `quality.source`；v4 保留未知字段 | 改 `DEFAULT_OVERSCAN` 或需要无损新旧互读 |
| 5 | CI 门禁（fmt/clippy/真实 PDF）与"固定 CI 机器上的时序门禁" —— 性能基线本身已落地（D-106） | 准备发版或接入 CI |
| 6 | 预览图"整页旧图打底 + 新块叠加"与"整页糊一下"的最终取舍 | 用户对糊的时长/程度有新的反馈 |
