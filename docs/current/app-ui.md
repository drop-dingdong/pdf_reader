# UI 层（现状）

> `app` crate = `view`（纯逻辑，可单测）+ `app`（egui 胶水，需真机）+ `persist`（存档 JSON）。
> 这里的每条"每帧顺序"与"层级"都直接对应代码；改动请同步本文。

## 1. 模块划分

| 模块 | 内容 | 怎么验证 |
| --- | --- | --- |
| [`view.rs`](../../crates/app/src/view.rs) (~2.9k) | 坐标换算、输入映射、`KeyedStore`、画法决策（`plan_page`/`preview_source`/`underlay_source`）、快照拼合（`compose_snapshot`/`compose_region_snapshot`）、浮窗几何、持久化辅助 | `cargo test -p app --lib` |
| [`app.rs`](../../crates/app/src/app.rs) (~4.2k) | egui 胶水：面板、画布、交互、浮窗、侧栏、截图、对话框 | **需要真实窗口 → 人工验收**（见 [`testing.md`](testing.md) §6） |
| [`ink.rs`](../../crates/app/src/ink.rs) (~0.2k) | 墨迹手势状态机（纯逻辑）：页面空间采样、0.5 抽稀、2000 点上限、收笔判定 | `cargo test -p app --lib ink::` |
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
→ 选区/搜索高亮 → 链接框 → **标注叠加层（高亮/下划线/删除线/墨迹 + 正在画的一笔）**
→ 引用点 → 内容空间浮窗(draw_floats)
→ 正文原位编辑框 → 临时悬停预览(draw_hover_preview) → 侧栏悬浮浮窗
```

"后画的在上面"：临时预览必须在固定浮窗之后，才能与"点击取消"竞争时赢。

## 3. 面板结构

| 区域 | 内容 |
| --- | --- |
| 顶栏（`Panel::top`，全屏为 `Area`） | 第 0 行：文件路径（中间截断）+ `保存自定义数据` + `写入 PDF`（原地，先确认再备份）+
`从 PDF 重建`（S6c，先备份）+ `导出标注 PDF`；第 1 行：侧栏开关、＋文字/＋图片预览、Pin悬浮响应；第 2 行：上一页/页码/下一页、缩放比与菜单、顺/逆时针、平移/选字/**墨迹**、纵向/横向、横向滚动、链接框/引用点、全屏 |
| 侧栏（拖拽条 + 自绘面板） | 标签页「目录」（虚拟化行）/「自定义preview」（行内 `→` `✎` `❎`） |
| 中央画布 | 见 §2 画布顺序；左侧 5px 拖拽条 |
| 底部搜索栏（中央区域 `Panel::bottom`） | 命中计数、上一条/下一条、输入框 |
| 状态栏 | 页码、缩放、消息、错误 |
| 浮窗层 | 固定浮窗（可拖动/改大小/关闭/点击置顶）+ 临时预览（最上层） |

## 4. 交互状态机

| 状态 | 进入 | 退出 | 行为 |
| --- | --- | --- | --- |
| `DragMode::Pan` / `Select` | 工具栏「平移/选字」 | 切换工具 | 平移 = `ScrollBy`；选字 = `SelectBegin/Extend`（起点不在字上时不做动作） |
| `DragMode::Ink`（墨迹工具） | 工具栏「墨迹」 | 抬笔入库；`Esc` 或切工具 → **丢弃整笔** | 按下（`press_origin`）起笔 → 拖动累积**页面空间**点（0.5 抽稀、2000 点上限）→ **抬笔时** `annotate_ink`（一笔 = 一条标注 = 一次撤销）。拖动中只画临时折线，不进标注集合、不产生命令 |
| Pin（自定义 preview 取点） | ＋文字/＋图片预览 | **只在切到选字/截图时退出** | 十字光标；点击页面 → 对话框 → 截图（图片预览） |
| 截图模式（`CaptureState`） | 截图按钮 / 图片 preview / 点击引用点 | `Enter` 保存、`Esc` 取消 | 主视图动作全部短路（但 `pump` 继续）；蒙层挖洞；`Enter` 后输出区域信息（stderr） |
| 悬停预览 | 指针在带落点的页内链接 / 侧栏 Pin 行上 | 指针离开 / 滚动静默窗口 / **正在画墨迹** | 显示目标区域截图；已有同来源浮窗时改为高亮置顶；画墨迹期间一律不弹（笔画可能压住链接，D-121） |
| 原位编辑 | 双击页码/缩放/浮窗标题/浮窗正文 | 失焦或「完成」 | 编辑期间不采集视图键盘命令（`memory().focused()`） |
| 密码框 | `CoreError::PasswordRequired` | 输入正确密码重试 | 模态 |

### 4.1 标注（S1–S4b）

* **几何只在未旋转页面空间**；叠加层在画布上按布局换算后绘制，**不进渲染管线**（不产生 `RenderKey`、不改瓦片）。
* 种类：高亮 / 下划线 / 删除线（文本行矩形）+ **墨迹**（折线 + 页面空间线宽）。新建时文本类默认半透明黄、
  墨迹默认不透明深色（`pdfcore::annotation::{TEXT_MARK_*, INK_*}`）。
* 墨迹绘制（`paint_ink`）：**按页面矩形裁剪**（跨页拖拽时不会画到相邻页的空白上，与 PDF 阅读器一致）；
  折角补半径 = 半线宽的圆点（egui 的折线逐段四边形，直角处会漏缝）。
* 注释正文**直接属于注释**（`Annotation.body`），点击注释打开独立浮窗（`FloatOwner::Annotation`），
  双击正文编辑、失焦提交为一条 `SetBody` 撤销命令；不进侧栏、不写 `.json`。
* P3 起「保存」= 写 PDF + 写 `.json`（如有自定义数据改动）；`.annot.json` 不再生成。
  未保存的新注释关闭/切换文件即丢弃。
* 外部注释也进入主集合：点击打开正文浮窗、可改正文/删除；几何/颜色仍只读。
  正文修改/删除**只存内存**，由「写入 PDF」按对象号落盘；`保存` 不会把它们写进 A。
* 悬停/右键删除**只看几何**，与“有没有正文”无关。
* **正文不新增存档字段**：正文是主存档 `custom_texts` 里标题为 `#annot:<id>` 的条目；点标注打开浮窗，
  引用缺失时显示"正文已丢失"但不影响加载。
* 删除：悬停 + `Delete`，或右键菜单「删除标注」；两者都可 `Ctrl+Z` 撤销。
* 撤销：全局单栈（`Ctrl+Z` / `Ctrl+Shift+Z`）+ 可视历史窗口（点击行只定位，不做状态跳转）；
  历史窗口**虚拟化**（`ScrollArea::show_rows`，行高 18、最大高 360）—— 1000 条编辑时也只排版可见行
  （整表渲染实测一帧 323ms，虚拟化后 ~16ms，D-121）。
* 保存：保存按钮与 `Ctrl+S` 走同一条 `save_all`（先文本后图层，各自临时文件 + rename）。
* 压力门槛：回放 `ink_many.json`（1000 条）+ 结构基线保证"标注不进渲染管线"；
  时间上限只在测试里（debug 宽松值），见 [`testing.md`](testing.md)。
* **导出带标注的 PDF（S5）**：工具栏「导出标注 PDF」→ 同目录 `<stem>.annotated.pdf`（**不覆盖**）；
  主进程组 job、子进程写回、冒烟校验后 atomically rename；细节与排查见 [`writeback.md`](writeback.md)。
* **原地写入（S5c）**：工具栏「写入 PDF」→ 模态确认 → 保存 sidecar → 子进程按 `/NM` 同步写临时文件 →
  关闭会话（释放文件句柄）→ 备份 `<文件名>.bak-<时间>` → 原子替换 → 重开并恢复视图。
  **overlay 保留**（读回 PDF 注释之前，删掉就等于丢编辑能力）。
  允许**空标注**写回：把 PDF 里我们写过的注释全部删掉（界面上删光之后按钮仍可用，
  条件见 `pdf_marked_annotations`）。
* **从 PDF 重建（S6c）**：工具栏「从 PDF 重建」→ 确认框（显示 PDF 里几条标记注释、当前 overlay 几条）→
  备份 `.annot.json` / `.json` → 删除 A、只删 J 里 `#annot:` 正文条目 → 重开（S6a 从 PDF 导入）→ 落盘。
  没写进 PDF 的标注会丢，这是“以 PDF 为准”的语义。

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

## 9. 可测试性接缝（D-103）

| 接缝 | 作用 |
| --- | --- |
| `PdfReaderApp::new_headless(clock, diag, initial)` | 无 `CreationContext` 也能构造（回放/测试） |
| `Clock`（`clock.rs`） | 行为性截止时间（160ms 预览窗口、400ms 兜底、250ms 静默、高亮消退）统一读它；测试注入可推进的手动时钟。**耗时测量**仍用真实 `Instant::now()` |
| `Diagnostics`（`diag.rs`） | 诊断去处：stderr 或内存缓冲（按动作分组） |
| `last_plan: HashMap<PageIndex, PlanStamp>` | 每帧每页的画法快照（无条件记录），`[画法]` 边沿触发与回放断言共用 |
| `KeyedStore::iter()` | 把 egui 的 `TextureId` 反查成 `RenderKey`（回放器据此判断"哪些格子是清晰层"） |
| `view::region_device_rect` | 预览区域的像素矩形（唯一来源：拼图与断言共用） |

## 10. 已知债务

* `app.rs` 单体偏大（~4.2k 行、一个 impl）：绘制/侧栏/浮窗/截图/存档胶水混在一处。
  新逻辑优先落 `view.rs`；拆分是待办（见 [`../decisions/log.md`](../decisions/log.md) 的开放项）。
* 上传与像素转换没有字节预算：`MAX_UPLOADS_PER_FRAME` 按**页**计，一页可能一次传几十块。
* 纹理层按条目数限额（128），没有显存预算。
* "决策 → 绘制"没有自动化测试（见 [`testing.md`](testing.md) §5）。
