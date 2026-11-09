//! 文档层：后端入口、只读结构信息、大纲与链接。
//!
//! 这一层对应 PDF 里"不变的那部分"：页数、页尺寸、`/Rotate`、元数据、书签树、超链接。
//! 页面**像素**由 [`crate::render::PageRaster`] 负责，页面**文字**由 [`crate::text::TextLayer`] 负责。

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use crate::error::{CoreError, Result};
use crate::geometry::{Point, Rect, Rotation, Size};
use crate::ids::PageIndex;
use crate::layout::PageGeometry;
use crate::render::PageRaster;
use crate::text::TextLayer;

/// 打开文档的数据来源。
///
/// 显式建模"路径 vs 内存"两种来源，是为了同时支持：常规开文件（可 mmap / 增量加载）、
/// 浏览器/网络下载后直接打开、以及单测里用内存字节流（不产生临时文件）。
#[derive(Debug)]
pub enum Source<'a> {
    /// 文件路径。
    Path(&'a Path),
    /// 内存中的完整 PDF 字节。
    Bytes(&'a [u8]),
}

/// 文档权限（来自 PDF 的加密字典；未加密文档全部为 `true`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// 是否允许打印。
    pub print: bool,
    /// 是否允许复制/提取文本（为 `false` 时 UI 应禁用复制按钮）。
    pub copy_text: bool,
    /// 是否允许修改内容。
    pub modify: bool,
    /// 是否允许添加批注。
    pub annotate: bool,
}

impl Permissions {
    /// 全部允许（未加密文档的默认值）。
    pub const fn all_allowed() -> Self {
        Self {
            print: true,
            copy_text: true,
            modify: true,
            annotate: true,
        }
    }
}

impl Default for Permissions {
    /// 默认全部允许：未加密的 PDF 不受限制，这样"忘记设置权限"不会误伤正常文档。
    fn default() -> Self {
        Self::all_allowed()
    }
}

/// 文档级元数据与统计信息。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentInfo {
    /// 总页数。
    pub page_count: u32,
    /// 标题。
    pub title: Option<String>,
    /// 作者。
    pub author: Option<String>,
    /// 主题。
    pub subject: Option<String>,
    /// 关键词。
    pub keywords: Option<String>,
    /// 生成程序。
    pub producer: Option<String>,
    /// 是否加密。
    pub encrypted: bool,
    /// 权限。
    pub permissions: Permissions,
    /// 文件字节数（内存来源时为 `None`）。
    pub file_size_bytes: Option<u64>,
}

impl fmt::Display for DocumentInfo {
    /// 供窗口标题使用：`标题 — 作者`，缺字段时自动退化。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.title, &self.author) {
            (Some(title), Some(author)) => write!(f, "{title} — {author}"),
            (Some(title), None) => write!(f, "{title}"),
            (None, Some(author)) => write!(f, "{author}"),
            (None, None) => write!(f, "未命名文档"),
        }
    }
}

/// 跳转目标：大纲项与超链接共用。
///
/// **总是已解析**：后端在 `open` 时把 PDF 的命名目标（`/Named`、`/Dests` 字典里的名称）
/// 统一解析成具体页，因此这里没有"待解析"变体 —— UI 点击链接时不需要再访问后端，
/// 也不会遇到"点了没反应"的中间状态（解析失败的名称见 [`NavigationIndex::unresolved`]）。
#[derive(Debug, Clone, PartialEq)]
pub enum Destination {
    /// 跳到某页（可选页内偏移，坐标系为 PDF 页面空间）。
    Page {
        /// 目标页。
        page: PageIndex,
        /// 页内落点（通常是大纲项指向的纵坐标）。
        offset: Option<Point>,
    },
    /// 外部 URI。
    Uri(String),
}

/// 页内超链接。
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// 链接热区（PDF 页面空间，左下原点）。
    pub rect: Rect,
    /// 跳转目标。
    pub destination: Destination,
}

/// 大纲（书签）节点。子节点用 `Vec` 直接嵌套，保持与 PDF 树一致的结构。
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineNode {
    /// 显示标题。
    pub title: String,
    /// 跳转目标；少数节点只有标题没有目标。
    pub destination: Option<Destination>,
    /// 子节点。
    pub children: Vec<OutlineNode>,
}

/// 导航索引：打开时**一次性解析并合并存储**的全部导航数据。
///
/// ## 为什么合并成一个结构
/// * 点击链接/大纲时不再访问后端（纯内存查找，O(1)）；
/// * 页内链接可以随页面一起预取，缩略图侧边栏也能一次拿到；
/// * 命名目标解析失败时不会被静默丢弃，而是记进 [`NavigationIndex::unresolved`] 供诊断。
///
/// 代价是打开时要遍历大纲树、每页 `/Annots` 与 `/Dests`。若实测拖慢打开，
/// 后端可以把构建改成 `OnceLock` 惰性执行 —— **接口签名不变**。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NavigationIndex {
    /// 大纲树（`destination` 已解析）。
    pub outline: Vec<OutlineNode>,
    /// 逐页链接热区；下标 = 页号，越界即该页无链接。
    pub links_by_page: Vec<Vec<Link>>,
    /// 名称 → 目标（PDF 交叉引用、命名目标）。
    pub named_destinations: HashMap<String, Destination>,
    /// 无法解析的名称；仅用于诊断（UI 可忽略）。
    pub unresolved: Vec<String>,
}

impl NavigationIndex {
    /// 空索引（无大纲、无链接）—— 给不支持导航的后端用。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 某页的链接热区；越界返回空切片（UI 不必自己判边界）。
    pub fn links_on(&self, page: PageIndex) -> &[Link] {
        self.links_by_page
            .get(page.get() as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// 是否没有任何导航数据。
    pub fn is_empty(&self) -> bool {
        self.outline.is_empty() && self.links_by_page.iter().all(Vec::is_empty)
    }
}

/// 后端能力声明。
///
/// ## 这个类型存在的目的
/// 让 UI 在**运行时**决定"要不要显示搜索框 / 复制按钮 / 密码输入框"，而不是在编译期硬编码
/// PDFium 的假设。这样同一个 UI 可以同时跑在功能完整的 PDFium 后端和功能受限的 hayro 后端上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// 是否提供文本层（决定能否选中、复制、搜索）。
    pub text_layer: bool,
    /// 是否支持全文搜索（`text_layer` 为真但仍可能不支持搜索）。
    pub search: bool,
    /// 是否支持加密文档与密码。
    pub encryption: bool,
    /// 是否提供大纲。
    pub outline: bool,
    /// 是否提供页内链接。
    pub links: bool,
    /// 后端允许的**并发渲染**上限。PDFium 必须为 1（它不是线程安全的）。
    pub max_concurrency: u8,
}

impl Capabilities {
    /// 什么能力都没有（仅能渲染）。
    pub const fn minimal() -> Self {
        Self {
            text_layer: false,
            search: false,
            encryption: false,
            outline: false,
            links: false,
            max_concurrency: 1,
        }
    }

    /// 全能力（PDFium 后端的典型取值）。
    pub const fn full() -> Self {
        Self {
            text_layer: true,
            search: true,
            encryption: true,
            outline: true,
            links: true,
            max_concurrency: 1,
        }
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::minimal()
    }
}

/// **后端工厂**：本 crate 中唯一"知道某个具体 PDF 库存在"的抽象。
///
/// ## 这个 trait 存在的目的
/// 它是可替换性的根节点：`egui` 前端只持有 `Arc<dyn PdfBackend>`（或直接持有它产出的
/// [`OpenedDocument`]），因此把 `pdfium` 后端换成 `hayro` 后端、或在测试里换成内存后端，
/// 都不需要改动 UI 与会话代码。
///
/// ## 实现者义务
/// * `open` 遇到加密文档且未提供密码时返回 [`crate::error::CoreError::PasswordRequired`]，
///   调用方据此弹密码框后重试；密码错误返回 [`crate::error::CoreError::IncorrectPassword`]。
/// * 不得让 `PdfBackend` 的实例在 `open` 中持有文档所有权：文档生命周期由返回的
///   [`OpenedDocument`] 中的 `Arc` 决定，这样"关闭文档"就等于 drop，不需要额外的 close 方法。
/// * 必须至少提供一个 [`PageRaster`]；[`TextLayer`] 可缺省（能力声明为 `false`）。
pub trait PdfBackend: Send + Sync {
    /// 后端名称（用于"关于"对话框与日志，例如 `"pdfium"`）。
    fn name(&self) -> &'static str;

    /// 声明能力，UI 据此决定显示哪些功能入口。
    fn capabilities(&self) -> Capabilities;

    /// 打开文档。
    fn open(&self, source: Source<'_>, password: Option<&str>) -> Result<OpenedDocument>;
}

/// **文档结构**：只读、无状态、可并发访问的文档信息。
///
/// ## 这个 trait 存在的目的
/// 把"文档结构查询"从具体后端中抽出，使得布局引擎（[`crate::layout::PageLayout`]）与
/// 会话（[`crate::session::DocumentSession`]）可以在不知道 PDFium 存在的情况下工作，
/// 也让单测可以构造一个"3 页、每页尺寸写死"的假文档。
///
/// ## 实现者义务
/// * 所有方法都是 `&self`，需要内部可变性的实现（如惰性缓存）请用 `Mutex`/`OnceLock`。
/// * **`page_geometries()` 必须在打开时备好**并缓存（O(1) 返回 `Arc` 克隆）；
///   布局要一次性拿到全部页尺寸才能算出内容总高度与滚动范围。
/// * `PageGeometry` 里的尺寸与旋转都是**未旋转**的 PDF 原始值；旋转由调用方叠加。
/// * 导航属于"可选能力"：不支持时返回 [`NavigationIndex::empty`]，只有真正的解析失败才报错。
pub trait Document: Send + Sync {
    /// 元数据与页数（必须 O(1) 返回，通常在打开时已经解析完毕）。
    fn info(&self) -> &DocumentInfo;

    /// **批量**页几何（原始尺寸 + 每页自带的 `/Rotate`）。
    ///
    /// 打开时已备好，O(1) 返回；调用方只做一次 `Arc` 克隆，不需要逐页跨 FFI 查询。
    fn page_geometries(&self) -> Result<Arc<[PageGeometry]>>;

    /// 某页的原始尺寸（PDF point，未旋转）。
    ///
    /// 默认实现查批量表；越界返回 [`CoreError::PageOutOfRange`]。
    /// 频繁路径请直接用 [`Document::page_geometries`]，避免重复的 `Arc` 克隆与边界检查。
    fn page_size(&self, page: PageIndex) -> Result<Size> {
        let geometries = self.page_geometries()?;
        geometries
            .get(page.get() as usize)
            .map(|geometry| geometry.size)
            .ok_or(CoreError::PageOutOfRange {
                page,
                page_count: geometries.len() as u32,
            })
    }

    /// 某页自带的 `/Rotate`；默认实现查批量表。越界同样返回 [`CoreError::PageOutOfRange`]。
    fn page_rotation(&self, page: PageIndex) -> Result<Rotation> {
        let geometries = self.page_geometries()?;
        geometries
            .get(page.get() as usize)
            .map(|geometry| geometry.rotation)
            .ok_or(CoreError::PageOutOfRange {
                page,
                page_count: geometries.len() as u32,
            })
    }

    /// 导航索引：大纲 + 逐页链接 + 命名目标，**打开时一次性解析并合并存储**。O(1) 返回。
    ///
    /// 取代了早期设计里的 `outline()` 与 `links(page)`：点击链接/大纲时不再访问后端。
    /// 不支持导航的后端返回 [`NavigationIndex::empty`] 即可（不要报错）。
    fn navigation(&self) -> Result<Arc<NavigationIndex>> {
        Ok(Arc::new(NavigationIndex::empty()))
    }
}

/// 一次 `open` 的成果：把文档、光栅器、可选文本层与能力声明打包在一起。
///
/// 这是"用 trait 拼装阅读器"的第一个交接点：后端把三个 `Arc<dyn ...>` 交出来，
/// 会话再把它们与缓存、调度器、布局引擎、选区模型拼成一个 [`crate::session::DocumentSession`]。
///
/// 该类型是 `Clone` 的（内部全是 `Arc`），因此可以"再次装配"：例如把同一个文档
/// 换一套能力声明（去掉文本层）来验证 UI 的降级路径。
#[derive(Clone)]
pub struct OpenedDocument {
    /// 文档结构。
    pub document: Arc<dyn Document>,
    /// 光栅器（必需）。
    pub raster: Arc<dyn PageRaster>,
    /// 文本层（可选：`Capabilities::text_layer` 为 `false` 时必须为 `None`）。
    pub text: Option<Arc<dyn TextLayer>>,
    /// 能力声明。
    pub capabilities: Capabilities,
}

impl OpenedDocument {
    /// 装配。
    pub fn new(
        document: Arc<dyn Document>,
        raster: Arc<dyn PageRaster>,
        text: Option<Arc<dyn TextLayer>>,
        capabilities: Capabilities,
    ) -> Self {
        Self {
            document,
            raster,
            text,
            capabilities,
        }
    }

    /// 页数。
    pub fn page_count(&self) -> u32 {
        self.document.info().page_count
    }

    /// 是否可用文本层。
    pub fn has_text_layer(&self) -> bool {
        self.text.is_some() && self.capabilities.text_layer
    }

    /// 取文本层；缺失时返回 `None`（UI 应据此隐藏搜索/复制入口）。
    pub fn text_layer(&self) -> Option<&Arc<dyn TextLayer>> {
        self.text.as_ref()
    }
}

impl fmt::Debug for OpenedDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenedDocument")
            .field("info", self.document.info())
            .field("capabilities", &self.capabilities)
            .field("has_text_layer", &self.has_text_layer())
            .finish()
    }
}
