# 技术选型决策记录（PDF 阅读器）

> 约束：开源；Rust 或 Python；Windows / macOS / Linux；可打包为完整二进制；不依赖用户需额外安装的库，系统自带库可用。
> 状态：**已定稿**。详细论证过程见附录（略），结论如下。

## 1. 决策

| 层 | 选择 | 版本（2026-10） | 许可证 |
| --- | --- | --- | --- |
| 语言 | Rust | stable | — |
| UI | `egui` + `eframe`（`glow` 后端） | 0.37 | MIT / Apache-2.0 |
| PDF 后端 | PDFium，经 `pdfium-render` 绑定 | 0.9.4 / PDFium 8100 | BSD-3-Clause / MIT |
| 打包 | 三平台原生构建（GitHub Actions 矩阵） | — | — |

**交付形态**：主程序（10–20MB）+ 随包分发的 PDFium 动态库（10–15MB），整体约 25–40MB。Windows 为 `exe + dll`，macOS 为 `.app`（含 `Contents/Frameworks/`），Linux 为 AppImage。除操作系统自身框架（Win32/UCRT、Cocoa/Metal、glibc/X11/Wayland/libGL）外无外部依赖，用户零安装。

## 2. 选型理由

- **egui**：阅读器的界面本质是"一块大自定义画布 + 一圈薄控件"（工具栏、侧边栏、状态栏），正是立即模式 GUI 的最优场景；`Painter` 自绘与纹理上传直接，空闲不重绘不耗 CPU；`eframe` 一套代码出三平台原生窗口；内建 AccessKit 无障碍。活跃度最高（月下载 236 万，0.37 于 2026-10-15 发布）。
- **PDFium**：与 Chrome 内置阅读器同源，渲染保真度天花板；**文本层/搜索/加密/批注/表单齐备**；BSD-3-Clause 对宿主项目零传染。
- **解耦**：UI 只依赖自定义 trait（见 [architecture.md](architecture.md)），不直接依赖 PDFium。后端可在不改 UI 的前提下替换为 hayro 等，并可用假后端做单元测试。

## 3. 排除项

| 方案 | 排除原因 |
| --- | --- |
| hayro（纯 Rust） | API 只有 `render`/`render_into`，**无文本提取与搜索**，**不支持加密 PDF**，缺 blending/knockout —— 做不了"阅读器"。保留为备选后端。 |
| Tauri / Electron | Linux 依赖发行版的 `webkit2gtk`，Windows 旧版需装 WebView2 —— 恰属"需要用户额外安装的库"。 |
| MuPDF / PyMuPDF | AGPL-3.0，会把整个项目变成 AGPL。 |
| Poppler | Linux 系统库齐备，但 Windows/macOS 需自行编译大量依赖，违背易打包目标。 |
| Python + PySide6 | `QPdfView`/`QPdfSearchModel` 开箱即用、出成品最快，但包体 150–250MB 且有 LGPLv3 合规成本。列为备选 B。 |

## 4. 打包现实（重要）

**PDFium 没有官方预编译静态库**：`bblanchon/pdfium-binaries` 只发布动态库，且明确表示不打算提供静态构建。因此：

- **默认路线**：动态库随包分发 —— 完全满足"用户零安装"。
  - 注意用 `std::env::current_exe()` 目录定位库，**不要依赖 CWD**（macOS 双击 `.app` 时 CWD 为 `/`）。
  - Linux 需 `$ORIGIN` rpath；macOS 需 `@executable_path/../Frameworks`。
- **可选终态（M3）**：静态链接，需用 `./build.sh -s <os> <arch>` 或复用其 `build.yml`（`build_type=static`）自建 PDFium，再以 `PDFIUM_STATIC_LIB_PATH` + `features = ["static", "libstdc++"]` 构建。

## 5. 里程碑

| 阶段 | 内容 |
| --- | --- |
| M1 走通 | 打开 PDF、显示首页、翻页、缩放（动态库路线） |
| M2 可用阅读器 | 虚拟化连续滚动、文本选中/复制、搜索、目录与缩略图、加密 PDF 密码、快捷键 |
| M3 可发布 | 三平台打包与 CI、可选静态化、macOS 签名公证、AppImage、i18n |
| M4 增强 | 批注高亮、双页/连续模式、链接历史、最近文件、打印 |

## 6. 关键风险

| 风险 | 对策 |
| --- | --- |
| PDFium 非线程安全 | 单后台渲染线程 + 优先级队列（当前页 > 预取页）；不把渲染丢进通用线程池 |
| 打包后找不到 pdfium | `current_exe()` 定位 + rpath；CI 加"干净容器启动冒烟测试" |
| egui 破坏性升级 | 锁定 `0.37`，升级集中在独立 PR |
| Linux 分发碎片化 | AppImage 为主，声明 glibc 基线 |

## 7. 参考

- [pdfium-render](https://lib.rs/crates/pdfium-render) · [pdfium-binaries（"Static builds"一节）](https://github.com/bblanchon/pdfium-binaries)
- [egui](https://lib.rs/crates/egui) · [hayro](https://lib.rs/crates/hayro) · [Qt for Python 部署](https://doc.qt.io/qtforpython-6/deployment/index.html)
