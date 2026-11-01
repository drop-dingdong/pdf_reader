# agent.md —— 协作约定

> 适用于本仓库全部改动（人工与 AI 协作均适用）。

## 1. 测试与门禁

- 每次改动必须编写或更新相关测试；交付前必须让 `./scripts/check.sh` 通过。
- 依赖真实窗口/GPU/人工观察的验证项交给用户，并说明“怎么看、看什么”。

## 2. 提交

- 交付前必须 `git commit`。**只提交本次改动的文件**；工作区里的临时样本、模式位变化不要一起提交。

## 3. 文档

- 文档集见 [docs/README.md](docs/README.md)，只描述现状；历史原因写提交说明或留在 git 历史。
- 触及架构、契约、可观察行为、持久化格式或阈值时，同步更新：
  - [docs/architecture.md](docs/architecture.md) 的不变量清单；
  - [docs/app-ui.md](docs/app-ui.md) 的交互/保存语义；
  - [docs/status.md](docs/status.md) 与 [docs/roadmap.md](docs/roadmap.md)（完成/新增未决项）。
- 不要再引用已删除的 `docs/current/`、`docs/decisions/`；需要时用 `git show` 取历史。

## 交付前自检

- [ ] 新增/更新了测试？`./scripts/check.sh` 全绿？
- [ ] UI/真机项已列出人工验收集？
- [ ] 触及绘制/缓存/分块时跑过回放器与基线比较？
- [ ] 文档已同步（架构/UI/status/roadmap）？
- [ ] 已按“只提交本次改动”提交？
