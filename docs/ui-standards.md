# UI 样式与交互规范

> 状态：2026-11-07 共识已确定；**第 1 步（hover 固定 + dest goto）已实施，
> SVG/侧栏/主题尚未实施**。本文是后续新增按钮、侧栏行、图标、主题/颜色时的默认依据；
> 如果新需求与本文冲突，先改本文再改代码。
>
> 当前代码仍使用部分 Unicode 图标、默认跟随系统的主题和未统一的侧栏行；
> 实际行为以 [app-ui.md](app-ui.md) 为准，本文描述的是**目标约定**。

## 1. 已确认的决策

| 事项 | 共识 |
| --- | --- |
| dest 浮窗 | 增加 `→` goto；跳到 `DestMark.point`（链接目的地），不是截图 rect 中心；暂不提供 `↗` 链接复制 |
| 临时 hover 浮窗 | 点击/拖动应固定为正式浮窗；这是待修 bug，先于样式调整处理 |
| 图标 | 全部改用内置 SVG 矢量图，不再依赖生僻 Unicode 字符；允许 ASCII `@` 等少数稳定字符，但同类图标应统一 |
| 默认主题 | 浅色；`settings.ui.dark_mode: bool`，`false=浅色`、`true=深色`，默认 `false` |
| 主题来源 | 不依赖系统主题；启动时显式 `ctx.set_theme(...)`，菜单切换立即生效 |
| 侧栏行 | 统一按自定义 preview 行：整行一个交互区域，右侧透明 `⋯`，右键整行弹菜单 |
| 页面背景色 | **本轮不做**；透明 PDF 的白底/背景色设置留作后续独立事项 |
| 全量多级状态树 | 不做；Phase 3 只保留 Runtime/SaveFlow/Markdown/AnnotationPdf 四组 |

## 2. 图标规范（SVG）

### 2.1 资源与尺寸

- 目录：`crates/app/assets/icons/`；
- 文件：单个 SVG，`viewBox="0 0 16 16"` 或 `0 0 18 18`，透明背景；
- 单色：使用占位填充（如 `#000000`），由渲染层替换为主题色；
- 不嵌入文字、渐变、滤镜、外链字体；
- 默认绘制尺寸 16pt；浮窗标题按钮使用 18×18 点击区，图标 13–14pt；
- 图标资源用 `include_bytes!`/`include_str!` 打进二进制，不运行时读文件。

首批图标：

| 名称 | 用途 |
| --- | --- |
| `close` | 浮窗/对话框关闭 |
| `edit` | 编辑正文/标题 |
| `link` | 复制浮窗引用链接 |
| `goto` | 跳到来源页/位置 |
| `more` | 侧栏 `⋯` 菜单 |
| `text-preview` | 文字 preview 类型 |
| `image-preview` | 图片 preview 类型 |
| `delete` | 删除注释/preview |
| `image-ref` | 复制图片引用（可选；当前 `@` 是 ASCII，可暂留） |

### 2.2 渲染与缓存

建议新增 `crates/app/src/app/icons.rs`：

```text
IconId -> SVG bytes -> resvg 光栅化（size × pixels_per_point，按主题色替换 fill）
      -> egui TextureHandle
```

- 缓存 key 至少包含：`IconId + 逻辑尺寸 + 颜色 + pixels_per_point`；
- 缓存放在 `PdfReaderApp` 的 UI/资源状态中，不要每帧 `load_texture`；
- `resvg` 已经是项目依赖（数学 SVG 在用），不新增依赖；
- 标准 `egui::Button` 能显示纹理时用 `Button::image_and_text`/自定义 `IconButton`；
  浮窗标题的小按钮用 `Painter::image`；
- 图标按钮必须有 `on_hover_text`；禁用态用 `add_enabled` 或统一的不透明度；
- 不在新代码里使用 `CUSTOM_SYMBOL_*` 这类生僻 Unicode 常量；已有常量在迁移完成后删除，
  `view.rs` 的测试一并更新。

### 2.3 跨平台验收

Windows 和 Linux 都需人工看一眼：

- 关闭、编辑、链接、goto、更多、文字/图片类型图标都有字形；
- 深浅主题下图标对比度足够；
- 高 DPI（125%/150%）不模糊、不偏移。

## 3. 浮窗标题按钮

### 3.1 位置算法

从右到左固定槽位：

```text
close: right - 13
slot 1: close_center.x - 26
slot 2: close_center.x - 52
slot 3: close_center.x - 78
```

按浮窗类型填充，**不留空槽**：

| 浮窗类型 | 槽位顺序 |
| --- | --- |
| 注释 | close → edit → link → goto |
| 文字 preview | close → edit → link → goto |
| 图片 preview | close → `@`（图片引用）→ link → goto |
| 引用点/dest 浮窗 | close → goto |
| 外部注释且未开启外部链接 | close → edit → goto（没有 link 槽） |
| 截图/测试浮窗 | 仅 close |

标题可用宽度：

```text
reserved = 54 + 26 * (实际额外按钮数)
title_width = title_rect.width() - reserved
```

新增按钮时先确认它属于哪个槽位；不要在代码里再写一套固定 `-26/-52/-78`。

### 3.2 行为

- `close`：关闭浮窗；正文有未保存草稿时走现有确认；
- `edit`：打开正文编辑器；没有文字内容时不显示；
- `link`：复制 `pd-float://` 链接；只有注释/自定义 preview 有；
- `goto`：注释/自定义 preview 跳来源；dest 浮窗跳 `mark.point`；
- `@`：复制 `pd-img` 图片引用，仅图片 preview；
- 所有按钮点击/拖动都受 `interactive`（指针不在编辑窗口/弹层上）约束；
- 标题栏按钮不抢正文双击；按钮在标题交互之后注册。

### 3.3 dest 浮窗

- 不加 `↗`，因为 `pd-float://dest/<id>` 尚未定义；
- `→` 使用 `float_goto_target` 的 `(page, mark.point)`；
- 临时 hover 浮窗点击固定的 bug 在加 goto 前修复，并补回归测试。

## 4. 侧栏行规范

### 4.1 统一结构

两类行都使用同一 anatomy：

```text
[整行 response: click / hover / secondary-click]
  [类型图标 16] [标题/摘要，单行截断]     [页数文本] [⋯ 透明按钮]
```

- **整行**是单击/hover/右键的区域；
- `⋯` 是透明的次要按钮，只负责打开菜单，不单独形成一块 hover 背景；
- 右键菜单挂在**整行 response** 上，不是只挂在左侧 label 上；
- hover 时整行背景变化；文字颜色不单独只变左侧；
- 行高、左右内边距、页数文本宽度由同一个 row helper 决定；
- 注释行保留“按页分组/类型筛选/最近修改”等现有结构，只替换行内交互与样式。

### 4.2 菜单

菜单由 `⋯` 和整行右键共用：

| 行类型 | 菜单项 |
| --- | --- |
| 文字 preview | 复制引用链接、跳到位置、编辑标题、删除条目 |
| 图片 preview | 复制引用链接、跳到位置、编辑标题、复制图片引用、删除条目 |
| 注释 | 复制引用链接（外部注释按设置禁用）、跳到位置、删除注释 |

- 外部注释不可复制链接时用禁用项 + tooltip，不隐藏到无法理解；
- 菜单项顺序固定，删除放最后；
- 禁止再给行内加第二个图标按钮；新增功能优先进菜单。

### 4.3 新增侧栏

1. 先确认是**新标签页**还是现有列表的新行类型；
2. 新行必须复用 row helper，不得复制一套不同的 hover/右键逻辑；
3. 新标签页要加 `SidebarPane` 枚举、面板入口和持久化（如需要）；
4. 行菜单项按 4.2 的顺序扩展；
5. 同步 [app-ui.md](app-ui.md) 与本文。

## 5. 主题与颜色

### 5.1 主题设置

```json
"ui": { "dark_mode": false }
```

- `false` = 浅色，`true` = 深色，默认 `false`；
- 启动时显式设置：

```rust
ctx.set_theme(if settings.ui.dark_mode {
    egui::Theme::Dark
} else {
    egui::Theme::Light
});
```

- 顶栏提供「样式 ▾」菜单：浅色/深色两个选项；
- 切换后立即 `set_theme`，并同步 `settings_dirty`；
- 不依赖 `ThemePreference::System`：Linux 下 KDE 主题主要影响窗口标题栏，
  egui 内容是否跟随系统并不保证。

### 5.2 颜色角色

新增/修改颜色时，先映射到语义角色，再取 `ui.visuals()` 或统一 palette：

| 角色 | 用途 |
| --- | --- |
| `panel_bg` | 顶栏/侧栏/对话框背景 |
| `canvas_bg` | 画布空白背景 |
| `text` / `weak_text` | 正文/次要信息 |
| `border` | 浮窗/面板边框 |
| `hover_bg` | 行/按钮 hover |
| `accent` | 选中、链接、活动状态 |
| `danger` | 关闭/删除/错误 |
| `warning` | 高亮提示、提醒 |
| `float_bg` / `float_title` | 浮窗自绘背景/标题栏 |

规则：

- 新代码禁止直接写 `Color32::from_rgb(...)` 作为主题色；必须通过语义角色/`ui.visuals()`；
- 深浅主题都要检查对比度；
- 页面纸张背景色是**独立概念**，不跟 UI 深浅主题绑定（本轮不做）；
- 主题切换后要检查自绘浮窗、侧栏、状态栏、按钮，不要只检查 egui 原生控件。

### 5.3 颜色审计

当前 `app/floats.rs`、`app/sidebar.rs`、`app/paint.rs` 等仍有数十处硬编码颜色。
主题实施时：

1. 先列出现有硬编码颜色与用途；
2. 把能映射到 `Visuals` 的换成语义角色；
3. 浮窗深色外观保留独立 palette，但提供浅色版本；
4. 新增颜色必须先加角色，不能散落；
5. 跨 Windows/Linux 人工评审后，再改默认主题行为。

### 5.4 诊断

仅在 `PDF_READER_DIAGNOSE=1` 时，启动后打印一次：

```text
[主题] theme=Light dark_mode=false panel_fill=... window_fill=... text=...
```

用途是对照 Windows/Linux 的实际取色，不进入正常日志。

## 6. 新增按钮/颜色的操作清单

### 6.1 新增一个按钮

1. 确定是浮窗标题、顶栏、侧栏菜单还是对话框；
2. 选一个 SVG `IconId`，没有就新增资源（尺寸/单色规则见 §2）；
3. 确定语义、tooltip、禁用条件、是否需要确认；
4. 浮窗标题按钮：按 §3 槽位顺序插入，更新 `reserved`；
5. 顶栏按钮：用统一 `IconButton`/`Button`，不要手写 Unicode；
6. 侧栏按钮：默认进 `⋯` 菜单，不加行内图标；
7. 加自动化测试（可测的）和人工验收项；
8. 同步本文 / [app-ui.md](app-ui.md)。

### 6.2 新增一个侧栏

见 §4.3；额外要求：

- 行必须支持整行单击、hover、右键；
- 有分页/分组时保留分组标题；
- 菜单项顺序固定；
- 不引入第二套 `⋯`/hover 实现。

### 6.3 新增一个颜色/主题设置

1. 先定义语义角色（§5.2），不要直接加 RGB；
2. 设置字段加到 `settings.rs` 的 `UiSettings`，同时更新：
   - `Default`；
   - `to_json`；
   - `from_json`（缺字段用默认）；
   - `app/edit.rs::sync_style_settings`；
3. 顶栏/菜单入口；
4. 浅色/深色/跨 OS 验收；
5. 文档：`persistence.md` 设置表、`app-ui.md`、本文。

## 7. 代码位置索引

| 内容 | 位置 |
| --- | --- |
| SVG 资源 | `crates/app/assets/icons/`（新增） |
| 图标渲染/缓存 | `crates/app/src/app/icons.rs`（新增） |
| 浮窗标题按钮/关闭 | `crates/app/src/app/floats.rs` |
| 顶栏按钮/样式菜单 | `crates/app/src/app/toolbar.rs` |
| 侧栏行/菜单 | `crates/app/src/app/sidebar.rs` |
| 页面/浮窗无状态绘制 | `crates/app/src/app/paint.rs` |
| 类型符号常量（待迁移） | `crates/app/src/view.rs` 的 `CUSTOM_SYMBOL_*` |
| 设置字段 | `crates/app/src/settings.rs` |
| 设置同步 | `crates/app/src/app/edit.rs::sync_style_settings` |
| 状态组 | `crates/app/src/app/state.rs` |
| 主题应用/诊断 | 启动构造 + `toolbar.rs` + `PDF_READER_DIAGNOSE` |

## 8. 验收

自动化：

- SVG 解码/缓存尺寸/颜色 key 的单测；
- `dark_mode` 设置默认值/JSON 往返/旧设置缺字段；
- 侧栏行 helper 的整行响应/菜单项（能脱离窗口的部分）；
- 浮窗按钮槽位计算（纯函数）；
- 全量 `./scripts/check.sh`。

人工（Windows + Linux）：

- 关闭/编辑/链接/goto/更多/文字/图片图标均可见；
- 浅色、深色下按钮/浮窗/侧栏对比度正常；
- 两类侧栏行 hover、右键、`⋯` 行为一致；
- 浮窗标题按钮不重叠，外部注释无 link 时 goto 不悬空；
- dest 浮窗 `→` 跳到目的地 point；
- 点击临时 hover 浮窗能固定。

## 9. 非目标/后续

- **页面背景色设置**：本轮不做。透明 PDF 的白底/可配置纸张背景是独立渲染事项，
  需要同时处理页面绘制、截图/自定义 preview 快照和主题关系；触发条件：用户遇到透明
  页面显示异常，或需要非白纸张背景。
- **全量多级状态树**：不做；见 [state-grouping-evaluation.md](state-grouping-evaluation.md)。
- **统一重写所有历史图标**：按模块渐进迁移，不要求一次性替换所有旧 Unicode。

## 10. 实施顺序

1. ~~修临时 hover 浮窗点击固定 bug + dest 浮窗 `→`（point）。~~
   已完成：点击/拖动临时预览可固定；dest `→` 使用 `mark.point`。
2. SVG 图标系统 + 关闭/文字/图片/编辑/link/goto/更多/删除替换；
3. 侧栏两类行统一；
4. 主题 `dark_mode` + 样式菜单 + 颜色审计 + 跨 OS 评审；
5. 页面背景色暂不做。
