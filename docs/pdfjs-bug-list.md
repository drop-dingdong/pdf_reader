# pdf.js 测试集问题文件清单

> 基于 `pdfjs-corpus-scan.tsv` 的本地快照；测试集不随仓库分发。
> 本清单用于以后修阅读器/注释兼容性时快速定位，不代表承诺全部支持。
> 完整逐文件数据见 [pdfjs-corpus-scan.tsv](pdfjs-corpus-scan.tsv)。

## 1. 阅读器/空文档假设问题

这些不是注释格式问题，而是 PDFium 能打开但页数/页面树不符合应用假设：

| 文件 | 版本 | 现象 | 类别 |
| --- | --- | --- | --- |
| `2.0_Brotli-Prototype-FileA.pdf` | 2.0 | PDFium 打开但 0 页；应用会报 `页号越界：page#1（共0页）` | 空文档/损坏 PDF；应用应拒绝打开而不是访问第 1 页 |
| `issue19484_1.pdf` | 1.6 | 打开但 0 页；`pdf_index` 页面对象不存在 | 页树/对象索引 |
| `issue19484_2.pdf` | 1.6 | 同上 | 页树/对象索引 |
| `poppler-395-0-fuzzed.pdf` | 1.7 | 打开但 0 页；页面对象不存在 | fuzzed/页树 |
| `Pages-tree-refs.pdf` | 1.7 | PDFium `Unknown` 打开失败；页面树深度/结构异常 | 页树 |

`2.0_Brotli` 值得单独修一个防御：**打开阶段或布局前若 `page_count == 0`，直接给出“空文档/不支持”提示，不能进入第 1 页。**

## 2. 打开失败（建议明确拒绝，不追注释）

20 个中多数是加密或损坏；不建议把它们纳入注释兼容目标：

### 需要密码

```text
bug1782186.pdf
crypt_filters.pdf
issue15893_reduced.pdf
issue21579.pdf
issue3371.pdf
issue6010_1.pdf
issue6010_2.pdf
pr6531_1.pdf
pr6531_2.pdf
print_protection.pdf
saslprep-r6.pdf
```

### 格式损坏/fuzzed

```text
Pages-tree-refs.pdf
REDHAT-1531897-0.pdf
bug1020226.pdf
bug1250079.pdf
bug1606566.pdf
bug1980958.pdf
issue9105_other.pdf
poppler-742-0-fuzzed.pdf
poppler-937-0-fuzzed.pdf
```

处理原则：打开失败时显示后端错误/密码框，不进入注释流程。

## 3. 注释索引/读写问题

### 1.5

| 文件 | 现象 |
| --- | --- |
| `issue17215.pdf` | 只有 Caret（不支持类型）；文件是**空用户密码的加密 PDF**，权限禁止修改注释，应用拒绝写回 |
| `issue9418.pdf` | 文件损坏，`/Pages` 找不到 |

`issue12504` / `issue14565` 已由“直接对象不被旧 ObjStm 覆盖”修复。`issue17215` 不属于 C2 索引 bug，而是加密/权限限制。

### 1.6

| 文件 | 现象 |
| --- | --- |
| `highlight.pdf` | 页级计数不匹配 |
| `issue17069.pdf` | 页面对象不存在；注释写回失败 |
| `issue19484_1.pdf` / `_2.pdf` | 0 页；页面对象不存在 |

### 1.7

| 文件 | 现象 |
| --- | --- |
| `bug1815476.pdf` | 页面对象不存在；无法写回 |
| `bug864847.pdf` | `/Pages` 找不到 |
| `empty_protected.pdf` | 页面对象不存在 |
| `issue2177.pdf` | `/Pages` 找不到 |
| `secHandler.pdf` | 页面对象不存在 |
| `perf_structure_annotations.pdf` | 页级计数不匹配 |
| `bug1868759.pdf` | 已由间接 `/Length` 修复覆盖（原 `找不到 /Pages`） |

### 1.4

| 文件 | 现象 |
| --- | --- |
| `issue14438.pdf` | 页级计数不匹配 |
| `rc_annotation.pdf` | 页级计数不匹配 |
| `colors.pdf` | `没有解析到任何对象` |
| `poppler-85140-0.pdf` | 页面对象不存在 |

应用层对 `<1.5` 仍拒绝注释；这些只用于未来评估 1.4 兼容性。

## 4. 处理顺序建议

1. **先修 0 页防御**（影响阅读器稳定性，不依赖注释）。
2. **C2 页级能力**：`issue12504` / `issue14565` 这类只让不匹配页/对象只读。
3. **C2 对象索引**：`bug1815476`、`empty_protected`、`issue17069`、`secHandler` 等页面对象缺失。
4. **写入后读不回**：`issue17215` 单独查，可能是写回页对象/`/Annots` 引用更新问题。
5. 加密/严重损坏文件：只保证错误提示，不纳入注释写回。

## 5. 备注

- 本清单来自一次本地快照；pdf.js 上游更新后文件可能变化。
- 文件名中的 `issue*` / `bug*` 是 pdf.js 测试集命名，不代表本项目的 issue 编号。
- 测试集本身不提交；扫描脚本见 [pdfjs-corpus.md](pdfjs-corpus.md)。
