//! 回放/基线用的**样本**：现场生成，不入库。
//!
//! 五种样本覆盖"分块 / 旋转 / 超大页 / 多页 / 预览截图"这几条与渲染强相关的路径。
//! 生成用 `backend_pdfium::fixtures`（纯字符串拼 PDF，不需要动态库），因此样本可以在
//! 任何环境里复现 —— 这比把几十 MB 的 PDF 提交进仓库可靠。
//!
//! 需要 sidecar 的样本（`preview`）会一并写出 `<pdf>.json`，这样"打开 → 唤出预览"
//! 就能在回放里复现真实的数据链路（存档 → 按记录分辨率请求渲染 → 拼图）。

use std::io;
use std::path::{Path, PathBuf};

/// 一种样本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    /// A4 三页，满页横线（放大后必然分块）。
    A4,
    /// 两页，其中一页 `/Rotate 90`，带链接与大纲。
    Rotated,
    /// 单页 A0（100% 就超过 4MP → 直接走分块）。
    Large,
    /// 120 页 A4（跳页 / 滚动范围）。
    Many,
    /// A4 三页 + sidecar 里一条图片 preview（记录分辨率 800%）。
    Preview,
}

impl Sample {
    /// 样本名（命令行用）。
    pub fn name(self) -> &'static str {
        match self {
            Self::A4 => "a4",
            Self::Rotated => "rotated",
            Self::Large => "large",
            Self::Many => "many",
            Self::Preview => "preview",
        }
    }

    /// 从命令行名字解析。
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "a4" => Some(Self::A4),
            "rotated" | "two-page" => Some(Self::Rotated),
            "large" => Some(Self::Large),
            "many" => Some(Self::Many),
            "preview" => Some(Self::Preview),
            _ => None,
        }
    }

    /// 默认文件名。
    pub fn file_name(self) -> &'static str {
        match self {
            Self::A4 => "a4_3p.pdf",
            Self::Rotated => "rotated_2p.pdf",
            Self::Large => "large_a0.pdf",
            Self::Many => "many_120p.pdf",
            Self::Preview => "preview.pdf",
        }
    }

    /// 全部样本（脚本自检用）。
    pub fn all() -> [Self; 5] {
        [Self::A4, Self::Rotated, Self::Large, Self::Many, Self::Preview]
    }

    /// PDF 字节。
    pub fn pdf_bytes(self) -> Vec<u8> {
        match self {
            Self::A4 | Self::Preview => {
                backend_pdfium::fixtures::many_page_pdf(3, 595.28, 841.89)
            }
            Self::Rotated => backend_pdfium::fixtures::two_page_pdf(),
            // A0：2384×3370 pt，100% 时 2384×3370 px ≈ 8MP > 4MP，必然分块
            Self::Large => backend_pdfium::fixtures::many_page_pdf(1, 2383.94, 3370.39),
            Self::Many => backend_pdfium::fixtures::many_page_pdf(120, 595.28, 841.89),
        }
    }

    /// 写进目录（文件名固定），返回 PDF 路径。
    pub fn write(self, dir: &Path) -> io::Result<PathBuf> {
        self.write_to(&dir.join(self.file_name()))
    }

    /// 写到指定路径；需要 sidecar 的样本会同时写 `<path>.json`。
    pub fn write_to(self, path: &Path) -> io::Result<PathBuf> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(path, self.pdf_bytes())?;
        if self == Self::Preview {
            let sidecar = sidecar_path(path);
            std::fs::write(sidecar, preview_sidecar())?;
        }
        Ok(path.to_path_buf())
    }
}

/// 与 app 的规则一致：`<pdf>.json`（不是替换扩展名）。
pub fn sidecar_path(pdf: &Path) -> PathBuf {
    let mut name = pdf.as_os_str().to_os_string();
    name.push(".json");
    PathBuf::from(name)
}

/// 预览样本的 sidecar：一条图片 preview，**记录分辨率 800%**。
///
/// 选 800% 是为了让"尺寸与当前缩放无关"可判定：脚本分别在 100% 与 800% 下唤出它，
/// 实际纹理尺寸必须相同（= 区域 × 10.486，两端取整）。
fn preview_sidecar() -> String {
    // 区域 150×80pt（A4 第 1 页中部），800% → 约 1573×839 px，需要 4×2 = 8 块
    format!(
        "{{\n  \"version\": 2,\n  \"custom_images\": [\n    {{\"page\": 0, \
         \"point\": [150, 680], \"rect\": [72, 640, 150, 80], \
         \"body\": [320, 200], \"scale\": 8.0, \"title\": \"回放预览\"}}\n  ]\n}}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_names_round_trip() {
        for sample in Sample::all() {
            assert_eq!(Sample::parse(sample.name()), Some(sample));
            assert!(!sample.file_name().is_empty());
        }
        assert_eq!(Sample::parse("nope"), None);
        assert_eq!(Sample::parse("two-page"), Some(Sample::Rotated), "旧名字仍可用");
    }

    #[test]
    fn preview_sample_writes_a_sidecar_that_the_app_can_read() {
        let dir = std::env::temp_dir().join(format!("pdf-sample-test-{}", std::process::id()));
        let pdf = Sample::Preview.write(&dir).expect("写样本");
        let sidecar = sidecar_path(&pdf);
        assert!(sidecar.exists(), "预览样本必须带 sidecar");
        let text = std::fs::read_to_string(&sidecar).expect("读 sidecar");
        let data = crate::persist::SavedData::from_json(&text).expect("sidecar 应能被解析");
        assert_eq!(data.custom_images.len(), 1);
        assert_eq!(data.custom_images[0].scale, 8.0, "记录分辨率");
        assert_eq!(data.version, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pdf_bytes_are_non_empty_for_every_sample() {
        for sample in Sample::all() {
            let bytes = sample.pdf_bytes();
            assert!(bytes.len() > 200, "{} 太小", sample.name());
            assert_eq!(&bytes[..5], b"%PDF-", "{} 不是 PDF", sample.name());
        }
    }
}
