# UI 层（现状）

> `app` crate = `view`（纯逻辑，可单测）+ `app`（egui 胶水，需真机）+ `persist`（存档 JSON）。
> 这里的每条"每帧顺序"与"层级"都直接对应代码；改动请同步本文。

## 1. 模块划分

| 模块 | 内容 | 怎么验证 |
| --- | --- | --- |
| [`view.rs`](../../crates/app/src/view.rs) (~2.9k) | 坐标换算、输入映射、`KeyedStore`、画法决策（`plan_page`/`preview_source`/`underlay_source`）、快照拼合（`compose_snapshot`/`compose_region_snapshot`）、浮窗几何、持久化辅助 | `cargo test -p app --lib` |
| [`app.rs`](../../crates/app/src/app.rs) (~4.2k) | egui 胶水：面板、画布、交互、浮窗、侧栏、截图、对话框 | **需要真实窗口 → 人工验收**（见 [`testing.md`](testing.md) §6） |
| [`persist.rs`](../../crates/app/src/persist.rs) (~0.7k) | 存档 JSON（手写序列化 + 极简 JSON 解析）+ 版本迁移 | `cargo test -p app --lib persist::` |
| `main.rs` / `lib.rs` | 启动、拖放、命令行参数 | 人工 |

约定：**能脱离窗口验证的逻辑一律放 `view.rs`**；`app.rs` 只留"读输入、调会话、画"。

## 2. 每帧顺序（`PdfReaderApp::ui`）

```text
1. handle_dropped_files                  拖入文件立刻打开（空状态下也生效）
2. session.take()                        借出会话，避免与其它字段借用冲突
3. collect_commands                     键盘/滚轮 → ViewerCommand（截图模式下丢弃）
4. pending_layout → session.set_layout   纵向/横向切换
5. apply_and_pump                        session.handle(命令) + pump 事件
6. 预览窗口判定（previewing）             缩放/旋转后 160ms 内不请求渲染
7. ensure_textures                       上传整页/块纹理；裁剪到"当前+两档底料"
8. draw_toolbar                          顶栏（三行；全屏时为悬浮 Area）
9. draw_sidebar(ui, &mut session)        侧栏（目录 / 自定义preview 两个标签页）
10. draw_status                          底部状态栏
11. draw_canvas(ui, &mut session)        画布：页面 → 高亮/链接 → 浮窗 → 悬停预览
12. RequestVisible                       提交渲染（预览窗口内跳过）
13. draw_password_prompt / draw_custom_dialog / draw_edit_dialogs
```

画布内部的顺序（决定遮挡关系）：

```text
纸张(白底+页框) → 底料 → 主图层(Preview/Tiles) → 瓦片红线(可选诊断)
→ 选区/搜索高亮 → 链接框 → 引用点 → 内容空间浮窗(draw_floats)
→ 正文原位编辑框 → 临时悬停预览(draw_hover_preview) → 侧栏悬浮浮窗
```

"后画的在上面"：临时预览必须在固定浮窗之后，才能与"点击取消"竞争时赢。

## 3. 面板结构

| 区域 | 内容 |
| --- | --- |
| 顶栏（`Panel::top`，全屏为 `Area`） | 第 0 行：文件路径（中间截断）+ `保存自定义数据`；第 1 行：侧栏开关、＋文字/＋图片预览、Pin悬浮响应；第 2 行：上一页/页码/下一页、缩放比与菜单、顺/逆时针、平移/选字、纵向/横向、横向滚动、链接框/引用点、全屏 |
| 侧栏（拖拽条 + 自绘面板） | 标签页「目录」（虚拟化行）/「自定义preview」（行内 `→` `✎` `❎`） |
| 中央画布 | 见 §2 画布顺序；左侧 5px 拖拽条 |
| 底部搜索栏（中央区域 `Panel::bottom`） | 命中计数、上一条/下一条、输入框 |
| 状态栏 | 页码、缩放、消息、错误 |
| 浮窗层 | 固定浮窗（可拖动/改大小/关闭/点击置顶）+ 临时预览（最上层） |

## 4. 交互状态机

| 状态 | 进入 | 退出 | 行为 |
| --- | --- | --- | --- |
| `DragMode::Pan` / `Select` | 工具栏「平移/选字」 | 切换工具 | 平移 = `ScrollBy`；选字 = `SelectBegin/Extend`（起点不在字上时不做动作） |
| Pin（自定义 preview 取点） | ＋文字/＋图片预览 | **只在切到选字/截图时退出** | 十字光标；点击页面 → 对话框 → 截图（图片预览） |
| 截图模式（`CaptureState`） | 截图按钮 / 图片 preview / 点击引用点 | `Enter` 保存、`Esc` 取消 | 主视图动作全部短路（但 `pump` 继续）；蒙层挖洞；`Enter` 后输出区域信息（stderr） |
| 悬停预览 | 指针在带落点的页内链接 / 侧栏 Pin 行上 | 指针离开（或 250ms 静默窗口内滚动） | 显示目标区域截图；已有同来源浮窗时改为高亮置顶 |
| 原位编辑 | 双击页码/缩放/浮窗标题/浮窗正文 | 失焦或「完成」 | 编辑期间不采集视图键盘命令（`memory().focused()`） |
| 密码框 | `CoreError::PasswordRequired` | 输入正确密码重试 | 模态 |

## 5. 浮窗（`Float`）

* 内容：`FloatContent::{Text, Image}`；`custom: Option<usize>` 记录来源（侧栏 Pin 条目）；
  `origin: Option<(PageIndex, usize)>` 记录来源（链接目的地）。
* **尺寸权威来自浮窗自身**（`Float.body`）：只在创建与拖右下角改大小时写入，
  回写到记录（`body_size`），**不从矩形反算**（避免亚像素误差逐帧漂移）。
* 层级：`floats` 的顺序即 z 序；点击/标题栏点击 → `move_to_last`；临时预览最后绘制。
* 关闭：右上 ✕（有可见记号）；`Esc` 关闭最上层（截图模式除外）。

## 6. 侧栏

* 目录：打开文档时 `view::flatten_outline` 摊平成行，`ScrollArea::show_rows` 只布局可见行。
* 自定义preview：每行 = 类型符号 + 标题（截断）+ 页码 + `→`（跳转）+ `✎`（改题目）+ `❎`（删除）；
  行为**先占位 → 绘制 → 最后在同一矩形上 `ui.interact`**（否则 hover/click 全不生效）。
* 悬浮：有正式浮窗 → 只高亮置顶；没有 → 显示临时预览（可被「Pin悬浮响应」开关关闭）。

## 7. 纹理上传与裁剪

```text
ensure_textures(allow_upload)
  targets = view::texture_pages(可见页 ±1 + 当前页)     // 上传与裁剪用同一集合
  for page in targets（最多 MAX_UPLOADS_PER_FRAME=2 页）:
      upload_page(page)                                // 整页图 + 该页所有已缓存块
  textures.retain_preview_scales(&pages, 当前渲染比例)  // 每页三档
```

`allow_upload = false`（拖动期间）只做裁剪，松手后补齐。

## 8. 与核心的接口（只在会话上）

| 用途 | 调用 |
| --- | --- |
| 布局/视口 | `layout()` / `placement(page)` / `visible_pages(&mut out)` |
| 渲染 | `RequestVisible` 命令、`pending_renders()`、`pump` 由 `apply_and_pump` 负责 |
| 纹理 | `image_for(page)` / `tiles_for(page)` / `tile_grid(page)` / `render_scale(page)` |
| 预览截图 | `region_tiles` / `request_region`（见 [`persistence.md`](persistence.md) §4） |
| 文本 | `text_position_at` / `highlight_selection` / `selected_text` / `search` |
| 导航 | `navigation()`（打开时已解析）、`link_at` |

## 9. 已知债务

* `app.rs` 单体偏大（~4.2k 行、一个 impl）：绘制/侧栏/浮窗/截图/存档胶水混在一处。
  新逻辑优先落 `view.rs`；拆分是待办（见 [`../decisions/log.md`](../decisions/log.md) 的开放项）。
* 上传与像素转换没有字节预算：`MAX_UPLOADS_PER_FRAME` 按**页**计，一页可能一次传几十块。
* 纹理层按条目数限额（128），没有显存预算。
* "决策 → 绘制"没有自动化测试（见 [`testing.md`](testing.md) §5）。
