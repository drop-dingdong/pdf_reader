# 存档与预览图（现状）

> 两件事放在一起，因为它们是同一条链：**存档只记元数据 + 分辨率，像素靠按需渲染重建**。

## 1. 持久化数据

| 数据 | 位置 | 形态 | 代码 |
| --- | --- | --- | --- |
| 自定义数据（链接、被引用点、自定义 preview） | 与 PDF 同目录：`<pdf>.json`（如 `aa/bb.pdf` → `aa/bb.pdf.json`） | 手写 JSON，无第三方依赖 | [`persist.rs`](../crates/app/src/persist.rs) |
| 用户设置（UI 尺寸、注释默认样式、每文件阅读状态/最近文件） | 配置目录 `settings.json` | 手写 JSON | [`app/src/settings.rs`](../crates/app/src/settings.rs)（见 §6） |

自定义数据**不存像素**：只存"页号 + 页面空间矩形 + 浮窗正文尺寸 + 题目/文字 + **分辨率**"，
载入后按分辨率重新请求渲染并合成（见 §4）。旧 `.annot.json` 不再读取/创建，遗留文件忽略。

## 2. 文件格式（v4）

```jsonc
{
 "version": 4,
 "links":    [ [ {"rect":[x,y,w,h], "dest":{"kind":"page","page":3,"offset":[x,y]}} ], … ],
 "dest_marks": [ [ {"point":[x,y], "rect":[x,y,w,h], "body":[w,h], "scale":8.0,
                   "image_id":"dest-…", "image_revision":0} ], … ],
 "custom_previews": [
  {"kind":"image", "page":1, "point":[x,y], "rect":[x,y,w,h], "body":[w,h], "scale":2.5,
   "title":"…", "image_id":"custom-…", "image_revision":0},
  {"kind":"text", "page":2, "point":[x,y], "rect":[x,y,w,h], "body":[w,h], "scale":1,
   "title":"…", "text":"…", "image_id":"", "image_revision":0}
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
| `image_id` | ▲v4 稳定图片记录 id；文字 preview 为空 |
| `image_revision` | ▲v4 图片内容版本；影响像素的修改递增 |

v3 不再写 `custom_images` / `custom_texts`，也不使用 `order` 字段；
旧 v1/v2 的两个数组仍可读入，并按“先图片后文字”的顺序迁移成 `custom_previews`。

### 2.1 标注图层（已移除）

`.annot.json` 是 2026-11-01 之前的注释存储格式。注释改为直接写进 PDF（`/NM`/`/Contents`）后，
程序**不再读取、也不再创建** `<pdf>.annot.json`；同目录里遗留的旧文件会被完全忽略，
也不会再产生 `.annot.json.bak-*`。旧文件可以手动删除。

## 3. 版本与迁移

| 版本 | 变更 | 迁移 |
| --- | --- | --- |
| **v1** | 没有 `version` 字段（或写 `1`）；**没有 `scale`** | 缺 `version` 按 v1 读 |
| **v2** | `dest_marks[]` 与 `custom_images[]` 增加 `scale` | v1 → v2：缺失/非法（≤0、非有限）的 `scale` 填 `DEFAULT_SCALE = 1.0`，并**置脏** |
| **v3** | preview 改为单一有序 `custom_previews[]`（每条带 `kind`） | v1/v2 读入后迁移为统一列表，置脏 |
| **v4** | dest_marks 与图片 preview 增加 `image_id` / `image_revision` | v1–v3 读取后按页/下标生成 `legacy-*` id，置脏 |
| **v5+** | 未定义 | **拒绝读取**（见下面第 6 条）：不加载、不保存、不覆盖 |

规则（新增版本时必须遵守）：

1. **解析与迁移分开**：`SavedData::from_json` 只解析；`persist::migrate(&mut data)` 显式迁移，
  返回 `Migration { from, to, changed, notes }`。
2. **只做加法**：给新字段填默认值；不删字段、不改名、不改已有字段语义。默认值集中在解析函数里。
3. 一个版本一个 `if data.version < N` 分支，可连续升级；**幂等**。
4. `changed == true` → 调用方置 `data_dirty`（菜单/标题出现 `*`），下次保存写回当前版本。
5. `to_json` **永远写 `CURRENT_VERSION`**。
6. **禁止向前兼容**：`version > CURRENT_VERSION` 的存档**不读、也不写**。
  * 载入：`persist::load` 返回 `LoadError::NewerVersion`，应用当作"没有自定义数据"，
   并记住"存档被占用"（`sidecar_locked`）；
  * 保存：直接拒绝并提示原因，「保存」置灰（悬停说明）——**绝不覆盖**。
  * 理由：旧程序只认识一部分字段，读一半再保存就是用残缺数据覆盖完整数据，
   比"什么都不做"更糟；而"读进来但不许保存"又会让用户的编辑被静默丢弃。

可观测：stderr `[存档] v1 → v2：…` / `v2 → v3：…` / `v3 → v4：…`；
打开后状态栏提示"存档已升级到 v3，请保存"。守它的测试：
`persist::tests::v1_…` / `v2_…` / `newer_…` / `invalid_scale_…`。

## 4. 预览图的生成（初次生成与重建）

数据流：

```text
记录 {rect(页面空间), scale} → render = render_scale_for(scale, overscan)  // 吸到波段
 → TileGrid::new(旋转后页尺寸, render) → tiles_for_page_rect 找出覆盖的块
   ├─ region_tiles 全部就绪 → compose_region_snapshot：在【渲染像素】里 1:1 拼图
   │   输出尺寸 = 区域 × 渲染比例（与当前缩放无关）；未覆盖=白；零覆盖=None
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
| 「保存」 | 数据脏时把当前内存数据写成 JSON（写 `CURRENT_VERSION`），成功后 `data_dirty = false` |
| 置脏 | 改矩形/重截图/新建/删除/改题目/改正文/浮窗尺寸真的变化（带容差）/存档迁移 |
| 切换文档 | 先清理 UI 与自定义数据，**再**读新文档的存档（顺序错会导致数据丢失） |

## 6. 用户设置（`settings.json`）

与 PDF 无关的"使用者习惯"，**全局一份**：

```json
{
 "version": 1,
 "ui": { "window_size": [1100, 900], "sidebar_width": 260,
     "pin_hover_enabled": true, "horizontal_scroll": false,
     "show_links": false, "show_dest_points": false, "show_annotations": true,
     "markdown_floats": false, "hover_preview_markdown": false },
 "save": { "backup_in_place": false },
 "annotation_defaults": {
  "markup_color": [255, 214, 0], "highlight_opacity": 0.35,
  "ink_color": [25, 25, 25], "ink_width": 2.0
 },
 "documents": [
  { "path": "/data/a.pdf", "page": 3, "zoom": 1.25, "anchor": [0.0, 412.5],
   "horizontal": false, "opened_at": 1762000000 }
 ]
}
```

| 字段 | 含义 |
| --- | --- |
| `ui.window_size` / `sidebar_width` | 窗口与侧栏尺寸（逻辑像素）；只在**已有设置文件**时用于恢复窗口大小 |
| `ui.pin_hover_enabled` / `horizontal_scroll` | 顶栏两个开关的持久化值 |
| `ui.show_links` / `show_dest_points` / `show_annotations` | 引用显示样式（链接框 / 引用点 / 标注）的持久化值 |
| `ui.markdown_floats` | 文字浮窗是否默认按 Markdown 渲染（顶栏「Markdown」，默认 `false`；只影响显示，不改源文本/`.json`） |
| `ui.hover_preview_markdown` | 侧栏 hover 临时预览是否也按 Markdown 渲染（顶栏「hover MD」，默认 `false`；会触发公式/图片 owner 渲染） |
| `save.backup_in_place` | 原地保存前是否留一份 `.bak`（默认 `false`，文件菜单可切换） |
| `annotation_defaults` | 新注释的默认样式（颜色、高亮透明度、墨迹线宽） |
| `documents` | **每个文件一份**的阅读状态，同时充当"最近文件"（最多 12 条，按 `opened_at` 降序；文件菜单 →「最近打开」展示前 8 条） |
| `documents[].anchor` | ▲2026-11-01 **页面锚点**：视口左上角在内容空间的坐标 `[x, y]`（内容空间与缩放/排版一起保存，所以能回到原位置）；旧文件缺这个字段就只按页码恢复 |

规则：

* 位置：Linux `$XDG_CONFIG_HOME/pdf-reader/settings.json`（缺省 `~/.config/…`）、
 Windows `%APPDATA%\pdf-reader\settings.json`、macOS `~/Library/Application Support/…`；
* 缺字段用默认、未知字段忽略、数值夹取（透明度 0.1–1、线宽 0.5–8、缩放 0.1–10）；
* 损坏 → 改名 `settings.json.bad-<时间>`，用默认值继续；
* 写盘：临时文件 + rename；时机 = 样式/视图变化后的空闲帧、切换文档、关闭窗口；
* 回放/无窗口构造不读写设置（`settings_path = None`），测试不会碰用户配置。

## 7. 已知限制

* **不保留未知字段**（也不再需要）：旧版本拒绝读取新版本存档，
 "原样回写未知字段"就没有意义了 —— 代价是降级使用时完全看不到那份存档的数据，
 需要用户手动把 `<pdf>.json` 挪走才会"重新开始"。
* `scale` 记录的是"显示比例"，未记录 `overscan`；改变 `DEFAULT_OVERSCAN` 后同一存档重建出的
 像素尺寸会有 ≤1.6 倍差异（待办：记录渲染比例快照）。
* 旧档迁移后 `scale = 1.0` 而 `body` 仍是当年的大窗口 → 预览会偏糊（不自动改用户窗口尺寸）。
* 文本 preview 与图片 preview 共用 `SavedCustom`（`scale` 对文字无意义，写 1.0）。

## 8. 将来的版本（还没做，先记着）

按上面的规则，"加一个版本"要动的只有三处：`CURRENT_VERSION`、`from_json` 的默认值、
`migrate` 里的一个 `if data.version < N` 分支；**不需要**做向前兼容。

| 候选 | 内容 | 触发条件 |
| --- | --- | --- |
| v4 | 记录 `overscan` 快照（或直接记录渲染比例），让预览的像素尺寸不受 `DEFAULT_OVERSCAN` 变化影响 | 真要改 `DEFAULT_OVERSCAN` 时 |
| v4 | `quality.source`：记录预览是"当时截的图"还是"后来重建的"，便于诊断 | 出现"预览看起来不对"的反馈时 |
| v4 | `body` 与 `scale` 的一致性策略（旧档迁移后 `body` 偏大导致预览偏糊） | 旧档数量多、被投诉糊时 |

不再计划：保留未知字段。新版本存档不允许被旧版本读取，"原样回写"没有使用场景。
