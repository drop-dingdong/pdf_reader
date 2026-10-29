# P4 对象级保存器：评估与计划

> 状态：**P4a spike 已通过**（2026-10-29，D-157）；P4b 集成待做。

## 1. 现状

写回分两段：

1. PDFium 改注释/加注释：`FPDF_SaveAsCopy(..., FPDF_INCREMENTAL)`；
2. `pdf_repair` 只把**内联**注释外置成独立对象。

问题：

* PDFium 的“增量保存”仍会把大量对象追加一遍，而不是只写变化对象；
* 目标对象 `generation > 0` 时 PDFium 会写出新 gen0，重开解析旧代（D-153），
  目前用“全量保存”兜底；
* 全量保存会展开 ObjStm、重写所有对象，Okular 另存可能报警；
* 实测：Okular 样本 8,228 → 全部改正文 23,409 字节；Adobe 12,011 → 23,409。

P4 的目标是像 Okular 一样**只追加变化的对象**，并去掉 gen>0 的全量兜底。

## 2. 不改的部分

* 新注释的**外观生成**（高亮/下划线/删除线/墨迹 AP）继续用 PDFium（D-132）；
* 我们的标记注释（`/NM = pd-fr:<id>`）继续由 marker 差分管理；
* 外部注释的正文/删除仍由对象号定位（P2）；
* `.annot.json` 不再生成（P3）。

## 3. 设计

### 3.1 新增 `pdf_patch`（对象级增量）

对 PDFium 输出的字节做一次后处理（或对源文件直接处理）：

1. 用 `pdf_index` 解析页面树、对象、ObjStm，拿到页对象 dict + `/Annots` refs；
2. **正文更新**：
   * 复制目标注释 dict，替换 `/Contents`；
   * 分配新对象号（`max+1`），追加 `<num> 0 obj ... endobj`；
   * 复制页对象 dict，把 `/Annots` 里对应 ref 换成新对象号；
   * 追加新页对象；
   * 追加经典增量 xref + trailer `/Prev`；
3. **删除**：复制页对象，从 `/Annots` 去掉 ref（含 `/Popup`），追加页对象 + xref；
4. 原对象/ObjStm 不动；被替换的注释成为孤儿对象，不参与页面。

用“新对象 + 改页引用”而不是改旧对象，原因：

* 避免 generation > 0 / 同号多版的解析歧义；
* 不依赖 PDFium 的 SetStringValue；
* 页对象覆盖是 PDF 增量更新的标准做法。

### 3.2 与 PDFium 的分工

* PDFium 只负责**新增/几何/AP**（`pages`）；
* `updates` / `deletes` 不再交给 PDFium；PDFium 保存后用 `pdf_patch` 写入；
* 这样外部注释、我们自己的正文修改都走对象级路径；
* `needs_full_save` 删除；P3 的 gen>0 兜底取消。

### 3.3 xref 兼容

* 源文件可能是经典 xref 或 xref stream + ObjStm；
* 先尝试在末尾追加**经典 xref**（`/Prev` 指向最后 startxref）；
* 用 PDFium/Okular 验证；若 Adobe/Okular 不接受，再实现 xref stream 输出。
* 页对象若在 ObjStm 中，追加同号直接对象覆盖，xref 指向新位置。

## 4. 分阶段

| 阶段 | 内容 | 验收 |
| --- | --- | --- |
| **P4a** | `pdf_index` 暴露对象 dict；`pdf_patch` 只做正文更新；Okular/Adobe 样本 | ✅ 已通过：同 `(num,gen)` 追加版本，PDFium 读回新正文；ObjStm 保留；增长 <1.2 KB |
| **P4b** | 集成写回：PDFium 不再处理 updates；删除 + Popup | Okular/Foxit 删除一致；无全量兜底 |
| **P4c** | 文件增长/对象数对比；去掉 `needs_full_save`；回归 | 连续保存/撤销重做/P2/P3 全绿；Okular 另存正常 |

## 5. 风险

| 风险 | 处理 |
| --- | --- |
| 经典 xref 追加在 xref stream 后不被接受 | P4a 先验证；失败改 xref stream |
| ObjStm 页面对象无法直接改 | 追加同号直接对象覆盖；已由 pdf_index 支持 |
| `/Contents` 是 literal/hex/间接引用 | 复制 dict 时删除旧键再插入；保留原有编码策略（UTF-16） |
| 页 `/Annots` 是间接数组 | 复制数组对象或直接改成内联数组；pdf_index 已解析 |
| 重复键/PDF 语法 | 用 balanced parser，不按字符串替换 |
| 新对象号冲突 | `Size`/最大对象号 +1；xref 全量登记新增对象 |
| 旧注释孤儿对象 | 可接受；后续压缩/清理另做 |

## 6. 待确认

* 是否接受“正文更新后对象号改变”（旧对象孤儿）？这是避开 gen/多版问题的最稳方案；
* P4 是否只优化正文/删除，新增注释仍由 PDFium 产生（本计划默认是）。
