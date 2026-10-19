# app —— 桌面壳（egui/eframe）

```bash
# 1) 准备 PDFium 动态库（只需一次）
scripts/fetch-pdfium.sh

# 2) 运行
cargo run -p app -- /path/to/file.pdf     # 命令行打开
cargo run -p app                          # 启动后把 PDF 拖进窗口

# 3) 测试（纯逻辑 + 存档 + 联调，不需要窗口）
cargo test -p app
```

诊断：`PDF_READER_DIAGNOSE=1` 打开运行期诊断（另见 `PDF_READER_TILE_BORDERS=1`）。

## 模块划分

| 模块 | 内容 | 怎么验证 |
| --- | --- | --- |
| `view.rs` | 坐标换算、输入映射、`KeyedStore`、画法决策（`plan_page`）、快照拼合、浮窗几何 | `cargo test -p app --lib`（纯逻辑） |
| `app.rs` | egui 胶水：面板、画布、交互、浮窗、侧栏、截图、对话框 | **需要真实窗口 → 人工验收** |
| `persist.rs` | 自定义数据存档（手写 JSON + 版本迁移） | `cargo test -p app --lib persist::` |

约定：**能脱离窗口验证的逻辑一律放 `view.rs`**，`app.rs` 只留"读输入、调会话、画"。

## 文档

* 现状：[docs/current/app-ui.md](../../docs/current/app-ui.md)（每帧顺序、面板结构、交互状态机、浮窗）
* 渲染与画法：[docs/current/rendering.md](../../docs/current/rendering.md)
* 存档与预览图：[docs/current/persistence.md](../../docs/current/persistence.md)
* 诊断开关与排查路径：[docs/current/diagnostics.md](../../docs/current/diagnostics.md)
* **人工验收清单**：[docs/current/testing.md](../../docs/current/testing.md) §6
* 工作约定（测试/提交/验收分工）：[agent.md](../../agent.md)
