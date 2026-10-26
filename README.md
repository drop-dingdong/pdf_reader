# PDF Reader

一个用 Rust 写的 PDF 阅读器：**渲染核心不依赖 UI**（`pdfcore`），PDFium 只出现在一个薄适配层里
（`backend-pdfium`），桌面界面用 egui/eframe（`app`）。

```
crates/pdfcore          trait / 几何 / 会话状态，零依赖
crates/backend-pdfium   pdfium-render 适配层（唯一接触 PDFium FFI 的地方）
crates/app              egui 界面 + 无窗口回放器（回归断言与性能基线）
```

## 特性

- **分块渲染**：高倍缩放下把页面切成瓦片按需渲染，波段划分保证"同一波段内缩放不重渲"（D-94）
- **不露白**：整页底图 → 瓦片预览 → 清晰瓦片的过渡链，缩放/翻页/回滚都有兜底（D-110）
- **可回归**：无窗口回放器驱动真实 `App::ui`，6 条断言覆盖露白、翻页次数、预览尺寸、
  提前切瓦片、逐帧覆盖率、"稳定后必须清晰"
- **可度量**：忙帧耗时 p50/p95/p99、上传字节、纹理/位图占用，结构指标可跨机器比较并入库当门禁
- 文本层选择与复制、搜索、大纲、页内链接（按目的地类型着色）、引用点、浮窗截图、旋转、
  单页/连续/双页布局、自定义 preview 与存档

## 快速开始

依赖：Rust ≥ 1.75，以及 PDFium 动态库（仓库不提交二进制，用脚本下载）。

```bash
git clone <repo> && cd pdf_reader
./scripts/fetch-pdfium.sh          # → vendor/pdfium-<平台>/lib/
cargo run -p app --                 # 空状态启动，拖入 PDF 或用 Ctrl+O 打开
cargo run -p app -- some.pdf       # 直接打开
```

Windows（PowerShell，不需要 bash）：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\fetch-pdfium.ps1
cargo run -p app -- some.pdf
```

找不到动态库时会给出明确提示（尝试过的每个路径都列出，并标注文件是否存在）。
`app --selftest` 可单独验证"能不能加载 PDFium"。

## 构建与打包

```bash
cargo build --release -p app           # target/release/app
./scripts/package.sh                   # → dist/pdf_reader_<平台>/{app, lib/libpdfium.so, BUILD-INFO.txt}
```

打包产物自带 PDFium（`lib/` 与可执行文件同级），**不需要** rpath / `LD_LIBRARY_PATH`，
从任何目录启动都能找到库。**只能在目标平台上打包**（脚本调用宿主 `cargo build`；不接受跨平台请求）。
Windows 用 `scripts\package.ps1`。

## 质量门禁

```bash
./scripts/check.sh
```

它跑：`cargo fmt --check` → `cargo clippy -- -D warnings` → `cargo test --workspace`
（含 12 个回放脚本与 6 条断言）→ 回放**结构基线**比较（`perf/baseline/*.json`，跨机器可比）
→ 文档链接检查。

单独跑回放：

```bash
cargo run -p app -- --replay scripts/replay/tiling_800.json --out /tmp/run.jsonl
cargo run -p app -- --replay scripts/replay/tiling_800.json --timing    # 用真实时钟测耗时
```

## 环境变量

| 变量 | 作用 |
| --- | --- |
| `PDFIUM_PATH` | 指定 PDFium 库文件或所在目录 |
| `PDF_READER_DIAGNOSE=1` | 打开运行期诊断（贴图/画法/波段/纹理与位图占用） |
| `PDF_READER_TILE_BORDERS=0` | 关掉瓦片红边（默认开启，用于调试分块错位） |
| `PDF_READER_TEXTURE_BUDGET_MB` | GPU 纹理预算（默认 256 MiB，软上限：当前视口不淘汰） |
| `PDF_READER_BITMAP_BUDGET_MB` | CPU 位图缓存预算（默认 512 MiB，0 = 不限制） |
| `PDF_READER_DEV_ENTRY=1` | 显示验收辅助入口（截图 / 测试浮窗） |

## 文档

- [`docs/README.md`](docs/README.md)：文档分开两类 —— `docs/current/` 描述**现状架构**
  （架构、渲染、界面、持久化、诊断、测试），`docs/decisions/` 记录**架构说明、历史变化与决策**
  （含编号决策日志、原理与路线）。
- 旧的设计/评审文档已移出工作区，需要时从 git 历史取（`git show <commit>:docs/...`）。

## 已知限制

- 颜色通道顺序由 `backend-pdfium` 的 `set_reverse_byte_order(false)` 固定，改动它必须同步
  `view::color_image` 的交换逻辑（`rendering.md` §7.1.1）。
- 极端长宽比的页面按"最长边"定分块层级，会出现很多很窄的瓦片（已知，尚未按轴分别分级）。
- 无头回放只覆盖主视图渲染主线；浮窗、侧栏、截图、原位编辑仍以人工验收为主。
- macOS 分发需要签名/公证；Windows 未在 CI 中验证。

## 许可

MIT OR Apache-2.0
