# 内部图片引用（设计基线）

> 状态：**B1 完成，B2 进行中**。sidecar v4、`ImageStore`、`pd-img` loader、打开文字浮窗时的 owner
> 懒渲染已接入；图片 @、DestMark 右键、编辑窗口列表、摘要导出 base64 已接入；hover Markdown 开关待做。
> 目标：让 Markdown 正文引用软件内部生成的截图/图片，不读取本地或网络图片。

## 1. 目标与边界

- 支持两类来源：
  - **自定义图片 preview**（`CustomPreview`，kind = Image）；
  - **链接目的地/被引用点截图**（`DestMark`）。
- Markdown 里用普通图片语法引用；图片数据由应用内部解析。
- 图片可以**懒生成**：文件刚打开时已有 preview 没有截图，不要求立即渲染。
- 不启用 `file://`/`http://` 图片 loader；只认内部 scheme。
- LaTeX 公式图片不进入这套引用体系（见第 8 节）。

## 2. 数据模型

### 2.1 图片记录

每个来源在创建时获得稳定记录：

```text
ImageRecord {
    record_id: UUID/ULID,     // 创建时生成，跨会话稳定
    revision: u64,            // 仅影响像素的修改递增
    source: ImageSource,
    width: u32,
    height: u32,
}
```

- `record_id` **不随图片渲染而生成**；创建 preview/链接目的地时就存在。
- `revision` 在以下情况递增：
  - 自定义 preview 的 page/rect/scale/源图片替换；
  - 链接目的地的目标区域/scale 重选；
  - PDF 内容变化导致同一区域重新渲染；
  - 影响 PNG 像素的任何来源修改。
- 仅改标题、正文、显示宽度等不影响图片像素的字段，不递增 revision。

### 2.2 sidecar

`CustomPreview` 与 `DestMark` 的保存结构新增：

```text
image_id: String      // record_id
image_revision: u64
```

- sidecar 版本升级；旧数据迁移时生成 id、revision = 0。
- 不保存 base64/PNG 像素；像素由来源重新渲染，避免 `.pdf.json` 膨胀。
- 读取旧版本时缺字段用默认值；写入当前版本。

### 2.3 来源定位

`ImageSource` 必须足够重新渲染同一图片：

- `CustomPreview`：持久 preview id + page + rect + scale + rotation/PDF revision；
- `DestMark`：page + 目的地 id/下标 + rect + scale。

## 3. Markdown 包装器

Markdown 使用标准图片语法，URI 是内部包装器：

图片语法仍是普通 Markdown 图片；URI 使用内部 `pd-img` 协议，包含
`record_id`、`rev` 与 URL 编码后的 `src`（具体编码由实现固定，不手工编辑）。

- `record_id` 是检索键；
- `rev` 用于判断缓存是否过期；
- `src` 是来源回退地址，用于图片不在缓存时让 owner 重新渲染；
- URI 对用户不透明，由应用生成，不建议手写。

**不自动改写 Markdown 文本**。如果来源更新：

- 优先 `record_id` 不变、只 bump revision；
- 旧引用仍然有效，下一帧取新图；
- 若确实要换新 id，必须维护 old→new 迁移并在显式用户动作时更新正文，不在后台线程修改。

## 4. 解析流程

```text
Markdown 图片 URI
  → 解析 record_id / rev / source
  → ImageStore 按 (record_id, revision) 查 PNG
     命中：交给 image loader
     未命中：返回 Pending，Markdown 先跳过/占位
  → 通知来源 owner 渲染
     CustomPreview：按 page/rect/scale 渲染
     DestMark：按 page/rect/scale 渲染
  → 完成后编码 PNG，写入 ImageStore
  → 失效 egui 图片缓存，request_repaint
  → 下一帧 Markdown 完整渲染
```

- 不阻塞 UI；不要求图片在编辑窗口打开时已经存在。
- Markdown 在图片未就绪时跳过图片，图片完成后重新排版。
- 渲染失败：占位 + 状态栏提示；保留 URI，可重试。
- 纹理上传只在图片真正可见/被引用时进行；不长期持有所有 `TextureHandle`。

## 5. 缓存与内存

- `ImageStore` 存 PNG 字节（或 RGBA），键为 `(record_id, revision)`。
- 设总字节上限与条数上限（例如 64MB / 32 张，具体可调）；LRU 淘汰。
- 淘汰后仍被引用的图片：下次解析未命中 → 走来源重新渲染。
- 旧 revision 可优先淘汰；公式缓存与内部图片缓存分开。
- 记录 `width/height`，用于 Markdown 占位/减少布局跳动。

## 6. UI 入口

- 自定义图片 preview / 链接目的地标识旁加 `@`：
  - 点击复制包装器 URI 到剪贴板；
  - tooltip 说明“复制 Markdown 图片引用”。
- 页面标记区域右键菜单：`复制图片引用`。
- 正文编辑窗口可提供“插入内部图片”列表，但列表应来自**记录**，不是“已生成快照”：
  - 选择时再确保快照生成；
  - 不要求用户先打开图片 preview。
- 正常 Markdown 插入形式仍是普通图片语法，URI 为内部 `pd-img` 包装器。

## 7. 导出

- 内部存储（注释正文、`.pdf.json`）始终保留包装器/id，不写 base64。
- 仅在导出到外部格式或与外部软件交互时：
  - 把引用替换为 `data:image/png;base64,...`，或写出 PNG 资源文件；
  - 给出体积提示；
  - 不默认把 base64 写进 PDF 注释正文（会显著膨胀）。
- 导出是可逆的：包装器仍是内部主数据。

## 8. LaTeX 图片

- 公式图片继续走 `MathCache`（`MathBackend`，当前 `latex-rust` PNG）。
- 不分配 `record_id`，不进入 `ImageStore` 引用体系。
- 可共用底层“PNG → egui 纹理上传”工具，但缓存/错误/DPI 生命周期独立。

## 9. 线程与生命周期

- 渲染调度走现有 session/worker 体系，不直接起裸线程访问 PDF。
- UI 线程只做：解析、占位、纹理上传、状态更新。
- 关闭/切换文档：清空当前文档的 ImageStore/viewport 状态；sidecar 保存 id/revision。
- 保存后原地重开同一文档：保留 id 与缓存；换文档则失效。
- 来源更新：bump revision、失效对应缓存、请求重绘；Markdown 文本不变。

## 9.1 只在打开的文字浮窗内处理图片

这是明确的生命周期约束，不是全局后台服务：

- **没有文字类浮窗打开时**，不扫描 Markdown、不轮询图片变化、不触发 owner 渲染；
- 只有 `FloatContent::Text` 的 Markdown 浮窗处于打开状态时，才解析其中的 `pd-img` 引用；
- 只处理**当前打开浮窗实际引用**的图片，不遍历所有 preview/链接目的地；
- 关闭最后一个文字浮窗后，取消/忽略 pending 渲染；之后即使 preview/目的地变化，也不主动检测；
- 下次打开引用它的文字浮窗时，再按 `record_id + revision` 检查/重新渲染；
- 图片缓存可以保留，但“是否过期”只在被引用/渲染路径上判断，不做定时扫描。

实现上需要维护一个“当前打开的文字浮窗引用集”；owner 渲染任务带上引用计数，
最后一个引用关闭后可以取消尚未开始的任务。

## 10. 分阶段

1. **B1 数据与解析**
   - sidecar 加 `image_id`/`revision`，版本升级与迁移；
   - `ImageStore`、`pd-img` loader、`Pending` 流程；
   - 自定义图片 preview 与 DestMark 的 owner 渲染接入。
2. **B2 UI 与导出**
   - `@` 复制包装器、右键菜单、编辑窗口插入；
   - 导出时 base64/资源替换；
   - 占位尺寸、错误提示、内存上限。
3. **测试**
   - id 持久化/迁移；
   - 缓存未命中 → owner 渲染 → Markdown 完成；
   - revision 更新后旧引用取新图；
   - 导出替换；
   - 多浮窗/多文档隔离。

## 11. 关键决定

- **id 稳定**：创建时生成；内容变化用 revision，不轻易换 id。
- **内部引用不改正文**：后台不写 Markdown；换 id 只走显式迁移。
- **懒渲染**：未命中返回 Pending，owner 触发；Markdown 可先跳过。
- **纹理按需**：存 PNG，不常驻 TextureHandle。
- **导出才 base64**：内部始终保留包装器。
- **LaTeX 独立**：公式不进入图片引用体系。
