//! 文档适配：共享状态、页几何批量表、导航索引预解析。

use std::collections::HashMap;
use std::sync::Arc;

use pdfcore::prelude::*;
use pdfium_render::prelude::{
    PdfBookmark, PdfDestination, PdfDestinationViewSettings, PdfDocument,
    PdfDocumentMetadataTagType, PdfPage, PdfPageIndex, PdfPageRenderRotation, PdfPoints, PdfRect,
    PdfSecurityHandlerRevision, Pdfium,
};

use crate::error::to_core_error;

/// 被 [`PdfiumDocument`]、[`crate::raster::PdfiumRaster`]、[`crate::text::PdfiumTextLayer`]
/// 三个 trait 实现共享的文档状态。
///
/// 三者都以 `Arc<SharedDocument>` 持有同一份状态，正好对应
/// [`OpenedDocument`] 里"文档 / 光栅器 / 文本层"三个 `Arc<dyn ...>`。
pub(crate) struct SharedDocument {
    /// pdfium 文档句柄。生命周期为 `'static` 是因为它借用的是进程级 `Pdfium` 单例。
    pub(crate) document: PdfDocument<'static>,
    pub(crate) info: DocumentInfo,
    pub(crate) geometries: Arc<[PageGeometry]>,
    pub(crate) navigation: Arc<NavigationIndex>,
}

impl SharedDocument {
    /// 打开文档并**一次性备好**全部结构信息（页几何 + 导航索引 + 元数据）。
    ///
    /// 这正是 D-01 / D-08 的落地：运行期查询都变成纯内存操作。
    pub(crate) fn open(
        pdfium: &'static Pdfium,
        source: Source<'_>,
        password: Option<&str>,
    ) -> Result<Self> {
        let document = match source {
            Source::Path(path) => pdfium.load_pdf_from_file(path, password),
            // 用 `byte_vec` 而不是 `byte_slice`：PDFium 的 FPDF_LoadMemDocument64 要求
            // 缓冲区在文档存活期间一直有效，而 `Source::Bytes` 是借用进来的，
            // 无法保证这一点。`load_pdf_from_byte_vec` 会把 Vec 的所有权交给
            // pdfium-render，从而让文档句柄的生命周期与进程级 `Pdfium` 一致（`'static`）。
            // 代价是一次文件字节的拷贝（仅内存来源有此代价，路径来源零拷贝）。
            Source::Bytes(bytes) => pdfium.load_pdf_from_byte_vec(bytes.to_vec(), password),
        }
        .map_err(to_core_error)?;

        let page_count = document.pages().len() as u32;
        let geometries = collect_geometries(&document, page_count)?;
        let info = collect_info(&document, page_count);
        let navigation = Arc::new(collect_navigation(&document, page_count, &geometries));

        Ok(Self {
            document,
            info,
            geometries,
            navigation,
        })
    }

    /// 取某页，并做边界检查（越界返回 [`CoreError::PageOutOfRange`]）。
    pub(crate) fn page(&self, index: PageIndex) -> Result<PdfPage<'_>> {
        let page_count = self.geometries.len() as u32;
        if index.get() >= page_count {
            return Err(CoreError::PageOutOfRange {
                page: index,
                page_count,
            });
        }
        self.document
            .pages()
            .get(index.get() as PdfPageIndex)
            .map_err(to_core_error)
    }
}

/// **文档结构**的 PDFium 实现。
pub struct PdfiumDocument(pub(crate) Arc<SharedDocument>);

impl Document for PdfiumDocument {
    fn info(&self) -> &DocumentInfo {
        &self.0.info
    }

    fn page_geometries(&self) -> Result<Arc<[PageGeometry]>> {
        Ok(self.0.geometries.clone())
    }

    fn navigation(&self) -> Result<Arc<NavigationIndex>> {
        Ok(self.0.navigation.clone())
    }
}

// ------------------------------------------------------------------ 采集

fn collect_geometries(
    document: &PdfDocument<'static>,
    page_count: u32,
) -> Result<Arc<[PageGeometry]>> {
    let mut geometries = Vec::with_capacity(page_count as usize);
    for index in 0..page_count {
        let page = document
            .pages()
            .get(index as PdfPageIndex)
            .map_err(to_core_error)?;
        let rotation = to_rotation(page.rotation().map_err(to_core_error)?);
        // 关键：PDFium 的 FPDF_GetPageWidthF/HeightF 返回的是**已应用 /Rotate 的显示尺寸**，
        // 而 pdfcore 的 PageGeometry.size 约定为"未旋转的 MediaBox 尺寸"（旋转由布局叠加）。
        // 因此这里在旋转 90°/270° 时把宽高换回来。
        let displayed = Size::new(points(page.width()), points(page.height()));
        let size = if rotation.swaps_dimensions() {
            Size::new(displayed.height, displayed.width)
        } else {
            displayed
        };
        // 渲染盒原点（MediaBox ∩ CropBox 的左下角）：文本/注释坐标按 MediaBox 给出，
        // 而渲染按这个盒子 —— 原点不为 0 的 PDF 上，所有页面空间几何都会整体偏移，
        // 因此这里记录下来，供文本与链接坐标归一化（见 PageGeometry::origin）。
        let origin = match page.boundaries().bounding() {
            Ok(boundary) => Point::new(
                points(boundary.bounds.left()),
                points(boundary.bounds.bottom()),
            ),
            Err(_) => Point::ORIGIN,
        };
        geometries.push(PageGeometry::with_origin(size, rotation, origin));
    }
    Ok(geometries.into())
}

fn collect_info(document: &PdfDocument<'static>, page_count: u32) -> DocumentInfo {
    let metadata = document.metadata();
    let tag =
        |kind: PdfDocumentMetadataTagType| metadata.get(kind).map(|tag| tag.value().to_string());

    let permissions_api = document.permissions();
    // 未加密文档的权限查询会失败（pdfium 返回 -1），因此缺省视为"允许"。
    let permissions = Permissions {
        print: permissions_api.can_print_high_quality().unwrap_or(true)
            || permissions_api
                .can_print_only_low_quality()
                .unwrap_or(false),
        copy_text: permissions_api
            .can_extract_text_and_graphics()
            .unwrap_or(true),
        modify: permissions_api
            .can_modify_document_content()
            .unwrap_or(true),
        annotate: permissions_api
            .can_add_or_modify_text_annotations()
            .unwrap_or(true),
    };
    // 有安全处理器（security handler）= 文档加密。
    //
    // **坑**：pdfium-render 对未加密文档返回的是 `Ok(Unprotected)`（PDFium 的 -1），
    // 不是 `Err`。早先写成 `.is_ok()` 把每个文档都判成"加密"，导出时被一律拒绝。
    let encrypted = match permissions_api.security_handler_revision() {
        Ok(PdfSecurityHandlerRevision::Unprotected) => false,
        // Revision2/3/4 是加密；未知修订号（例如 AES-256 的 5/6）按加密保守处理
        Ok(_) | Err(_) => true,
    };

    DocumentInfo {
        page_count,
        title: tag(PdfDocumentMetadataTagType::Title),
        author: tag(PdfDocumentMetadataTagType::Author),
        subject: tag(PdfDocumentMetadataTagType::Subject),
        keywords: tag(PdfDocumentMetadataTagType::Keywords),
        producer: tag(PdfDocumentMetadataTagType::Producer),
        encrypted,
        permissions,
        // 内存来源没有文件大小；路径来源由 app 层用 fs::metadata 填充（后端不猜）
        file_size_bytes: None,
    }
}

/// 采集导航索引：大纲树 + 逐页链接。
///
/// **命名目标不需要额外解析**：PDFium 在提供 link/bookmark 的 destination 时已经把
/// `/Named`、`/Dests` 名称解析成了具体页（[`PdfDestination::page_index`] 直接给页号）。
/// pdfium-render 没有暴露"名称 → 目标"的独立查询，因此 `named_destinations` 保持为空，
/// 这也符合 D-08 的目标：**UI 永远不会拿到"未解析"的目标**。
fn collect_navigation(
    document: &PdfDocument<'static>,
    page_count: u32,
    geometries: &[PageGeometry],
) -> NavigationIndex {
    /// 把页面空间的矩形归一化到"渲染盒"坐标系（减去该页原点）。
    fn normalize(rect: Rect, geometries: &[PageGeometry], page: PageIndex) -> Rect {
        match geometries.get(page.get() as usize) {
            Some(geometry) if geometry.origin != Point::ORIGIN => Rect::new(
                rect.x - geometry.origin.x,
                rect.y - geometry.origin.y,
                rect.width,
                rect.height,
            ),
            _ => rect,
        }
    }

    // 只从**顶层**开始：`bookmarks().iter()` 是整棵树的先序遍历，深处节点会被
    // 祖先的 `iter_direct_children()` 再收集一次（深度 d 的节点出现 d 次）。
    let mut outline = Vec::new();
    let mut current = document.bookmarks().root();
    while let Some(bookmark) = current {
        outline.push(bookmark_to_node(&bookmark));
        current = bookmark.next_sibling();
    }

    let mut links_by_page = Vec::with_capacity(page_count as usize);
    for index in 0..page_count {
        let mut links = Vec::new();
        if let Ok(page) = document.pages().get(index as PdfPageIndex) {
            for link in page.links().iter() {
                let Some(destination) = link.destination().and_then(|dest| to_destination(&dest))
                else {
                    continue;
                };
                let Ok(rect) = link.rect() else { continue };
                // 归一化到渲染盒坐标系，否则原点不为 0 的 PDF 上热区会整体偏移
                let normalized = normalize(to_rect(rect), geometries, PageIndex::new(index));
                links.push(Link {
                    rect: normalized,
                    destination,
                });
            }
        }
        links_by_page.push(links);
    }

    NavigationIndex {
        outline,
        links_by_page,
        named_destinations: HashMap::new(),
        unresolved: Vec::new(),
    }
}

fn bookmark_to_node(bookmark: &PdfBookmark<'_>) -> OutlineNode {
    OutlineNode {
        title: bookmark.title().unwrap_or_default(),
        destination: bookmark
            .destination()
            .and_then(|dest| to_destination(&dest)),
        children: bookmark
            .iter_direct_children()
            .map(|child| bookmark_to_node(&child))
            .collect(),
    }
}

/// pdfium 的 destination → 接口层目标（总是已解析成具体页）。
fn to_destination(destination: &PdfDestination<'_>) -> Option<Destination> {
    let page = destination.page_index().ok()?;
    let offset = match destination.view_settings() {
        Ok(PdfDestinationViewSettings::SpecificCoordinatesAndZoom(Some(x), Some(y), _)) => {
            Some(Point::new(points(x), points(y)))
        }
        _ => None,
    };
    Some(Destination::Page {
        page: PageIndex::new(page.max(0) as u32),
        offset,
    })
}

// ------------------------------------------------------------------ 小工具

/// `PdfPoints` → `f32`。
pub(crate) fn points(value: PdfPoints) -> f32 {
    value.value
}

/// pdfium 的 `/Rotate` → 接口层旋转。
pub(crate) fn to_rotation(rotation: PdfPageRenderRotation) -> Rotation {
    match rotation {
        PdfPageRenderRotation::None => Rotation::None,
        PdfPageRenderRotation::Degrees90 => Rotation::Clockwise90,
        PdfPageRenderRotation::Degrees180 => Rotation::Clockwise180,
        PdfPageRenderRotation::Degrees270 => Rotation::Clockwise270,
    }
}

/// 接口层旋转 → pdfium 旋转。
pub(crate) fn to_pdfium_rotation(rotation: Rotation) -> PdfPageRenderRotation {
    match rotation {
        Rotation::None => PdfPageRenderRotation::None,
        Rotation::Clockwise90 => PdfPageRenderRotation::Degrees90,
        Rotation::Clockwise180 => PdfPageRenderRotation::Degrees180,
        Rotation::Clockwise270 => PdfPageRenderRotation::Degrees270,
    }
}

/// pdfium 矩形（PDF 页面空间，左下原点）→ 接口层矩形。
///
/// 接口层的 [`Rect`] 只是"x/y/宽/高"的容器；[`PageText`] 的字符包围盒统一采用
/// PDF 页面空间，因此这里以 `left/bottom` 为原点，不做 y 翻转。
pub(crate) fn to_rect(rect: PdfRect) -> Rect {
    let left = points(rect.left());
    let bottom = points(rect.bottom());
    Rect::new(
        left,
        bottom,
        points(rect.right()) - left,
        points(rect.top()) - bottom,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_mapping_round_trips() {
        for rotation in [
            Rotation::None,
            Rotation::Clockwise90,
            Rotation::Clockwise180,
            Rotation::Clockwise270,
        ] {
            assert_eq!(to_rotation(to_pdfium_rotation(rotation)), rotation);
        }
    }
}
