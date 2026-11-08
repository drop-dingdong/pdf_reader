//! PDFium 动态库的定位与绑定（进程级单例）。

use std::path::PathBuf;
use std::sync::OnceLock;

use pdfcore::prelude::*;
use pdfium_render::prelude::Pdfium;

/// 进程级 PDFium 绑定结果（成功或失败原因都缓存，避免反复尝试加载库）。
///
/// 之所以用单例：PDFium 的库句柄与 `FPDF_InitLibrary` 都是**进程级全局状态**，
/// 每个文档各建一份既浪费也容易出问题。`Pdfium` 本身实现了 `Send + Sync`
/// （见 pdfium-render 的 `thread_safe` feature），因此可以安全放进 `OnceLock`。
static PDFIUM: OnceLock<std::result::Result<Pdfium, String>> = OnceLock::new();

/// 取得进程级 PDFium 绑定。
///
/// 查找顺序：
/// 1. 环境变量 `PDFIUM_PATH`（可以是库文件本身，也可以是包含它的目录）；
/// 2. 从当前目录向上最多 3 层查找 `vendor/pdfium-<平台>/lib/`（开发机用
///    `scripts/fetch-pdfium.sh` 生成）；
/// 3. 可执行文件所在目录；
/// 4. 系统库（`Pdfium::bind_to_system_library`）。
pub fn pdfium() -> Result<&'static Pdfium> {
    match PDFIUM.get_or_init(bind) {
        Ok(pdfium) => Ok(pdfium),
        Err(message) => Err(CoreError::Backend(message.clone())),
    }
}

/// 本机平台标识（与 `scripts/fetch-pdfium.sh` 的命名一致）。
pub fn platform_tag() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "aarch64") => "mac-arm64",
        ("macos", "x86_64") => "mac-x64",
        ("windows", "x86_64") => "win-x64",
        ("windows", "x86") => "win-x86",
        ("windows", "aarch64") => "win-arm64",
        _ => "unknown",
    }
}

fn bind() -> std::result::Result<Pdfium, String> {
    let mut attempts: Vec<String> = Vec::new();

    for directory in candidate_directories() {
        let path = Pdfium::pdfium_platform_library_name_at_path(&directory);
        match Pdfium::bind_to_library(&path) {
            Ok(bindings) => return Ok(Pdfium::new(bindings)),
            Err(error) => {
                // 区分"没下载/放错位置"与"文件在但加载失败（位数不对、缺 VC 运行库…）——
                // Windows 上这两种情况的排查方向完全不同
                let state = if path.exists() {
                    "文件存在"
                } else {
                    "文件不存在"
                };
                attempts.push(format!("{}（{state}）：{error}", path.display()));
            }
        }
    }

    match Pdfium::bind_to_system_library() {
        Ok(bindings) => Ok(Pdfium::new(bindings)),
        Err(error) => {
            attempts.push(format!("系统库：{error}"));
            Err(format!(
                "无法加载 PDFium 动态库。请运行 scripts/fetch-pdfium.sh（或设置 PDFIUM_PATH）。\n尝试过：\n  {}",
                attempts.join("\n  ")
            ))
        }
    }
}

/// 已存在的 PDFium 动态库**文件路径**（与 [`pdfium`] 相同的候选顺序）。
///
/// 给写回（`writeback`）用：它要用 `libloading` 自己加载同一个库。
/// 返回 `None` 表示候选目录里没有；调用方可以退回平台默认库名（系统库）。
pub fn pdfium_library_path() -> Option<PathBuf> {
    for directory in candidate_directories() {
        let path = Pdfium::pdfium_platform_library_name_at_path(&directory);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// 候选目录，按优先级排列。
fn candidate_directories() -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();

    // 1) 环境变量：允许指向库文件本身或所在目录
    if let Ok(raw) = std::env::var("PDFIUM_PATH") {
        let path = PathBuf::from(raw);
        if path.is_dir() {
            candidates.push(with_separator(path));
        } else if let Some(parent) = path.parent() {
            candidates.push(with_separator(parent.to_path_buf()));
        }
    }

    // 2) 从当前目录（cargo test 时是 crate 目录）向上找仓库内的 vendor/
    let platform = platform_tag();
    if let Ok(cwd) = std::env::current_dir() {
        let mut directory = Some(cwd.as_path());
        for _ in 0..3 {
            let Some(current) = directory else { break };
            candidates.push(with_separator(
                current
                    .join("vendor")
                    .join(format!("pdfium-{platform}"))
                    .join("lib"),
            ));
            directory = current.parent();
        }
    }

    // 3) 可执行文件所在目录（打包后库与程序同目录或在 lib/ 下）
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(with_separator(parent.to_path_buf()));
            candidates.push(with_separator(parent.join("lib")));
            // 有的 PDFium 分发包把 Windows 运行库放在 bin/ 下（lib/ 里是导入库 .dll.lib）
            candidates.push(with_separator(parent.join("bin")));
        }
    }

    candidates
}

fn with_separator(mut path: PathBuf) -> String {
    let mut text = path.to_string_lossy().into_owned();
    if !text.ends_with(std::path::MAIN_SEPARATOR) {
        text.push(std::path::MAIN_SEPARATOR);
    }
    path.clear();
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_tag_is_known() {
        assert_ne!(
            platform_tag(),
            "unknown",
            "未识别的平台会导致找不到 vendor 目录"
        );
    }

    /// 打包布局：库与程序同目录、在 `lib/` 下、或在 `bin/` 下（Windows 上游包）。
    #[test]
    fn candidate_directories_cover_exe_dir_lib_and_bin() {
        let exe = std::env::current_exe().expect("测试二进制路径");
        let parent = exe.parent().expect("父目录");
        let candidates = candidate_directories();
        let wanted = [
            with_separator(parent.to_path_buf()),
            with_separator(parent.join("lib")),
            with_separator(parent.join("bin")),
        ];
        for path in wanted {
            assert!(
                candidates.iter().any(|item| item == &path),
                "候选目录缺少 {path}（打包后的查找依赖它）；实际：{candidates:?}"
            );
        }
    }

    #[test]
    fn candidate_directories_include_vendor_even_without_env() {
        // 不设置 PDFIUM_PATH 时，必须至少包含仓库内的 vendor 路径（开发机默认布局）
        let candidates = candidate_directories();
        assert!(
            candidates
                .iter()
                .any(|path| path.contains(&format!("pdfium-{}", platform_tag()))),
            "候选目录应包含 vendor/pdfium-<平台>/lib，实际：{candidates:?}"
        );
    }

    /// 绑定失败时必须是可读的错误（含引导信息），而不是 panic。
    #[test]
    fn missing_library_reports_actionable_error() {
        // 用一个必然不存在的路径初始化：OnceLock 可能已被其它测试填充，
        // 因此这里只验证 bind() 的错误文案形态。
        let saved = std::env::var("PDFIUM_PATH").ok();
        std::env::set_var("PDFIUM_PATH", "/nonexistent/pdfium/dir");
        let result = bind();
        match saved {
            Some(value) => std::env::set_var("PDFIUM_PATH", value),
            None => std::env::remove_var("PDFIUM_PATH"),
        }
        // 若系统里恰好有 libpdfium，则允许成功
        if let Err(message) = result {
            assert!(
                message.contains("无法加载 PDFium"),
                "错误文案应可操作：{message}"
            );
        }
    }
}
