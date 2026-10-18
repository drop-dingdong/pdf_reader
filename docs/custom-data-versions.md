# 自定义数据存档（`<pdf>.json`）的版本与迁移

> 关联：[decision-log.md](decision-log.md) 的 D-64（存档设计：只存元数据、不存像素）、
> D-100（v2：记录截图分辨率）。代码在 `crates/app/src/persist.rs`。

## 1. 文件与内容

- 路径：与 PDF 同目录同名、追加 `.json`（`aa/bb.pdf` → `aa/bb.pdf.json`）。
- 只存**可重建图像所需的元数据，像素一律不入档**：链接、被引用点、自定义图片 / 文字 preview。
- 载入后按记录里的**分辨率**（v2 起）重新请求渲染并合成预览图。

## 2. 版本表

| 版本 | 变更 | 读取旧版的迁移 |
| --- | --- | --- |
| **v1** | 初版：`links` / `dest_marks` / `custom_images` / `custom_texts`；被引用点与自定义 preview 记录 `page` / `point` / `rect` / `body` / `title` / `text`。**没有 `version` 字段**（后期曾写成 `"version": 1`），也**没有记录截图分辨率** | 缺 `version` 时按 v1 |
| **v2** | 被引用点（`dest_marks[]`）与自定义**图片**（`custom_images[]`）新增 `scale`：**截图时的显示比例**（`1.0` = 100%）。预览图按它请求渲染，不再跟随"当前缩放" | v1 → v2：缺失/非法（≤0、非有限）的 `scale` 填 `DEFAULT_SCALE = 1.0`；升级后**置脏**，下次保存写回 `"version": 2` |

## 3. 读写约定（新增版本时必须遵守）

1. **解析与迁移分开**：`SavedData::from_json` 只解析（`version` 填文件里的值，缺省 1）；
   迁移由 `persist::migrate(&mut data)` 显式执行，返回
   `Migration { from, to, changed, notes }`。
2. **只做加法**：给新字段填默认值；不删除、不改名、不改变已有字段的语义。
   缺字段的默认值集中在解析函数里（如 `parse_scale`），一处定义、一处测试。
3. **一个版本一个分支**：`if data.version < N { …; data.version = N; }`，可连续升级（v1 → v2 → v3）。
4. **幂等**：同一份数据再迁移一次必须无变化（`changed == false`）。
5. **置脏**：`changed == true` 时调用方必须置 `data_dirty`（保存按钮变 `*保存自定义数据`），
   保证"读了旧档 → 保存即升级"。
6. **写入永远写 `CURRENT_VERSION`**（`to_json`）。
7. **比当前版本更新的存档**：不改动数据，只提示"未识别字段会在保存时丢失"（已知限制）。
8. **每个版本至少两条测试**：旧版样例能读 + 迁移正确；当前版本往返不变。
   旧版样例 JSON 直接写在测试里，以后加版本照抄一份即可。

## 4. 字段默认值表（缺失 / 非法时）

| 字段 | 出现在 | 缺失或非法时 |
| --- | --- | --- |
| `version` | 根 | 按 `1`（v1） |
| `scale` | `dest_marks[]`、`custom_images[]`（v2） | `DEFAULT_SCALE = 1.0` |
| `title` | `custom_*[]` | 空串 |
| `text` | `custom_*[]` | `None`（图片预览） |

## 4.1 预览图怎么生成（v2 `scale` 的用途）

存档里的 `scale` 是**截图时的显示比例**，它决定"预览图按什么分辨率重建"（D-101）：

1. `render = render_scale_for(scale, overscan)` —— 把记录值**吸到波段**上，这样与主视图
   同波段时能直接命中已有缓存；
2. `TileGrid::new(页尺寸(旋转后), render)` 建网格，`tiles_for_page_rect` 找出覆盖该区域
   的块范围（页面空间 → 设备像素，右/下边界落点不算下一块）；
3. `DocumentSession::region_tiles` 全部就绪 → `view::compose_region_snapshot` 在
   **渲染像素**里 1:1 拼图（输出尺寸 = 区域 × 渲染比例，**与当前缩放无关**）；
4. 缺块 → `DocumentSession::request_region` 提交覆盖该区域的块（记在独立的在飞表里，
   不会被 `cancel_stale_jobs` 取消），本帧先不出图；下一帧重试。
   被引用点由"悬停链接"每帧重试；自定义图片由"侧栏悬浮/点击"和"浮窗正在显示"两条路径重试。

因此"初次生成"不再依赖"用户是否在这个缩放下走过那块地方"。诊断（`PDF_READER_DIAGNOSE=1`）：

```
[预览] 初次生成：被引用点 页=3 区域pt=(…) 记录分辨率=800% 渲染比例=10.486 可见块=(…) 提交渲染=6 块（等待就绪）
[预览] 初次生成完成：被引用点 页=3 … 尺寸=2097x1573
[预览] 初次生成失败：…（区域为空 / 不在页面内 / 块都为空）
```

## 5. 迁移的可观测性

- 读到旧档会打印（stderr）：
  `[存档] v1 → v2：N 处截图没有记录分辨率，按 100% 处理（保存后写回 v2）`。
- 打开后状态栏消息提示"存档已升级到 v2，请保存"，且保存按钮带 `*`。
- 预览图首次生成失败会打印 `[预览] …`（见 D-101，§4.1）。

## 6. 将来要做的事（roadmap）

- **v3（近期）**：`scale` 的语义细化为"**渲染比例快照**"——连 `overscan` 一起记录
  （`{ display_scale, overscan, render_scale }`），避免同一份存档在不同 `overscan`
  设置下重建出不同分辨率；迁移时用 `1.6`（当前 `DEFAULT_OVERSCAN`）补齐。
- **v3**：每条记录加 `quality: { scale, source: "capture" | "legacy" }`，界面可区分
  "用户当时截的清晰图"与"迁移来的 100% 兜底图"，并提供一键"重截更高清"。
- **v3**：`body` 与 `scale` 的一致性策略——旧档的 `body` 是"当时缩放下的窗口尺寸"，
  而 `scale` 迁移为 1.0；可选地把 `body` 收缩到 `rect × scale`（默认**不自动改**，
  避免动用户的窗口尺寸，只在 UI 上提示"内容可放大"）。
- **v4（中期）**：**保留未知字段**。当前解析会丢掉不认识的键；要做到"新版写、旧版改"
  不丢数据，需要在 `SavedData` 里带一个 `extra: Json` 并原样回写。
- **v4**：预览图改为**按需渲染 + 内存缓存淘汰**（不落盘），支持"手动重截"与"后台预热"。
- **长期**：文本与图片 preview 的字段分开（当前共用 `SavedCustom`，`scale` 对文字无意义）；
  存档加密 / 校验和 / `created_by` 等诊断字段。

## 7. 加版本的清单（照着做）

1. `persist.rs`：`CURRENT_VERSION += 1`；新字段加进 `Saved*` 结构与 `to_json` / `parse_*`（给默认值）；
   `migrate` 里加一个 `if data.version < N` 分支。
2. 本文件的版本表 + 默认值表各加一行。
3. `persist.rs` 测试：旧版样例 JSON（含新字段缺失）、迁移结果、幂等、当前版本往返。
4. `app.rs`：若新字段影响行为，接到 `DestMark` / `CustomPreview`，并在保存处写出。
5. `decision-log.md`：记一条决定（为什么加这个字段、迁移策略、代价）。
