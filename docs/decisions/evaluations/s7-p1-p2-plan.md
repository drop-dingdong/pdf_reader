# S7 实施计划：P1 正文归注释 + P2 外部正文/删除

> 状态：**计划待确认**（2026-10-28）。不改代码。
> 依据：[`s7-body-overlay.md`](s7-body-overlay.md)（D-145/D-146/D-147）。

## 1. 范围

### P1：正文归注释

* `Annotation` 自带 `body: Option<String>`；删除 `body_title`；
* 注释正文成为**独立浮动对象**（不属于 CustomPreview，不进侧栏、不写 `.json`）；
* 正文编辑进入撤销栈；
* `.annot.json` 升 v3（几何 + 正文）；迁移 v2：从 `.json` 的 `#annot:<id>` 正文搬进 v3；
* 保留 `.annot.json`（本阶段不删）；保留 PDF 写回路径不变。

### P2：外部注释正文 + 删除

* 四种注释一视同仁：读正文、改正文、删除；几何/颜色仍只读（UI 没有这些操作）；
* 身份用 xref 对象号 `(num, gen)`；不做几何/正文指纹；
* 删除父注释时一起删 `/Popup` 子对象；
* 外部注释的正文修改/删除**只存内存**，由「写入 PDF」落盘，不写 A/J；
* 本阶段继续保留 `.annot.json`（承载“我们自己的注释/未写回的新注释”）。

### 非目标

* P3（去 `.annot.json`）、P4（对象级保存器）不在本计划实现；
* 不改悬浮预览/链接预览的通用行为，只新增 `FloatOwner::Annotation`；
* 不支持外部注释改颜色/几何；
* 不支持内联字典的外部注释（见 §5 限制）。

## 2. 数据模型（P1）

### 2.1 `pdfcore::annotation`

```rust
pub struct Annotation {
    pub id: AnnotationId,
    pub page: PageIndex,
    pub kind: AnnotationKind,
    pub color: [u8; 3],
    pub opacity: f32,
    /// 正文；`None` = 没有正文（不写 /Contents）。
    pub body: Option<String>,
}
```

* `Annotation::new` 默认 `body: None`；
* `AnnotationStore::set_body(id, Option<String>) -> bool`；
* 所有 `body_title` 引用改掉（搜索 `annotation_body_title`、`#annot:`）。

### 2.2 `.annot.json` v3

`ANNOT_VERSION = 3`，`SavedAnnotation` 去掉 `body_title`，加：

```json
{
  "id": 7,
  "page": 0,
  "kind": "ink",
  "points": [[...]],
  "width": 2.0,
  "color": [25, 25, 25],
  "opacity": 1.0,
  "body": "正文"
}
```

* v2 读入：`body_title` → 到 `.json` 的 `custom_texts` 里按标题找正文；
  找得到 → `body`；找不到 → `body: None`（保留“正文已丢失”提示语义，但不再显示为 preview）；
* v3 读入：直接用 `body`；
* 迁移后在内存中标记 `overlay_dirty = true`，下次保存写 v3；
* `.json` 里的 `#annot:<id>` 条目在迁移后从 `custom_previews` 移除（`data_dirty = true`），
  但**不立即删文件**；保存时写新的 `.json`（不再含这些条目）。

### 2.3 加载顺序调整

当前：`load_overlay` → `load_sidecar`。v2 正文在 sidecar 里，所以 P1 改为：

1. `load_sidecar`（拿到 `custom_previews`，含旧 `#annot:` 正文）；
2. `load_overlay`（v2 用标题查正文；v3 直接用 `body`）；
3. 迁移清理 `#annot:` 预览条目；
4. 读回 PDF（S6a）时正文直接进 `Annotation.body`。

## 3. 浮动对象与编辑（P1）

### 3.1 `FloatOwner`

```rust
enum FloatOwner {
    Annotation(AnnotationId),
    Custom(usize), // 现有 custom_previews 下标
}
```

* `Float` 的 `custom: Option<usize>` 换成 `owner: FloatOwner`；
* `shift_float_custom_indices` 只处理 `Custom`；
* `activate_custom_preview` 不变（只服务 Custom）；
* 新增 `open_annotation_float(id, at)`：
  * 已存在同 `Annotation(id)` 的浮窗 → 高亮置顶；
  * 否则用 `annotation.body.unwrap_or_default()` 建浮窗，标题“标注 #id”；
  * 不再创建/查找 CustomPreview；
* 删除注释时关闭它的浮窗（`FloatOwner::Annotation(id)`）；
* 侧栏/预览列表只遍历 `custom_previews`，自然不再出现注释正文。

### 3.2 正文编辑 + 撤销

* `AppEdit::SetBody { id, before: Option<String>, after: Option<String> }`；
* 编辑浮窗失焦/Enter 提交时：
  * 取旧 `annotation.body`，若不同则 `apply_edit(SetBody)`；
  * 每次提交一条历史，不按字符记录；
  * `overlay_dirty = true`；
* Ctrl+Z/Ctrl+Shift+Z 对正文生效；
* 删除/重做注释：`AddAnnotation`/`DeleteAnnotation` 快照包含 `body`，删除 `orphan_bodies`。

### 3.3 导出/读回/写回

* `ExportItem.body` 来自 `annotation.body`；
* S6a `merge_readback`：
  * 导入标记注释时 `annotation.body = item.body`；
  * 不再创建 `#annot:` preview；
* S6b `import_foreign`（P1 暂不改）保持只读几何；
* 写回 job 的 body 来源改为 overlays 的 `annotation.body`（P1 不改协议，只改调用点）。

## 4. P1 测试

| 类型 | 用例 |
| --- | --- |
| 模型 | `Annotation::new` body None；`set_body` 语义 |
| 持久化 | v3 往返（四种注释 + 正文）；v2→v3 迁移（有正文/无正文/正文缺失）；`.json` 里 `#annot:` 被清掉 |
| 回放 | 新建注释 → 打开正文浮窗 → 编辑 → undo/redo → 保存 → 重开（正文还在、侧栏无 `#annot:`） |
| 导出/写回 | 正文来自 annotation；空正文不写 `/Contents` |
| 回归 | 现有 347 tests 全绿；预览列表条目数/mm 行为符合预期 |

## 5. P2：一视同仁 + xref 身份

### 5.1 模型扩展

```rust
struct PdfObjectRef { num: u32, gen: u16 }

pub struct Annotation {
    // P1 字段…
    /// 来自 PDF 的对象号；新建未写入时为 None。
    pub pdf_ref: Option<PdfObjectRef>,
}
```

* 从 PDF 读入的四种注释全部进 `AnnotationStore`（不再有 `foreign_annotations` 只读层）；
* 外部注释的 `id` 用本地分配的 `AnnotationId`（仅内存）；`pdf_ref` 是真实身份；
* 删除/改正文只改内存 + 标记 `pdf_dirty`（新脏位，供「写入 PDF」使用）；
* 几何/颜色不可编辑（UI 不提供；不是权限差异）。

### 5.2 身份解析（**先做 spike**）

这一步是本计划的技术风险点：

1. **解析页面 `/Annots`**
   * 直接对象：`/Annots [ 9 0 R <<…>> ]` → `Some(9,0)` / inline；
   * 页面对象本身可能在 ObjStm → 需要解压 ObjStm 才能拿到页面字典。
2. **ObjStm / xref stream**
   * xref stream 的 type 2 条目给出对象号 + 在哪个 ObjStm + 偏移；
   * 需要解压 ObjStm、按偏移读出对象字典；
   * Adobe 样本是检验样本。
3. **与 PDFium 读回的对应**
   * PDFium 的 `/Annots` 顺序与数组一致；按页把 `ReadAnnotation` 与解析出的 ref 对齐；
   * 内联条目 → `pdf_ref = None`（P2 不支持编辑/删除，明确提示）。
4. **写前重读**
   * 打开文档到写入之间，PDF 可能被外部改动；
   * 写回前重新解析当前源文件，核对对象号；找不到/重复 → 拒绝并提示。

**spike 验收**：对 `tests/fixtures/s6b/*.pdf`，每页的 ref 数量与 PDFium 注释数量一致；
能拿到 Okular/Foxit/Firefox 的直接对象号；Adobe 的 ObjStm 也能拿到；不能有猜测。

### 5.3 写回协议（job）

```json
{
  "pdf": "...", "output": "...", "incremental": true,
  "pages": [...],
  "updates": [ { "num": 16, "gen": 0, "body": "新正文" } ],
  "deletes": [ { "num": 16, "gen": 0 } ]
}
```

* `updates`：`FPDFAnnot_SetStringValue("Contents", …)`（正文 only）；
* `deletes`：从页面 `/Annots` 去掉对应 ref；如果链接到 `/Popup` 子对象，一并删；
* 新增：沿用 `pages`（新注释，无 ref）；
* 仍然只处理四种注释；链接/Widget 不动。

### 5.4 Popup 删除

* 父注释通过 `FPDFAnnot_GetLinkedAnnot(annot, "Popup")` 找子对象；
* 在页面注释列表里按引用/句柄找到索引并 `FPDFPage_RemoveAnnot`；
* 若子对象找不到（孤儿/共享）→ 跳过并记录警告，不影响父删除。

### 5.5 P2 与 A 的关系

* PDF 里已有的注释从 PDF 读；
* `.annot.json`（P2 仍存在）只承载“我们本地的、还没写进 PDF 的注释”；
* 外部注释的正文修改/删除不写 A/J；
* `保存自定义数据` 不落盘外部修改；只有「写入 PDF」落盘；
* 状态栏/按钮提示（至少文字）：“外部注释有未写入 PDF 的修改”。

## 6. P2 测试

| 类型 | 用例 |
| --- | --- |
| spike | 5 个真实样本的 ref 数量/对象号；Adobe ObjStm |
| 读正文 | Okular/Evince/Firefox/Adobe/Foxit 样本 `/Contents` 与浮窗正文一致 |
| 改正文 | 改外部的正文 → 写 PDF → 重开/本程序读回为新正文 |
| 删除 | 删除带 Popup 的高亮 → 父/子都消失；其它注释/链接不受影响 |
| 拒绝 | 对象号缺失/重复/内联 → 拒绝并提示 |
| 回归 | 我们自己的注释写回（D-144）不受影响；347 tests 全绿 |

## 7. 提交顺序（建议）

1. **P1-a**：模型 `body`、A v3、迁移、测试（不动 UI）；
2. **P1-b**：`FloatOwner`、注释浮窗、正文编辑 + SetBody 撤销、预览列表清理；
3. **P1-c**：导出/读回/写回改用 `body`；回归测试；
4. **P1.5**：确认无回归 → 删除 v2 迁移路径，固定 v3；
5. **P2-a**：`pdf_index` spike（xref/ObjStm/Annots refs）+ 测试；
6. **P2-b**：统一模型 + 外部正文/删除（内存）+ job updates/deletes + Popup；
7. **P2-c**：测试/手动验收（Okular 改正文、删除、写回后再打开）。

## 8. 风险与回退

| 风险 | 回退 |
| --- | --- |
| P1 迁移把正文弄丢 | 迁移前备份 `.json`/A；v2 文件不删；测试覆盖有/无/缺失正文 |
| 浮窗改造影响自定义 preview | `FloatOwner` 分支保持 Custom 行为不变；回归现有 preview 测试 |
| P2 ObjStm 解析失败 | spike 先做；失败则 P2 只支持直接对象，ObjStm 明确提示“暂不支持删除/改正文” |
| 误删外部注释 | 对象号缺失/重复一律拒绝；写前重读；删除前备份 |
| 未保存修改丢失 | 用户已接受显式保存；P2 至少给文字提示 |

## 9. 待确认

1. P2 的“外部注释删除”是否允许删除**带正文**的注释（正文随注释一起消失）？默认允许。
2. P2 不支持内联外部注释（无可用的 xref 身份）是否可接受？还是必须外置后支持？
3. `pdf_ref` 是否需要在 A v3 里缓存（加速重开）？建议不缓存，打开时重读，避免对象号失效。
