# backend-pdfium

把 [`pdfium-render`](https://crates.io/crates/pdfium-render) 适配成 [`pdfcore`](../pdfcore) 的 trait 实现。
本文件只讲**怎么把 PDFium 跑起来**与适配层的取舍；架构现状见
[docs/current/architecture.md](../../docs/current/architecture.md) 与
[docs/current/rendering.md](../../docs/current/rendering.md)（§4 后端渲染路径与校验）。

| pdfcore trait | 本 crate 的实现 | 关键点 |
| --- | --- | --- |
| `PdfBackend` | `PdfiumBackend` | `max_concurrency = 1`（PDFium 非线程安全） |
| `Document` | `PdfiumDocument` | 打开时一次性备好**页几何表**与**导航索引**（D-01 / D-08） |
| `PageRaster` | `PdfiumRaster` | 整页 + **真正的部分渲染**块（`set_origin`），块尺寸/整像素有硬校验（D-95） |
| `TextLayer` | `PdfiumTextLayer` | 逐字符包围盒 + 搜索 |

## 一、准备 PDFium 动态库（必做）

`pdfium-render` 在**运行期**加载 PDFium，因此编译不需要它，但**跑测试/运行程序需要**：

```bash
scripts/fetch-pdfium.sh            # 自动识别平台
scripts/fetch-pdfium.sh linux-x64  # 或显式指定
```

产物放在 `vendor/pdfium-<平台>/lib/`（已被 `.gitignore` 忽略）。

> **Windows 的坑**：bblanchon 的 tar 包里，运行库 `pdfium.dll` 在 **`bin/`** 下，
> 而 `lib/pdfium.dll.lib` 是**导入库**（链接期用，运行时不加载）。
> `scripts/fetch-pdfium.sh` 会把运行库归位到 `lib/` 并删掉导入库/静态库；
> 读取顺序里也包含 `bin/`，所以两种布局都能用。
>
> **Windows 上推荐用 PowerShell 脚本**（不依赖 bash）：
> `scripts\fetch-pdfium.ps1`（下载并归位）、`scripts\package.ps1`（构建+打包+`--selftest`）。
>
> 加载失败时的排查顺序：`app.exe --selftest` 的输出里每个候选路径都标了
> "文件存在/不存在" —— 不存在 = 没下载或位置不对；存在但失败 = 位数不对（x64 vs x86）
> 或缺少 VC++ 运行库。查找顺序见
[`library::pdfium`](src/library.rs)：`PDFIUM_PATH` 环境变量 → 仓库内 `vendor/`（从当前目录向上 3 层）
→ 可执行文件同级及 `lib/` → 系统库。

> **如果 GitHub 下载卡住**（部分地区/网络环境常见），可任选一种替代来源，解压出
> `libpdfium.so` / `libpdfium.dylib` / `pdfium.dll` 放到 `vendor/pdfium-<平台>/lib/` 即可：
> * NuGet 上的 `bblanchon.PDFium.*` 系列（含各平台原生库）；
> * PyPI 上 `pypdfium2` 的 wheel（内部为 `pypdfium2_raw/libpdfium.so`）；
> * 发行版自带的 `libpdfium`（此时无需 vendor 目录，会走系统库回退）。
>
> 本项目在开发环境实测：GitHub release 资产会卡住，改用 PyPI 镜像的 wheel 取到了
> `libpdfium.so`（7.7 MB，含所需 `FPDF_*` 符号）。

## 二、运行测试

```bash
cargo test -p backend-pdfium
```

测试用的 PDF 由 `tests/common/mod.rs` **现场生成**（2 页、其中 1 页带 `/Rotate 90`、
含大纲与链接注释），因此仓库里没有二进制夹具。若找不到动态库，测试会**跳过并打印指引**
（而不是静默通过或直接 panic）。

## 三、实现中踩到的两个真实坑（都有测试守着）

1. **`PdfPage::width()/height()` 返回的是已应用 `/Rotate` 的显示尺寸**，而 `pdfcore` 的
   `PageGeometry::size` 约定为"未旋转的 MediaBox"。适配层因此在 90°/270° 时把宽高换回来
   （见 `document.rs::collect_geometries`），否则布局会把页面再旋转一次。
2. **旋转由适配层全权负责**：渲染时固定传 `rotate(最终旋转, automatic_orientation = false)`，
   不再叠加 PDFium 自己的隐式旋转，否则"页面自带旋转 ⊕ 用户旋转"会双重应用。
   对应测试：`rotated_page_is_rendered_in_displayed_orientation`。

## 四、已知取舍

* `Source::Bytes` 会**拷贝一次**字节：PDFium 的 `FPDF_LoadMemDocument64` 要求缓冲区在文档
  存活期间有效，因此使用 `load_pdf_from_byte_vec` 把所有权交给 pdfium-render（路径来源零拷贝）。
* `NavigationIndex::named_destinations` 目前为空：pdfium-render 未暴露"名称 → 目标"查询，
  但 PDFium 在给出 link/bookmark 的 destination 时**已经把命名目标解析成具体页**，
  所以 UI 仍然不会遇到"未解析"的目标。
* `TextSpan` 目前只给一个覆盖全页的段（字号/样式提取留待后续）；
  影响的是样式提示，不影响选中与高亮所依赖的字符包围盒。
