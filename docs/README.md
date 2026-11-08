# 文档地图

当前文档只描述**现在**；历史设计与逐条决策留在 git 里（`git log -- docs/` / `git show <commit>:docs/...`），
不再维护决策编号日志。改动如果引入新的约束或契约，直接更新本文档集，并在提交说明里写清原因。

## 先读哪一篇

| 想知道什么 | 文档 |
| --- | --- |
| 项目现在能做什么、门禁状态、最近完成的东西 | [status.md](status.md) |
| 下一步准备做什么、哪些是暂缓项、哪些是明确不做的 | [roadmap.md](roadmap.md) |
| 分层、线程模型、坐标系、**不变量清单**、写回/整理/设置的架构约束 | [architecture.md](architecture.md) |
| 渲染与分块：比例/波段、网格、渲染键、缓存、画法决策 | [rendering.md](rendering.md) |
| UI：面板、每帧顺序、交互状态机、浮窗/侧栏/便签/拖拽、保存提示 | [app-ui.md](app-ui.md) |
| 浮窗链接语法/身份规则/外部注释策略/失败语义 | [float-links.md](float-links.md) |
| `PdfReaderApp` 状态分组的 Phase 3 评估（未实施） | [state-grouping-evaluation.md](state-grouping-evaluation.md) |
| `view.rs` 拆分评估（未实施；建议先拆测试） | [view-split-evaluation.md](view-split-evaluation.md) |
| PDF 1.4–2.0 注释字段差异（实现/排查查表） | [annotation-versions.md](annotation-versions.md) |
| pdf.js 测试集扫描说明（本地集合，不随仓库分发） | [pdfjs-corpus.md](pdfjs-corpus.md) |
| pdf.js 测试集问题文件清单 | [pdfjs-bug-list.md](pdfjs-bug-list.md) |
| pdf.js 扫描结果 TSV | [pdfjs-corpus-scan.tsv](pdfjs-corpus-scan.tsv) |
| UI 样式/图标/侧栏/主题的规范（已实施） | [ui-standards.md](ui-standards.md) |
| 文件格式与持久化：PDF 注释字段、`<pdf>.json`、`settings.json` | [persistence.md](persistence.md) / [readback.md](readback.md) / [writeback.md](writeback.md) |
| 写回 PDF（子进程、FFI、外观、校验、排查） | [writeback.md](writeback.md) |
| 从 PDF 读回注释、对象号配对、外部注释 | [readback.md](readback.md) |
| 诊断开关与真机排查 | [diagnostics.md](diagnostics.md) |
| 测试分层、命令、夹具、人工验收清单 | [testing.md](testing.md) |

## 维护规矩

1. 只写“现在是什么样”；历史原因写进提交说明或保留在 git 历史，不再堆进正文。
2. 代码改了要同步对应文档，尤其是 [architecture.md](architecture.md) 的不变量清单和
  [app-ui.md](app-ui.md) 的交互/保存语义。
3. 新增“以后要做/暂缓/不做”的事项写进 [roadmap.md](roadmap.md)，不要散落在文档里。
4. 链接用相对路径；能在代码里找到的事实优先给 `文件:行号`。
5. 面向“下一个接手的人”写：先给结论与表格，再给必要的理由。
