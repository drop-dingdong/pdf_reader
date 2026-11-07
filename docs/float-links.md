# 浮窗链接（`pd-float://`）契约

> 状态：2026-11-07 设计定稿，随实现更新。本文是链接语法、身份规则与失败语义的**可追溯依据**；
> 与之冲突的实现以本文为准（或先改本文再改代码）。

## 1. 目标与非目标

目标：

- 文字浮窗正文里的 Markdown 链接可以打开/置顶**本应再打开一次**的浮窗；
- 同一 PDF 内链接尽量跨会话稳定，跨会话不可验证时**明确断链**，绝不静默指向错误目标；
- 创建入口：浮窗标题栏 `↗`（复制链接）、`→`（跳到来源）；侧栏条目通过 `⋯`/右键菜单复制；
- 链接只影响“打开/置顶/高亮/跳转”，不自动扫描、不级联打开。

非目标：

- 不做全局数据库、云同步、跨文档身份校验；
- 不链接测试浮窗、截图结果浮窗、引用点浮窗；
- 不把外部注释改写成自有注释（不写外部对象的 `/NM`）；
- 不保证外部注释链接跨会话准确（见 §5）。

## 2. 链接语法

```text
pd-float://annotation/<page>/<id>     注释（推荐；page 从 0 开始）
pd-float://annotation/<id>            注释旧写法（兼容；仅在 page 不可知时用）
pd-float://preview/<record_id>        preview 稳定记录 id（推荐）
pd-float://preview/<page>/<id>        preview 旧写法（兼容；id 是每页内序号）
```

- 只识别 `pd-float://`；`?query`/`#fragment` 在解析基础目标后单独处理；
- 页号是内部 `PageIndex`，**从 0 开始**，与界面显示的“第 N 页”差 1；
- `record_id` 是不透明字符串（当前形如 `preview-<hex>-<seq>`），不要手写猜测。

## 3. 注释

### 3.1 自有注释

- 写回时注释对象带 `/NM = pd-fr:<id>`；重开时按 `/NM` 恢复 id。
- **除删除外的修改必须保持 id**：
  - 改正文、改作者/时间（只读字段除外）→ 同 id；
  - 移动墨迹（`MoveAnnotation`）→ 同 id，`pdf_ref = None` 重建对象；
  - 改颜色/透明度/墨迹线宽 → 同 id，`pdf_ref = None` 重建对象（实现前会换 id，已修正）。
- 新建内部注释使用**时间戳/单调、不复用**的 id 分配：
  秒时间戳左移 20 位 + 同一秒内序号，并取 `max(现有最大 id + 1)`；同进程内单调递增。
  id 必须 **< 2^53**：写回 job 用 JSON number 传 id，f64 只能精确表示 2^53 以内的整数，
  超过会在保存/重开后变成另一个 id。这样删除注释后，旧 id 不会再被新注释复用。
- 仅当目标满足“内部注释（不在 `external_ids`）+ 页号匹配 + id 存在”时才打开；
  `/NM` 被外部编辑器改写后，目标会变成外部注释 → 断链提示。

### 3.2 外部注释

- 外部注释没有我们的 `/NM`，id 是加载时按顺序分配的，跨会话可能变化，
  甚至可能指向另一条注释；因此：
  - 默认**不提供**复制入口（浮窗 `↗` 隐藏、侧栏/右键菜单项禁用）；
  - 设置项 `ui.allow_external_annotation_links`（名称“允许跨会话外部注释链接”）默认 false；
  - 开启后才允许复制；链接带**本次文档加载** token：
    `pd-float://annotation/<page>/<id>?s=<token>`
- 跟随语义：
  - token 匹配：按当前会话打开；
  - token 不匹配且开关关闭：直接断链；
  - token 不匹配且开关开启（A+B）：
    1. 校验 `page + kind`（以及可用的 `created`）；
    2. 不匹配 → 断链；
    3. 匹配但缺少 `created`/无法完全验证 → 弹确认框，用户确认才打开。
- 开关只控制“复制入口”；**跟随**已有链接始终按 token/属性校验，不受开关放宽。

## 4. 自定义 preview

- `CustomPreview.image_id` 在语义上作为**preview 记录 id**（图片 preview 本来就用它，
  文字 preview 复用它，不新增字段）；`image_revision` 对文字 preview 固定为 0。
- 记录 id 在创建时生成；sidecar v5 迁移为旧的文字 preview 补 id 并置脏（提示保存）。
- 删除 preview 后记录 id 消失 → 链接断；撤销删除恢复同一 id → 链接重新可用。
- 删除被 `pd-img` 引用的图片 preview 时，引用它的文字浮窗**不关闭**；下一次图片 owner
  扫描会清掉 `ImageStore` 的旧 revision 和纹理缓存，不再继续显示已删除的图。
- 链接/`↗`/`→` 对 `FloatOwner::Custom` 的文字和图片 preview 都提供；
  `↗` 生成的 URI 用 `preview/<record_id>`；旧写法 `preview/<page>/<id>` 仍可解析。
- 图片 preview 的 `→` 跳到 `preview.page` / `preview.point`。

## 5. 浮窗类型与可链接性

| 浮窗 | 可链接 | 说明 |
| --- | --- | --- |
| 文字 preview（`Custom` + Text） | ✅ | 稳定记录 id |
| 图片 preview（`Custom` + Image） | ✅ | 稳定记录 id（`image_id`） |
| 注释浮窗（`Annotation`） | ✅ 仅内部注释 | `/NM`；外部注释按 §3.2 |
| 引用点浮窗（`None` + `origin`） | ❌ | 由 PDF 导航表派生；未来可用 `DestMark.image_id` 做 `pd-float://dest/...` |
| 截图结果/测试浮窗（`None`） | ❌ | 没有持久目标 |

> “引用点浮窗是派生的”指它的**生命周期来自 PDF 导航表**（打开时重建，sidecar 只保存覆盖值），
> 不是说数据不持久。图片 preview 由用户创建，因此二者生命周期不同。

## 6. 打开/置顶/高亮/跳转

- 目标未打开：复用 `open_annotation_note`（注释）或 `spawn_custom_float`（preview）
  在画布内按台阶错开的位置新建；新浮窗带一次性高亮。
- 目标已打开：置顶（`floats` 末尾）+ 一次性高亮计时。
- 断链/歧义：只在状态栏提示，不打开、不崩溃。
- `→`：注释跳 `annotation.page` + `bounds` 中心并给定位轮廓；
  preview 跳 `preview.page` / `preview.point`；截图/引用点浮窗不显示。
- 多个链接指向同一 dest/preview：打开/置顶的是同一个浮窗（按 `origin` / `custom` 查找）。

## 7. 指针归属（与链接/浮窗相关的不变量）

- 画布内容的原始命中（注释 hover、右键、链接 hover）先裁剪到画布矩形，
  再排除弹层/窗口/模态、连续动作和任意浮窗矩形；
- 弹层判定按**图层顺序**（非 Background），不用 `is_pointer_over_egui()`；
- 浮窗内按 `floats` 顺序（后画在上）判定；每个浮窗先注册只感知 drag 的全 rect 遮罩，
  防止拖动漏到下层浮窗或 PDF 正文；
- 浮窗的 raw click 置顶也必须先判图层，编辑窗口覆盖处不得置顶浮窗。

## 8. 迁移与兼容

- sidecar `CURRENT_VERSION = 5`：
  - v4 → v5：文字 preview 的 `image_id` 为空时生成记录 id，并置数据脏（提示保存）；
  - 旧版（v4）遇到 v5 会按 D-107 拒绝加载 sidecar，避免静默清空文字 id；
  - 旧写法链接（`annotation/<id>`、`preview/<page>/<id>`、`preview/<id>`）继续解析。
- 不批量重写 PDF 的 `/NM`；现有注释 id 保持不变，新注释用新分配器。

## 9. 失败提示（状态栏）

- `断链：注释 #N 不存在（可能来自其他文档）`
- `断链：目标不是本程序写入的注释（/NM 可能被外部编辑器改写）`
- `断链：第 N 页没有 id=… 的自定义preview`
- `链接不明确：…请改用 pd-float://preview/<页>/<id>`
- `外部注释链接跨会话失效` / `无法验证外部注释目标…`
- `无法识别的浮窗链接：…`

## 10. 验收

自动化：

- URI 解析（三种 preview 写法、annotation page/id、非法输入）；
- 文字/图片 preview 记录 id 生成、复制链接、关闭后重开、已有置顶；
- 注释 id 在 restyle/move/body 编辑后不变；删除后新注释不复用旧 id；
- 内部注释页号匹配、外部注释 token/属性校验/断链；
- 编辑窗口覆盖浮窗时点击不置顶；
- 侧栏 `⋯` 菜单复制与跳转。

人工见 [testing.md](testing.md) 的浮窗条目，重点：

- 标题栏 `↗`/`→` 布局、图片 preview 的 `@` 仍在；
- 外部注释开关关闭时看不到复制入口，开启后同会话可用、重开断链；
- 文字 preview 删除/撤销/保存重开后链接行为。
