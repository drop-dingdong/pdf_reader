# 存档与预览图（现状）

> 两件事放在一起，因为它们是同一条链：**存档只记元数据 + 分辨率，像素靠按需渲染重建**。

## 1. 两类持久化数据

| 数据 | 位置 | 形态 | 代码 |
| --- | --- | --- | --- |
| 自定义数据（链接、被引用点、自定义 preview） | 与 PDF 同目录：`<pdf>.json`（如 `aa/bb.pdf` → `aa/bb.pdf.json`） | 手写 JSON，无第三方依赖 | [`persist.rs`](../../crates/app/src/persist.rs) |
| 标注图层（高亮 / 下划线 / 删除线 / 墨迹） | 与 PDF 同目录：`<pdf>.annot.json` | 手写 JSON，**独立 version** | `persist.rs` 的"标注图层"段 |
| 偏好设置与最近文件 | 由 `KeyValueStore` 承载（真实实现留给后续） | 键值 | `pdfcore::store`（`AppSettings` / `RecentFiles`） |

自定义数据**不存像素**：只存"页号 + 页面空间矩形 + 浮窗正文尺寸 + 题目/文字 + **分辨率**"，
载入后按分辨率重新请求渲染并合成（见 §4）。标注图层同样不存像素，只存**页面空间几何**。

## 2. 文件格式（v3）

```jsonc
{
  "version": 3,
  "links":       [ [ {"rect":[x,y,w,h], "dest":{"kind":"page","page":3,"offset":[x,y]}} ], … ],
  "dest_marks":  [ [ {"point":[x,y], "rect":[x,y,w,h], "body":[w,h], "scale":8.0} ], … ],
  "custom_previews": [
    {"kind":"image", "page":1, "point":[x,y], "rect":[x,y,w,h], "body":[w,h], "scale":2.5,
     "title":"…"},
    {"kind":"text",  "page":2, "point":[x,y], "rect":[x,y,w,h], "body":[w,h], "scale":1,
     "title":"…", "text":"…"}
  ]
}
```

字段语义：

| 字段 | 含义 |
| --- | --- |
| `rect` / `point` | **PDF 页面空间**（左下原点、point） |
| `body` | 浮窗**正文尺寸**（egui 单位）；浮窗创建时的初始大小，用户改窗口后回写 |
| `scale` | ▲v2 截图时的**显示比例**（`1.0` = 100%）；决定预览图按什么分辨率重建 |
| `kind` | ▲v3 `image` / `text`；统一列表按当前显示顺序保存 |
| `title` / `text` | 题目 / 文字预览的正文 |

v3 不再写 `custom_images` / `custom_texts`，也不使用 `order` 字段；
旧 v1/v2 的两个数组仍可读入，并按“先图片后文字”的顺序迁移成 `custom_previews`。

### 2.1 标注图层（`<pdf>.annot.json`，v2）

**为什么单独一个文件**（D-118）：几何是"应用层装饰"，注释正文仍在主存档的 `custom_texts` 里
（靠哨兵标题 `#annot:<id>` 引用）。这样删掉 `.annot.json` 就等于清空所有标注，
也不会牵动主存档的版本迁移。

```jsonc
{
  "version": 2,
  "pages": [                                  // 下标 = 页号，每页一组
    [ {"id":1, "kind":"highlight", "color":[255,214,0], "opacity":0.35,
       "quads":[[50,690,450,50]], "body":"#annot:1"},
      {"id":2, "kind":"ink", "color":[255,214,0], "opacity":0.35,
       "points":[[50,700],[120,720],[200,690]], "width":3.00, "body":"#annot:2"} ]
  ]
}
```

| 字段 | 含义 |
| --- | --- |
| `id` | 标注 id；`#annot:<id>` 是正文条目的标题（删除后不复用，撤销栈要对得上） |
| `kind` | `highlight` / `underline` / `strike_out` / `ink`；未知种类按高亮处理，几何无效则整条跳过 |
| `quads` | 文本类的行矩形（**未旋转页面空间**）；墨迹没有 |
| `points` / `width` | ▲v2 墨迹的折线与线宽，**都是页面空间单位**（缩放后粗细才一致） |
| `color` / `opacity` | RGB 与不透明度 |
| `body` | 正文引用（哨兵标题）；正文本身在主存档的 `custom_texts` 里，可以缺失（显示"正文已丢失"） |

| 图层版本 | 变更 | 迁移 |
| --- | --- | --- |
| **v1** | 只有 `quads`（三种文本类标注） | — |
| **v2** | 增加 `points` + `width`（墨迹） | **无需迁移**：v1 文件按原样读得懂；写回时一律写 v2 |
| **v3+** | 未定义 | **拒绝读取**（`LoadError::NewerVersion`），**不加载也不覆盖** |

规则与主存档一致（§3 第 6 条）：`version > ANNOT_VERSION` 的图层不读、不写。
反过来，**写了墨迹就必须是 v2**——v1 的读取端不认识 `ink`，会静默丢掉这条，
用户再保存一次就真丢了。

守它的测试：`persist::annot_tests::{overlay_round_trips_through_json, ink_round_trips_through_json_with_points_and_width,
a_version_1_overlay_still_loads, ink_with_too_few_points_is_skipped_at_the_model_layer, newer_overlay_version_is_refused}`；
端到端：`crates/app/tests/annot_roundtrip.rs`（保存 → 重开，墨迹几何与线宽原样回来）。

## 3. 版本与迁移

| 版本 | 变更 | 迁移 |
| --- | --- | --- |
| **v1** | 没有 `version` 字段（或写 `1`）；**没有 `scale`** | 缺 `version` 按 v1 读 |
| **v2** | `dest_marks[]` 与 `custom_images[]` 增加 `scale` | v1 → v2：缺失/非法（≤0、非有限）的 `scale` 填 `DEFAULT_SCALE = 1.0`，并**置脏** |
| **v3** | preview 改为单一有序 `custom_previews[]`（每条带 `kind`） | v1/v2 读入后迁移为统一列表，置脏 |
| **v4+** | 未定义 | **拒绝读取**（见下面第 6 条）：不加载、不保存、不覆盖 |

规则（新增版本时必须遵守）：

1. **解析与迁移分开**：`SavedData::from_json` 只解析；`persist::migrate(&mut data)` 显式迁移，
   返回 `Migration { from, to, changed, notes }`。
2. **只做加法**：给新字段填默认值；不删字段、不改名、不改已有字段语义。默认值集中在解析函数里。
3. 一个版本一个 `if data.version < N` 分支，可连续升级；**幂等**。
4. `changed == true` → 调用方置 `data_dirty`（保存按钮带 `*`），下次保存写回当前版本。
5. `to_json` **永远写 `CURRENT_VERSION`**。
6. **禁止向前兼容**（D-107）：`version > CURRENT_VERSION` 的存档**不读、也不写**。
   * 载入：`persist::load` 返回 `LoadError::NewerVersion`，应用当作"没有自定义数据"，
     并记住"存档被占用"（`sidecar_locked`）；
   * 保存：直接拒绝并提示原因，「保存自定义数据」按钮置灰（悬停说明）——**绝不覆盖**。
   * 理由：旧程序只认识一部分字段，读一半再保存就是用残缺数据覆盖完整数据，
     比"什么都不做"更糟；而"读进来但不许保存"又会让用户的编辑被静默丢弃。

可观测：stderr `[存档] v1 → v2：…` / `v2 → v3：…`；
打开后状态栏提示"存档已升级到 v3，请保存"。守它的测试：
`persist::tests::v1_…` / `v2_…` / `newer_…` / `invalid_scale_…`。

## 4. 预览图的生成（初次生成与重建）

数据流：

```text
记录 {rect(页面空间), scale}  →  render = render_scale_for(scale, overscan)   // 吸到波段
  → TileGrid::new(旋转后页尺寸, render) → tiles_for_page_rect 找出覆盖的块
     ├─ region_tiles 全部就绪 → compose_region_snapshot：在【渲染像素】里 1:1 拼图
     │     输出尺寸 = 区域 × 渲染比例（与当前缩放无关）；未覆盖=白；零覆盖=None
     └─ 缺块 → request_region 提交这些块（独立 region_jobs，不被 cancel_stale_jobs 取消）
               本帧不出图；下一帧重试
```

触发点：

| 对象 | 触发 | 重试入口 |
| --- | --- | --- |
| 被引用点（链接目的地） | 悬停链接（`update_hover_preview`） | 每帧（只要指针还在链接上） |
| 自定义图片 preview | 侧栏条目悬浮/单击；**浮窗正在显示** | 每帧（`ensure_pending_custom_snapshots`，只处理被浮窗引用的） |

诊断（`PDF_READER_DIAGNOSE=1`）：

```text
[预览] 初次生成：被引用点 页=3 区域pt=(…) 记录分辨率=800% 渲染比例=10.486 可见块=(…) 提交渲染=6 块（等待就绪）
[预览] 初次生成完成：被引用点 页=3 … 尺寸=2097x1573
[预览] 初次生成失败：…（区域为空 / 不在页面内 / 块都为空）
```

守它的测试：`app/tests/tiling_plan.rs::region_render_feeds_the_preview_at_the_recorded_scale`、
`view::tests::compose_region_snapshot_copies_tiles_one_to_one`、
`pdfcore::tiling::tests::page_rect_probe_matches_a_full_scan`。

## 5. 载入与保存时机

| 时机 | 行为 |
| --- | --- |
| 打开 PDF | 读 `<pdf>.json` → `migrate` → 用存档的链接**替换** `NavigationIndex.links_by_page`、覆盖被引用点的 `rect/body/scale`、重建 `custom_previews`（`snapshot: None`，按需生成） |
| 保存按钮 | 把当前内存数据写成 JSON（写 `CURRENT_VERSION`），成功后 `data_dirty = false` |
| 置脏 | 改矩形/重截图/新建/删除/改题目/改正文/浮窗尺寸真的变化（带容差）/存档迁移 |
| 切换文档 | 先清理 UI 与自定义数据，**再**读新文档的存档（顺序错会导致数据丢失） |

## 6. 已知限制

* **不保留未知字段**（也不再需要）：既然旧版本**拒绝读取**新版本存档（D-107），
  "原样回写未知字段"就没有意义了 —— 代价是降级使用时完全看不到那份存档的数据，
  需要用户手动把 `<pdf>.json` 挪走才会"重新开始"。
* `scale` 记录的是"显示比例"，未记录 `overscan`；改变 `DEFAULT_OVERSCAN` 后同一存档重建出的
  像素尺寸会有 ≤1.6 倍差异（待办：记录渲染比例快照）。
* 旧档迁移后 `scale = 1.0` 而 `body` 仍是当年的大窗口 → 预览会偏糊（不自动改用户窗口尺寸）。
* 文本 preview 与图片 preview 共用 `SavedCustom`（`scale` 对文字无意义，写 1.0）。

## 7. 将来的版本（还没做，先记着）

按上面的规则，"加一个版本"要动的只有三处：`CURRENT_VERSION`、`from_json` 的默认值、
`migrate` 里的一个 `if data.version < N` 分支；**不需要**做向前兼容。

| 候选 | 内容 | 触发条件 |
| --- | --- | --- |
| v3 | 记录 `overscan` 快照（或直接记录渲染比例），让预览的像素尺寸不受 `DEFAULT_OVERSCAN` 变化影响 | 真要改 `DEFAULT_OVERSCAN` 时 |
| v3 | `quality.source`：记录预览是"当时截的图"还是"后来重建的"，便于诊断 | 出现"预览看起来不对"的反馈时 |
| v3 | `body` 与 `scale` 的一致性策略（旧档迁移后 `body` 偏大导致预览偏糊） | 旧档数量多、被投诉糊时 |

不再计划：保留未知字段（v4）。理由见 D-107 —— 新版本存档不允许被旧版本读取，
"原样回写"没有使用场景。
