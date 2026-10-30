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

### D-132 Okular 保存兼容：墨迹外观改用 PDFium 原生 `AppendObject`（带 `/Resources`/`/Matrix`）

- **问题（用户实测）**：在 Okular 里修改我们写回的墨迹颜色，保存阶段 Okular 提示“文件有问题”
  （控制台只有 `kf.kio.workers.file: copy()` 那一行，看不出具体原因）。用户提供了一份用 Okular
  自己画的 freehand 注释（同属 `/Ink`）作参照。
- **对比结论**：Okular 自己的 ink 注释**没有 `/AP`**，只有 `/InkList` + `/NM(okular-{uuid})`；
  我们之前用 `FPDFAnnot_SetAP` 生成的 Form **只有 `/BBox`**，缺 `/Resources` 与 `/Matrix`。
  这是最可疑的兼容性缺口（PDF 规范里不是硬性必需，但 Poppler 的 PDF 写入器会挑剔）。
- **修复**：
  1. 墨迹外观改成 `FPDFPageObj_CreateNewPath` + `FPDFPath_LineTo` + `FPDFPageObj_SetStroke*`
     + `FPDFAnnot_AppendObject` —— PDFium 生成完整的 Form（`/BBox`、`/Matrix`、`/Resources`），
     并自动把路径对象挂到注释上；
  2. 同时 `FPDFAnnot_SetBorder` 写 `/BS` 笔宽（编辑器改颜色/线宽时需要）。
- **影响**：`backend-pdfium/src/writeback.rs`（FFI 表 + `append_ink_object`）、`docs/current/writeback.md` §6/§9/§15。
- **验证**：`a_thick_right_angle_has_no_gap_at_the_joint` 仍通过（原生路径是 miter 连接，直角无缝）；
  写出的 AP 字典经检查含 `Matrix[1 0 0 1 0 0]` 与 `Resources<</ExtGState…>>`。`check.sh` 全绿（326 tests）。
- **备注**：D-129 的 `1 J 1 j` 是旧 `SetAP` 方案；墨迹改为原生 AP 后由 PDFium 生成（miter），
  直角不再有缺口，锐角由 PDF 的 miter limit 决定。下划线/删除线仍是 `SetAP`。
- **状态**：已实现；需要用户在 Okular 里复测“改墨迹颜色 → 保存”。

### D-133 S6（读回 PDF 注释）评估完成，等待 4 个决策

- **结论**：可行。PDFium 的读注释 API 齐全；建议分 S6a（读回我们自己的标记注释）/ S6b（外部注释只读）/
  S6c（显式迁移）三步，不要一次做成"任意阅读器注释可编辑"。
- **评估中发现的三个前置问题**：
  1. 写回**没有写 `/CA`**（透明度）——我们自己的 0.35 高亮读回会变成 1.0，round-trip 不保真；
     做 S6a 前先补；
  2. 读注释要用裸 FFI，而主进程的 pdfium-render 全局锁不覆盖它；打开新文档时旧会话 worker 可能还在渲染，
     建议**子进程** `--readback`（与写回同构，密码走 stdin）；
  3. 外部 ink 可能**一条注释多条 path**，模型目前只有一条折线；S6b 需要 `strokes` 或多注释方案。
- **合并策略推荐**：overlay 优先（PDF 是投影，overlay 是编辑源）；overlay 不存在时才从 PDF 恢复
  `pd-fr:<id>` 注释；无标记的外部注释只读、不进 overlay 文件。
- **交付物**：[`evaluations/s6-readback.md`](evaluations/s6-readback.md)（250 行：能力核查、字段映射、
  合并规则、只读语义、迁移、容错、风险、切分、测试计划、取样方法）。
- **需要你确认**：合并策略 / 外部注释范围 / 迁移时机 / 读回进程模型（见评估 §12）。
- **状态**：评估完成，未实现。

### D-134 S6 决策：overlay 优先 / 外部只读几何 / 迁移显式做 / 读回放主进程

- **用户确认的四个选择**：
  1. 合并策略 **overlay 优先**：`.annot.json` 存在时它是编辑源；PDF 只用于校验/恢复；
     overlay 里没有的标记注释视为已删除（下次写回删掉）；
  2. 外部注释（无 `pd-fr:` 标记）到**只读几何**为止：不改、不删、不进 overlay 文件；
  3. 迁移**先不做**；之后加显式"以 PDF 为准重建 overlay"命令（先备份）；
  4. 读回放**主进程**，打开新文档时**先关旧会话**（而不是子进程）。
- **实现约束（D-135 落实）**：裸 FFI 不经过 pdfium-render 的全局锁，所以 `open()` 里
  必须在读回前 `self.session = None`（drop 会 join 渲染 worker）；打开期间不能有别的 PDFium 调用。

### D-135 S6a 实现：读回标记注释 + overlay 合并

- **实现**：`backend-pdfium/src/readback.rs`（GetSubtype/GetColor/GetBorder/GetAttachmentPoints/
  GetInkList*/GetStringValue/GetAP），`app.open` 里读回，`merge_readback` 按 D-134 合并；
  状态栏提示"已从 PDF 恢复 N 条标注"。
- **实现中踩到的四个坑**：
  1. 分类常量：Ink = **15**、StrikeOut = 12；一开始读回只收 9..12，墨迹全丢；
  2. `FPDFAnnot_GetColor` 对**带 AP 的注释必定失败**（文档明说）—— 改为解析 AP 内容流的
     `RG/rg/G/g` 与 `CA/ca`；`GetAP` 返回的是 **UTF-16LE**，不是字节流；
  3. 写回原先没写 `/CA`，高亮读回恒为 1.0 —— 写回改为把 `opacity` 写进 `/CA`，
     下划线/删除线的 AP 里也写 `CA/ca`；
  4. 读回是裸 FFI：app 的回放测试默认多线程并行，和 pdfium-render 渲染线程并发直接 **SIGSEGV**。
     给 `replay_scripts` 加了进程内互斥锁串行执行（生产路径由"先 drop 旧会话"保证串行）。
- **验证**：`backend-pdfium/tests/readback.rs` 4 条（四类 round-trip 含 id/正文/颜色/透明度、
  重写后稳定、旋转页坐标不变、无标记注释 id=None）；`replay_scripts` 新增
  `annotations_are_recovered_from_the_pdf_when_the_overlay_is_missing`（overlay 缺失时恢复 1 条）；
  `check.sh` 全绿（331 tests）。
- **未做**：S6b（外部注释只读几何 + 多笔迹）、S6c（显式迁移）；外部注释目前直接跳过。
- **状态**：S6a 完成，S6b/S6c 待做。

### D-136 页面位图默认不渲染 PDF 注释（`FPDF_ANNOT` 关闭）

- **起因（用户提问）**：PDFium 渲染页面时是否一定会渲染注释？
  不是：`FPDF_RenderPageBitmap` 由 `FPDF_ANNOT` flag 控制；`pdfium-render` 的 `PdfRenderConfig`
  **默认打开**（`do_set_flag_render_annotations = true`），而我们之前没有关。
- **影响**：写回后的 PDF 里已有注释，位图又烤一遍，与 overlay 双份绘制；删除 overlay 注释后
  界面上不会消失（位图里还留着）。S6b 的外部注释也会被烤进来，和"只读叠层"的设计分叉。
- **决定**：`PdfiumBackend::new()` **默认不渲染注释**（`render_annotations(false)`），
  应用只画 overlay；验证写回外观的测试显式 `with_annotations(true)`。
  这样"页面位图 = 纯页面内容"成为后端不变量（INV-14）。
- **影响文件**：`backend-pdfium/src/backend.rs`、`raster.rs`、`tests/writeback.rs`、
  `docs/current/rendering.md`、`architecture.md`。
- **验证**：`annotations_are_not_baked_into_the_bitmap_by_default` —— 同一份写回 PDF：
  默认渲染红色像素 0，`with_annotations(true)` 后 >100。`check.sh` 全绿（332 tests）。
- **状态**：生效。注意：**外部注释在 S6b 之前完全不可见**（这是有意的，避免烤进不可交互的位图）。

### D-137 补回缺失正文（A 在、J 不在）；坏 overlay 当作不存在

- **问题（用户问文件组合时发现）**：只保留 `<pdf>.annot.json`、删掉 `<pdf>.json` 时，
  overlay 的 `body_title` 还在但正文条目没了 → 点击显示“正文已丢失”；更糟的是**写入 PDF 会拿不到正文**，
  把 PDF 里原有的 `/Contents` 清空。
- **修复**：
  1. 打开时即使 overlay 存在，也把 PDF 读回结果按 id 建表；对“有 `body_title`、正文条目缺失”的注释，
     从 PDF 的 `/Contents` 补建正文条目（`data_dirty`），状态栏提示“补回 N 条正文”；
  2. 新建正文条目时置 `data_dirty`（否则保存只写 `.annot.json`，正文又丢）；
  3. overlay 文件损坏时按“不存在”处理（仍提示警告），允许从 PDF 恢复，而不是显示空图层。
- **影响**：`app/src/app.rs`（`merge_readback`、open 的 overlay 加载）；`docs/current/readback.md` §3/§3.1。
- **验证**：现有 332 tests 全绿；人工场景：写回 → 删 `.json` → 重开（正文补回、提示）→ 再写入 PDF（正文仍在）。
- **状态**：生效。

### D-138 修正：没有正文的注释无法命中/删除

- **问题（用户实测）**：`<pdf>.json` 不在、注释没有正文（例如从 PDF 读回、`/Contents` 为空）时，
  点击显示“（正文已丢失）”，且 **hover+Delete 与右键删除都无效**。
- **根因**：叠加层的命中判定写成了“只有 `body_title.is_some()` 才设置 `hovered_annotation`”
  （墨迹与文本类都是），而删除/右键菜单完全依赖 `hovered_annotation`。S6a 读回的无正文注释
  `body_title = None`，于是永远命中不了。
- **修复**：
  1. 命中只看几何（墨迹折线距离 / 文本 quad 矩形）；`body_title` 只用于高亮对应浮窗；
  2. 占位文案区分“（没有正文）”（`body_title == None`）与“（正文已丢失）”（有引用但条目丢失）。
- **影响**：`app/src/app.rs` 的叠加层悬停段与 `open_annotation_note`；`docs/current/app-ui.md`。
- **验证**：332 tests 全绿；人工场景：写回一条无正文注释 → 删 A/J → 重开（注释可 hover/删除）。
- **状态**：修复生效。

### D-139 S6c：从 PDF 重建标注；空标注也能写回/导出

- **问题（用户实测）**：界面上把标注删光后，「写入 PDF」「导出标注 PDF」都变灰 ——
  条件是“内存里至少一条标注”。于是“PDF 里写过、现在全删了”这种状态无法写回（PDF 里的注释删不掉）。
- **修复**：
  1. `PdfReaderApp.pdf_marked_annotations`：打开时记录 PDF 里带 `pd-fr:` 标记的注释数；
     按钮条件改为 `有标注 || pdf_marked > 0`，删光之后仍可写回/导出“空集合”；
  2. `writeback` 允许 `pages: []`（`validate` 不再拒绝）——同步段会删掉所有标记注释、保留链接；
     `export`/`stage_in_place` 也允许空 items；
  3. **S6c「从 PDF 重建」**：确认框 → 备份 A/J → 删 A、只删 J 的 `#annot:` 正文 →
     重开（S6a 从 PDF 导入）→ 自动落盘；清空撤销历史、恢复视图。
- **影响**：`backend-pdfium/src/writeback.rs`（validate）、`app/src/export.rs`（空集合）、
  `app/src/app.rs`（计数、按钮、重建流程/确认框）、`docs/current/{app-ui,writeback,readback}.md`。
- **验证**：
  * `empty_spec_removes_all_marked_annotations_but_keeps_links`：写 1 条 → 空 pages 写回 →
    只剩链接；再跑稳定；
  * `empty_export_is_allowed_and_reaches_the_child`：空集合不再提前拒绝；
  * `check.sh` 全绿（333 tests）；
  * 重建流程是 UI 流程，交人工验收（备份文件、正文恢复、未写回标注丢失）。
- **状态**：S6c 完成；S6b 等外部样本。

### D-140 S6b：外部注释只读层（真实阅读器样本）

- **样本**（用户采集，复制到 `crates/backend-pdfium/tests/fixtures/s6b/`，来源见该目录 README）：
  Okular / Evince / Firefox / Adobe / Foxit 对同一份 `rotated_2p` 的注释；Adobe 的注释在 **ObjStm** 里。
  观察：
  * 四类注释齐全（Evince 没有墨迹，Firefox 只有高亮+墨迹）；
  * 墨迹都是**单 path**（Okular 56 点、Firefox 128、Adobe 181、Foxit 104）；
  * `/BS` `/W` 有的写 1.0、有的 2.0、有的没有（回退 2.0 合理）；
  * `/C` 有的缺（Evince 全黄）、有的带自定义颜色；`/CA` 有的 0.35/0.40；
  * 所有外部注释都**没有 `pd-fr:` 标记**；`without_content` 版本没有 `/Contents`。
- **决定**：
  1. 外部注释放**独立只读层** `foreign_annotations`，不进 `AnnotationStore`、不写 overlay、不进撤销栈；
  2. 读回时取 `/InkList` 的**全部 path**；模型仍只有一条折线，因此**多条 path 拆成多条只读注释**
     （样本全是单 path；等"外部注释可编辑/分组"成为需求时再引入 `strokes` 模型）；
  3. 只读层只绘制几何，不提供删除/正文浮窗；`显示标注` 开关同时控制它；
  4. 不把它计入 `pdf_marked_annotations`（不影响「写入 PDF/重建」的激活条件）。
- **影响**：`readback.rs`（`ink_strokes` 全 path、`ReadAnnotation` 扩展）、`app.rs`（只读层/绘制/状态栏）、
  replay 报告新增 `foreign_annotations`、`docs/current/readback.md`、fixtures + `tests/s6b_samples.rs`。
- **验证**：6 条样本测试（类型/颜色/线宽/正文/坐标/ObjStm）+ replay
  `foreign_annotations_are_shown_read_only`（Okular 5 条、overlay 仍为 0、不写 A）；
  `check.sh` 全绿（340 tests）。
- **已知限制**：外部注释没有正文浮窗、不能编辑；多 path 会显示成多条只读项；
  Chromium / Xournal++ 的批注 Okular 也识别不了，本程序读不到（用户允许忽略）。
- **状态**：S6b（只读几何）完成；跨阅读器兼容性（Okular 改我们注释后保存报错）仍待处理。

### D-141 跨阅读器兼容：高亮透明度统一；原地写入改增量保存

- **问题（用户实测）**：
  1. 外部高亮在我们的阅读器里**不透明**，在 Okular 里透明（我们自己的高亮在 Okular 里透明）；
  2. 在 `adobe.pdf` 上插入我们的注释、写入、删除再写入后，Okular **无法编辑/保存**（提示文件有问题）。
- **样本结论**（`tests/fixtures/s6b/`）：
  * Okular/Evince 的文字注释**没有 `/AP`**；Firefox 有 AP；Foxit 的文件有的有、有的没有；
  * 高亮大多**没有 `/CA`**（或 `=1`），但 Okular 按“高亮默认半透明”绘制；Adobe 显式写了 `0.40`；
  * Adobe 的注释在**对象流（ObjStm）**里，PDFium 全量重写会把它展开成普通对象。
- **修复**：
  1. 透明度统一：读 `/CA`，**缺失或 ≥1 时高亮按 0.35、其它按 1.0**；显式 `<1` 照用。
     这样外部高亮与我们自己的高亮观感一致（D-136 的位图不烤注释策略不变）。
  2. 原地写入改用 `write_incremental`（`FPDF_INCREMENTAL`，job 字段 `"incremental": true`）：
     追加改动，保留 Adobe/Okular 的对象流与外部注释原结构；导出副本仍用全量重写。
     `write()` 保持全量；`write_incremental()` 只给原地流程用。
- **验证**：
  * `incremental_save_keeps_external_annotations_and_object_streams`：对 Adobe 样本空写回，
    输出前缀保留原文件、`/ObjStm` 仍在、注释数（含 Popup）不变；
  * `incremental_job_preserves_external_annotations`（job 层）；
  * 样本测试补了透明度策略断言（Okular/Evince/Firefox 0.35、Adobe 显式 0.40）；
  * `check.sh` 全绿（342 tests）。
- **待用户验证**：`scripts/replay/sample/compat/` 下的 5 个实验文件（见 README）在 Okular 里
  编辑/保存的结果——用于确认“增量保存”够不够，还是必须进一步改 AP 格式（例如去掉 `/AP`）。
- **状态**：代码已实现；跨阅读器兼容性等待 Okular 实测。

### D-142 跨阅读器兼容（二）：把内联注释外置成独立对象

- **Okular 实测结果**（用户）：
  * `ours_full` / `ours_incremental`：打开正常、可编辑、另存为**无报错**，但**修改不生效**；
    导出的 `*_annot_changed.pdf` / `*_color_changed.pdf` 大小与源文件**完全相同**（等于原样复制）；
  * `adobe_incremental`：修改生效（另存为文件变大，附录里能看到 changed）；
  * `adobe_full`：另存为报错；
  * `ours_no_ap`：打开就报错（说明“去掉 AP”不是正确方向）。
- **根因**：PDFium 新建的注释是**直接内联**在 `/Annots [ <<…>> ]` 里的，没有对象号；
  Poppler/Okular 的注释对象依赖 `Ref`，保存时对直接字典的修改会丢失。
  Adobe/Okular 样本的注释都是独立对象（`35 0 R`）；`adobe_incremental` 能工作正因为它没被动过。
- **修复**：新增 `backend-pdfium/src/pdf_repair.rs`，在 PDFium 保存后追加一段**经典增量更新**：
  把页面 `/Annots` 里的内联字典复制成新对象、页面对象重写为引用数组、
  追加 `xref`（每个对象一个子段）与 `trailer <</Size … /Root … /Prev <原 startxref>>>`。
  全量/增量保存都会走这一步；没有内联注释时返回 `None`（不改字节）。
- **验证**：
  * `annotations_are_written_as_indirect_objects`：`/Annots` 里不再有 `<<`，读回 id/正文正常；
  * `full_save_externalizes_external_annotations_too`：Adobe 全量保存后 11 个注释（含 Popup）
    全部是引用，读回 5 条；
  * 既有 12 条 writeback 测试（渲染/计数/增量）全绿；
  * `check.sh` 全绿（待跑：346 tests）。
- **待用户验证**：重新生成的 `scripts/replay/sample/compat/ours_full.pdf` / `ours_incremental.pdf`
  （现在注释已外置）在 Okular 里编辑高亮/颜色后另存，修改应当生效。
- **状态**：实现完成，等待 Okular 实测。

### D-143 导出副本也改增量保存；`adobe_full` 的报错不再影响产品路径

- **用户实测**：`ours_full` / `ours_incremental`（注释已外置）在 Okular 里编辑后另存，修改终于生效；
  但 `adobe_full`（PDFium 全量重写 Adobe 的 PDF）另存仍报错；`adobe_incremental` 正常。
- **结论**：全量重写会展开 Adobe 的对象流、重排所有对象，Poppler 仍会报错；增量保存不动原对象，
  才是跨阅读器安全的写回方式。注释内联问题（D-142）修好后，产品路径没有理由再用全量重写。
- **决定**：`export`（导出副本）也改用 `incremental: true`；`write()` 的全量模式只留给测试/工具。
  导出件现在是“原文件 + 增量段”，因此体积会比源文件大。
- **影响**：`app/src/export.rs`（export 的 job 也带 incremental）、`docs/current/writeback.md`、
  job 测试（断言导出 job 里 `"incremental": true`）。
- **验证**：`check.sh` 全绿（346 tests）。
- **状态**：实现完成；`adobe_full.pdf` 只是诊断用的“全量重写”样本，产品不再生成这种文件。

### D-144 差分增量写入：只写被修改的注释；内容修改复用对象号

- **用户问题**：增量写入是不是不管有没有变化都重写一遍？能不能只针对被修改的注释增量写入？
  以及“内容修改”和“注释本身修改”要分开。
- **现状（修复前）**：每次写入会把所有带 `pd-fr:` 标记的注释删引用、按 overlay 全部重建，
  所以未变化的注释也会换成新对象，文件增长偏大。
- **修复**：写回前先用 `readback` 读回源 PDF 的标记注释，做差分：
  1. 正文（`/Contents`）变了、几何/颜色/透明度没变 → `FPDFAnnot_SetStringValue` **原地更新**，
     对象号不变，页面对象不动（像 Okular 的保存：只追加一个对象版本）；
  2. 完全没变 → 同步逻辑不写（但 PDFium 保存器仍会追加整份对象，见下）；
  3. 几何/颜色/透明度变了 → 删旧引用、新建对象（PDFium 对已存在墨迹 `SetColor` 失败，
     也没有可靠的改几何 API）；
  4. 新增/删除 → 只改页面 `/Annots` 引用（新增才建对象）；
  5. 外部注释（无标记）不碰。
- **影响**：`backend-pdfium/src/writeback.rs`（差分 + `same_kind`/`update_contents`）、
  `docs/current/writeback.md`；`WritebackReport.annotations` 语义变为“总数”。
- **验证**：`content_only_edit_reuses_the_same_annotation_object`：
  第一次写 `[9 0 R 11 0 R]`；只改正文后再写，引用不变（对象 11 被复用）、读回正文为 NEW；
  改几何后再写，引用数组变化（换新对象）。`check.sh` 全绿（347 tests）。
- **说明**：当前 UI 只能改正文/增删（没有改颜色/几何的入口），所以实际写回走的是
  “内容原地更新”路径；将来加入颜色/几何编辑时走“替换对象”路径。
- **实测限制**：PDFium 的 `FPDF_INCREMENTAL` 仍会整份序列化追加（无变化时 2979→4351 字节）。
  差分只减少“重建未变对象”的工作，文件增长仍主要来自 PDFium 保存器；要真正对象级增量，
  需自己写增量更新（`pdf_repair` 已具备解析基础，待评估）。
- **状态**：实现完成。

### D-145 S7 方向评估：正文归注释 → 外部正文 → 保存器 → 去 overlay（待确认）

- **用户提出的目标顺序**：先读/改外部注释正文 → 去 `.annot.json` 与 preview 中间层 → 改保存器。
- **评估结论**：方向合理，但建议**调整顺序**：
  1. **正文归注释（G2）先做**：`Annotation` 自带 `body`，不再用 `#annot:` preview 承载；
     否则外部注释正文只能临时再造 preview，下一步又推翻；
  2. 外部注释正文（G1）复用 `body`，只读几何、允许改内容；
  3. 对象级保存器（G3）是“只追加变化对象”的前提；
  4. 去 `.annot.json`（G4）最后做：**未保存的新注释需要载体**，保存器/自动写回是前置条件。
- **关键风险**：去 overlay 后未保存数据无处可放（必须显式保存/自动写回/journal）；
  旧 `.json` 里的 `#annot:` 正文迁移；外部注释正文写回；ObjStm 对象号。
- **交付物**：[`evaluations/s7-body-overlay.md`](evaluations/s7-body-overlay.md)（现状耦合、四目标、顺序分析、
  分阶段建议、风险、待决策）。
- **待确认**：顺序 / 去 overlay 后保存策略 / 外部注释编辑边界 / `.annot.json` 最终形态。
- **状态**：评估完成，未改代码。

### D-146 S7 决策修订：浮动对象抽象、显式保存/丢弃、外部正文+删除、P4 降级

- **用户确认/补充**：
  1. 接受调整顺序（正文归注释 → 外部正文 → … → 保存器）；
  2. 注释正文应是一种**独立的浮动对象**（类似文字 preview，但不属于 CustomPreview、不进侧栏、
     不写 `.json`），由注释触发；
  3. 未写进 PDF 的新注释：关闭/切换文件时**默认丢弃**；显式保存；不做自动写回；
     不做 journal（等以后统一脏位提示）；
  4. 外部注释目前只需**读正文、改正文、删除**；几何/颜色仍只读；
  5. `.annot.json` 迁移后删除。
- **对分析的影响**：
  * `Float` 来源改为枚举 `FloatOwner::{Annotation, CustomPreview,…}`；正文编辑写入 `Annotation.body`
    并进撤销栈；
  * 外部注释删除需要身份匹配（优先 `/NM`，否则几何+正文指纹；匹配失败拒绝），并删 `/Popup` 子注释；
  * 外部正文/删除只存内存，由「写入 PDF」落盘，不写入 A/J；
  * 显式保存 + 丢弃未保存后，**去 `.annot.json`（P3）可以早于对象级保存器（P4）**：
    P3 用 PDFium 增量保存（会变大但正确），P4 再优化“只追加变化对象”。
- **修订顺序**：**P1 正文归注释 → P2 外部正文/删除 → P3 去 A → P4 对象级保存器**。
- **交付物**：[`evaluations/s7-body-overlay.md`](evaluations/s7-body-overlay.md) §9–§12。
- **状态**：分析修订完成；下一步写 P1/P2 的可执行计划，暂不改代码。

### D-147 S7 第二轮修订：注释一视同仁、用 xref 对象号做身份、关闭 A v3 通道

- **用户意见**：
  1. 身份匹配（几何/正文指纹）可以删掉；将来所有来源的注释一视同仁，用 **xref 里的对象标识**
     区分（对象号 + generation）；
  2. Popup 子注释要一起删除（正文 `/Contents` 是直接挂在注释上的，Popup 是独立子对象）；
  3. 本阶段只在自己的机器跑，不要求 A/J 的完整兼容；P1 升 v3 并确认无回归后，
     适时**关闭 v2→v3 的升级通道**。
- **评估结论**：
  * 一视同仁 + `(num, gen)` 是更干净的模型，`pd-fr:` 标记将退出同步逻辑；
  * 但 xref 身份依赖对象层解析：**内联字典没有对象号**（需要外置或下标兜底）、
    **ObjStm**（Adobe）需要解压对象流才能拿到注释字典、别的工具全量重写会重排对象号
    （只在当前文件版本内使用，写前重读，找不到就拒绝）；
  * P2 的“对象号定位”和 P4 的“对象级保存”共享 `pdf_repair` 的解析工作；
  * A v3 迁移后固定 v3、删除旧迁移路径（P1.5）。
- **修订阶段**：P1 正文归注释 → P1.5 关闭 v3 通道 → P2 外部正文/删除（xref 身份）→
  P3 去 A → P4 对象级保存器。
- **交付物**：[`evaluations/s7-body-overlay.md`](evaluations/s7-body-overlay.md) §13–§14。
- **状态**：分析第二轮修订完成；下一步写 P1/P2 可执行计划。

### D-148 S7 实施计划（P1/P2）完成，待确认

- **P1 正文归注释**：`Annotation.body`、A v3（几何+正文）、v2→v3 迁移（从 `.json` 的 `#annot:` 搬）、
  `FloatOwner::{Annotation, Custom}`、注释浮窗正文编辑 + `AppEdit::SetBody`、预览列表移除注释正文。
- **P1.5**：确认无回归后关闭 v2→v3 通道，固定 v3。
- **P2 外部正文/删除**：四种注释一视同仁；`pdf_ref: Option<(num,gen)>`；读 `/Contents`、改正文、删除
  （含 Popup 子注释）；对象号缺失/重复/内联 → 拒绝；外部修改只存内存，由「写入 PDF」落盘。
- **P2 技术风险**：xref table/stream、ObjStm、内联字典没有对象号；先做 `pdf_index` spike
  （5 个真实样本），失败则 P2 只支持直接对象并明确提示。
- **计划文档**：[`evaluations/s7-p1-p2-plan.md`](evaluations/s7-p1-p2-plan.md)（数据模型、v3 迁移、浮窗、
  撤销、P2 身份/写回协议/Popup、测试、提交顺序、风险、待确认）。
- **状态**：计划待确认，未改代码。

### D-149 P1 实现：正文归注释、浮动对象、A v3 迁移

- **模型**：`Annotation.body: Option<String>`（删除 `body_title`）；正文直接属于注释。
- **浮动对象**：`FloatOwner::{Annotation, Custom, None}`；注释正文浮窗与自定义 preview 分开，
  点击注释打开/置顶自己的浮窗；正文编辑走 `AppEdit::SetBody`（可撤销），删除注释时关闭其浮窗。
- **持久化**：`ANNOT_VERSION = 3`，A 写入 `body`；v2 的 `body`（`#annot:<id>` 标题）通过
  `SavedOverlay::migrate_v2_bodies` 从 `.json` 的 custom_texts 搬到注释，然后统一删除
  `#annot:` preview 条目；v2 仍可读（P1.5 再关闭通道）。
- **加载顺序**：先载 `.json`（自定义 preview），再载 A 并迁移（v2 正文需要 J 的文本）。
- **导出/读回/写回**：正文都从 `Annotation.body` 取；S6a 导入/补回正文不再创建 preview。
- **验证**：`annotation_body_tests`（SetBody 可撤销、删除/撤销保留正文、不创建 preview）；
  `v2_body_titles_migrate_into_body`；replay `v2_overlay_body_migrates_into_annotation_body`
  （A 升 v3、J 里 `#annot` 消失）；现有 tests 全绿（`annotate_zoom`/`ink`/`ink_many`/`undo_redo`
  结构基线因不再有正文 preview 而重录）。
- **未做**：P1.5（关闭 v2→v3 通道）、P2；外部注释仍只读几何。
- **状态**：P1 实现完成，待人工验真窗口（打开/双击编辑/撤销/保存/重开）。

### D-150 P1.5：关闭 `.annot.json` v2→v3 迁移通道

- P1 已验证（正文归注释、浮窗编辑、撤销、保存/重开），按计划关闭旧通道：
  * `SavedAnnotation` 只保留 `body`，删除 `body_title`；
  * `SavedOverlay::from_json` 遇到 `version < 3` 直接拒绝（提示“v2→v3 迁移通道已关闭”）；
  * 删除 `migrate_v2_bodies`、`annotation_body_title` 与相关迁移/清理逻辑；
  * 打开时仅在 A 成功加载（v3）后清理残留 `#annot:` preview，避免 v2/坏 A 时误删旧正文。
- **影响**：旧 v2 `.annot.json` 不再可直接打开；用户机器上的文件已在 P1 保存为 v3。
- **验证**：persist 的 v1/v2 用例改为“拒绝”；`check.sh` 全绿（350 tests）。
- **状态**：P1.5 完成；下一步 P2 的 xref/ObjStm spike。

### D-150 P1.5：关闭 `.annot.json` v2 通道；结构基线排除字体图集

- **P1.5**：P1 验证后关闭 v2→v3：
  * `SavedAnnotation` 只保留 `body`；`SavedOverlay::from_json` 拒绝 `version < 3`；
  * 删除 `migrate_v2_bodies`、`annotation_body_title`；打开时只在 A(v3) 成功加载后清理 `#annot:` 残留；
  * persist 的 v1/v2 用例改为“拒绝”。
- **顺带修基线抖动**：P1 去掉正文 preview 后，`annotate_zoom` 等基线在 0/120 字节之间翻转。
  根因是 `upload_bytes` 把 **egui 字体图集**（`TextureId::Managed(0)`）也算进去了；
  UI 文字/字形缓存变化与页面渲染无关。现在排除字体图集，并重录全部结构基线。
- **影响**：`app/src/replay.rs`（上传统计）、`perf/baseline/*`、`testing.md`。
- **验证**：`check.sh` 全绿（350 tests）；重复跑基线稳定。
- **状态**：P1.5 完成；下一步 P2 的 xref/ObjStm 对象号 spike。

### D-151 P2 spike：页面树与 /Annots 对象号定位跑通

- **目标**：不依赖 PDFium，从 PDF 字节得到“页序 + 每页 /Annots 的 `(num,gen)` 引用”，
  为 P2 的“一视同仁 + xref 身份”做准备。
- **实现**（`backend-pdfium/src/pdf_index.rs`）：
  * 扫直接对象、可 FlateDecode 的 **ObjStm**；
  * 从 trailer `/Root` → `/Pages` → `/Kids` 走页面树（支持 `/Annots` / `/Kids` 是独立数组对象的情况）；
  * `/Annots` 返回 `Some((num,gen))` 或 `None`（内联）；
  * 新增 `flate2` 依赖。
- **样本结果**（引用数含 Popup）：
  * Okular [5,1]；Evince [7,2]；Firefox [5,2]；Adobe（ObjStm）[7,4]；Foxit [9,2]；
  * 所有样本都是独立对象，没有内联注释。
- **意义**：Adobe 的 ObjStm 也能定位；对象号可用于 P2 的差分（改正文/删除）。
  注意：`/Annots` 里包含 Popup 子对象，P2 需要按 `/Subtype` 过滤出四种注释，不能把索引直接当注释顺序。
- **验证**：`pdf_index::tests::page_annotation_refs_for_real_samples`（6 个 fixture，精确引用数）；
  `check.sh` 全绿（351 tests）。
- **状态**：spike 完成；下一步 P2：`Annotation.pdf_ref`、统一读入、正文编辑/删除（含 Popup）与 job 协议。

### D-152 P2 实现：外部注释正文/删除按对象号落盘

- **模型**：`Annotation.pdf_ref: Option<PdfObjectRef>`（运行时；不写 A）；四种注释一视同仁。
- **读入**：外部注释不再有只读层，直接进入 `annotations`；`external_ids` 标记“不写 A”。
  `pdf_index::PageAnnot` 增加 subtype/Popup/hidden；按页与 PDFium 读回顺序配对，
  只取 Highlight/Underline/StrikeOut/Ink 且非隐藏。
- **编辑/删除**：外部正文浮窗复用 `SetBody`；删除走 `DeleteAnnotation`。
  外部修改置 `pdf_dirty`，不置 `overlay_dirty`；`保存` 不写外部注释，提示用「写入 PDF」。
- **写回**：`WritebackSpec` 增加 `updates` / `deletes`；子进程先按 `pdf_index` 把对象号
  映射到页/下标，再改 `/Contents` 或删除（Popup 子对象一并删）；找不到对象号即拒绝。
- **导出/原地写入**：`ExportJob` 增加 `updates`/`deletes`；app 由
  `opened_pdf_refs - 当前 refs` 得删除集合，由外部注释当前 body vs 打开时 body 得更新集合。
- **内联注释**：仍可读、但无对象号，编辑/删除会提示“暂不支持”；不误写。
- **验证**：
  * backend：Okular 按对象号改正文/删除；Foxit 带 Popup 父+子一起删；
  * app：`pdf_ref_changes`（外部正文→updates、删除→deletes、不写 A）、job parser；
  * replay：Okular 5 条外部注释进入主集合（`foreign_annotations` 计数保留为外部数）；
  * `pdf_index` 6 样本精确断言；`check.sh` 全绿（356 tests）。
- **未做**：P3（去 A）、P4（对象级保存器）；多 path ink 的外部删除仍按对象整体处理。
- **状态**：P2 核心完成；待真实文件手动验收（改正文/删除→写入 PDF→重开/Okular 看）。

### D-153 修复：Okular 注释改正文后本程序读回旧值

- **现象**：在阅读器里改 Okular 样本的 highlight 正文并写入 PDF，Okular 看到新值，
  本程序重开仍显示旧值；Foxit 的删除线改正文两边都正常。
- **根因**：该注释在源文件里是 `11 1 R`（generation 1）。PDFium 增量保存把它写成
  `11 0` 新对象并改页引用，但重新打开时仍解析到旧的 `11 1`；之前增量写坏的文件里
  同名 `(num,gen)` 有多版时也会取到旧版。Foxit 的对象没有这个问题。
- **修复**：`write_incremental()` 只在目标对象 `generation > 0` 时自动改用**全量保存**
  （PDFium 重写后统一 gen0，读回一致）；其它情况仍走增量，保留 Adobe ObjStm 和
  evince/firefox 的多版历史。
- **收窄原因**：最初还按“同一 (num,gen) 多版”触发全量，但 evince/firefox 本身就有多版，
  全量重写会让 Okular 保存时报警；Adobe 的 `adobe_inc_probe`（保留 ObjStm）在 Okular
  保存正常，证明增量才是正确路径。旧版增量写坏的 gen0 多版文件需从原始样本重新生成。
- **验证**：
  * Okular `11 1` 改正文，增量入口实际走全量 → 读回新正文；
  * 全量保存后的文件再增量改 gen0 → 读回新正文（`full_save_then_incremental_update_reads_back`）；
  * 用户 `okular.pdf` 这类多版 gen0 文件 → 检测后全量，读回修复；
  * Foxit 删除线/Popup 测试保持通过。
- **说明**：`.annot.json` 只在「保存」写 overlay 时生成；「写入 PDF」不会创建它。
  外部注释不进 A，所以只改外部注释后没有 `.annot.json` 是预期行为。
- **状态**：P2 修复完成，待用户用 `okular.pdf` 再验一次改正文/写入/重开。

### D-154 P3：`.annot.json` 退休，PDF 成为唯一来源

- **保存语义**：
  * 注释新增/改正文/删除 → `pdf_dirty`；「保存」/Ctrl+S 直接原地写 PDF（子进程 + 备份），
    不再写 `.annot.json`；
  * 自定义数据（preview/链接/引用点）仍写 `.json`；先写 `.json` 再写 PDF；
  * 未保存的新注释在关闭/切换文件时丢弃（没有 A 可恢复）；
  * `写入 PDF` 仍可单独触发同一路径。
- **旧 A 迁移**：打开时若存在 `.annot.json`，读出作为迁移输入并置 `pdf_dirty`；成功写 PDF 后
  把它改名为 `.annot.json.bak-<纳秒>`（保留一次备份），不再生成/更新。
- **写回组成**：`pages` 放**所有自有注释**（`external_ids` 之外；新注释和已写回的标记注释都算），
  让 marker 差分能保留上一次的注释；外部注释按对象号 `updates`/`deletes`。
  早先只放 `pdf_ref=None` 会把第一次已写回的标记注释当成“已删除”清掉（连续两次保存丢第一条）。
- **测试**：
  * `annot_roundtrip` 改为断言“保存后无 A、PDF 有两条”；
  * 新增“未保存新注释重开即丢弃”；
  * 新增“旧 v3 A 迁移进 PDF、A 退休且保留 `.bak-`”；
  * 回放保存通过注入真实 app 子进程（`PDF_READER_WRITEBACK_EXE`，仅测试用）。
- **验证**：`check.sh` 全绿（约 360 tests；以实际输出为准）。
- **状态**：P3 核心完成；P4（对象级保存器）与脏位 UI 待做。

### D-155 P3 修正：撤销/重做跨写回、稳定对象 id、分离脏位

- **问题 1**：写回重开后，撤销栈里的注释快照仍带旧 `pdf_ref`（或 `None`）。再次写回时
  同一个注释既出现在 `pages`（marker 差分）又被算进 `deletes`，于是被删掉。
- **修复**：
  * `opened_id_by_ref`：打开时记录 PDF 对象号 → 标记 id；删除集合按 id 判断
    “对象是否仍在当前集合”，不再只看当前 `pdf_ref`；
  * 写回重开保留撤销栈，`reconcile_history_refs()` 把 Add/Delete 快照重新绑到新对象号；
  * 外部注释用 `pdf_ref_ids` 复用同一对象号对应的本地 id（SetBody 历史跨重开仍有效）；
  * 新开文档（非写回重开）清空撤销栈与对象号映射（符合用户模型）。
- **问题 2**：注释编辑让“保存自定义数据”星号亮起，且点击走 `save_all`。
  * 注释编辑只置 `pdf_dirty`；自定义数据只置 `data_dirty`；
  * 顶部按钮只显示/保存自定义数据；`写入 PDF`/Ctrl+S 负责注释；
  * `save_custom_data` 无脏位时直接提示“无自定义数据改动”，不再报“没有打开的文档”。
- **脏位模型**：用**状态指纹**代替简单布尔：
  * `pdf_fingerprint`（注释 id/页/几何/颜色/正文，按 id 排序，不含 pdf_ref）；
  * `data_fingerprint`（preview 文本/题目/几何 + 被引用点）；
  * 打开/保存记录 clean 指纹；每次编辑、撤销、重做后重算；
    撤销回到保存点会自然清零；两个脏位完全分离。
  * 这样与用户提出的“栈位置 + 操作类别”等价，但不受撤销分支/重排影响；
    `SetBody` 已是单条历史命令。
- **测试**：`consecutive_saves_keep_earlier_annotations`、
  `undoing_and_redoing_after_save_then_new_annotation_keeps_both`、
  `undo_back_to_saved_state_clears_the_right_dirty_flag`；
  `check.sh` 全绿（363 tests）。
- **状态**：P3 修正完成；P4（对象级保存器）待做。

### D-156 P4 计划：对象级增量保存器

- **问题**：PDFium 增量保存仍追加大量对象；gen>0 需要全量兜底；全量会展开 ObjStm。
- **方案**：PDFium 只负责新增/AP；`updates`/`deletes` 由新的 `pdf_patch` 在保存后处理：
  * 正文更新 = 复制注释 dict + 新对象号 + 页 `/Annots` 换引用；
  * 删除 = 复制页对象 + 去掉 ref（含 Popup）；
  * 追加经典 xref（`/Prev`）；原对象/ObjStm 不动。
- **阶段**：P4a 页对象/正文更新 spike；P4b 集成/删除；P4c 去全量兜底与回归。
- **文档**：[`evaluations/p4-object-saver.md`](evaluations/p4-object-saver.md)。
- **状态**：计划待实施；先做 P4a。

### D-157 P4a：同 generation 追加正文版本 spike 通过

- 新增 `pdf_patch::append_content_updates`：
  * 复制目标注释 dict，替换 `/Contents`（UTF-16BE hex）；
  * 以**同一对象号 + 同一 generation**追加 `N G obj … endobj`；
  * 追加经典 xref + trailer `/Prev`；页引用/ObjStm/原对象不动。
- `pdf_index` 暴露 `object_dict` / `trailer`（读取直接对象与 ObjStm）。
- **验证**：
  * Okular `11 1` highlight 改正文 → PDFium 读回新正文，注释数不变，增长 <1.2 KB；
  * Adobe ObjStm 内对象 → 同样读回，ObjStm 保留；
  * `tests/pdf_patch.rs` 两条测试通过。
- **未做**：尚未接入 writeback（当前产品仍走 PDFium updates/全量兜底）；
  P4b 再把 updates 从 PDFium 移出，并处理删除。
- **状态**：P4a 完成；下一步 P4b。

### D-158 P4b-1：正文更新接入对象级 patch

- `writeback` 不再把 `updates` 交给 PDFium（增量模式）：
  * 只有正文更新且没有 pages/deletes → 直接 `pdf_patch` 源字节，完全跳过 PDFium 保存；
  * 混合 pages/deletes → PDFium 保存后再 `pdf_patch` 补 updates；
  * 全量保存时仍由 PDFium 更新（避免旧 gen 引用）。
- `needs_full_save` 只保留 `deletes` 的 gen>0；正文更新不再触发全量。
- app 的 `pdf_ref_changes` 恢复为只产生**外部注释** updates；自有注释由 marker 差分处理。
- **验证**：
  * Okular `11 1` 更新：输出 < 9.5 KB（原 8.2 KB），读回新正文；
  * Adobe ObjStm 更新：输出 < 14 KB（原 12 KB），ObjStm 保留、读回新正文；
  * 原 P2/P3 测试通过；`check.sh` 全绿（366 tests）。
- **未做**：删除仍由 PDFium 处理，gen>0 删除仍全量兜底；P4b-2 再做页对象改写。
- **状态**：P4b-1 完成。

### D-159 P4b 修正：连续两次写回同一外部注释

- **现象**：对 Okular 原始样本同一条注释连续改正文并写回两次，第二次报
  “注释字典里出现非键 token”。
- **根因**：`pdf_patch` 的 value 解析把 `/P 3 0 R` 只吃到 `3`，剩下 `0 R`
  被当成非键 token；第一次 patch 后 `/Contents` 插在字典末尾，第二次解析才走到 `/P`。
- **修复**：数值值解析识别完整的 `num num R` 引用；新增双重 patch 测试与
  writeback 连续两次写回测试。
- **体积**：外部注释更新是复制完整注释 dict：
  * highlight/underline/strikeout 约 +0.8 KB；
  * ink 含完整 `/InkList`，约 +2.1 KB；这是对象级追加的正常成本，仍远小于整份追加。
- **验证**：Okular 11 1 连续两次读回“第二次”；Adobe ObjStm 连续两次成功；
  `.annot.json` 仍不生成；`check.sh` 全绿（368 tests）。
- **状态**：P4b-1 修复完成；删除页对象改写待 P4b-2。

### D-160 外部注释删除后的撤销、写入提示计数

- **问题 1**：外部注释删除并写回后，撤销恢复的是旧 `pdf_ref`，但该对象已不在 PDF；
  `pages` 只收自有注释，导致再次写回时跳过，重开消失。
  * 修复：撤销恢复时若外部 id 的旧对象已不在 `opened_pdf_refs`，把它转为自有注释
    （移出 `external_ids`、清 `pdf_ref`），按新注释写入；会获得我们的 `/NM` 标记。
- **问题 2**：第二次正文写回提示“新增1/更新1”不一致。
  * 原因：提示用 `items.len()` 当“新增”，但 P3 把所有自有注释都放进 `pages` 以保留 marker；
  * 修复：分别统计 `pdf_ref=None` 的新注释、自有正文变化、外部 updates、deletes。
- **测试**：`undoing_external_delete_after_save_requeues_it_as_new`；
  `check.sh` 全绿（369 tests）。
- **状态**：P4b-1 修正；删除的页对象级改写仍待 P4b-2。

### D-161 修复：外部注释 id 抢占 pd-fr 标记 id，导致打开/删除丢注释

- **样本**：`scripts/replay/error_bug/rotated_2p_okular_0.pdf` 有 4 条 Okular 注释
  + 1 条 `pd-fr:3`（我们的高亮）。
- **根因**：merge 先给外部注释 `add` 分配 id（1、2、3），随后导入 `pd-fr:3`
  时 `insert` 用 id 3 覆盖了外部 ink；app 实际只有 4 条，但 `external_ids`
  仍以为有 4 条外部。后续删除/撤销基于错误集合，表现为“少两条/撤销只回来一条”。
- **修复**：merge 改成两遍：
  1. 先导入所有 `/NM = pd-fr:*` 的带标记注释，占住 id；
  2. 再给无标记外部注释分配 id（复用 `pdf_ref_ids` 时也跳过已占用 id）。
- **提示差异**：不是 Adobe/Okular 的差别，而是注释归属：
  * 外部注释改正文 → 更新；
  * 自有（带 pd-fr）注释在 `pages` 里 → 旧提示按 `pages.len()` 显示“新增”；
  现在分别统计新注释/自有正文变化/外部 updates/deletes。
- **验证**：`marked_ids_are_reserved_before_external_annotations`；
  重新打开 0.pdf 得到 **5 条（4 外部 + 1 自有）**；`check.sh` 全绿（370 tests）。
- **注意**：1.pdf/2.pdf 是旧 bug 产物，已丢的注释不会自动恢复；请从 0.pdf 用新版本重做。
- **状态**：P4b-1 修复；P4b-2 删除页对象改写待做。

### D-162 P4b-2：删除走页对象级增量

- `pdf_patch::append_deletes`：复制页对象，从 `/Annots` 去掉目标引用（含 `/Popup`），
  追加同 `(num,gen)` 页对象 + 经典 xref；支持间接 `/Annots` 数组。
- `write_incremental`：`pages` 为空且只有 updates/deletes 时直接对源字节 patch，
  跳过 PDFium 保存；删除不再需要 gen>0 全量兜底。
- 混合任务（还有新增/自有页面）仍由 PDFium 处理删除；统一模型留到 P5。
- **验证**：
  * Okular 高亮删除：只少一条，增长 <1.2KB；
  * Foxit 带 Popup 父注释删除：父+子一起消失；
  * Okular 第 2 页间接 `/Annots` 数组删除；
  * `check.sh` 全绿（373 tests）。
- **状态**：P4b-2 完成（外部-only 删除）；P5 统一模型待做。

### D-163 多 `%%EOF` 的来源：PDFium 保存 + 注释外置

- 用户观察：撤销删除后重写，文件有两个 `%%EOF` 段，一个 16 对象、一个 2 对象；
  只删除时只有一个 EOF 段。
- **不是 EOF 本身占空间**：`%%EOF` 只有 6 字节；体积来自两个增量修订里的对象。
- 分段来源：
  1. **PDFium 增量保存**：新增注释需要 PDFium 生成 AP，它会追加一整份
     文档修订（本次 16 个对象）；
  2. **`pdf_repair::externalize_annotations`**：PDFium 新建的注释是内联字典，
     为兼容 Okular，追加“新注释对象 + 页对象”一段（本次 2 个对象）；
  3. **`pdf_patch`**：外部正文/删除的对象级 patch，通常 1 个注释对象或 1 个页对象。
- 撤销删除后，恢复的注释按“新注释”重新写回，所以走 1+2 两段；只删除外部注释时
  走 `pdf_patch` 的页对象快路径，只有一段。
- 多 `%%EOF` 是合法的 PDF 增量修订；不能简单删掉旧段（会破坏 `/Prev` 链和对象版本）。
- **结论**：要减少新增注释的体积，不能靠删 EOF，需要：
  * 统一模型（已有注释正文/删除都只 patch）；
  * 新增注释若能由我们自己写对象/AP，或把外置步骤并入同一 xref，才能少一段；
  * 这些都是 P5/后续优化，不是当前正确性问题。
- **状态**：记录；无代码改动。

### D-164 自有注释改正文也走对象级 patch

- **现象**：`rotated_2p_foxit.pdf` 只改 `pd-fr:3` 高亮正文，文件从 40,678 → 50,032，
  多出一段 18 对象、~9 KB 的修订。
- **根因**：P3 把所有自有注释放进 `pages` 以保留 marker；即使没有新增注释，
  PDFium 也被调用保存整份文档。
- **修复**：
  * 只有**确实存在新注释**（`pdf_ref=None` 的自有注释）时才把自有注释放进 `pages`；
  * 没有新注释时 `pages=[]`，所有 PDF 已有注释（自有 + 外部）的正文变化都走 `updates`；
  * `write_incremental` 的 pages 空快路径用 `pdf_patch` 追加同 `(num,gen)` 版本；
  * 提示计数相应修正（自有正文变化也计为“更新”）。
- **验证**：
  * 用户文件 `rotated_2p_foxit.pdf` 改高亮正文：**40,678 → 41,017（+339 字节）**，
    EOF 段只多 1 个；
  * `check.sh` 全绿（374 tests）。
- **状态**：P4b 性能修正；新增注释仍需要 PDFium 保存 + 外置，P5 统一模型继续。

### D-165 P5：统一注释写回模型（新注释 / 已有注释）

- **模型**：
  * `pdf_ref=None` 且非外部内联 → 新注释，进 `pages`，由 PDFium 生成 AP；
  * `pdf_ref=Some` → 已有注释，正文变化进 `updates`（pdf_patch 同 gen 追加）；
  * 当前集合里不存在的 ref → `deletes`（页对象 patch）。
- **writeback**：
  * 不再按 `/NM` marker 删除“未列出的注释”；marker 只用于防止同一个新注释重试时重复添加；
  * 空 `pages` 不会隐式删 marker；
  * 删除只由显式 `deletes` 负责。
- **app**：
  * `pages` 只放新注释（排除外部内联），不再把所有自有注解塞进去；
  * `pdf_ref_changes` 对自有/外部一视同仁，正文变化都进 `updates`；
  * `external_ids` 只在“外部内联无对象号”与报告/撤销恢复时保留，不再决定写回路径。
- **效果**：自有高亮改正文只追加一个对象；连续保存/撤销重做保持；新注释仍走 PDFium+外置。
- **测试**：更新 writeback 契约测试；恢复被误删的 incremental/indirect/full 测试；
  `check.sh` 全绿（374 tests）。
- **状态**：P5 完成；P4b-2 删除已覆盖外部-only，混合任务仍由 PDFium（后续可继续对象化）。

### D-166 P5 修正：撤销已写回的删除（自有/外部统一）

- **问题**：删除写入后撤销再写入，注释没有回来。P5 后 `pages` 只收新注释，
  而撤销恢复只在 `external_ids` 里检查旧 ref；自有注释没有该标记，保留了失效 ref，
  于是既不在 pages 也不在 updates，被跳过。
- **修复**：撤销恢复 AddAnnotation 时，无论自有/外部，只要 `pdf_ref` 不在
  `opened_pdf_refs`（说明对象已被写回删除），就清空 ref、移出 external_ids，
  按新注释重新写回。
- **新增测试**：`undoing_own_delete_after_save_requeues_it_as_new`；
  原外部测试继续通过。
- **验证**：`check.sh` 全绿（375 tests）。
- **状态**：P5 修正完成。

### D-167 P4c：删除统一改为保存后的页对象 patch

- 增量写回不再让 PDFium 删除注释；保存后统一：
  * `updates` → `pdf_patch` 同 gen 追加；
  * `deletes` → `pdf_patch::append_deletes` 复制页对象、去掉 `/Annots` 引用（含 Popup）。
- `needs_full_save` 移除，`write_incremental` 不再按 gen>0 全量；
- 期望注释数按 deletes 修正，然后 verify；
- `page_objects` 接受省略 `/Type /Page` 的页对象（PDFium 增量页对象会这样写），
  否则混合“新增+删除”会误判；
- 新增 `mixed_new_and_delete_uses_page_patch_after_save`；
- 全量 `write()` 仍可用，但产品增量路径完全对象级。
- **验证**：`check.sh` 全绿（376 tests）；外部-only/混合删除、Popup、间接 Annots。
- **状态**：P4 对象级正文/删除完成；新增注释仍需 PDFium + 外置。

### D-168 P6a：新高亮走对象级写入

- `pdf_patch::append_new_highlights`：
  * 分配新对象号，写 Highlight dict（Rect/QuadPoints/C/CA/F/NM/Contents/P）；
  * 复制页对象，把新 ref 追加到 `/Annots`；
  * 追加对象 + 经典 xref；高亮不需要 AP。
- `write_incremental`：当新注释全部是 Highlight 时，跳过 PDFium 保存，
  直接对源字节 patch；可与 updates/deletes 组合。
- 效果：新增一条高亮只多“注释对象 + 页对象 + xref”一段，不再有 PDFium
  16 对象修订；下划线/删除线/墨迹仍走 PDFium（需要 AP）。
- **验证**：`new_highlight_uses_object_patch_and_one_revision`：
  链接+新高亮、文件增长 <2KB、只有源+新两段 EOF、正文读回；
  `check.sh` 全绿（377 tests）。
- **状态**：P6a 完成；underline/strike/ink 的新增仍待对象化。

### D-169 P6b：下划线、删除线、墨迹新增也对象化

- `pdf_patch::NewMarkup`/`append_new_markups`：
  * Highlight 无 AP；
  * Underline/StrikeOut 写 Form XObject AP（BBox=Rect、Matrix 单位阵、页面坐标线条）；
- `pdf_patch::NewInk`/`append_new_inks`：
  * 写 Form XObject AP：BBox 扩半线宽、Matrix 单位阵、`1 J 1 j`、
    路径 `m/l`、颜色/线宽/CA/ca、ExtGState；
  * 注释 dict 同时写 `/InkList`。
- `write_incremental` 的新注释快路径现在覆盖全部四种：Highlight/Underline/StrikeOut/Ink
  都直接对象级追加，不再需要 PDFium 保存/外置；新注释通常只增加一段小修订。
- **验证**：
  * 新高亮/下划线/删除线读回种类/正文；
  * 新墨迹读回 + 打开注释层渲染，深色路径像素 >100；
  * 增长 <3KB；`check.sh` 全绿（379 tests）。
- **风险**：手写 AP 的 Okular/Poppler 保存兼容性还需真实 Okular 验证；
  若某类异常，可按种类退回 PDFium 路径。
- **状态**：P6b 完成；待用户用 Okular 打开/另存新增三类注释验证。

### D-170 UI：新增下划线/删除线按钮

- 工具栏在「高亮」旁增加「下划线」「删除线」，均把当前选区变成对应文本标记；
- `pending_annotate` 从 bool 改为 `Option<MarkupTool>`，三种标记共用 `annotate_selection`；
- 历史描述区分高亮/下划线/删除线，不再统一显示“高亮”；
- 测试 `three_markup_buttons_map_to_three_annotation_kinds`；`check.sh` 全绿（380 tests）。
- **状态**：按钮完成；三种标记写回已由 P6b 对象化。

### D-171 下划线/删除线样式：随缩放线宽、不透明、下划线留 gap

- **线宽**：界面原来固定 2 屏幕像素，缩放不变；改为
  `TEXT_MARK_LINE_WIDTH(1.5pt) * placement.scale`，随缩放变化；AP 也写 1.5pt。
- **不透明**：`Annotation::new` 对 Underline/StrikeOut 默认 opacity=1.0；高亮仍 0.35。
- **下划线 gap**：`UNDERLINE_GAP=0.6pt`；界面画在文字底部下方，PDF AP 内容也下移；
  AP 的 BBox 和 PDFium 路径的注释 Rect 同时向下扩，避免被裁剪。
- **测试**：四类像素位置仍通过（下划线带更新为 gap 后位置）；
  `check.sh` 全绿（380 tests）。
- **说明**：外部注释仍按自己的 `/CA` 显示；新建下划线/删除线默认不透明。

### D-172 旋转页线条显示 + 无 /Annots 页写回

- **线条方向**：界面原来先 `rect_to_content` 再画水平线，旋转 90° 后会画错；
  改为把下划线/删除线的两个端点分别 `page_to_content`，线条随页面旋转。
  外部注释同样走这条路径。
- **无 /Annots 页**：`append_annots` 在页字典没有 `/Annots` 时只插入了数组、
  没有插键名，写成 `... /Contents 6 0 R [11 0 R]`，PDF 忽略，verify 第 2 页=0。
  修复为插入 `/Annots [...]`；旋转页无注释时新增高亮/下划线/删除线现在可保存。
- **gap**：`UNDERLINE_GAP` 0.6 → 2.4pt（4 倍）；仍同步扩展 AP BBox/PDFium Rect。
- **测试**：`new_markups_on_rotated_page_without_annots_are_saved`；旋转页 3 条
  新标记写入并读回；`check.sh` 全绿（381 tests）。
- **状态**：完成；请实测旋转页线条方向和 gap 观感。

### D-173 标记模式 + 样式栏

- 工具栏「高亮/下划线/删除线」改为**持久模式**：
  * 点击进入并自动切到选字工具；按钮高亮显示当前模式；
  * 拖选文字后松手自动施加；再点同一按钮/Esc/切工具退出；
  * 空白处起拖不会误用旧选区；H 仍是一次性快捷键。
- 「样式」菜单：黄/红/绿/蓝标记颜色 + 新高亮透明度滑块；
  下划线/删除线固定不透明，线宽随缩放。
- 删除线位置改为 `STRIKE_FRACTION=0.4`（不再简单取 quad 中心），
  减小大写字母把线抬高的观感；真正按字体基线仍需文本度量模型。
- 测试：标记模式进出、样式经撤销/重做保留；`check.sh` 全绿（383 tests）。

### D-174 标记模式记忆位、墨迹颜色、墨迹按钮归组

- **记忆位**：`tool_before_annotation` 记录进入标记模式前的工具；
  Esc/再点当前按钮退出时恢复（如进入前是墨迹，退出回墨迹），不再固定回选字；
  三种标记之间切换保留记忆；主动点其它工具则清空记忆。
- **墨迹颜色**：样式菜单新增「墨迹颜色」黑/红/绿/蓝；新墨迹用
  `add_annotation_styled(..., ink_color, INK_OPACITY)`，撤销/重做保留颜色。
- **按钮归组**：墨迹从「平移/选字/墨迹」工具行移到高亮/下划线/删除线旁边。
- 测试：模式记忆（含从墨迹进入）、墨迹颜色；`check.sh` 全绿（385 tests）。

### D-175 P7：注释管理面板

- 设计记录：`docs/decisions/evaluations/annotation-panel.md`（正文摘要、轮廓、只读标记、
  text-fit、墨迹优先级均已按讨论定稿）。
- 侧栏新增「注释」标签页：
  * 按页分组、类型 + 正文首行摘要，按行宽截断；无正文显示「无正文」；
  * 悬停 tooltip 显示压平后的前 280 字；
  * 点击行 → GoToPage + 中性白/黑轮廓（1.2s）+ 打开原有 Annotation 浮窗；
  * 类型筛选 + 仅外部 + 仅只读；只读外部内联注释行尾标「[只读]」；
  * 不新增持久化字段/摘要字段，不区分自有/外部颜色。
- 新增 `AnnotationRow` 收集/排序，按页、y 降序、x、id 稳定排序。
- **验证**：摘要/tooltip 单测；`check.sh` 全绿（386 tests）。

### D-176 注释列表：单一 tooltip + 右键删除

- 双重 tooltip 根因：`Label::truncate()` 默认在截断时自动加完整文本 tooltip，
  与自定义 tooltip 叠加；关闭 `show_tooltip_when_elided(false)`。
- 行右键菜单「删除注释（可撤销）」；复用 `delete_annotation(id)`，
  无对象号的外部内联注释仍拒绝并提示只读。
- `delete_hovered_annotation` 也改为调用同一入口，避免两条删除逻辑。
- 测试：删除可撤销、内联外部拒绝；`check.sh` 全绿（387 tests）。

### D-177 注释列表删除按钮 + 已有注释改色

- 列表每行增加「删」按钮；只读注释按钮禁用；保留行右键删除。
  删除仍走 `delete_annotation`，可撤销；tooltip 去掉右键提示。
- 画布标注右键与列表行右键共用 `annotation_context_menu`：
  * 颜色：黄/红/绿/蓝；
  * 删除；
  * 只读/外部内联显示不可修改提示。
- **自有注释改色**：若尚未写入 PDF，直接更新模型；若已有对象号，则
  **删除旧对象 + 以新 id/新对象重建**，复用 P4c/P6 的 deletes/pages 路径，
  避免为改色另写 AP 更新器。撤销/重做用 `AppEdit::RecolorAnnotation` 整体切换，
  浮窗 owner 跟着 id 迁移。
- 外部注释暂不改色（避免把别人的注释变成自有对象）；只读内联禁止删除。
- 测试：新/已保存注释改色、撤销/重做、外部拒绝；`check.sh` 全绿（390 tests）。

### D-178 注释列表对齐/删除符号 + 画布右键菜单锚点

- 列表行左对齐：固定 8px 左内边距，用 `allocate_ui_with_layout(LeftToRight)`
  （`add_sized` 会居中，不能只改 Label halign）；删除按钮用 U+274E、无填充背景，
  放在剩余宽度的 right-to-left 布局里，真正贴行尾；tooltip 不再提示右键。
- 画布右键菜单改为自定义 `Area`（不再用 `Response::context_menu`）：
  * 原始 `pointer.secondary_clicked()` 取指针下注释，记录 `context_annotation + context_menu_pos`；
  * 菜单/子菜单打开期间状态保持；在另一条注释上右键直接替换锚点；
  * 左键在菜单外关闭；右键打开的当帧不执行 outside-close，避免新菜单闪现后消失；
  * Esc 关闭菜单。
  这样移到颜色子菜单、或切换到另一条注释的菜单都不会消失。
- `check.sh` 全绿（391 tests）。

### D-179 墨迹平滑与抽稀

- 收笔时：
  * 端点固定；
  * 仅当中间点离前后点中点 <=1pt 才做 0.5/0.25/0.25 平滑，尖角不抹平；
  * RDP 抽稀，容差 0.35pt，保留形状。
- 复用 `InkStroke` 的 0.5pt 采样抽稀与 2000 点上限；一笔一标注不变。
- 测试：近共线点收笔减少、端点不变；`check.sh` 全绿（391 tests）。
- **Esc**：墨迹工具空闲时按 Esc 退出到平移；正在画的一笔按 Esc 仍只取消这一笔、
  留在墨迹模式（保持原有“取消半笔”语义）。

### D-180 第4/5条第一批：写回冲突检测 + 注释时间元数据

- **写回冲突检测**：
  * 打开时记录 PDF 的 `PdfFileStamp`（mtime + 长度）；
  * 原地写入前比较；变化则拒绝并提示重新打开，原文件/当前会话不动；
  * 暂存完成后、关闭会话前再比较一次；冲突则删除临时文件，保留当前会话。
- **时间元数据**：对象级新增注释和 PDFium 全量路径都写
  `/M (D:...Z)` 与 `/CreationDate (D:...Z)`（UTC）。
- 说明：本轮先落地高价值子项；外部内联注释编辑/删除、修订合并、压缩、
  `/T` 作者等仍留待后续。
- 测试：文件 stamp 变化、`/M`/`/CreationDate` 输出；`check.sh` 全绿（392 tests）。

### D-181 冲突提示措辞 + 正文更新刷新 /M

- 外部修改导致拒绝时，提示不再建议“重新打开后再保存”（会丢当前修改），改为：
  当前修改仍在编辑器中；可丢弃修改后重新打开，或使用“另存为”（尚未实现）保存到新文件。
  两处检查共用 `PDF_CONFLICT_MESSAGE`，避免初始/复查分支措辞再次分叉。
- `replace_contents` 在改正文后调用 `stamp_modified`：替换/插入 `/M (D:...Z)`。
  `/M` = 注释的 ModificationDate，`/CreationDate` 只在新建时写。
- 测试：正文 patch 输出含 `/M (`；`check.sh` 全绿（392 tests）。

### D-182 第4条：另存为

- 顶栏新增「另存为…」：输入目标路径 → 把当前 PDF + 当前注释导出到新文件；
  **不切换当前文档**，原文件不动，当前修改/撤销栈保留。
- 默认目标 `<stem>-另存为.pdf`，已存在则递增后缀；目标已存在一律拒绝，不覆盖。
- 无注释/无对象级改动时直接复制源 PDF；有改动时走 `export_with_refs`
  （与“导出标注 PDF”同一条增量写出路径）。
- 冲突提示改为“使用另存为”，不再写“尚未实现”。
- 测试：检查通过；`check.sh` 全绿（393 tests）。

### D-183 另存为：切换文档 + 数据文件勾选

- 成功后**切换到新文档**，复用“写入 PDF”的重开路径：恢复视图、保留撤销栈；
  原 PDF 不动。
- 另存为对话框新增两个 checkbox：
  * 写入原 PDF 的数据文件（归档到原路径）；
  * 写入新 PDF 的数据文件（新文档使用）；
  默认都勾选。
- 红字提示：
  * 新数据文件不勾选且有数据：新文件无 preview/被引用点尺寸，链接仍从 PDF 重新解析；
  * 两个都不勾选且有数据：不会写入任何数据文件，切换后仍在内存并标记未保存；
  * 只取消原数据文件：灰色说明原数据文件不更新，不阻止。
- 校验目标 PDF 与目标 `.pdf.json` 是否已存在；任一存在都拒绝，不覆盖。
- 数据写失败/导出失败不切换；新数据文件未写时，切换后恢复内存数据并标记未保存。
- `check.sh` 全绿；PDFium 单元测试因线程安全风险不放进 lib 测试，手动验证。

### D-184 另存为提示按真值表拆开

- 提示条件拆成 `SaveAsWarnings` 并按 8 种组合测试：
  * `-00`：新文件缺数据文件 → 1；
  * `-01`：无提示；
  * `-10`：新文件缺数据文件 → 1；
  * `-11`：无提示；
  * `+00`：1 + 3（新文件缺数据；无数据文件归档）；
  * `+01`：4（原数据文件不更新，灰色）；
  * `+10`：1 + 2（新文件缺数据；未保存数据已归档到原文件，但新文件没有，
    内存仍有未保存副本）；
  * `+11`：无提示。
- “新文件缺数据”只取决于是否有自定义数据；是否未保存只影响 2/3/4，
  不再把注释/PDF 脏混入。
- 自定义数据自身的撤销/重做仍是独立缺口，另做。
- 测试 `save_as_warning_truth_table`；`check.sh` 全绿。

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
