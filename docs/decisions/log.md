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
