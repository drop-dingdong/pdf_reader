# 交接：注释功能（另一个会话从这里继续）

> 分支 `feat/annotations`，最后提交见 `git log`。**S0–S4（模型 / 图层 v2 / 撤销 / 墨迹 / 压力门槛）已完成**并通过门禁。
> **S5（写回 PDF）已完成评估 + S5a spike**（[`decisions/evaluations/s5-writeback.md`](decisions/evaluations/s5-writeback.md)，D-123），
> 5 个决策已由用户确认：**导出副本 / 保留 overlay / 四种都写 / 受限文档拒绝 / 不覆盖**。
> **S5 全部完成**：S5a/S5b（导出副本）+ **S5c（原地写入，`/NM` 标记同步、备份、保留 overlay）**。
> 只剩 **S6（读回 PDF 注释）**，以及一次人工验收（原地写入流程 + 滚动恢复）。
> 已修：写回会同步已有注释的正文/颜色（删掉重建）；重开后延迟两帧恢复视图（D-131）；
> 墨迹外观改用 PDFium 原生 `AppendObject`（带 `/Resources`/`/Matrix`，修 Okular 保存报错，D-132）；
> S6a 读回 + overlay 合并完成（D-135）；页面位图默认不烤注释（INV-14，D-136）。
> 人工验收（真窗口）已确认通过：画一笔跟手、整笔撤销/重做、Esc 丢弃、800% 粗细与位置、两种删除方式；
> 导出成功且文件名正确、重复导出被“不覆盖”拒绝、**第 2 页旋转已正常、导出标注位置正确**（2026-10-27）。
> 权威决策：[`decisions/log.md`](decisions/log.md) 的 **D-118**（阶段计划）、**D-119**（墨迹模型/图层 v2）、
> **D-120**（墨迹工具与手势）、**D-121**（S4c：悬停闸门、历史虚拟化、压力门槛）；本文只记录进度、陷阱与下一步。

## 1. 已完成（按提交顺序）

| 阶段 | 内容 | 关键提交 |
| --- | --- | --- |
| **S0** | 回放器补 `click` / `drag`（**页面空间**坐标）+ `page_after` / `scroll_after` | `4f4a949` |
| **S1a** | `pdfcore::annotation`：`AnnotationId` / `AnnotationKind` / `Annotation` / `AnnotationStore` | `d6675fb` |
| **S1b** | 高亮创建（选区或页面矩形）+ 叠加绘制 + 点击看正文 + 1×/8× 几何不变断言 | `cd7875e` |
| **S2a** | `<pdf>.annot.json` 格式（独立 version）+ `annot_path` + `write_atomic` | `cd7278d` |
| **S2b** | 打开时载入图层、正文走 `custom_texts` 哨兵标题、`Ctrl+S` 保存所有脏文件 | `c6f3ece` |
| — | 保存按钮与 `Ctrl+S` 走同一条路径 + "保存→重开"端到端测试 | `20ddc9d` |
| **S3a** | `History<T>`：单栈、时间序、按字节裁剪、undo/redo | `10bfecb` |
| **S3b** | 标注接入撤销（`Ctrl+Z` / `Ctrl+Shift+Z`）+ 可视历史列表 + 右键删除 | `a048e8e`、`8948704`、`d2cd65d` |
| — | 删除自定义 preview 进栈 | `b616f31` |
| — | 引用点重选进栈 | `739b0bc` |
| **S4a** | `AnnotationKind::Ink`（页面空间折线 + 线宽、折线命中、半线宽包围盒）+ `annot.json` **v2**（`points`/`width`）+ 墨迹绘制与悬停 | `2fdc8bd` |
| **S4b** | 墨迹工具：`Tool::Ink` + 手势状态机（`ink.rs`：0.5 抽稀 / 2000 上限 / 单点不成笔）+ 抬笔才入库 + `Esc`/切工具丢弃 + 回放改真实拖拽（`set_tool`、手势内按键）+ JSONL 补标注字段 | `c0350b0` |
| **S4c** | 绘制期间屏蔽链接悬停（第三闸门）+ `seed_ink` 压力脚本（1000 条）+ **历史列表虚拟化**（323ms → 16ms）+ 结构/时间门槛 + `ink_many` 基线 | `3ea1e22` |
| **S5a** | 写回可行性探针：PDFium C API 能力 + 外观流实测（无 AP 的墨迹不可见、下划线 0 px）+ `SetAP` 通用解 | `89ac8f3` |
| **S5** | `backend-pdfium::writeback`（18 个 FFI、AP 生成、保存自检、加密/签名闸门）+ `app --writeback` + 工具栏「导出标注 PDF」（temp→rename、不覆盖、冒烟校验）+ FFI 说明文档 | `d6a4304` + 见 `git log` |
| **S5c** | 原地写入：`/NM` 标记同步（幂等、删过期）、`stage_in_place`、关闭会话→备份→原子替换→重开恢复视图、确认框；**overlay 保留** | 见 `git log`（D-130） |
| — | 修正：`encrypted` 恒真、`/Rotate` 应用两次、导出 job 路径未绝对化；墨迹按页裁剪 + 折角补点；导出 AP 的一条笔画 = 一个子路径 + 圆角连接（拐角裂缝） | `b689155`、`3228e94` + 见 `git log` |

## 2. 代码地图（新增/改动最多的地方）

| 文件 | 内容 |
| --- | --- |
| `crates/pdfcore/src/annotation.rs` | 标注模型与 `AnnotationStore`（页空间几何、id 不复用、`bounds` / `contains(tol)`）；`Ink` 的折线几何、`is_valid`、`kind.ink()` 访问器 |
| `crates/pdfcore/src/text.rs` | `PageText::range_for_page_rect`（矩形 → 字符范围 → 复用按行合并） |
| `crates/app/src/persist.rs` | 末尾"标注图层"段：`ANNOT_VERSION = 2` / `SavedOverlay` / `SavedAnnotation`（含 `points`/`width`）/ `annot_path` / `write_atomic` / `annotation_body_title` / `annotation_kind_from_saved` / `overlay_from_store` / `store_from_overlay` |
| `crates/app/src/history.rs` | `History<T>`（单栈、按字节裁剪、`descriptions()` 供可视列表；`len` / `description(index)` 供**虚拟化**列表） |
| `crates/app/src/ink.rs` | 墨迹手势状态机（纯逻辑）：页面空间采样、0.5 抽稀、2000 点上限、收笔判定；`cargo test -p app --lib ink::` |
| `crates/app/src/app.rs` | `AppEdit`（编辑命令）、`apply_edit`（唯一修改点）、`undo_edit` / `redo_edit`、`add_annotation`（所有"新建标注"的唯一入口）/ `add_highlight` / `annotate_ink`、`annotation_at`、`page_at_content`、`open_annotation_note`、`delete_hovered_annotation`、`shift_float_custom_indices`、`draw_history_window`（**虚拟化**）、`DEFAULT_INK_WIDTH`、**`Tool::Ink` / `DragMode::Ink` / `switch_tool` / `handle_ink_pointer`（墨迹手势）**、`seed_ink_strokes`（压力测试） |
| `crates/app/src/app/replay.rs` | 动作 `click` / `drag` / `highlight` / `ink`（真实拖拽，需先 `set_tool ink`）/ `set_tool` / `seed_ink`（压力测试）/ `save` / `undo` / `redo`；手势帧可携带同帧按键（Esc 取消）；JSONL 也写 `annotations` / `annotation_bounds` / `floats` / `previews_total` |
| `scripts/replay/*.json` + `perf/baseline/*.json` | 新脚本：`click_link` / `drag_pan` / `annotate_zoom` / `undo_redo` / **`ink`** / **`ink_many`**（1000 条压力，结构基线已入库，共 10 条） |
| 测试 | `crates/app/tests/{annot_roundtrip,annot_click}.rs`、`replay_scripts.rs` 里的 `annotation_geometry_*` 与 `undo_and_redo_*` |

## 3. 不可破坏的约定

1. **几何只存未旋转页面空间**；内容/屏幕坐标只出现在绘制与断言里。`annotation_geometry_does_not_depend_on_zoom` 是这条的守卫。
2. **标注不进渲染管线**：不产生 `RenderKey`、不改瓦片网格、不影响结构基线（渲染请求/上传字节/纹理峰值）。
3. **正文不新增主存档字段**：正文是 `custom_texts` 里标题为 `#annot:<id>` 的条目；主存档 `SavedData` 的 schema 不动。
4. **所有编辑只经过 `apply_edit`**（撤销/重做是"取命令 → 反转 → 再 apply"）。新编辑类型必须走这里，否则会出现"删了标注忘了删正文"这类不对称。
5. **正文在"删除那一刻"入库**（`orphan_bodies`），不能存创建时的快照 —— 否则"改正文 → 撤销 → 重做"会丢内容。
6. **保存/打开都是原子写**（`persist::write_atomic`），并且**按钮与快捷键必须同一条路径**（`save_all`）。
7. 图层版本高于 `ANNOT_VERSION` 时**拒绝加载且不覆盖**（与主存档 D-107 一致）。
8. **墨迹没有 `quads`**：`quads()` 对墨迹返回空切片 —— 只处理行矩形的代码会自然跳过它；
   需要墨迹时显式用 `kind.ink()`。**命中与包围盒不许用矩形近似**：折线距离、
   点集外扩半个线宽（`contains` / `bounds` 已实现）。
9. **写墨迹就必须是图层 v2**（`points` + `width`）：v1 的读取端会把 `ink` 当未知种类
   静默丢掉，再保存就是真丢数据。v1 文件仍可读，不需要迁移。
10. **墨迹只在抬笔时入库**：拖动中采样点只存在 `InkStroke`（不产生命令、不进撤销栈）；
    `Esc` / 切走工具 = 丢掉它。任何"拖动中就 `apply_edit`"的写法都会让取消留下半条。
11. **画墨迹期间关闭链接悬停预览**（`hover_preview_allowed` 的 `drawing` 闸门）：
    笔画会压住链接，这时弹目的地预览会打断绘制（用户实测）。
12. **长列表必须虚拟化**：历史窗口曾整表排版 1001 行 = 一帧 323ms；现在用
    `ScrollArea::show_rows` + `History::description(index)`。新增任何"可能上千行"的列表都照此办理。

## 4. 验证方式

```bash
export PATH=/tmp/rust/cargo/bin:$PATH CARGO_HOME=/tmp/rust/cargo RUSTUP_HOME=/tmp/rust/rustup
./scripts/check.sh          # fmt + clippy -D warnings + 全部测试 + 结构基线 + 文档链接
```

- 结构基线变了（渲染请求数/上传字节/纹理峰值/预览尺寸），要么修回归，要么确认是一次性变化后重录：
  `cargo run -q -p app -- --replay scripts/replay/<name>.json --record-structural perf/baseline/<name>.json`
- 回放单跑：`cargo run -q -p app -- --replay scripts/replay/<name>.json [--timing] [--out x.jsonl]`
- 样本现场生成：`cargo run -q -p app -- --samples scripts/replay`

## 5. 踩过的坑（都会复发，做 S4 前先读）

| 坑 | 症状 | 解法 |
| --- | --- | --- |
| egui 的 `hovered` 滞后一帧 | 3 帧（移动/按下/抬起）**一次点击都收不到** | 手势先发**两帧**移动再按下（`gesture_for`） |
| 回放"稳定"判定 | 3 个静止帧就结束动作，手势还没喂完 | `quiet_now` 要求 `gesture.is_none()` |
| `hovered_annotation` 在帧首清空 | "悬停 + Delete"永远删不到 | 不在帧首清空，由标注绘制段每帧重算 |
| 保存按钮只存主存档 | 正文在、标注没了、目录里没有 `.annot.json` | 按钮也调 `save_all()` |
| 计时高亮不请求重绘 | 空闲时高亮"不定长"地停在屏幕上 | `request_repaint_after(剩余时间)` + 过期清状态 |
| 正文快照时机 | 改正文后撤销再重做，正文回退到旧值 | 正文在删除时进 `orphan_bodies` |
| `Action` 分支插入位置 | 格式化后多行签名导致 replace 不中，动作变空操作 | 改完先 `cargo run -- --replay` 手工确认效果 |
| 基线误伤纯交互脚本 | `submitted == 0` 被判"录制跑错" | 判据是"无提交**且**无纹理字节" |
| 新增 `AnnotationKind` 变体 | 绘制、存档、`describe` 的 `match` 全都不穷尽 | 这是好事，逐个补全；**别用 `_ => {}` 糊过去**，否则新种类会被静默忽略 |
| 用包围盒做墨迹命中 | L 形墨迹的包围盒里有大片空白，"点在空白处却选中了墨迹" | 点到折线距离 + 半线宽（`contains` 已实现） |
| 墨迹基线里 `提交=0` | 看起来像"录制跑错" | 上传字节 > 0 就是正常（页面早渲染完，动作只是加标注）；与 `undo_redo` 基线一致 |
| 起笔只取按下点 | 一笔的开头少一段（快速起笔尤其明显） | 用 `pointer.press_origin()` **并**补采 `drag_started` 那一帧的当前位置 |
| 回放里"拖到一半取消" | 普通 `key` 动作只能发生在两个动作之间，打断不了正在跑的手势 | 手势帧自带 `keys`（`GestureFrame`），`ink` 动作的 `cancel: true` 就是这么注入 Esc 的 |
| 历史窗口整表渲染 | 1000 条编辑时撤销那一帧 **323ms**（`ui.label` 逐行排版 + 布局缓存） | `ScrollArea::show_rows` + 按行取描述；压力脚本 `ink_many.json` 的 `undo1` 帧 < 100ms 是回归网 |
| 用"拖动"造 1000 笔 | 每笔 8 帧，压力脚本要上万帧，门禁变慢且噪声大 | 压力脚本用模型级 `seed_ink`（仍走 `annotate_ink` 的真实入口） |

## 6. 下一步

### S4 墨迹

**已完成（S4a，`2fdc8bd`，D-119）**：模型（折线 + 线宽 + 不变量）、`bounds`（半线宽外扩）、
`contains`（点到折线距离）、`annot.json` v2、回放动作 `ink`、墨迹绘制与悬停、
`add_annotation` 统一入口；测试 `annotation::tests::ink_*`、`persist::annot_tests::ink_*`、
回放 `ink_stroke_is_one_annotation_that_undo_and_redo_can_move`、`annot_roundtrip` 的墨迹重开断言。

**S4 已全部完成**：S4a（`2fdc8bd`）、S4b（`c0350b0`）、S4c（`3ea1e22`）。

- 人工验收（用户确认）：画一笔跟手 / 松手整笔出现且历史描述"墨迹（第 N 页）" /
  `Esc` 立刻消失不留东西 / 800% 下更粗且已画位置不飘 / 两种删除方式可用；
- 已知边界（不是 bug）：单击不产生点（单点不成笔）；指针移出窗口不特殊处理（egui 仍给 `dragged`）；
  "绘制中切工具"在 UI 上不可触达（拖动时点不到工具栏），代码仅作防御；
- 性能（本机 debug）：1000 条标注画一帧 ~3ms；造 1000 笔 ~4ms；撤销+历史窗口 ~17ms（虚拟化前 323ms）；
  结构门槛：造标注 `submitted == 0`、纹理层峰值不变（`ink_many` 基线）。

### S5 写回 PDF（评估完成，实现待确认）

评估：[`decisions/evaluations/s5-writeback.md`](decisions/evaluations/s5-writeback.md)（D-122）。三条硬结论：

1. **墨迹必须自己生成 `/AP`**：无 AP 的 `/Ink` 在 PDFium 里只有 1 个彩色像素（不可见），补 AP 后 162 个；
   文本类相反（无 AP 时 PDFium 合成高亮：黄色 3996 px，文字仍可读）。
2. **`pdfium-render 0.9.4` 帮不上忙**：无注释 API、句柄 `pub(crate)`、save flags 写死 0 →
   要自带一层小 FFI；而 FFI 绕过它的全局锁，会和后台渲染线程竞争 → 建议**子进程**写回。
3. **我们读不回 PDF 注释** → 不能"写成功后删 overlay"，S5 先做**导出副本**（overlay 保留）。

**S5a spike 已完成**（D-123，example 可复现：`cargo run -p backend-pdfium --example s5_spike`）：

- 墨迹 `AddInkStroke` + 外观、高亮/下划线/删除线 `AppendAttachmentPoints`（= `/QuadPoints`）全部可用；
- **下划线/删除线没有 `/AP` 就不可见**（下划线 0 px；删除线只覆盖文字 24 px）；`AppendObject` 对下划线失败；
- **`FPDFAnnot_SetAP` 是通用解**：Form 的 `/BBox` = `/Rect`、内容用页面坐标、**没有 `/Resources`**（不能设透明度）；
- `/Contents` UTF-16LE 往返正确；保存 flags 0/2 同尺寸可重开；既有链接/页数不受影响；
- **旋转页位置未验证**（spike 渲染传了 `Rotation::None`）→ S5b 必测。

**S5b-1 已完成（D-124）**：`backend-pdfium::writeback`（18 个 FFI 符号 + AP 生成 + 保存 + 自检）、
`app --writeback <job.json>` 子进程入口、四种标注、加密/签名闸门、拒绝 `source == output`；
完整 FFI/排查文档在 [`current/writeback.md`](current/writeback.md)。

**已修正用户实测的三个问题**：D-127（导出 job 的路径未绝对化，子进程找到 `/tmp/scripts/…`）、
D-126（`encrypted` 恒真、页面 `/Rotate` 应用两次）。摘要：
`DocumentInfo.encrypted` 不再恒真（之前所有文档都被当成加密，
导出被拒）；页面 `/Rotate` 不再被应用两次（之前 `/Rotate 90` 的页显示成 180° + 压扁）。
旋转页的注释位置现在有像素级回归测试（`intrinsic_rotation`、`writeback` 旋转页断言）。

**S5b-2 已完成**（D-125）：工具栏「导出标注 PDF」→ 同目录 `<stem>.annotated.pdf`；
temp + 冒烟校验 + rename，**不覆盖**；加密文档父进程预检，签名在子进程拒绝；
正文按 `#annot:<id>` 取。细节见 [`current/writeback.md`](current/writeback.md)。

**S5c 已完成（D-130）**：原地写入走"子进程暂存 → 关会话 → 备份 → 原子替换 → 重开"；
`/NM = "pd-fr:<id>"` 标记让重复写入幂等（已有则跳过、overlay 里没了则删），其它注释不碰；
**overlay 保留**（S6 之前不迁移）。

**剩下的（按顺序）**：

1. **人工验收**：原地写入全流程（确认框 → 备份文件出现 → 原文件被替换 → 视图恢复到原页/缩放）；
   重复点一次「写入 PDF」不应增加注释；在 Chrome/Acrobat 里看四种标注与旋转页；
2. 已知缺口的夹具：加密文档 / 带数字签名（闸门目前只有代码审查级保障）；
3. **S6**：评估已完成（D-133）；决策已确认（D-134）；**S6a 已实现**（D-135）——
   打开时读回 `pd-fr:` 标记的注释、overlay 优先合并、缺 overlay 时恢复（[`current/readback.md`](current/readback.md)）。
   **S6b 已完成**（D-140）：外部注释只读层（Okular/Evince/Firefox/Adobe/Foxit 样本），
   不写 overlay、不参与删除/写回；**S6c** 已完成（D-139）。
   剩余：跨阅读器兼容性（Okular 修改我们写的注释后保存报“文件有问题”）、可选的迁移/清理。

**S5c（原地替换 + 迁移）依赖 S6（能读回 PDF 注释）**；届时再评估"关-写-重载"与"子进程"两条路（D-124）。

### 已知未做（不是 bug，是没到）

- 改颜色、正文编辑的**合并规则**（300ms / 失焦提交）—— 这两条编辑路径目前还不存在。
- 截图（`compose_region_snapshot`）**不含**标注；要不要包含需先决定（需要自己光栅化）。
- 标记的"选框"类（S5 之后）。

## 7. 新会话开场建议

1. `git log --oneline -15` + 读本文；
2. `./scripts/check.sh` 确认基线仍绿；
3. 读 `crates/app/src/app.rs` 里的 `AppEdit` / `apply_edit` / `undo_edit`（这是所有编辑的唯一入口）；
4. S0–S5 都已完成（回放脚本 `ink.json`/`ink_many.json`、写回 example/测试都在）。
   **下一步是人工验收 + 夹具补齐**，不是新功能：
   * 在 Chrome/Acrobat 里抽查旋转页与四种标注（本仓库渲染器的问题已在 D-126 修正，
     但外部阅读器的注释绘制路径仍值得看一眼）；
   * 补加密/签名夹具，把两道闸门从"代码审查级"升到"自动化"；
   * S5c/S6 之前不要改 `InkStroke` 语义、几何坐标系与图层 v2。
   不要在主进程里调用写回 FFI（D-124）。
