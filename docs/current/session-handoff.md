# 会话交接：注释 / 另存为 / 内联 / 写回

> 快照时间：2026-10-31  
> 分支：`feat/annotations`  
> HEAD：`4165d05 feat(annotations): 新注释写 /T 作者`  
> 门禁：`./scripts/check.sh` 通过，**399 tests**。  
> 工作区没有代码 diff；存在用户样本/模式位变化，**不要提交**。

## 1. 当前已完成

### 注释写回

- P3/P4/P5/P6：对象级新增、正文更新、删除、AP；
- 新注释全部绕过 PDFium（高亮/下划线/删除线/墨迹）；
- 新高亮+墨迹合并为**一段**增量（`append_new_annotations`，D-192）；
- 无改动不重写 PDF；空任务 backend 直接复制（D-191）；
- `/M`、`/CreationDate`、`/T` 作者（D-181/D-193）。

### 外部注释

- 引用注释：正文/删除；
- 内联注释：正文/删除通过页对象 `/Annots` patch（D-190）；
- 内联样本：`python3 scripts/make_inline_sample.py out.pdf`；
- 内联改色仍不支持；跨写回历史不保证。

### UI

- 高亮/下划线/删除线模式，Esc 退出并恢复原工具；
- 样式颜色、墨迹颜色；
- 注释列表：按页分组、摘要、筛选、删除、右键定位；
- 自定义 preview 稳定会话 ID、完整撤销（创建/删除/标题/正文/resize/截图替换）；
- 连续 resize 合并为一条撤销，并同步浮窗 body。

### 数据文件 / 另存为

- sidecar v3：单一有序 `custom_previews`，无 `order`；v1/v2 迁移（D-187）；
- 另存为：目标 PDF + 目标 `.json` 双占用检查；两个数据文件 checkbox；
  - 提示按“有数据/未保存”真值表显示（D-184）；
  - 成功后切换文档、保留撤销栈、继承内存 preview/dest/ID；
- 写入 PDF：无改动直接跳过；备份、原子替换、冲突检测。

## 2. 下次优先验证

以下项目尚未由用户完整确认，建议下次先验：

1. **v3 sidecar**
   - 打开 v2 `.json`（最好含交错的 image/text preview）→ 保存；
   - 确认 JSON 只有 `custom_previews`，无 `order`；
   - 旧版本程序应拒绝 v3（D-107）。
2. **另存为两个 checkbox 的 4 种组合**
   - 重点：`+10`（只写原）提示措辞已修正；“新文件没有数据”提示；
   - 目标 PDF/JSON 已存在时拒绝；
   - 成功后切换、撤销栈、preview/dest 数据是否连续。
3. **preview 撤销**
   - 创建/删除/标题/正文/浮窗大小；
   - 连续 resize 只一条；
   - 图片 preview 首次截图替换后撤销（浮窗是否关闭/恢复）。
4. **合并增量**
   - 同时新高亮+墨迹：`%%EOF` 应为“源 + 新”两段，Okular 可打开。
5. **`/T` 作者**
   - `PDF_READER_AUTHOR=...` 后新增注释，文本查看器确认 `/T`；
   - 不设置时是否按系统用户名/省略。
6. **无改动写入**
   - 已确认：按钮不可用/文件不变；再确认另存为字节一致。

## 3. 下一步目标

只剩第 5 条：**压缩/整理增量**。

建议先做设计，不直接写代码：

- 明确是“合并 xref/旧修订”还是“全量重写压缩”；
- 必须输出新文件 / 先备份，不能自动覆盖；
- 数字签名、对象流、xref stream、Okular 兼容性；
- PDFium 全量保存会展开 ObjStm，不能直接作为压缩方案；
- 可选 `qpdf`/`mutool` 作为外部工具，或自己实现完整 writer；
- 先决定“整理副本”入口与失败回滚，再实现。

## 4. 已知限制 / 后续

- 外部内联注释改色不支持；
- 内联注释跨写回历史不能完全重绑；
- 另存为不检查源文件在打开后被外部修改（写入 PDF 会检查）；
- `/T` 无 UI，只有环境变量/系统用户；
- 样式设置缓存、text-fit 视图仍是后续调研项；
- PDFium 完整 app 测试不应放进 lib 测试进程（线程安全，D-? 记录在对话）。

## 5. 快速命令

```bash
cd /root/data/pdf_reader
PATH=/tmp/rust/cargo/bin:$PATH \
CARGO_HOME=/tmp/rust/cargo \
RUSTUP_HOME=/tmp/rust/rustup \
./scripts/check.sh

python3 scripts/make_inline_sample.py /tmp/inline.pdf
```

相关文档：

- [writeback.md](writeback.md)
- [persistence.md](persistence.md)
- [app-ui.md](app-ui.md)
- [decisions/log.md](../decisions/log.md)（D-181～D-193）
