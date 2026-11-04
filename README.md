# PDF Reader

一个用 Rust 写的本地 PDF 阅读器，支持**把注释写回 PDF**。渲染核心不依赖 UI（`pdfcore`），
PDFium 只出现在薄适配层（`backend-pdfium`），桌面界面用 egui/eframe（`app`）。

```
crates/pdfcore          trait / 几何 / 会话 / 布局 / 渲染调度，零依赖
crates/backend-pdfium   PDFium 适配、注释读回、写回（子进程 FFI）、整理副本
crates/app              egui 界面 + 回放器 + 设置/存档/摘要
```

## 功能

- **阅读**：连续/横向滚动、缩放、旋转、单页/连续、选字复制、搜索、大纲、页内链接、
  引用点、浮窗截图、全屏、最近文件与阅读位置（页 + 锚点）恢复。
- **注释**：高亮 / 下划线 / 删除线 / 墨迹 / 便签；正文、作者、时间；颜色/透明度/线宽；
  撤销/重做；侧栏按页/最近修改浏览、搜索、筛选。
- **写回**：注释直接写进 PDF（`/NM`、`/Contents`、`/AP` 等），Okular 等可读可改；
  读回按 `/NM` 恢复 id。外部注释只读几何/颜色，可改正文/删除。
- **保存语义**：保存 / 另存为 / 覆盖保存共用一条写回路径；未保存时关闭/切换文件会提示；
  外部修改冲突时只能覆盖保存或另存为。
- **整理副本**：去掉历史修订、生成更小的单段 PDF（`--compact`）；不覆盖源文件。
- **自定义数据**：文字/图片 preview、链接、引用点尺寸 → 同目录 `<pdf>.json`。
- **文字浮窗 Markdown**：顶栏全局开关；注释/便签/文字 preview 的阅读视图，正文可滚动、
  链接只提示；单换行按笔记语义保留；`$...$` / `$$...$$` LaTeX 数学按 PNG 纹理渲染；
  正文编辑用可调整大小的正式窗口（内部可滚动）；图片浮窗与写回路径不变。
- **设置**：窗口/侧栏、注释默认样式、每文件阅读状态/最近文件、顶栏显示开关、原地保存备份。

## 快速开始

依赖：Rust ≥ 1.99（LaTeX 数学的 `latex-rust` 要求），以及 PDFium 动态库（仓库不提交二进制，用脚本下载）。

```bash
./scripts/fetch-pdfium.sh          # → vendor/pdfium-<平台>/lib/
cargo run -p app --                 # 空状态启动，拖入 PDF 或 Ctrl+O 打开
cargo run -p app -- some.pdf       # 直接打开
```

Windows（PowerShell）：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\fetch-pdfium.ps1
cargo run -p app -- some.pdf
```

找不到动态库时提示里会列出尝试过的路径；`app --selftest` 可单独验证能否加载 PDFium。

## 构建与打包

```bash
cargo build --release -p app           # target/release/app
./scripts/package.sh                   # → dist/pdf_reader_<平台>/{app, lib/pdfium, BUILD-INFO.txt}
```

打包产物自带 PDFium（`lib/` 与可执行文件同级），不需要 rpath / `LD_LIBRARY_PATH`。
只能在目标平台上打包（Windows 用 `scripts\package.ps1`）。

## 质量门禁

```bash
./scripts/check.sh
```

`fmt --check` → `clippy -D warnings` → `cargo test --workspace` → 回放结构基线比较 → 文档链接检查。
当前约 **489 个测试**；PDFium 就绪时会跑真实样本。

单独跑回放：

```bash
cargo run -p app -- --replay scripts/replay/tiling_800.json --out /tmp/run.jsonl
cargo run -p app -- --replay scripts/replay/tiling_800.json --timing
```

整理副本：

```bash
cargo run -p app -- --compact in.pdf out.pdf
```

## 环境变量

| 变量 | 作用 |
| --- | --- |
| `PDFIUM_PATH` | 指定 PDFium 库文件或所在目录 |
| `PDF_READER_DIAGNOSE=1` | 运行期诊断（贴图/画法/波段/纹理与位图占用） |
| `PDF_READER_TILE_BORDERS=0` | 关掉瓦片红边（默认开启，用于调试分块错位） |
| `PDF_READER_TEXTURE_BUDGET_MB` | GPU 纹理预算（默认 256 MiB，软上限） |
| `PDF_READER_BITMAP_BUDGET_MB` | CPU 位图缓存预算（默认 512 MiB，0 = 不限制） |
| `PDF_READER_DEV_ENTRY=1` | 显示验收辅助入口（截图 / 测试浮窗） |
| `PDF_READER_WRITEBACK_EXE` | 测试用：指定写回子进程可执行文件 |

## 文档

- [docs/README.md](docs/README.md)：文档地图
- [docs/status.md](docs/status.md)：当前能力、门禁状态、已知限制
- [docs/architecture.md](docs/architecture.md)：分层、线程、坐标系、**不变量清单**
- [docs/roadmap.md](docs/roadmap.md)：未完成/暂缓/明确不做
- [docs/app-ui.md](docs/app-ui.md) · [docs/rendering.md](docs/rendering.md) ·
  [docs/persistence.md](docs/persistence.md) · [docs/readback.md](docs/readback.md) ·
  [docs/writeback.md](docs/writeback.md) · [docs/diagnostics.md](docs/diagnostics.md) ·
  [docs/testing.md](docs/testing.md)

旧的设计/评审文档与决策日志已从工作区移除，需要时从 git 历史取：
`git log --all -- docs/`、`git show <commit>:docs/...`。

## 许可

MIT OR Apache-2.0

LaTeX 数学使用 [latex-rust](https://crates.io/crates/latex-rust)（MIT OR Apache-2.0），
其内嵌的 STIX Two Math 字体为 SIL OFL 1.1；发布包需一并保留其许可声明。
