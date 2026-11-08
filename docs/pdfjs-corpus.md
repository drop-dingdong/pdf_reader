# pdf.js 测试集扫描说明

> 测试集**不随本项目分发**，也不应提交到仓库。本文件只说明来源、用途、扫描脚本和结果。

## 1. 来源与许可

- 本地目录：`pdfjs_test_pdfs/`，来自 Mozilla pdf.js 的测试 PDF 集合
  （[mozilla/pdf.js](https://github.com/mozilla/pdf.js)，通常位于 `test/pdfs/`）。
- 该集合混合了多种来源和许可的 PDF，包含第三方样本、fuzzed 文件、加密文件；
  部分条目是 `.link` 外链而不是 PDF 本身。
- 本项目**不复制、不打包、不提交**这些 PDF；只在本地用作兼容性回归。
- 因此扫描结果依赖本地快照；pdf.js 上游更新后，同一路径的文件可能变化。

## 2. 扫描脚本

保留的脚本：

```bash
cargo run -p app --example pdfjs_scan -- pdfjs_test_pdfs docs/pdfjs-corpus-scan.tsv
```

它只读测试集，在系统临时目录生成用于回读的副本，不修改原文件。

输出列：

```text
path version open pages readback skipped index_ok mismatch add_ok add_seen error
```

## 3. 结果

完整结果：[pdfjs-corpus-scan.tsv](pdfjs-corpus-scan.tsv)

扫描快照：1089 个 `.pdf`；其中 20 个 PDFium 打不开（11 个加密、9 个格式损坏/fuzzed）。

| 指标 | 数量 |
| --- | ---: |
| 总 PDF | 1089 |
| PDFium 可打开 | 1069 |
| `pdf_index` 完全失败 | 36 |
| `pdf_index` 页级计数不匹配 | 6 |
| 新注释写入失败 | 40 |
| 新注释写入后读不回 | 46 |
| 新建 + 读回完全成功 | 1042 |

按版本（后端“新建 + 读回”完全成功数；`<1.5` 的注释在应用层仍按策略禁用）：

| 版本 | 文件数 | 完全成功 |
| --- | ---: | ---: |
| 1.0 | 5 | 5 |
| 1.1 | 5 | 4 |
| 1.2 | 7 | 7 |
| 1.3 | 41 | 38 |
| 1.4 | 239 | 230 |
| 1.5 | 107 | 105 |
| 1.6 | 215 | 209 |
| 1.7 | 459 | 434 |
| 2.0 | 9 | 7 |

## 4. 结论

- 注释成功/失败**取决于文件结构**，不是单一版本号；
- 1.5 的对象流/xref 是主要风险点，但不是全部 1.5 都失败；
- 1.7 也可能失败，说明 `pdf_index` 的覆盖范围比“版本门禁”更重要；
- 加密、fuzzed、严重损坏的文件不建议纳入注释兼容目标，只需明确拒绝/提示；
- 应用层 `<1.5` 只拒绝注释编辑、不拒绝打开的策略保持。

## 5. 相关文档

- [PDF 1.4–2.0 注释字段差异](annotation-versions.md)
- [pdf.js 测试集问题文件清单](pdfjs-bug-list.md)
- [架构与写回](architecture.md)、[writeback.md](writeback.md)
