# 文档地图

本仓库的文档分成**两类**，互不混写：

| 类别 | 目录 | 回答的问题 | 维护方式 |
| --- | --- | --- | --- |
| **现状** | [`current/`](current/) | 程序**现在是什么样**：分层、模块、机制、开关、测试 | 代码一改就要同步改 |
| **决策与背景** | [`decisions/`](decisions/) | **为什么**是这样、**历史**上怎么变来的、做过哪些决策 | 只追加；历史原文在 git |

工作约定（测试要求、提交要求、人工验收的分工）见仓库根目录的 [`agent.md`](../agent.md)，它不属于上面两类，仍然有效。

## 一、现状（`current/`）

按"我想知道什么"选一篇：

| 文档 | 内容 | 什么时候读 |
| --- | --- | --- |
| [architecture.md](current/architecture.md) | 分层与依赖、模块地图、线程模型、坐标系、**不变量清单** | 第一次读代码；改动前确认边界 |
| [rendering.md](current/rendering.md) | 渲染与分块：比例/波段、网格、渲染键、块矩形与后端校验、摆放、缓存保留、**画法决策** | 动缩放、分块、绘制、缓存时**必读** |
| [app-ui.md](current/app-ui.md) | UI 层：面板结构、每帧顺序、交互状态机、浮窗/侧栏/截图/悬停 | 动界面与交互时 |
| [persistence.md](current/persistence.md) | 自定义数据存档（`.pdf.json`）v2、版本与迁移规则、预览图重建 | 动存档格式或预览图时 |
| [diagnostics.md](current/diagnostics.md) | 所有诊断开关与输出格式、常见问题的排查路径 | 定位真机问题时 |
| [testing.md](current/testing.md) | 测试分层与命令、真实 PDF 夹具、**人工验收清单** | 交付前；写测试时 |

## 二、决策与背景（`decisions/`）

| 文档 | 内容 |
| --- | --- |
| [decisions/README.md](decisions/README.md) | 这一类的规则：ID 编号、条目格式、什么时候写 |
| [decisions/log.md](decisions/log.md) | **决策索引**：D-00a … D-101 每条一行；**新决策从 D-102 起追加在这里** |
| [decisions/rationale.md](decisions/rationale.md) | 架构说明：当前设计的关键理由与取舍（按主题，引决策 ID） |
| [decisions/history.md](decisions/history.md) | 历史变化：演进阶段、被推翻的做法、踩过的坑与教训 |

## 三、旧文档去哪了

2026-02 的文档重构把此前 `docs/` 下的设计/历史文档**全部移出工作区**，只保留在 git 历史里
（最后一次同时存在的提交是 **`778dc60`**）。需要原文时：

```bash
git show 778dc60:docs/decision-log.md      # 旧决策记录（101 条，含完整理由）
git show 778dc60:docs/data-flow.md         # 旧数据流（含 512 网格时代的场景）
git ls-tree -r --name-only 778dc60 docs/   # 看当时有哪些文档
```

已移出的文件（避免再被引用）：

`architecture.md` · `backlog.md` · `custom-data-versions.md` · `data-flow.md` · `decision-log.md` ·
`deep-zoom-design.md` · `feedback-round-1..5.md` · `framework-selection.md` ·
`patch-plan-tiling-v2.md` · `patch-plan-tiling-v3.md` · `preview-ladder-design.md` ·
`trait-definitions.md` · `traits.md`

它们的内容已经被吸收进上面的现状文档与决策索引；**不要再新增指向它们的链接**。

## 四、写文档的规矩

1. **现状文档只写"现在"**：不写"以前怎样"、不写"将来要做"（将来放 `decisions/log.md` 的开放项）。
2. **决策只追加**：`decisions/log.md` 里的历史条目不改写；要推翻就写一条新决策并注明取代了谁。
3. **代码改了就同步改现状文档**：尤其是 `rendering.md` 的判据表与 `architecture.md` 的不变量清单。
4. **链接用相对路径**，指向具体文件；能在代码里找到的事实优先给 `文件:行号`。
5. 面向"下一个接手的人"写：先给结论与表格，再给理由（理由放 `decisions/`）。
