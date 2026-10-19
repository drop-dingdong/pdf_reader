# 存档与预览图（现状）

> 两件事放在一起，因为它们是同一条链：**存档只记元数据 + 分辨率，像素靠按需渲染重建**。

## 1. 两类持久化数据

| 数据 | 位置 | 形态 | 代码 |
| --- | --- | --- | --- |
| 自定义数据（链接、被引用点、自定义 preview） | 与 PDF 同目录：`<pdf>.json`（如 `aa/bb.pdf` → `aa/bb.pdf.json`） | 手写 JSON，无第三方依赖 | [`persist.rs`](../../crates/app/src/persist.rs) |
| 偏好设置与最近文件 | 由 `KeyValueStore` 承载（真实实现留给后续） | 键值 | `pdfcore::store`（`AppSettings` / `RecentFiles`） |

自定义数据**不存像素**：只存"页号 + 页面空间矩形 + 浮窗正文尺寸 + 题目/文字 + **分辨率**"，
载入后按分辨率重新请求渲染并合成（见 §4）。

## 2. 文件格式（v2）

```jsonc
{
  "version": 2,
  "links":       [ [ {"rect":[x,y,w,h], "dest":{"kind":"page","page":3,"offset":[x,y]}} ], … ],   // 每页一组
  "dest_marks":  [ [ {"point":[x,y], "rect":[x,y,w,h], "body":[w,h], "scale":8.0} ], … ],         // 每页一组
  "custom_images":[ {"page":1, "point":[x,y], "rect":[x,y,w,h], "body":[w,h], "scale":2.5,
                     "title":"…"} ],
  "custom_texts": [ {"page":2, "point":[x,y], "rect":[x,y,w,h], "body":[w,h], "scale":1,
                     "title":"…", "text":"…"} ]
}
```

字段语义：

| 字段 | 含义 |
| --- | --- |
| `rect` / `point` | **PDF 页面空间**（左下原点、point） |
| `body` | 浮窗**正文尺寸**（egui 单位）；浮窗创建时的初始大小，用户改窗口后回写 |
| `scale` | ▲v2 截图时的**显示比例**（`1.0` = 100%）；决定预览图按什么分辨率重建 |
| `title` / `text` | 题目 / 文字预览的正文 |

## 3. 版本与迁移

| 版本 | 变更 | 迁移 |
| --- | --- | --- |
| **v1** | 没有 `version` 字段（或写 `1`）；**没有 `scale`** | 缺 `version` 按 v1 读 |
| **v2** | `dest_marks[]` 与 `custom_images[]` 增加 `scale` | v1 → v2：缺失/非法（≤0、非有限）的 `scale` 填 `DEFAULT_SCALE = 1.0`，并**置脏** |

规则（新增版本时必须遵守）：

1. **解析与迁移分开**：`SavedData::from_json` 只解析；`persist::migrate(&mut data)` 显式迁移，
   返回 `Migration { from, to, changed, notes }`。
2. **只做加法**：给新字段填默认值；不删字段、不改名、不改已有字段语义。默认值集中在解析函数里。
3. 一个版本一个 `if data.version < N` 分支，可连续升级；**幂等**。
4. `changed == true` → 调用方置 `data_dirty`（保存按钮带 `*`），下次保存写回当前版本。
5. `to_json` **永远写 `CURRENT_VERSION`**。
6. 比当前版本**更新**的存档：不改数据，只提示"未识别字段保存时会丢"。

可观测：stderr `[存档] v1 → v2：N 处截图没有记录分辨率，按 100% 处理（保存后写回 v2）`；
打开后状态栏提示"存档已升级到 v2，请保存"。守它的测试：`persist::tests::v1_…` / `v2_…` /
`newer_…` / `invalid_scale_…`。

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

* **未知字段不保留**：新版写的字段被旧版读入再保存会丢（待办：`extra` 字段原样回写）。
* `scale` 记录的是"显示比例"，未记录 `overscan`；改变 `DEFAULT_OVERSCAN` 后同一存档重建出的
  像素尺寸会有 ≤1.6 倍差异（待办：记录渲染比例快照）。
* 旧档迁移后 `scale = 1.0` 而 `body` 仍是当年的大窗口 → 预览会偏糊（不自动改用户窗口尺寸）。
* 文本 preview 与图片 preview 共用 `SavedCustom`（`scale` 对文字无意义，写 1.0）。
