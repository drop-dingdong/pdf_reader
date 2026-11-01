# 写回 PDF

把内存里的注释写成**真正的 PDF 注释**（不是旁挂 JSON）。读回见 [readback.md](readback.md)。

## 1. 为什么在子进程

主进程有渲染线程，PDFium 的裸 FFI 与 `pdfium-render` 并行不安全。因此写回/整理都由
`app` 以子进程执行：

```text
app（编排） --job.json--> app --writeback（子进程，裸 FFI） --输出--> app（校验 → 原子替换）
```

- 子进程入口：`app --writeback <job.json>`；整理副本：`app --compact <in> <out>`。
- 子进程只做 PDF 对象操作 + 一次冒烟渲染，不创建窗口。
- 测试可用 `PDF_READER_WRITEBACK_EXE` 指定子进程可执行文件（回放测试进程本身不处理 `--writeback`）。

## 2. 写什么

| 注释种类 | PDF 对象 | 关键字段 |
| --- | --- | --- |
| 高亮 | `/Subtype /Highlight` | `/QuadPoints`、`/C`、`/CA`、**不写 `/AP`** |
| 下划线 | `/Subtype /Underline` | `/QuadPoints`、`/C`、`/AP`（线段 Form） |
| 删除线 | `/Subtype /StrikeOut` | `/QuadPoints`、`/C`、`/AP`（线段 Form） |
| 墨迹 | `/Subtype /Ink` | `/InkList`、`/BS /W`、`/AP`（PDFium 路径对象生成的 Form） |
| 便签 | `/Subtype /Text` | `/Rect`、`/Contents`、`/Name /Comment`、`/C`、`/CA`、**不写 `/AP`** |

所有新旧注释统一写：

- `/NM (pd-fr:<id>)`：幂等标记，读回按它恢复 id；
- `/Contents <UTF-16BE>`：正文（可空）；
- `/T`：作者（优先内存里的作者，否则环境变量/系统用户）；
- `/M`、`/CreationDate`：修改/创建时间（PDF 日期串，带时区）；
- `/P <页对象>`：所属页；`/F 4`：可打印。

外观约定：

- 高亮不写 AP，让阅读器按 `/QuadPoints` 半透明合成（写 AP 会盖住文字）；
- 下划线/删除线写线段 Form；墨迹用 PDFium 路径对象生成带 `/Resources`/`/Matrix` 的 Form；
- 便签靠阅读器自己画图标，不写 AP；
- 透明度写 `/CA`（读回据此还原，见 [readback.md](readback.md)）。

## 3. 新增 / 更新 / 删除

写回任务携带三组数据：

1. **新注释**：没有 PDF 对象号的注释（`pdf_ref = None`）→ 新建对象并追加到页 `/Annots`。
2. **对象更新**：已有对象号的正文修改 → 对同 `(num,gen)` 追加新版本（`/Contents`、`/M`、`/T`）。
  不重建对象，避免 id 漂移。
3. **删除**：对象号不再被任何注释引用的（被删、改样式/移动后重建、重复 id 清理）→
  追加页对象，把对应引用从 `/Annots` 去掉。

幂等性：同一 id 重复保存不会叠加对象——已有 `/NM = pd-fr:<id>` 的对象要么被更新、要么被删除；
`/Annots` 里同一 id 的多份历史对象在保存时清理，读回取**最后一份**。

外部注释（没有 `pd-fr:` 标记）：

- 几何/颜色只读；
- 改正文/删除会写回对应对象（正文更新走对象号；内联注释走 `/Annots` 位置更新/删除）；
- 不会把外部注释“变成自有注释”。

## 4. 保存模式

统一入口 `app::write_document`（见 [architecture.md](architecture.md) §4.5）：

| 模式 | 临时文件位置 | 替换 | 额外 |
| --- | --- | --- | --- |
| 原地保存 | 源文件旁边 | `rename` 覆盖源文件 | 可检查外部修改；可选 `.bak` 备份；关闭会话后再替换 |
| 另存为 | 目标目录 | `rename` 覆盖目标 | 可覆盖已存在目标（需确认）；写源/目标 `.json` |
| 覆盖保存 | 同“原地保存” | 同左 | 目标就是源文件，需明确确认忽略外部修改 |

写回前/后都检查：

- 页数一致；
- 注释数量与任务一致（删除/新增分别核对）；
- 至少渲染一页做冒烟检查（打不开/渲染失败即失败）。

任一校验失败 → 临时文件删除、原文件不动、错误返回界面（保存对话框保留并显示）。

## 5. 文件对象层面的注意点

- 增量写回追加经典 xref 表 + trailer `/Prev`；不重写整个文件。
- 页面对象以追加新版本的方式修改 `/Annots`，旧版本仍在历史链里。
- `/Length` 等流长度必须精确；写 AP 时由 PDFium 生成流对象。
- 注释的 `/Rect` 必须覆盖外观（下划线会向下扩 `UNDERLINE_GAP`，否则 AP 被裁剪）。
- 加密文档拒绝写入；签名由整理副本拒绝（见 `pdf_compact`），普通写回只追加对象，不动签名块。
- Windows 上替换原文件前必须关闭会话（释放文件句柄）。

## 6. 命令行与排查

```bash
cargo run -p app -- --writeback /tmp/job.json   # 子进程入口（通常由应用调用）
cargo run -p app -- --compact in.pdf out.pdf
```

常见问题：

| 现象 | 检查 |
| --- | --- |
| 写回子进程启动失败 | `app --selftest`；`PDF_READER_WRITEBACK_EXE` 是否指向可执行文件 |
| “PDF 已被其他程序修改” | 外部改写导致 `pdf_file_stamp` 变化 → 另存为或明确覆盖 |
| 写回成功但本程序看不到注释 | 读回配对（[readback.md](readback.md)）；`/NM` 是否写对；页 `/Annots` 是否包含对象 |
| 同一注释出现多份 | 旧对象未进 `deletes`；检查 `pdf_ref_changes` 的同 id 分支 |
| 便签在 Okular 可见、本程序不可见 | 读回 subtype `Text(1)`、`GetRect` 与对象号配对 |
| 备份文件 | 设置 `save.backup_in_place` 开启时才有 `<pdf>.bak-<时间>` |

## 7. 测试

- `crates/backend-pdfium/tests/writeback.rs`：对象字段、增量、删除、内联、AP、像素冒烟。
- `crates/backend-pdfium/tests/annotation_meta.rs`：作者/时间、便签读写回、正文更新只刷 `/M`。
- `crates/app/tests/annot_roundtrip.rs`：应用层写回→重开→再写回。
- `crates/app/tests/writeback_cli.rs`：CLI 子进程路径。

## 8. 相关

- 读回与配对：[readback.md](readback.md)
- 保存/另存为/提示状态机：[app-ui.md](app-ui.md) §2
- 整体流程：[architecture.md](architecture.md) §4.5
