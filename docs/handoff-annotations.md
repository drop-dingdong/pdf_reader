# 交接：注释功能（另一个会话从这里继续）

> 分支 `feat/annotations`，最后提交 `c0350b0`。S0–S3 + **S4a/S4b（墨迹：模型、图层 v2、回放、工具与手势）已完成**；
> 只剩 **S4c（性能验收 + 人工手感）** 与 S5（写回 PDF）。
> 权威决策见 [`decisions/log.md`](decisions/log.md) 的 **D-118**（阶段计划）、**D-119**（墨迹模型/图层 v2）、
> **D-120**（墨迹工具与手势）；本文只记录进度、陷阱与下一步。

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

## 2. 代码地图（新增/改动最多的地方）

| 文件 | 内容 |
| --- | --- |
| `crates/pdfcore/src/annotation.rs` | 标注模型与 `AnnotationStore`（页空间几何、id 不复用、`bounds` / `contains(tol)`）；`Ink` 的折线几何、`is_valid`、`kind.ink()` 访问器 |
| `crates/pdfcore/src/text.rs` | `PageText::range_for_page_rect`（矩形 → 字符范围 → 复用按行合并） |
| `crates/app/src/persist.rs` | 末尾"标注图层"段：`ANNOT_VERSION = 2` / `SavedOverlay` / `SavedAnnotation`（含 `points`/`width`）/ `annot_path` / `write_atomic` / `annotation_body_title` / `annotation_kind_from_saved` / `overlay_from_store` / `store_from_overlay` |
| `crates/app/src/history.rs` | `History<T>`（单栈、按字节裁剪、`descriptions()` 供可视列表） |
| `crates/app/src/ink.rs` | 墨迹手势状态机（纯逻辑）：页面空间采样、0.5 抽稀、2000 点上限、收笔判定；`cargo test -p app --lib ink::` |
| `crates/app/src/app.rs` | `AppEdit`（编辑命令）、`apply_edit`（唯一修改点）、`undo_edit` / `redo_edit`、`add_annotation`（所有"新建标注"的唯一入口）/ `add_highlight` / `annotate_ink`、`annotation_at`、`page_at_content`、`open_annotation_note`、`delete_hovered_annotation`、`shift_float_custom_indices`、`draw_history_window`、`DEFAULT_INK_WIDTH`、**`Tool::Ink` / `DragMode::Ink` / `switch_tool` / `handle_ink_pointer`（墨迹手势）** |
| `crates/app/src/app/replay.rs` | 动作 `click` / `drag` / `highlight` / `ink`（真实拖拽，需先 `set_tool ink`）/ `set_tool` / `save` / `undo` / `redo`；手势帧可携带同帧按键（Esc 取消）；JSONL 现在也写 `annotations` / `annotation_bounds` / `floats` / `previews_total` |
| `scripts/replay/*.json` + `perf/baseline/*.json` | 新脚本：`click_link` / `drag_pan` / `annotate_zoom` / `undo_redo` / **`ink`**（结构基线已入库，共 9 条） |
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

## 6. 下一步

### S4 墨迹

**已完成（S4a，`2fdc8bd`，D-119）**：模型（折线 + 线宽 + 不变量）、`bounds`（半线宽外扩）、
`contains`（点到折线距离）、`annot.json` v2、回放动作 `ink`、墨迹绘制与悬停、
`add_annotation` 统一入口；测试 `annotation::tests::ink_*`、`persist::annot_tests::ink_*`、
回放 `ink_stroke_is_one_annotation_that_undo_and_redo_can_move`、`annot_roundtrip` 的墨迹重开断言。

**已完成（S4b，`c0350b0`，D-120）**：`Tool::Ink`、手势状态机（`ink.rs`）、抬笔才入库、
`Esc` / 切工具丢弃、起笔补采、不透明深色默认、回放真实拖拽（`set_tool` + `cancel`）、
JSONL 标注字段。回放断言：一次拖拽 = 1 条、撤销/重做、Esc 后数量不变、1×/2× 包围盒逐值相同。

**S4c：性能与人工验收**（剩下的全部）：

1. 1000 条墨迹以内忙帧 p99 不明显退化：新压力脚本（可以先 `ink` 动作循环造 N 笔，
   或临时加一个"批量造标注"的模型级动作 —— 注意别把压力脚本塞进 `every_script` 的常规门禁里，
   它的帧数会拖慢 `check.sh`）；
2. 人工验收（`agent.md` 分工，必须真窗口）：切到「墨迹」画一笔是否跟手、`Esc` 是否干净丢弃、
   缩放后笔画粗细与位置是否一致、悬停 + `Delete` 能否删、撤销后是否整笔消失；
3. 顺带记录两个已知边界：指针移出窗口不特殊处理（egui 继续给 `dragged`）、单击不产生点（单点不成笔）。

### S5 写回 PDF（单独评估，不承诺）

- `FPDFAnnot_*` 创建/更新；原子保存（临时文件 + rename）；写成功后**单向迁移**：删除 `<pdf>.annot.json` 与被引用的 `custom_texts` 条目；加密/权限（`Permissions::annotate`）/只读路径；备份策略。**这一步会修改用户文件**，动手前先出一份评估。

### 已知未做（不是 bug，是没到）

- 改颜色、正文编辑的**合并规则**（300ms / 失焦提交）—— 这两条编辑路径目前还不存在。
- 截图（`compose_region_snapshot`）**不含**标注；要不要包含需先决定（需要自己光栅化）。
- 标记的"选框"类（S5 之后）。

## 7. 新会话开场建议

1. `git log --oneline -15` + 读本文；
2. `./scripts/check.sh` 确认基线仍绿；
3. 读 `crates/app/src/app.rs` 里的 `AppEdit` / `apply_edit` / `undo_edit`（这是所有编辑的唯一入口）；
4. 墨迹三件套（模型/持久化/工具）都已入库；`scripts/replay/ink.json` 是现成的回归脚本。
   S4c 直接做压力脚本与人工验收清单，不要再改 `InkStroke` 的语义（抬笔入库是硬约定）。
