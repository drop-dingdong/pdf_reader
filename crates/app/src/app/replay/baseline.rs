//! 性能基线：**结构指标硬门禁 + 时序指标只报告**，并把本机硬件信息写进基线文件（D-106）。
//!
//! ## 为什么这么分
//! 结构指标（提交了多少渲染请求、上传多少字节、纹理条目峰值、预览尺寸）由**代码与脚本**
//! 决定，换台机器也一样 —— 可以当门禁。时序指标（每帧 CPU 时间、RSS 峰值）同时受机器与
//! 构建配置影响，跨机器比较没有意义 —— 只能**同机前后对比**，所以基线文件必须带上硬件信息，
//! 让它成为"附带测试条件"而不是一个孤零零的数字。
//!
//! ## 用法
//! ```text
//! cargo run -p app --release -- --replay scripts/replay/tiling_800.json --timing \
//!     --record perf/tiling_800.json          # 记一次本机基线
//! cargo run -p app --release -- --replay scripts/replay/tiling_800.json --timing \
//!     --compare perf/tiling_800.json         # 与基线比（结构不符 → 退出码 1）
//! ```

use std::fmt::Write as _;
use std::path::Path;

use super::{escape, Report};

/// 基线格式版本。
pub const VERSION: u32 = 4;

/// 只记结构指标的基线（硬件无关，可以入库、可以在 CI 里跨机比较）。
pub const PROFILE_STRUCTURAL: &str = "structural";
/// 含时序的基线（只能在同一台机器、同一构建配置下比较）。
pub const PROFILE_TIMING: &str = "timing";

/// 跑这份基线时的机器与构建条件。
#[derive(Debug, Clone, PartialEq)]
pub struct Machine {
    /// CPU 型号（Linux 读 `/proc/cpuinfo`，其它平台 `unknown`）。
    pub cpu: String,
    /// 可用并行度。
    pub cores: usize,
    /// 物理内存总量（MB；未知为 0）。
    pub ram_mb: u64,
    /// 操作系统。
    pub os: String,
    /// 架构。
    pub arch: String,
    /// 构建配置：`debug` / `release`（**时序指标必须同构建比较**）。
    pub build: String,
}

impl Machine {
    /// 探测本机。
    pub fn detect() -> Self {
        Self {
            cpu: cpu_model(),
            cores: std::thread::available_parallelism().map_or(0, |value| value.get()),
            ram_mb: total_ram_mb(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            build: if cfg!(debug_assertions) {
                "debug".into()
            } else {
                "release".into()
            },
        }
    }

    /// 一行摘要（打印用）。
    pub fn summary(&self) -> String {
        format!(
            "{} / {} 核 / {} MB / {}-{} / {}",
            self.cpu, self.cores, self.ram_mb, self.os, self.arch, self.build
        )
    }

    /// 硬件/构建条件是否一致（不一致时时序指标不可比）。
    pub fn same_condition(&self, other: &Self) -> bool {
        self.cpu == other.cpu
            && self.cores == other.cores
            && self.ram_mb == other.ram_mb
            && self.os == other.os
            && self.arch == other.arch
            && self.build == other.build
    }
}

/// 单个动作的结构指标（与硬件无关）。
#[derive(Debug, Clone, PartialEq)]
pub struct ActionBaseline {
    /// 动作 id。
    pub id: String,
    /// 该动作的忙帧数。
    pub busy_frames: u64,
    /// 该动作的忙帧 CPU 合计（毫秒）。
    pub cpu_ms_total: f64,
    /// 该动作的墙钟耗时（毫秒）。
    pub wall_ms: f64,
    /// 新增渲染请求数。
    pub submitted: u64,
    /// 打算上传的字节总数。
    pub upload_bytes: u64,
    /// 纹理条目峰值。
    pub textures_peak: usize,
    /// 纹理**字节**峰值（结构指标：与硬件无关）。
    pub texture_bytes_peak: u64,
    /// 预览纹理尺寸（没有则为空）。
    pub preview: Option<(usize, usize)>,
}

/// 一份基线。
#[derive(Debug, Clone, PartialEq)]
pub struct Baseline {
    /// 格式版本。
    pub version: u32,
    /// 基线类型：`timing`（含时序，仅本机可比）或 `structural`（只记结构指标，可入库/跨机比较）。
    pub profile: String,
    /// 脚本名。
    pub script: String,
    /// 机器条件。
    pub machine: Machine,
    /// 逐动作结构指标。
    pub actions: Vec<ActionBaseline>,
    /// **忙帧**耗时 p50/p95/p99/max（毫秒）——见 [`Report::busy_frame_percentiles`]。
    pub cpu_ms: (f64, f64, f64, f64),
    /// 忙帧 CPU 时间合计（毫秒）。
    pub cpu_ms_total: f64,
    /// 全部动作墙钟合计（毫秒）。
    pub wall_ms_total: f64,
    /// 忙帧数。
    pub busy_frames: u64,
    /// RSS 峰值（KB）。
    pub rss_peak_kb: u64,
}

impl Baseline {
    /// 所有动作累计的渲染请求数。
    pub fn total_submitted(&self) -> u64 {
        self.actions.iter().map(|action| action.submitted).sum()
    }

    /// 从一次回放记录基线。
    pub fn record(report: &Report, machine: Machine) -> Self {
        let actions = report
            .actions
            .iter()
            .map(|action| ActionBaseline {
                id: action.id.clone(),
                busy_frames: action.busy_frames,
                cpu_ms_total: action.busy_cpu_ms_total,
                wall_ms: action.elapsed_ms,
                submitted: action.submitted,
                upload_bytes: action.upload_bytes,
                textures_peak: action.textures_peak,
                texture_bytes_peak: action.texture_bytes_peak,
                preview: action
                    .previews
                    .first()
                    .map(|(_, width, height)| (*width, *height)),
            })
            .collect();
        let cpu_ms = report.busy_frame_percentiles();
        Self {
            version: VERSION,
            profile: PROFILE_TIMING.to_string(),
            script: report.script.clone(),
            machine,
            actions,
            cpu_ms,
            cpu_ms_total: report.busy_cpu_ms_total(),
            wall_ms_total: report.wall_ms_total(),
            busy_frames: report.busy_frames(),
            rss_peak_kb: report.rss_peak_kb(),
        }
    }

    /// 记录一份**只含结构指标**的基线：时序/内存字段清零，`compare` 会跳过机器与时序检查。
    ///
    /// 这份文件与硬件无关，可以提交进仓库，让 CI 守住"同一波段多渲一次""上传字节暴涨"
    /// 这类退化（D-113）。
    pub fn record_structural(report: &Report) -> Self {
        let mut baseline = Self::record(report, Machine::detect());
        baseline.profile = PROFILE_STRUCTURAL.to_string();
        baseline.cpu_ms = (0.0, 0.0, 0.0, 0.0);
        baseline.cpu_ms_total = 0.0;
        baseline.wall_ms_total = 0.0;
        baseline.rss_peak_kb = 0;
        baseline.busy_frames = 0;
        for action in &mut baseline.actions {
            action.busy_frames = 0;
            action.cpu_ms_total = 0.0;
            action.wall_ms = 0.0;
        }
        baseline
    }

    /// 是否是"只含结构指标"的基线。
    pub fn is_structural(&self) -> bool {
        self.profile == PROFILE_STRUCTURAL
    }

    /// 序列化成 JSON。
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{{");
        let _ = writeln!(out, "  \"version\": {},", self.version);
        let _ = writeln!(out, "  \"profile\": \"{}\",", escape(&self.profile));
        let _ = writeln!(out, "  \"script\": \"{}\",", escape(&self.script));
        let _ = writeln!(out, "  \"machine\": {{");
        let _ = writeln!(out, "    \"cpu\": \"{}\",", escape(&self.machine.cpu));
        let _ = writeln!(out, "    \"cores\": {},", self.machine.cores);
        let _ = writeln!(out, "    \"ram_mb\": {},", self.machine.ram_mb);
        let _ = writeln!(out, "    \"os\": \"{}\",", escape(&self.machine.os));
        let _ = writeln!(out, "    \"arch\": \"{}\",", escape(&self.machine.arch));
        let _ = writeln!(out, "    \"build\": \"{}\"", escape(&self.machine.build));
        let _ = writeln!(out, "  }},");
        let _ = writeln!(out, "  \"actions\": [");
        for (index, action) in self.actions.iter().enumerate() {
            let comma = if index + 1 == self.actions.len() {
                ""
            } else {
                ","
            };
            let preview = match action.preview {
                Some((width, height)) => format!("[{width}, {height}]"),
                None => "null".to_string(),
            };
            let _ = writeln!(
                out,
                "    {{\"id\": \"{}\", \"submitted\": {}, \"upload_bytes\": {}, \
                 \"textures_peak\": {}, \"texture_bytes_peak\": {}, \"busy_frames\": {}, \
                 \"cpu_ms_total\": {:.3}, \"wall_ms\": {:.1}, \"preview\": {preview}}}{comma}",
                escape(&action.id),
                action.submitted,
                action.upload_bytes,
                action.textures_peak,
                action.texture_bytes_peak,
                action.busy_frames,
                action.cpu_ms_total,
                action.wall_ms
            );
        }
        let _ = writeln!(out, "  ],");
        let (p50, p95, p99, max) = self.cpu_ms;
        let _ = writeln!(
            out,
            "  \"cpu_ms\": {{\"p50\": {p50:.3}, \"p95\": {p95:.3}, \"p99\": {p99:.3}, \"max\": {max:.3}}},"
        );
        let _ = writeln!(out, "  \"cpu_ms_total\": {:.3},", self.cpu_ms_total);
        let _ = writeln!(out, "  \"wall_ms_total\": {:.1},", self.wall_ms_total);
        let _ = writeln!(out, "  \"busy_frames\": {},", self.busy_frames);
        let _ = writeln!(out, "  \"rss_peak_kb\": {}", self.rss_peak_kb);
        let _ = writeln!(out, "}}");
        out
    }

    /// 从 JSON 读回来。
    pub fn from_json(text: &str) -> Result<Self, String> {
        use crate::json::{Json, JsonLookup};
        let root = Json::parse(text)?;
        let root = root.as_object().ok_or("基线根节点不是对象")?;
        let number = |key: &str| -> Result<f64, String> {
            root.field(key)
                .and_then(|value| value.as_number())
                .map(f64::from)
                .ok_or_else(|| format!("基线缺 {key}"))
        };
        let machine_json = root.field("machine").ok_or("基线缺 machine")?;
        let machine_object = machine_json.as_object().ok_or("machine 不是对象")?;
        let text_field = |key: &str, default: &str| -> String {
            machine_object
                .field(key)
                .and_then(|value| value.as_str())
                .unwrap_or(default)
                .to_string()
        };
        let machine = Machine {
            cpu: text_field("cpu", "unknown"),
            cores: machine_object
                .field("cores")
                .and_then(|value| value.as_number())
                .unwrap_or(0.0) as usize,
            ram_mb: machine_object
                .field("ram_mb")
                .and_then(|value| value.as_number())
                .unwrap_or(0.0) as u64,
            os: text_field("os", "unknown"),
            arch: text_field("arch", "unknown"),
            build: text_field("build", "unknown"),
        };
        let mut actions = Vec::new();
        for item in root
            .field("actions")
            .ok_or("基线缺 actions")?
            .as_array()
            .ok_or("actions 不是数组")?
        {
            let object = item.as_object().ok_or("动作不是对象")?;
            let preview = object.field("preview").and_then(|value| {
                let pair = value.as_array()?;
                if pair.len() != 2 {
                    return None;
                }
                Some((pair[0].as_number()? as usize, pair[1].as_number()? as usize))
            });
            actions.push(ActionBaseline {
                busy_frames: object
                    .field("busy_frames")
                    .and_then(|value| value.as_number())
                    .unwrap_or(0.0) as u64,
                cpu_ms_total: object
                    .field("cpu_ms_total")
                    .and_then(|value| value.as_number())
                    .map_or(0.0, f64::from),
                wall_ms: object
                    .field("wall_ms")
                    .and_then(|value| value.as_number())
                    .map_or(0.0, f64::from),
                id: object
                    .field("id")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string(),
                submitted: object
                    .field("submitted")
                    .and_then(|value| value.as_number())
                    .unwrap_or(0.0) as u64,
                upload_bytes: object
                    .field("upload_bytes")
                    .and_then(|value| value.as_number())
                    .unwrap_or(0.0) as u64,
                textures_peak: object
                    .field("textures_peak")
                    .and_then(|value| value.as_number())
                    .unwrap_or(0.0) as usize,
                texture_bytes_peak: object
                    .field("texture_bytes_peak")
                    .and_then(|value| value.as_number())
                    .unwrap_or(0.0) as u64,
                preview,
            });
        }
        let cpu_json = root.field("cpu_ms").ok_or("基线缺 cpu_ms")?;
        let cpu_object = cpu_json.as_object().ok_or("cpu_ms 不是对象")?;
        let pick = |key: &str| -> f64 {
            cpu_object
                .field(key)
                .and_then(|value| value.as_number())
                .map_or(0.0, f64::from)
        };
        Ok(Self {
            version: number("version")? as u32,
            profile: root
                .field("profile")
                .and_then(|value| value.as_str())
                .unwrap_or(PROFILE_TIMING)
                .to_string(),
            script: root
                .field("script")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string(),
            machine,
            actions,
            cpu_ms: (pick("p50"), pick("p95"), pick("p99"), pick("max")),
            cpu_ms_total: root
                .field("cpu_ms_total")
                .and_then(|value| value.as_number())
                .map_or(0.0, f64::from),
            wall_ms_total: root
                .field("wall_ms_total")
                .and_then(|value| value.as_number())
                .map_or(0.0, f64::from),
            busy_frames: root
                .field("busy_frames")
                .and_then(|value| value.as_number())
                .unwrap_or(0.0) as u64,
            rss_peak_kb: number("rss_peak_kb")? as u64,
        })
    }

    /// 写文件。
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(path, self.to_json())
    }

    /// 读文件。
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        Self::from_json(&text)
    }
}

/// 一条比较结论的严重程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// 结构指标不符 —— **失败**（退出码 1）。
    Gate,
    /// 时序/内存回归或条件不同 —— **只提示**。
    Warn,
}

/// 一条比较结论。
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    /// 严重程度。
    pub severity: Severity,
    /// 说明。
    pub message: String,
}

impl Finding {
    fn gate(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Gate,
            message: message.into(),
        }
    }

    fn warn(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warn,
            message: message.into(),
        }
    }
}

/// 时序指标的告警阈值（相对基线，默认 +30%）。
pub const TIMING_WARN_RATIO: f64 = 0.30;
/// 结构指标里"字节量"的容差（渲染顺序会轻微影响上传总量）。
pub const BYTE_TOLERANCE_RATIO: f64 = 0.10;

/// 比较当前报告与基线。
pub fn compare(baseline: &Baseline, report: &Report, machine: &Machine) -> Vec<Finding> {
    let mut findings = Vec::new();
    if baseline.version != VERSION {
        findings.push(Finding::warn(format!(
            "基线格式版本 {} ≠ 当前 {VERSION}（指标口径变过，建议重录）",
            baseline.version
        )));
    }
    let structural = baseline.is_structural();
    if !structural && !baseline.machine.same_condition(machine) {
        findings.push(Finding::warn(format!(
            "机器条件不同（基线：{}；当前：{}），时序指标只作参考",
            baseline.machine.summary(),
            machine.summary()
        )));
    }
    // "录制时跑错了"的判据：**既没有提交渲染，也从未上传过纹理** —— 说明整段回放
    // 根本没渲染出任何东西（例如脚本找不到 PDF）。只看 `submitted` 会误伤纯交互脚本：
    // 点击链接/拖拽平移时页面若已被预取渲染过，本来就不该有新的渲染请求。
    let any_texture = baseline
        .actions
        .iter()
        .any(|action| action.texture_bytes_peak > 0);
    if baseline.total_submitted() == 0 && !any_texture {
        findings.push(Finding::gate(
            "基线里既没有任何渲染请求、也没有任何纹理（很可能录制时跑错了），请重新录制",
        ));
    }
    if baseline.script != report.script {
        findings.push(Finding::gate(format!(
            "脚本不同：基线是 {}，当前是 {}",
            baseline.script, report.script
        )));
        return findings;
    }
    // 结构指标：逐动作逐个比较
    if baseline.actions.len() != report.actions.len() {
        findings.push(Finding::gate(format!(
            "动作数不同：基线 {}，当前 {}",
            baseline.actions.len(),
            report.actions.len()
        )));
    }
    for (index, expected) in baseline.actions.iter().enumerate() {
        let Some(actual) = report.actions.get(index) else {
            break;
        };
        if expected.id != actual.id {
            findings.push(Finding::gate(format!(
                "第 {index} 个动作对不上：基线 {}，当前 {}",
                expected.id, actual.id
            )));
            continue;
        }
        if expected.submitted != actual.submitted {
            findings.push(Finding::gate(format!(
                "动作 {} 的渲染请求数 {} ≠ 基线 {}（同一波段不应多渲）",
                expected.id, actual.submitted, expected.submitted
            )));
        }
        if expected.textures_peak != actual.textures_peak {
            findings.push(Finding::gate(format!(
                "动作 {} 的纹理条目峰值 {} ≠ 基线 {}",
                expected.id, actual.textures_peak, expected.textures_peak
            )));
        }
        if !within(
            expected.texture_bytes_peak,
            actual.texture_bytes_peak,
            BYTE_TOLERANCE_RATIO,
        ) {
            findings.push(Finding::gate(format!(
                "动作 {} 的纹理字节峰值 {:.1}MB 偏离基线 {:.1}MB 超过 {:.0}%",
                expected.id,
                actual.texture_bytes_peak as f64 / (1024.0 * 1024.0),
                expected.texture_bytes_peak as f64 / (1024.0 * 1024.0),
                BYTE_TOLERANCE_RATIO * 100.0
            )));
        }
        if !within(
            expected.upload_bytes,
            actual.upload_bytes,
            BYTE_TOLERANCE_RATIO,
        ) {
            findings.push(Finding::gate(format!(
                "动作 {} 的上传字节 {} 偏离基线 {} 超过 {:.0}%",
                expected.id,
                actual.upload_bytes,
                expected.upload_bytes,
                BYTE_TOLERANCE_RATIO * 100.0
            )));
        }
        let actual_preview = actual
            .previews
            .first()
            .map(|(_, width, height)| (*width, *height));
        if expected.preview != actual_preview {
            findings.push(Finding::gate(format!(
                "动作 {} 的预览尺寸 {:?} ≠ 基线 {:?}",
                expected.id, actual_preview, expected.preview
            )));
        }
    }
    // 只含结构指标的基线不比较时序/内存
    if structural {
        return findings;
    }
    // 时序指标：只报告
    let (p50, p95, p99, _) = report.busy_frame_percentiles();
    let (b50, b95, b99, _) = baseline.cpu_ms;
    // `max` 只记录不比较：同一台机器上两次跑能差 50%（一次冷启动/一次 GC 就够），
    // 拿它当门禁或告警只会产生噪声；p50/p99 才是"稳不稳"的指标。
    for (name, actual, expected) in [("p50", p50, b50), ("p95", p95, b95), ("p99", p99, b99)] {
        if expected > 0.0 && actual > expected * (1.0 + TIMING_WARN_RATIO) {
            findings.push(Finding::warn(format!(
                "帧耗时 {name} {actual:.2}ms 比基线 {expected:.2}ms 慢 {:.0}%",
                (actual / expected - 1.0) * 100.0
            )));
        }
    }
    // "这个动作一共烧了多少 CPU / 等了多久"比单帧分位数更稳，也更接近手感
    for (name, actual, expected) in [
        (
            "忙帧 CPU 合计",
            report.busy_cpu_ms_total(),
            baseline.cpu_ms_total,
        ),
        ("墙钟合计", report.wall_ms_total(), baseline.wall_ms_total),
    ] {
        if expected > 0.0 && actual > expected * (1.0 + TIMING_WARN_RATIO) {
            findings.push(Finding::warn(format!(
                "{name} {actual:.1}ms 比基线 {expected:.1}ms 慢 {:.0}%",
                (actual / expected - 1.0) * 100.0
            )));
        }
    }
    let rss = report.rss_peak_kb();
    if baseline.rss_peak_kb > 0 && rss > baseline.rss_peak_kb * 2 {
        findings.push(Finding::warn(format!(
            "RSS 峰值 {} MB 超过基线的两倍（{} MB）",
            rss / 1024,
            baseline.rss_peak_kb / 1024
        )));
    }
    findings
}

/// `actual` 是否在 `expected` 的容差内（双方都为 0 也算在内）。
fn within(expected: u64, actual: u64, ratio: f64) -> bool {
    if expected == 0 {
        return actual == 0;
    }
    let delta = expected as f64 * ratio;
    (actual as f64 - expected as f64).abs() <= delta
}

/// CPU 型号（Linux 读 `/proc/cpuinfo` 的 `model name`）。
fn cpu_model() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") {
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("model name") {
                    if let Some(value) = rest.split_once(':').map(|(_, value)| value) {
                        return value.trim().to_string();
                    }
                }
            }
        }
        "unknown".to_string()
    }
    #[cfg(not(target_os = "linux"))]
    {
        "unknown".to_string()
    }
}

/// 物理内存总量（MB）。
fn total_ram_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    if let Some(kb) = rest
                        .split_whitespace()
                        .next()
                        .and_then(|v| v.parse::<u64>().ok())
                    {
                        return kb / 1024;
                    }
                }
            }
        }
        0
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::replay::{ActionReport, Report};

    fn action(id: &str, submitted: u64, upload: u64) -> ActionReport {
        ActionReport {
            id: id.to_string(),
            action: "zoom".to_string(),
            frames: 3,
            settled: true,
            diag: Vec::new(),
            frames_data: Vec::new(),
            violations: Vec::new(),
            notes: Vec::new(),
            previews: Vec::new(),
            submitted,
            textures_peak: 4,
            texture_bytes_peak: 1024 * 1024,
            upload_bytes: upload,
            elapsed_ms: 1.0,
            busy_frames: 1,
            busy_cpu_ms: (1.0, 1.0, 1.0, 1.0),
            busy_cpu_ms_total: 1.0,
            page_after: 0,
            scroll_after: (0.0, 0.0),
            annotations: 0,
            annotation_bounds: None,
        }
    }

    fn report(actions: Vec<ActionReport>) -> Report {
        Report {
            script: "demo".to_string(),
            actions,
            violations: Vec::new(),
        }
    }

    fn machine() -> Machine {
        Machine {
            cpu: "Test CPU".to_string(),
            cores: 8,
            ram_mb: 16_384,
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            build: "debug".to_string(),
        }
    }

    #[test]
    fn baseline_round_trips_through_json() {
        let baseline = Baseline::record(&report(vec![action("a", 3, 1024)]), machine());
        let text = baseline.to_json();
        let parsed = Baseline::from_json(&text).expect("应能解析");
        assert_eq!(parsed.version, VERSION);
        assert_eq!(parsed.script, "demo");
        assert_eq!(parsed.machine, machine());
        assert_eq!(parsed.actions, baseline.actions);
        assert_eq!(parsed.rss_peak_kb, baseline.rss_peak_kb);
        assert!((parsed.cpu_ms.0 - baseline.cpu_ms.0).abs() < 0.001);
    }

    #[test]
    fn identical_runs_produce_no_findings() {
        let report = report(vec![action("a", 3, 1024), action("b", 0, 0)]);
        let baseline = Baseline::record(&report, machine());
        let findings = compare(&baseline, &report, &machine());
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn extra_render_requests_are_a_gate() {
        let baseline = Baseline::record(&report(vec![action("a", 3, 1024)]), machine());
        let worse = report(vec![action("a", 5, 1024)]);
        let findings = compare(&baseline, &worse, &machine());
        assert!(
            findings
                .iter()
                .any(|finding| finding.severity == Severity::Gate
                    && finding.message.contains("渲染请求数")),
            "{findings:?}"
        );
    }

    #[test]
    fn upload_byte_drift_within_tolerance_is_ignored_but_big_drift_is_not() {
        let baseline = Baseline::record(&report(vec![action("a", 1, 1000)]), machine());
        let small = report(vec![action("a", 1, 1050)]);
        assert!(
            compare(&baseline, &small, &machine()).is_empty(),
            "5% 内应放过"
        );
        let big = report(vec![action("a", 1, 1500)]);
        assert!(
            compare(&baseline, &big, &machine())
                .iter()
                .any(|finding| finding.severity == Severity::Gate),
            "50% 偏差应拦下"
        );
    }

    #[test]
    fn a_different_machine_only_warns() {
        let report = report(vec![action("a", 1, 1000)]);
        let baseline = Baseline::record(&report, machine());
        let mut other = machine();
        other.cpu = "Another CPU".to_string();
        other.build = "release".to_string();
        let findings = compare(&baseline, &report, &other);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].severity, Severity::Warn);
        assert!(findings[0].message.contains("机器条件不同"));
    }

    #[test]
    fn an_old_format_baseline_warns() {
        let mut old = Baseline::record(&report(vec![action("a", 1, 100)]), machine());
        old.version = 1;
        let findings = compare(&old, &report(vec![action("a", 1, 100)]), &machine());
        assert!(
            findings
                .iter()
                .any(|finding| finding.severity == Severity::Warn
                    && finding.message.contains("基线格式版本")),
            "{findings:?}"
        );
    }

    #[test]
    fn a_baseline_without_any_render_requests_is_rejected() {
        // 真机第一次录制就踩到：帧太便宜，整段都停在缩放预览窗口里，一次渲染都没发生。
        // 这样的基线必须判失败，否则它会"一路放行"。
        // 真正"什么都没发生"：没有提交渲染，也没有任何纹理字节
        let mut idle = action("a", 0, 0);
        idle.textures_peak = 0;
        idle.texture_bytes_peak = 0;
        let empty = Baseline::record(&report(vec![idle.clone()]), machine());
        let findings = compare(&empty, &report(vec![idle]), &machine());
        assert!(
            findings
                .iter()
                .any(|finding| finding.severity == Severity::Gate
                    && finding.message.contains("也没有任何纹理")),
            "{findings:?}"
        );
    }

    #[test]
    fn a_different_script_is_a_gate() {
        let baseline = Baseline::record(&report(vec![action("a", 1, 0)]), machine());
        let mut other = report(vec![action("a", 1, 0)]);
        other.script = "other".to_string();
        let findings = compare(&baseline, &other, &machine());
        assert_eq!(findings[0].severity, Severity::Gate);
    }
}
