# `view.rs` 拆分评估

> 状态：2026-11-07 评估；2026-11-09 **只拆测试已实施**。生产模块拆分仍暂缓。
> 当前行为以 [architecture.md](architecture.md) 为准。

## 1. 结论摘要

`view.rs` 拆分后：生产 **2008 行**，测试在 `view/tests.rs` **2245 行**（73 个
`#[test]`），合计约 4253 行。拆分前为单文件 4260 行。

所以第一问题不是“生产逻辑太散”，而是**测试和实现挤在同一个文件**：

- 查找生产函数要越过 2k 行测试；
- 新增一个测试很容易继续把文件撑大；
- 但生产函数本身大多是无窗口纯函数，拆分收益主要是**导航**，不是解耦。

**已按建议完成“只拆测试”：测试移到 `view/tests.rs`；生产模块拆分暂缓，
等实际维护痛点再评估。** 如果之后仍觉得生产部分难找，
再按 §5 的领域模块逐步拆，优先 `store.rs` / `links.rs` / `coords.rs`。

全量拆分（生产 + 测试同时重排）不建议现在做：会制造大量机械 diff，
而 `view.rs` 的测试是当前渲染/坐标/缓存行为的主要安全网。

## 2. 现状

| 指标 | 当前 |
| --- | --- |
| `view.rs` 总行数 | 4253 |
| 生产代码 | 2008 行 |
| 测试 | 2245 行（`view/tests.rs`） |
| `#[test]` | 73 |
| 外部使用方式 | `use crate::view::{...}` / `use crate::view::*`，大量 `app/*` 直接引用 |
| 依赖 | `egui`、`pdfcore` 的纯类型；无窗口/GPU |

生产段的大致内容：

| 行段 | 内容 |
| --- | --- |
| 1–213 | 视图/内容坐标、键盘/滚轮/缩放命令、链接框可见性、目标命令 |
| 214–345 | `SnapshotPage` + `compose_snapshot` 快照拼合 |
| 346–700 | `Preview`/`PageDraw`/`PlanStamp`/`DrawInput`、`plan_page`、区域快照 |
| 701–1058 | 浮窗几何/尺寸、页取舍、缩放文本、时间/提示文案、`overflow_layout` |
| 1059–1222 | 选区/veil、页面↔屏幕、浮窗夹取、dest 点量化/索引、滚轮归属 |
| 1223–1287 | `color_image`、`wheel_targets_*` |
| 1288–1563 | `PageShape`/`PageFacts`/`TextureClass`/`Desired`/`desired_textures`、`texture_pages` |
| 1564–1947 | `KeyedStore<V>` 与 `texture_bytes`（约 380 行） |
| 1948–1997 | `FloatLinkTarget` / `parse_float_link` |
| 1998–4260 | `mod tests`（73 个测试 + 辅助函数） |

## 3. 方案比较

| 方案 | 内容 | 收益 | 风险/成本 |
| --- | --- | --- | --- |
| A. 维持现状 | 不改 | 零回归 | 4.2k 文件；测试继续增长 |
| B. 只拆测试（推荐先做） | `view.rs` → `#[cfg(test)] mod tests;`，测试移到 `view/tests.rs` | 生产文件约 2k；测试仍集中；无生产 API 变化 | 1–2 小时；纯移动；`use super::*` 保持 |
| C. 生产按领域拆 | `view/mod.rs` + 子模块 + 全量 re-export | 每模块 200–600 行；职责清晰 | 1–2 天；机械 diff，测试/私有可见性需处理 |
| D. B+C 一次做 | 全部重排 | 一步到位 | 2–3 天；回归/冲突面最大，不建议 |

## 4. 为什么先拆测试

- 测试全是 `mod tests`，移动边界非常明确；
- 生产函数在测试中通过 `super::*` 使用，不需要改调用方；
- 代码行为零变化，`cargo test`/回放基线可直接兜底；
- 完成后再决定生产模块是否值得拆，避免“测试+生产同时动”的定位困难。

实现方式：

```rust
// view.rs 末尾
#[cfg(test)]
mod tests;
```

把现有 `#[cfg(test)] mod tests { ... }` 的内容原样移到 `crates/app/src/view/tests.rs`。
注意：

- `view.rs` 与 `view/tests.rs` 可以共存（子模块文件）；
- 测试里的辅助函数（`placement`、`key`、`facts` 等）一起移动；
- 不改变任何 `pub` 函数、不调整 import 顺序，先只移动。

## 5. 生产拆分的候选结构

如果测试拆完后仍然难找，建议按领域拆，`view/mod.rs` 只做 re-export：

```text
view/
  mod.rs          // pub use coords::*; pub use snapshot::*; ... + 公共常量
  types.rs        // Preview/PageDraw/DrawInput/PlanStamp/PageShape/... 共享类型
  coords.rs       // view_rect_for / page_rect_to_screen / tile_* / veil_bands
  snapshot.rs     // SnapshotPage / compose_snapshot / compose_region_snapshot
  plan.rs         // plan_page / needs_underlay / preview_render_scale
  textures.rs     // texture_pages / desired_textures / PageFacts
  store.rs        // KeyedStore / texture_bytes
  floats.rs       // 浮窗几何/尺寸/查找/夹取/dest 点
  links.rs        // FloatLinkTarget / parse_float_link
  ui.rs           // 键盘/滚轮/缩放、文案、时间、outline、overflow_layout
```

依赖方向：

```text
types ← coords ← snapshot / plan / floats
types ← textures ← plan
store 独立
links 独立
ui 依赖 coords/types
mod.rs 只 re-export，不写业务
```

规则：

- `mod.rs` 用 `pub use module::*;` 保持 `crate::view::xxx` 外部路径不变；
- 共享类型放 `types.rs`，避免 `plan`/`snapshot`/`textures` 互相循环；
- 不新增 `unsafe`、不引入 `Deref` 伪装；
- 每步只移动代码，不同时改行为/改测试内容；
- 每步独立提交，`./scripts/check.sh` + 回放基线兜底。

## 6. 风险与处理

| 风险 | 说明 | 处理 |
| --- | --- | --- |
| 外部 import 断裂 | 大量 `use crate::view::{...}` | `mod.rs` 全量 re-export；先编译 |
| 私有可见性 | 测试/子模块可能访问私有函数 | 先拆测试；生产拆分时把跨模块函数改 `pub(super)` 或按模块测试 |
| 循环依赖 | `plan`/`snapshot`/`textures` 互相用类型 | 共享类型放 `types.rs` |
| 机械 diff 冲突 | 文件大，移动多 | 一次只移动一组；测试单独提交 |
| 行为回归 | 纯逻辑容易被“顺手整理”改坏 | 只移动；不改 body；check/replay |
| 测试定位 | 拆生产后测试仍集中在一个文件 | 可选按模块拆测试，但不与生产拆同步做 |

## 7. 迁移步骤（若继续）

1. **只拆测试**：已完成，生产 2008 行；`view/tests.rs` 2245 行。
2. `store.rs`：`KeyedStore` + `texture_bytes`，独立、低风险；提交。
3. `links.rs`：`FloatLinkTarget` + `parse_float_link`，独立；提交。
4. `coords.rs`：坐标/视图/页面换算；提交。
5. `snapshot.rs` + `types.rs`；提交。
6. `plan.rs` / `textures.rs`；提交。
7. `floats.rs` / `ui.rs`；提交。
8. 全部完成后更新 [architecture.md](architecture.md)、[app-ui.md](app-ui.md)、[status.md](status.md)。

每步要求：

- `./scripts/check.sh` 全绿（当前 525 测试）；
- 10 个回放基线一致；
- 不改变任何公开函数签名/行为；
- 独立 commit，便于回滚。

## 8. 决策

**当前建议：先做 B（只拆测试），C 暂缓。**

理由：

- `view.rs` 的生产部分仍然是一个有内聚性的纯逻辑层，当前主要痛点是测试占了一半文件；
- 生产拆分收益是导航，但会触碰大量 `app/*` import、测试和私有可见性；
- 先拆测试能以极低风险把 4.2k 降到约 2k，再根据实际痛点决定是否继续。

触发继续 C 的条件：

- 测试拆完后生产文件仍经常需要长距离跳转；
- 新增功能继续集中在 `plan`/`textures`/`coords` 某一块；
- 出现“改一个坐标/缓存逻辑要跨半个文件”的实际维护成本。

## 9. 附：计数来源

- 行数：`wc -l crates/app/src/view.rs`；
- 测试数：`grep -c '^    #\\[test\\]'`；
- 生产/测试分界：`#[cfg(test)] mod tests` 从 1998 行开始；
- 外部引用：`crates/app/src/app/*` 中的 `use crate::view::{...}`。
