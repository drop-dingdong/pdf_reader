# 测试与验收

> 分工（见 [agent.md](../agent.md)）：**逻辑层必须自动化**；依赖真实窗口/GPU/人工观察的项
> 交给用户验收，并写清“看什么、怎么看”。

## 1. 命令

```bash
./scripts/check.sh           # fmt --check → clippy -D warnings → test → 回放基线 → 文档链接
cargo test --workspace         # 全部（PDFium 就绪时含真实样本）
cargo test -p pdfcore         # 核心：几何/布局/分块/波段/调度/缓存/会话
cargo test -p app           # UI 纯逻辑 + 存档 + 设置 + 联调
cargo test -p backend-pdfium      # 真实后端（缺动态库时静默跳过，见 §4）
cargo test -p pdfcore --lib tiling   # 单模块
```

## 2. 分层

| 层 | 位置 | 守什么 |
| --- | --- | --- |
| `pdfcore` 单元 | `crates/pdfcore/src/**/tests` | 坐标/旋转往返、布局/锚点、`TileGrid`、波段、LRU、调度器、文本/搜索 |
| `pdfcore` 装配/运行期 | `crates/pdfcore/tests/{assemble,runtime}.rs` | 命令/事件链、同波段零重渲染、取消、真实调度线程与缓存 |
| 后端单元 | `crates/backend-pdfium/src/**/tests` | 旋转映射、设备矩形整像素/越界校验、xref/patch 解析 |
| 真实 PDF | `crates/backend-pdfium/tests/*.rs` | 页几何/`/Rotate`、密码、文本盒、**块渲染 == 整页裁剪**、写回/读回、注释元数据、整理副本 |
| UI 纯逻辑 | `crates/app/src/view.rs` 测试 | `plan_page` 判据、可见块、预览/底料选择、拼合、`KeyedStore`、浮窗几何、摘要格式 |
| 存档/设置 | `crates/app/src/{persist,settings}.rs` 测试 | `.pdf.json` 往返与迁移、`settings.json` 往返/坏文件/夹取 |
| 历史/编辑 | `crates/app/src/{history,app}.rs` 测试 | 撤销/重做、保存点脏位、注释编辑、模式记忆、对象号 reconcile |
| UI 联调 | `crates/app/tests/*.rs` | 真实 `DocumentSession` + `view`、注释往返、replay 脚本、CLI 写回 |
| 回放 | `crates/app/src/app/replay.rs` + `scripts/replay/` | 驱动真实 `App::ui`，逐帧检查绘制输出 |

测试数量会变，以 `cargo test` 输出为准；当前约 450 个。

## 3. 基础设施

- `pdfcore::testkit`：`MemoryBackend`/`MemoryRaster`/`MemoryTextLayer`/`ImmediateScheduler`/`MemoryCache`
 等替身；`MemoryRaster` 执行与真实后端相同的尺寸/块矩形校验。
- 真实 PDF 夹具在 `crates/backend-pdfium/tests/common/mod.rs` 现场生成（含 `/Rotate 90`、
 大纲/链接、多页），仓库里没有二进制夹具。
- 回放样本：`app::replay::samples` + `scripts/replay/*.json`；错误样本不留在仓库。

## 4. PDFium 缺失

真实后端用例开头 `let Some(backend) = backend() else { return };`，找不到动态库时静默跳过。
准备：`scripts/fetch-pdfium.sh`。CI 上没库时这些用例“绿得没有意义”，门禁脚本会提示。
`app --selftest` 可单独验证加载。

## 5. 回放器（`--replay`）

```bash
cargo run -p app -- --replay scripts/replay/tiling_800.json --out /tmp/run.jsonl
cargo run -p app -- --replay scripts/replay/tiling_800.json --timing
```

- 确定性来自 `Clock::manual()`（每帧推进 20ms）+ 诊断缓冲；动作“稳定”= 连续 3 帧无在飞渲染、
 不在预览窗口、纹理/画法签名不变。
- 断言（5 条）：已清晰区域不变白；一次波段切换只翻一次；预览尺寸 == 区域 × 分辨率；
 可见区域不露白；不提前切瓦片。
- 输出 JSONL：`frames/settled/cpu_ms/upload_bytes/textures/violations/notes/previews/diag`；
 退出码 `0` 通过、`1` 违规、`2` 脚本/打开失败。
- 测不到：真实 GPU 上传/显存、字体、DPI、浮窗/侧栏/编辑等交互。

性能基线：`--timing --record perf/<脚本>.json`，之后 `--compare`；结构指标（提交数、纹理峰值、
预览尺寸、上传字节 ±10%）作门禁，时序只做同机前后对比。`perf/` 不入库。

## 6. 人工验收清单（当前）

> 准备：`scripts/fetch-pdfium.sh`；`cargo run -p app -- <文件.pdf>`；
> 需要日志加 `PDF_READER_DIAGNOSE=1`（见 [diagnostics.md](diagnostics.md)）。

### 6.1 基础与渲染

| # | 操作 | 预期 |
| --- | --- | --- |
| 1 | 打开 / 拖入 PDF（含空状态拖入） | 显示第一页；页数/缩放正确 |
| 2 | 滚动、翻页、缩放、旋转、横向布局/横向滚动 | 跟手；新页不卡；750% 以上无错位/重影 |
| 3 | 跨波段连续缩放后停止 | 接缝不错开；波段内换比例不重新渲染（日志无新 `[瓦片]`） |
| 4 | 800% 拖动 | 新区域先糊后清晰；不露白；已清晰区域不退回白 |
| 5 | 加密 PDF | 密码框；输错不崩溃 |
| 6 | 全屏 / 恢复 | 顶栏内容一致；指针靠近顶部才出现 |

### 6.2 阅读与预览

| # | 操作 | 预期 |
| --- | --- | --- |
| 7 | 拖选复制、双击选词、搜索、大纲/链接跳转 | 高亮/居中对齐/命中计数正确 |
| 8 | 悬停带落点链接、点击固定、滚动经过 | 悬停出预览；滚动/画墨迹时不弹；固定浮窗可置顶/拖动/缩放/关闭 |
| 9 | 文字/图片 preview、截图 Enter/Esc、Pin 状态 | 保存题目/正文；截图浮窗自包含；Pin 不因翻页/缩放退出 |
| 10 | 改浮窗尺寸后重开侧栏 | 尺寸保持（来自 `.pdf.json`） |
| 10a | 顶栏「Markdown」开/关，打开文字 preview/注释浮窗 | 开关立即改变所有文字浮窗；图片浮窗不变；重启后开关保持 |
| 10b | Markdown 长文/短文滚动、拖滚动条、标题栏拖动、右下角缩放 | 正文滚动不带动 PDF；短内容的横向滚动条也在浮窗底部；标题栏可拖；滚动条不压缩放手柄；滚轮在浮窗上只滚浮窗 |
| 10c | Markdown 链接、标题栏 ✎、双击正文、外部链接 | ✎ 稳定打开正式编辑窗口（可调整大小、可滚动）；正文仍可双击选词；外部链接只在状态栏提示、不打开浏览器；保存后显示更新且浮窗尺寸不变 |
| 10d | 在浮窗里键入三列表格 `\|a\|b\|c\|` | 表格从左开始、落在浮窗内；简单表格不出现内部横向滚动；改浮窗大小后重新排版 |
| 10e | 键入行内 `$x^2$`、块级 `$$\frac{1}{2}$$`、矩阵/积分 | 公式以清晰 PNG 显示；行内不破坏行高，块级在正文中居中；滚动不重排卡顿 |
| 10f | 键入错误公式 `$\frac{1}{$`、`$\text{中文}$`、`$$\n\n~，\n1~\n$$` | 回退为原文文本，不崩溃；状态栏提示原因，其他消息可顶替；`$$` 块内空行不应被误判为删除线；修正后重新渲染 |
| 10g | 长正文点 ✎ 编辑、拉大/拉小窗口、滚动、关闭再打开另一段文本 | 编辑区填满窗口、按钮固定在底部；单行不出现无谓滚动条；不同文本不继承上次滚动位置；取消不改内容；保存后一条撤销 |
| 10i | 正文改动未保存时关闭浮窗/切换文件 | 弹出「正文编辑尚未保存」：保存并继续 / 放弃修改 / 取消；取消留在编辑器 |
| 10h | 顶栏「?」Markdown/数学帮助 | 显示 Markdown 与数学语法/支持/限制/当前后端；错误信息与帮助描述一致 |

### 6.3 注释

| # | 操作 | 预期 |
| --- | --- | --- |
| 11 | 高亮/下划线/删除线拖选；墨迹拖画；便签点击输入 | 几何正确；可撤销/重做 |
| 12 | 点注释看浮窗、双击正文编辑、失焦提交 | 正文写入 `/Contents`；作者/时间显示本地时区 |
| 13 | 右键改颜色/透明度/线宽；Ctrl/右键移动墨迹 | 生效且一条撤销；外部墨迹拒绝移动并提示 |
| 14 | 便签/标记/墨迹模式来回切换 + Esc | 不高亮无关阅读模式；Esc 回到最初阅读模式 |
| 15 | 侧栏按页/最近修改、搜索正文/作者、筛选、页头折叠 | 列表与画布一致；外部/自有/内联标记正确 |
| 16 | 保存 → 重开 → Okular 查看 | 注释可见可改；id 不重复；再保存不叠对象 |
| 17 | 移动墨迹 → 保存 → 撤销 → 保存 | 回到原位（不消失）；重做回移动位置 |
| 18 | 保存后 Okular 修改正文/便签，再回本程序保存 | 修改保留；同一 id 取最后一份 |

### 6.4 保存、提示与设置

| # | 操作 | 预期 |
| --- | --- | --- |
| 19 | 只改 preview → 保存 | 只写 `.pdf.json`（PDF mtime 不变）；弹窗只提数据 |
| 20 | 只改注释 → 保存 | 只写 PDF；弹窗列注释计数；备份开启时说明 `.bak` |
| 21 | 两者都改 → 保存 | 弹窗两段都列；状态栏两段都报 |
| 22 | 脏状态关窗口 / 最近打开 / 拖放 | 提示：保存并继续 / 另存为 / 不保存 / 取消；再次点 X 不绕过 |
| 23 | Okular 改原文件 + 本程序有注释改动 → 关闭 | 提示里直接显示冲突，按钮变“覆盖保存…” |
| 24 | 保存失败/冲突后点保存 | 对话框不关闭，错误显示在框内；可在框内改走另存为 |
| 25 | 另存为到已存在文件（不勾覆盖 / 勾覆盖） | 不勾报错；勾选替换；目标是原文件时按原地写回 |
| 26 | 备份开关默认关 / 开 | 默认不产生 `.bak`；开启后原地保存产生一份 |
| 27 | 五个顶栏开关 + 注释默认样式改后重启 | 全部保持 |
| 28 | 打开带 `.pdf.json` 的干净文件 | 不脏；关闭不提示 |
| 29 | 打开带旧 `.annot.json` 的文件 | 旧文件被忽略（无迁移、无 `.annot.json.bak` 新产物） |

## 7. 交付前自检

- [ ] `./scripts/check.sh` 全绿
- [ ] 改动涉及的文档已同步（architecture 不变量、app-ui、status/roadmap）
- [ ] 需要真机的项已在回复里列清“看什么、怎么看”

## 8. 内联注释样本

`python3 scripts/make_inline_sample.py [out.pdf]` 生成含直接 `/Annots` 字典的高亮注释，
用于验证外部内联注释的正文编辑/删除。
