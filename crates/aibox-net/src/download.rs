//! 分段下载器（下载执行 + S1 安全红线）：
//!
//! - 8MB 分段 + 断点续传（`.part` 文件按字节续写，Range 请求从断点起）；
//! - 单段重试 2 次失败 → 整包降级到下一通道（跨源从头下载）；
//! - 全通道失败 → 结构化错误 [`DownloadError::AllChannelsDead`]（`NET_ALL_CHANNELS_DEAD`）；
//! - **产物三分判定**（下载完成后）：HTML/JSON 故障页、大小不符（截断）→ 判**通道故障**
//!   降级下一通道；大小相符但 SHA256 不符 → **真篡改**，立即中止（`NET_HASH_MISMATCH`，
//!   S1 零豁免、不降级不放行）；
//! - **慢速通道熔断**：单段实测速度低于阈值（默认 32 KiB/s）→ 立即放弃该通道并降级，
//!   把「直连 18 KB/s 卡死 90 秒」压到十几秒量级；
//! - **慢速安全阀**：若本轮**所有**通道都因过慢被熔断（例如用户只有 3G、
//!   没有 CN 网关兜底），熔断会把「慢但能下完」变成「直接失败」——这是 UX 倒退。
//!   因此这种情况下会对**第一个**通道关闭熔断（旧路径）**再试一次**，无限耐心地下完；
//!   只要有任一通道是真实故障（HTTP 错误/故障页/截断），就不触发安全阀（重试真故障没意义）；
//! - 进度回调为普通闭包/`FnMut` trait，**不依赖 Tauri**（供上层 emit `download_progress`）。
//!
//! 网络 IO 全部走 `ureq`（rustls，无系统 C 库）；单测用手写本地 HTTP 服务器离线覆盖。

use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::channel::ChannelType;
use crate::proxy::ProxyConfig;

/// 默认分段大小：8MB。
pub const SEGMENT_SIZE: u64 = 8 * 1024 * 1024;
/// 单段重试次数（2 次失败 → 整包降级）。
pub const SEGMENT_RETRIES: u32 = 2;
/// 默认单请求超时（连接 + 读）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// 慢速通道熔断：默认最低速度阈值（32 KiB/s，单段实测平均速度下限）。
///
/// **为什么要有熔断**：用户实测国内直连 GitHub release 只有约 18 KB/s，
/// 经 CN 网关约 288 KB/s。旧逻辑只在通道「失败」（HTTP 错误/故障页/截断）时才降级，
/// 一个「慢但不断」的通道会一直挂着——进度条卡在 0% 看着像死了，
/// 8MB 段 × (1 次 + 2 次重试) × 30s 超时 ≈ **90 秒**后才轮到下一个通道。
/// 熔断把这条路径压到十几秒量级。
///
/// **为什么默认取 32 KiB/s**：明显高于「直连能跑」的 18 KB/s（能被识别为慢），
/// 又远低于正常网关速度 288 KB/s（不会误伤好通道）；
/// 8MB 段在 32 KiB/s 下要 4 分钟，用户不可能等 → 判定「不可接受」是安全的。
pub const DEFAULT_MIN_SEGMENT_BPS: u64 = 32 * 1024;

/// 慢速判定（段级）最小样本：1 MiB。小于此值的段不判速度
/// （连接/首包开销占比过高，测速噪音大，容易误伤）。
pub const SLOW_SEGMENT_MIN_SAMPLE_BYTES: u64 = 1024 * 1024;
/// 慢速判定（流中窗口）最小样本：16 KiB。
pub const SLOW_WINDOW_MIN_SAMPLE_BYTES: u64 = 16 * 1024;
/// 慢速判定（流中窗口）最小观测时长：2s（窗口太短会被瞬时突发字节干扰）。
pub const SLOW_WINDOW: Duration = Duration::from_secs(2);
/// 整包小于此值（8 MiB）整体跳过熔断：整体就是个小包，误伤代价大于收益。
pub const SLOW_SKIP_TOTAL_BYTES: u64 = 8 * 1024 * 1024;

/// 一个候选下载通道（资源-通道矩阵的一行，由上层展开）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelUrl {
    pub channel: ChannelType,
    pub url: String,
}

impl ChannelUrl {
    pub fn new(channel: ChannelType, url: impl Into<String>) -> Self {
        Self {
            channel,
            url: url.into(),
        }
    }
}

/// 进度事件（上层映射为 `download_progress {jobId, done, total, channel}`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadProgress {
    pub channel: ChannelType,
    /// 已写入字节数（单调不减）
    pub downloaded: u64,
    /// 总字节数（HEAD 后已知）
    pub total: u64,
    /// 当前段（0-based）
    pub segment: usize,
    /// 总段数
    pub segments: usize,
    /// 本次是否从断点续传
    pub resumed: bool,
    /// 「本次进度所属通道是换源换来的」时的换源原因（`None` = 不是换源，或原因未知）。
    ///
    /// **为什么带这个字段**：用户看到「下载中途换了源」时需要知道是
    /// 「这条线路太慢（换个更快的）」还是「这条线路坏了（要换网络/代理）」——
    /// 两者的下一步动作完全不同。`download_fallback` 只在**换源后新通道的首个**
    /// 进度事件里带上原因（只带一次），上层据此给不同的人话提示。
    pub switch_reason: Option<ChannelFailureKind>,
}

/// 单通道失败原因（全通道失败时逐条汇总给人话报错）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelFailure {
    pub channel: ChannelType,
    pub url: String,
    pub reason: String,
    /// 失败**类别**（结构化判定，**不靠字符串匹配**）。
    ///
    /// **为什么要有类别**：上层要区分「这条通道是因为太慢被熔断放弃」
    /// 还是「真的挂了（HTTP 错误/传输错误/故障页/截断）」——
    /// 只有前者才值得用「不限速重试」的安全阀兜底（慢 ≠ 不能下）。
    pub kind: ChannelFailureKind,
}

/// 通道失败类别。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelFailureKind {
    /// 因实测速度低于阈值被熔断放弃（慢，但通道本身是好的 → 可走安全阀）。
    TooSlow { bps: u64, threshold: u64 },
    /// 真实故障（HTTP 错误/传输错误/故障页/截断/产物不符）。
    Fault,
}

impl ChannelFailureKind {
    /// 是否为「过慢失败」。
    pub fn is_too_slow(&self) -> bool {
        matches!(self, ChannelFailureKind::TooSlow { .. })
    }

    /// 换源原因码（事件 `channel_switched.reason` 的取值）。
    ///
    /// **为什么要有稳定的原因码**：上层（UI）要按原因给用户不同的白话提示，
    /// 又不能去匹配人话字符串（会随文案改动而失效）——所以这里给出机器可读的码，
    /// 文案本身放在前端 i18n 字典里。
    pub fn reason_code(&self) -> &'static str {
        match self {
            ChannelFailureKind::TooSlow { .. } => "slow-channel",
            ChannelFailureKind::Fault => "download-fallback",
        }
    }
}

/// 安全阀记录（所有通道都过慢 → 已用关闭熔断的方式重试过第一个通道，仍然失败才有此记录）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlowValveInfo {
    /// 被不限速重试的通道
    pub channel: ChannelType,
    pub url: String,
    /// 不限速重试仍然失败的原因
    pub reason: String,
}

/// 结构化下载错误。
#[derive(Debug)]
pub enum DownloadError {
    /// 未提供任何通道（调用方配置错误）
    NoChannels,
    /// 所有通道失败（上层唤起 Network Doctor / E1）
    AllChannelsDead {
        failures: Vec<ChannelFailure>,
        /// 安全阀已触发过的记录（`Some` = 所有通道都过慢 → 已用「不限速」方式重试过第一个通道，
        /// 仍然失败才走到这里；`None` = 没触发过安全阀）。
        slow_valve: Option<SlowValveInfo>,
    },
    /// SHA256 不匹配（S1：立即中止，不降级不放行）
    HashMismatch {
        channel: ChannelType,
        expected: String,
        actual: String,
    },
    /// 代理配置非法
    BadProxy(String),
    /// 本地 IO 错误（磁盘等；与通道无关，不降级）
    Io {
        context: String,
        source: std::io::Error,
    },
}

impl DownloadError {
    /// 机器可读错误码（TS 侧 human_error_code 映射）。
    pub fn code(&self) -> &'static str {
        match self {
            DownloadError::NoChannels => "NET_NO_CHANNELS",
            DownloadError::AllChannelsDead { .. } => "NET_ALL_CHANNELS_DEAD",
            DownloadError::HashMismatch { .. } => "NET_HASH_MISMATCH",
            DownloadError::BadProxy(_) => "NET_BAD_PROXY",
            DownloadError::Io { .. } => "NET_IO_ERROR",
        }
    }

    /// 人话报错（「当前网络无法访问任何下载源」+ 每通道失败原因表）。
    pub fn human_message(&self) -> String {
        match self {
            DownloadError::NoChannels => "没有可用的下载通道，请检查安装配置".to_string(),
            DownloadError::AllChannelsDead { slow_valve, .. } => match slow_valve {
                // 安全阀已用过 → 告诉用户/客服「不是没试，而是网真的慢到不行」
                Some(v) => format!(
                    "当前网络无法访问任何下载源（所有下载源都过慢，已用不限速方式重试过 {} 通道，仍失败；见通道失败原因表，可运行网络体检）",
                    v.channel.as_str()
                ),
                None => {
                    "当前网络无法访问任何下载源（见通道失败原因表，可运行网络体检）".to_string()
                }
            },
            DownloadError::HashMismatch { channel, .. } => format!(
                "下载内容校验失败，已中止安装（{} 通道产物与预期哈希不一致）",
                channel.as_str()
            ),
            DownloadError::BadProxy(e) => format!("代理配置无效：{e}"),
            DownloadError::Io { context, .. } => format!("本地文件读写失败：{context}"),
        }
    }

    /// 每通道失败原因表（TS 表格/诊断导出用）。
    pub fn channel_report(&self) -> Vec<(String, String)> {
        match self {
            DownloadError::AllChannelsDead { failures, .. } => failures
                .iter()
                .map(|f| (f.channel.as_str().to_string(), f.reason.clone()))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// 安全阀记录（`Some` = 所有通道都过慢，已用不限速方式重试过第一个通道）。
    ///
    /// **为什么暴露给上层**：客服/用户看到「NET_ALL_CHANNELS_DEAD」时，
    /// 需要能区分「网彻底不通」与「网很慢但我们试过了」——后者还有救（换个网络/等一会儿）。
    pub fn slow_valve(&self) -> Option<&SlowValveInfo> {
        match self {
            DownloadError::AllChannelsDead { slow_valve, .. } => slow_valve.as_ref(),
            _ => None,
        }
    }
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::AllChannelsDead {
                failures,
                slow_valve,
            } => {
                write!(f, "{} [", self.code())?;
                for (i, fail) in failures.iter().enumerate() {
                    if i > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{}: {}", fail.channel.as_str(), fail.reason)?;
                }
                if let Some(v) = slow_valve {
                    write!(
                        f,
                        "; slow-valve: unlimited-speed retry on {} failed: {}",
                        v.channel.as_str(),
                        v.reason
                    )?;
                }
                write!(f, "]")
            }
            DownloadError::HashMismatch {
                channel,
                expected,
                actual,
            } => write!(
                f,
                "{} channel={} expected={} actual={}",
                self.code(),
                channel.as_str(),
                expected,
                actual
            ),
            DownloadError::BadProxy(e) => write!(f, "{} {e}", self.code()),
            DownloadError::Io { context, source } => {
                write!(f, "{} {context}: {source}", self.code())
            }
            DownloadError::NoChannels => write!(f, "{}", self.code()),
        }
    }
}

impl std::error::Error for DownloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DownloadError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// 下载参数。
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// 分段大小（默认 8MB）
    pub segment_size: u64,
    /// 单段重试次数（默认 2）
    pub segment_retries: u32,
    /// 单请求超时（连接 + 读）
    pub timeout: Duration,
    /// 经活代理下载（`None` = 直连；死代理由上层判定后忽略）
    pub proxy: Option<ProxyConfig>,
    /// 期望总字节数（registry 包级 `sizeBytes`；`None` = 不知道 → 大小维度保守处理）
    pub expected_size: Option<u64>,
    /// 慢速通道熔断阈值：单段实测平均速度下限（字节/秒）。
    ///
    /// - `None` = 关闭熔断（保持旧行为：只在通道失败时降级）；
    /// - `Some(x)` = 实测速度低于 `x` B/s 的通道**立即放弃**并降级到下一通道
    ///   （不消耗段重试预算——重试一个慢通道毫无意义）。
    ///
    /// 默认 [`DEFAULT_MIN_SEGMENT_BPS`]（32 KiB/s）；整包 < 8 MiB 时自动不熔断。
    pub min_segment_bps: Option<u64>,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            segment_size: SEGMENT_SIZE,
            segment_retries: SEGMENT_RETRIES,
            timeout: DEFAULT_TIMEOUT,
            proxy: None,
            expected_size: None,
            min_segment_bps: Some(DEFAULT_MIN_SEGMENT_BPS),
        }
    }
}

/// 观测速率（纯函数）：`bytes / elapsed` → 每秒字节数（向下取整）。
///
/// 耗时为 0（无法测速）→ `None`。
pub fn observed_bps(bytes: u64, elapsed: Duration) -> Option<u64> {
    let secs = elapsed.as_secs_f64();
    if !(secs > 0.0) {
        return None;
    }
    Some((bytes as f64 / secs) as u64)
}

/// 慢速熔断判定（纯函数，便于离线单测）。
///
/// 判定规则：
/// | 情形 | 结论 |
/// |---|---|
/// | `threshold = None`（未开启熔断） | `false`（永不熔断） |
/// | `bytes < min_sample_bytes`（样本太小，测速噪音大） | `false`（跳过判定） |
/// | 耗时为 0（无法测速） | `false`（跳过判定） |
/// | 实测速率 `< threshold` | `true`（通道太慢 → 立即放弃并降级） |
pub fn segment_speed_too_slow(
    bytes: u64,
    elapsed: Duration,
    threshold: Option<u64>,
    min_sample_bytes: u64,
) -> bool {
    let Some(threshold) = threshold else {
        return false;
    };
    if bytes < min_sample_bytes {
        return false;
    }
    match observed_bps(bytes, elapsed) {
        Some(bps) => bps < threshold,
        None => false,
    }
}

/// 是否启用慢速熔断（纯函数）：阈值未配置 → 否；整包过小（< 8 MiB）→ 否。
pub fn slow_guard_enabled(total_bytes: u64, threshold: Option<u64>) -> bool {
    threshold.is_some() && total_bytes >= SLOW_SKIP_TOTAL_BYTES
}

/// 慢速熔断守卫（内部参数集合）。
#[derive(Debug, Clone, Copy)]
struct SpeedGuard {
    /// 最低速度阈值（字节/秒）
    threshold: u64,
    /// 流中测速窗口（最小观测时长）
    window: Duration,
    /// 流中窗口最小样本字节数
    window_min_bytes: u64,
    /// 段级判定最小样本字节数
    segment_min_bytes: u64,
}

/// 构造熔断守卫（纯函数）：整包过小 → 不熔断。
fn slow_guard_for(total: u64, threshold: Option<u64>) -> Option<SpeedGuard> {
    let threshold = threshold?;
    if !slow_guard_enabled(total, Some(threshold)) {
        return None;
    }
    Some(SpeedGuard {
        threshold,
        window: SLOW_WINDOW,
        window_min_bytes: SLOW_WINDOW_MIN_SAMPLE_BYTES,
        segment_min_bytes: SLOW_SEGMENT_MIN_SAMPLE_BYTES,
    })
}

/// 产物三分判定（纯函数：文件头字节 + 大小 + 哈希结果 → 结论）。
///
/// 判定表：
/// | 情形 | 结论 |
/// |---|---|
/// | SHA256 相符 | [`ProductVerdict::HashOk`]（内容对了就放行） |
/// | 文件头是 HTML/JSON 故障页 | [`ProductVerdict::ChannelFault`]（通道坏 → 降级下一通道） |
/// | 有 `expected_size` 且实际大小不符 | [`ProductVerdict::ChannelFault`]（截断 → 降级） |
/// | 大小相符（或未知）但 SHA256 不符 | [`ProductVerdict::Tamper`]（真篡改 → 中止，红线） |
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductVerdict {
    /// 哈希相符，产物可信
    HashOk,
    /// 通道故障（故障页/截断）：记为该通道失败，降级下一通道
    ChannelFault(String),
    /// 真篡改：立即中止（NET_HASH_MISMATCH），绝不降级
    Tamper,
}

/// 文件头字节 → 是否为服务端故障页（HTML/XML/JSON 错误响应而非安装包）。
///
/// 取前 512B，去 BOM/前导空白后匹配 `<!doctype` / `<html` / `<?xml` / `{"message"`。
pub fn looks_like_fault_page(head: &[u8]) -> bool {
    let mut s: Vec<u8> = head.to_vec();
    if s.starts_with(&[0xEF, 0xBB, 0xBF]) {
        s = s[3..].to_vec();
    }
    let text = String::from_utf8_lossy(&s).to_ascii_lowercase();
    let t = text.trim_start();
    t.starts_with("<!doctype")
        || t.starts_with("<html")
        || t.starts_with("<?xml")
        || t.starts_with("{\"message\"")
        || t.starts_with("{ \"message\"")
}

/// 产物三分判定（见 [`ProductVerdict`] 判定表）。
pub fn classify_product(
    head: &[u8],
    size: u64,
    expected_size: Option<u64>,
    sha_matches: bool,
) -> ProductVerdict {
    if sha_matches {
        return ProductVerdict::HashOk;
    }
    if looks_like_fault_page(head) {
        return ProductVerdict::ChannelFault(
            "server returned an HTML/JSON error page instead of the file".to_string(),
        );
    }
    if let Some(exp) = expected_size {
        if size != exp {
            return ProductVerdict::ChannelFault(format!(
                "truncated download: got {size} bytes, expected {exp}"
            ));
        }
    }
    // 无 sizeBytes 信息时也绝不放行（保守中止，不放松安全）
    ProductVerdict::Tamper
}

/// 成功回执。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadReceipt {
    pub channel: ChannelType,
    pub url: String,
    pub bytes: u64,
    /// 小写 hex（已按 S1 校验通过）
    pub sha256: String,
    /// 断点续传起始字节（0 = 全新下载）
    pub resumed_from: u64,
    /// 本通道发起的 GET 请求数
    pub get_requests: usize,
}

/// SHA-256（S1 用；纯函数）。
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex_encode(&hasher.finalize())
}

/// 流式 SHA-256 文件哈希。
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// 读文件头 `n` 字节（产物三分判定用；读失败按本地 IO 致命错误处理）。
fn read_file_head(path: &Path, n: usize) -> Result<Vec<u8>, DownloadError> {
    let mut f = File::open(path).map_err(|e| DownloadError::Io {
        context: format!("读取产物文件头 {}", path.display()),
        source: e,
    })?;
    let mut buf = vec![0u8; n];
    let got = f.read(&mut buf).map_err(|e| DownloadError::Io {
        context: format!("读取产物文件头 {}", path.display()),
        source: e,
    })?;
    buf.truncate(got);
    Ok(buf)
}

/// 多通道顺序降级下载：
/// 依次尝试 `channels`，成功通道按 S1 校验 SHA-256 后原子落盘；
/// 单段重试 2 次失败 → 下一通道（`.part` 删除，跨源从头下载）；
/// 全部失败 → [`DownloadError::AllChannelsDead`]。
///
/// **慢速安全阀**：若本轮所有通道都因过慢被熔断（见 [`SlowValve`]），则对**第一个**通道
/// 关闭熔断（旧路径）再试一次——慢 ≠ 不能下，不能让熔断把「慢慢能下完」变成「直接失败」。
///
/// `progress` 为进度闭包（不依赖 Tauri），随字节写入单调回调。
pub fn download_fallback(
    channels: &[ChannelUrl],
    dest: &Path,
    expected_sha256: &str,
    opts: &DownloadOptions,
    progress: &mut dyn FnMut(DownloadProgress),
) -> Result<DownloadReceipt, DownloadError> {
    if channels.is_empty() {
        return Err(DownloadError::NoChannels);
    }
    let agent = build_agent(opts)?;
    let part = part_path(dest);
    let mut failures: Vec<ChannelFailure> = Vec::new();
    // 安全阀：至多触发一次（内部状态保证）
    let mut valve = SlowValve::new();
    // 「待发的换源原因」：某通道被放弃后暂存，等下一个通道的**首个**进度事件带出去。
    //
    // **为什么这样传**：进度回调是上层唯一的进度入口，UI 只有在这里才知道
    // 「刚换了源，而且是因为慢/坏」——用 Cell 存一次、取出即清空，保证只发一次。
    let pending_switch: Cell<Option<ChannelFailureKind>> = Cell::new(None);

    for (idx, ch) in channels.iter().enumerate() {
        if idx > 0 {
            // 跨源从头下载：换通道前先丢掉上一个通道的半截产物
            // （否则会把两个源的字节拼在一起 → 哈希必然不符 → 误判篡改）
            let _ = fs::remove_file(&part);
        }
        // 包一层进度闭包：新通道的首个进度事件带上「待发换源原因」（带过即清空）
        let inner = &mut *progress;
        let mut wrapped = |mut p: DownloadProgress| {
            if let Some(reason) = pending_switch.take() {
                p.switch_reason = Some(reason);
            }
            inner(p);
        };
        match attempt_channel(&agent, ch, &part, dest, opts, expected_sha256, &mut wrapped) {
            Ok(receipt) => return Ok(receipt),
            Err(ChannelError::Fatal(e)) => return Err(e),
            Err(ChannelError::TooSlow {
                bps,
                threshold,
                detail,
            }) => {
                // 熔断命中：保留 .part（本通道已下到的字节）→ 安全阀可原通道续传
                failures.push(ChannelFailure {
                    channel: ch.channel,
                    url: ch.url.clone(),
                    reason: detail,
                    kind: ChannelFailureKind::TooSlow { bps, threshold },
                });
                // 还有下一个通道 → 记下「因过慢换源」，让 UI 告诉用户「线路慢」而非「线路坏」
                if idx + 1 < channels.len() {
                    pending_switch.set(Some(ChannelFailureKind::TooSlow { bps, threshold }));
                }
            }
            Err(ChannelError::Channel(reason)) => {
                // 真实故障（HTTP/传输/故障页/截断）：丢掉半截产物，降级下一通道
                let _ = fs::remove_file(&part);
                failures.push(ChannelFailure {
                    channel: ch.channel,
                    url: ch.url.clone(),
                    reason,
                    kind: ChannelFailureKind::Fault,
                });
                // 还有下一个通道 → 记下「因故障换源」
                if idx + 1 < channels.len() {
                    pending_switch.set(Some(ChannelFailureKind::Fault));
                }
            }
        }
    }

    // ---- 慢速安全阀：本轮所有通道都过慢 → 关闭熔断，对第一个通道再试一次 ----
    if let Some(idx) = valve.plan(&failures) {
        // 换源一致性：.part 里的半截数据只可能来自最后一个尝试的通道；
        // 与重试目标不是同一个通道时必须丢弃（否则会拼出跨源产物 → 误判篡改）
        if idx + 1 != channels.len() {
            let _ = fs::remove_file(&part);
        }
        let mut valve_opts = opts.clone();
        valve_opts.min_segment_bps = None; // 关闭熔断 → 旧路径：慢也慢慢下完
        let ch = &channels[idx];
        // 安全阀重试 = 换回第一个通道：原因沿用「本轮全部过慢」
        // （否则 UI 会把「重试慢通道」说成「线路不可用」，误导用户）
        let last_ch = channels.last().map(|c| c.channel);
        if Some(ch.channel) != last_ch {
            if let Some(kind) = failures.last().map(|f| f.kind.clone()) {
                pending_switch.set(Some(kind));
            }
        }
        // 包一层进度闭包：让安全阀重试的首个进度事件也带上换源原因（同上：只带一次）
        let inner = &mut *progress;
        let mut wrapped = |mut p: DownloadProgress| {
            if let Some(reason) = pending_switch.take() {
                p.switch_reason = Some(reason);
            }
            inner(p);
        };
        match attempt_channel(
            &agent,
            ch,
            &part,
            dest,
            &valve_opts,
            expected_sha256,
            &mut wrapped,
        ) {
            Ok(receipt) => return Ok(receipt),
            Err(ChannelError::Fatal(e)) => return Err(e),
            Err(e) => {
                // 安全阀也失败 → 保留原有语义报错，并带上「已不限速重试过」的可读原因
                let reason = match e {
                    ChannelError::Channel(r) => r,
                    ChannelError::TooSlow { detail, .. } => detail,
                    ChannelError::Fatal(_) => unreachable!("Fatal 已在上一个分支返回"),
                };
                let _ = fs::remove_file(&part);
                return Err(DownloadError::AllChannelsDead {
                    failures,
                    slow_valve: Some(SlowValveInfo {
                        channel: ch.channel,
                        url: ch.url.clone(),
                        reason,
                    }),
                });
            }
        }
    }

    let _ = fs::remove_file(&part); // 清理半截产物
    Err(DownloadError::AllChannelsDead {
        failures,
        slow_valve: None,
    })
}

/// 单通道尝试：下载 + 产物三分判定（S1）。
///
/// - `Ok(receipt)`：下载完成、S1 校验通过、已原子落盘；
/// - `Err(ChannelError::Channel)`：真实故障（HTTP/传输/故障页/截断）→ 可降级；
/// - `Err(ChannelError::TooSlow)`：熔断命中（过慢）；
/// - `Err(ChannelError::Fatal)`：致命（本地 IO / 真篡改）→ 立即中止，不降级。
fn attempt_channel(
    agent: &ureq::Agent,
    ch: &ChannelUrl,
    part: &Path,
    dest: &Path,
    opts: &DownloadOptions,
    expected_sha256: &str,
    progress: &mut dyn FnMut(DownloadProgress),
) -> Result<DownloadReceipt, ChannelError> {
    let done = download_one(agent, ch, part, opts, progress)?;
    // 产物三分判定（S1）：故障页/截断 → 通道故障降级；真篡改 → 中止
    let actual = sha256_file(part).map_err(|e| {
        ChannelError::Fatal(DownloadError::Io {
            context: format!("读取下载产物 {}", part.display()),
            source: e,
        })
    })?;
    let size = fs::metadata(part).map(|m| m.len()).unwrap_or(done.total);
    let head = read_file_head(part, 512).map_err(ChannelError::Fatal)?;
    let sha_matches = actual.eq_ignore_ascii_case(expected_sha256);
    match classify_product(&head, size, opts.expected_size, sha_matches) {
        ProductVerdict::HashOk => {
            if dest.exists() {
                fs::remove_file(dest).map_err(|e| {
                    ChannelError::Fatal(DownloadError::Io {
                        context: format!("覆盖目标文件 {}", dest.display()),
                        source: e,
                    })
                })?;
            }
            fs::rename(part, dest).map_err(|e| {
                ChannelError::Fatal(DownloadError::Io {
                    context: format!("落盘 {}", dest.display()),
                    source: e,
                })
            })?;
            Ok(DownloadReceipt {
                channel: ch.channel,
                url: ch.url.clone(),
                bytes: done.total,
                sha256: actual,
                resumed_from: done.resumed_from,
                get_requests: done.get_requests,
            })
        }
        ProductVerdict::ChannelFault(reason) => {
            // 通道坏（故障页/截断）→ 记该通道失败，降级下一通道
            let _ = fs::remove_file(part);
            Err(ChannelError::Channel(reason))
        }
        ProductVerdict::Tamper => {
            // 真篡改（大小相符但哈希不符）→ 立即中止，不降级不放行
            let _ = fs::remove_file(part);
            Err(ChannelError::Fatal(DownloadError::HashMismatch {
                channel: ch.channel,
                expected: expected_sha256.to_ascii_lowercase(),
                actual,
            }))
        }
    }
}

/// 慢速安全阀（一次性决策器）。
///
/// **为什么需要**：慢速熔断是「体验优化」（别让用户干等一个卡死的通道），
/// 但当**所有**通道都慢时，熔断会把「慢但能下完」变成「直接失败」——这是 UX 倒退
/// （例如用户只有 3G、没有 CN 网关兜底）。因此这种情况下对第一个通道关闭熔断（旧路径）再试一次。
///
/// **为什么必须结构化判定**：只有「本轮全部失败原因都是过慢」才允许兜底；
/// 只要有任一通道是真实故障（HTTP 错误/故障页/截断），说明「换源」这条路已经走到头，
/// 再重试第一个慢通道大概率也是白等（还会把用户的等待时间拉长）。
#[derive(Debug, Default, Clone, Copy)]
pub struct SlowValve {
    used: bool,
}

impl SlowValve {
    /// 新建（尚未触发）。
    pub fn new() -> Self {
        Self { used: false }
    }

    /// 判定是否触发安全阀：返回应重试的通道下标
    /// （`failures` 与 `channels` 顺序一一对应，故下标可直接用于索引通道表）。
    ///
    /// 触发条件（必须全部满足）：
    /// 1. 本轮至少有一个通道失败；
    /// 2. **所有**失败原因都是「过慢」（[`ChannelFailureKind::TooSlow`]）；
    /// 3. 安全阀尚未用过（**至多触发一次**，不循环）。
    pub fn plan(&mut self, failures: &[ChannelFailure]) -> Option<usize> {
        if self.used || failures.is_empty() {
            return None;
        }
        if !all_failures_too_slow(failures) {
            return None;
        }
        self.used = true;
        Some(0)
    }

    /// 安全阀是否已触发过。
    pub fn used(&self) -> bool {
        self.used
    }
}

/// 本轮失败是否**全部**为「过慢」（纯函数，便于离线单测）。
pub fn all_failures_too_slow(failures: &[ChannelFailure]) -> bool {
    !failures.is_empty() && failures.iter().all(|f| f.kind.is_too_slow())
}

/// 单通道错误：通道级（可降级）或致命（本地 IO / 配置）。
enum ChannelError {
    /// 真实故障（HTTP 错误/传输错误/故障页/截断）→ 降级下一通道
    Channel(String),
    /// 熔断命中（通道过慢）：结构化区分「过慢失败」与「真故障」
    TooSlow {
        /// 实测速率（字节/秒）
        bps: u64,
        /// 熔断阈值（字节/秒）
        threshold: u64,
        /// 人话原因（进通道失败原因表）
        detail: String,
    },
    /// 致命（本地 IO / 配置 / 真篡改）：立即中止，不降级
    Fatal(DownloadError),
}

struct ChannelDone {
    total: u64,
    resumed_from: u64,
    get_requests: usize,
}

/// 单次 HTTP 尝试的结果。
enum AttemptResult {
    /// 当前段读满
    SegmentDone,
    /// 服务器不支持 Range：放弃断点从头来（不消耗重试预算）
    Restart,
    /// 本次失败（消耗一次重试预算）
    Failed(String),
    /// 通道太慢（实测速率低于阈值）：**立即放弃该通道**，不消耗重试预算
    TooSlow {
        /// 实测速率（字节/秒）
        bps: u64,
        /// 熔断阈值（字节/秒）
        threshold: u64,
        /// 人话原因
        detail: String,
    },
}

/// 单通道下载可变状态（尝试之间的续写状态都在这里）。
struct DownState<'a> {
    agent: &'a ureq::Agent,
    ch: &'a ChannelUrl,
    file: &'a mut File,
    written: u64,
    /// None=未知是否支持 Range；首次响应决定
    range_mode: Option<bool>,
    total: u64,
    segment_size: u64,
    resumed_from: u64,
    total_segments: usize,
    gets: usize,
    /// 慢速熔断守卫（`None` = 不熔断）
    speed_guard: Option<SpeedGuard>,
    progress: &'a mut dyn FnMut(DownloadProgress),
}

impl DownState<'_> {
    /// 执行一次 HTTP 尝试（HEAD 已完成，总长已知）。
    fn attempt(&mut self, seg_end: u64) -> Result<AttemptResult, DownloadError> {
        let use_range = self.range_mode != Some(false);
        let mut request = self.agent.get(self.ch.url.as_str());
        if use_range {
            request = request.set("Range", &format!("bytes={}-{}", self.written, seg_end));
        }
        let response = request.set("Connection", "close").call();
        let resp = match response {
            Ok(r) => r,
            Err(ureq::Error::Status(code, _)) => {
                return Ok(AttemptResult::Failed(format!("HTTP {code}")))
            }
            Err(ureq::Error::Transport(t)) => return Ok(AttemptResult::Failed(t.to_string())),
        };

        let want: u64 = match resp.status() {
            206 => {
                self.range_mode = Some(true);
                seg_end + 1
            }
            200 => {
                // 服务器不支持/忽略 Range → 整包单流；已有断点只能放弃
                self.range_mode = Some(false);
                if self.written > 0 {
                    self.file
                        .set_len(0)
                        .map_err(|e| DownloadError::Io {
                            context: "重置断点文件".into(),
                            source: e,
                        })?;
                    self.written = 0;
                    return Ok(AttemptResult::Restart);
                }
                self.total
            }
            other => return Ok(AttemptResult::Failed(format!("unexpected HTTP {other}"))),
        };

        let start_written = self.written;
        let declared_cl: Option<u64> = resp
            .header("content-length")
            .and_then(|v| v.trim().parse::<u64>().ok());
        let mut reader = resp.into_reader();
        let mut buf = [0u8; 64 * 1024];
        self.file
            .seek(SeekFrom::Start(self.written))
            .map_err(|e| DownloadError::Io {
                context: "定位断点文件".into(),
                source: e,
            })?;
        let mut read_error: Option<String> = None;
        let mut eof_early = false;
        // 慢速熔断：窗口测速（响应头已到 → 只量 body 传输速率，不含握手开销）
        let mut win_start = Instant::now();
        let mut win_bytes: u64 = 0;
        while self.written < want {
            let n = match reader.read(&mut buf) {
                Ok(0) => {
                    eof_early = true;
                    break; // 提前 EOF → 短体
                }
                Ok(n) => n,
                Err(e) => {
                    read_error = Some(e.to_string());
                    break;
                }
            };
            let take = (n as u64).min(want - self.written) as usize;
            self.file
                .write_all(&buf[..take])
                .map_err(|e| DownloadError::Io {
                    context: "写入下载产物".into(),
                    source: e,
                })?;
            self.written += take as u64;
            let segment = if self.range_mode == Some(false) {
                0
            } else {
                (self.written / self.segment_size) as usize
            };
            (self.progress)(DownloadProgress {
                channel: self.ch.channel,
                downloaded: self.written,
                total: self.total,
                segment,
                segments: if self.range_mode == Some(false) {
                    1
                } else {
                    self.total_segments
                },
                resumed: self.resumed_from > 0,
                // 换源原因由 `download_fallback` 的外层闭包注入（这里不感知）
                switch_reason: None,
            });
            // 慢速熔断（流中窗口）：实测速率低于阈值 → 立即放弃该通道
            if let Some(guard) = self.speed_guard {
                // 只对足够大的段判定（小于 1 MiB 的段测速噪音大）
                let span = want.saturating_sub(start_written);
                if span >= guard.segment_min_bytes {
                    win_bytes += take as u64;
                    let win_elapsed = win_start.elapsed();
                    if win_elapsed >= guard.window && win_bytes >= guard.window_min_bytes {
                        if segment_speed_too_slow(
                            win_bytes,
                            win_elapsed,
                            Some(guard.threshold),
                            guard.window_min_bytes,
                        ) {
                            let bps = observed_bps(win_bytes, win_elapsed).unwrap_or(0);
                            return Ok(AttemptResult::TooSlow {
                                bps,
                                threshold: guard.threshold,
                                detail: format!(
                                    "通道过慢：{bps} B/s < {} B/s（窗口 {win_bytes} 字节 / {:.1}s）",
                                    guard.threshold,
                                    win_elapsed.as_secs_f64()
                                ),
                            });
                        }
                        // 本窗口合格 → 重置窗口，继续观测下一窗口
                        win_bytes = 0;
                        win_start = Instant::now();
                    }
                }
            }
        }
        // 段 Content-Length 完整性校验：声明长度 ≠ 实际读到 → 该段重试（既有重试机制）
        if eof_early || read_error.is_some() {
            let got = self.written - start_written;
            if let Some(cl) = declared_cl {
                if got != cl {
                    let extra = read_error
                        .as_ref()
                        .map(|e| format!(" (body read failed: {e})"))
                        .unwrap_or_default();
                    return Ok(AttemptResult::Failed(format!(
                        "content-length mismatch: got {got} bytes, declared {cl}{extra}"
                    )));
                }
            }
        }
        if let Some(err) = read_error {
            return Ok(AttemptResult::Failed(format!("body read failed: {err}")));
        }
        if self.written < want {
            return Ok(AttemptResult::Failed(format!(
                "short body: got {}/{} bytes",
                self.written, want
            )));
        }
        Ok(AttemptResult::SegmentDone)
    }
}

fn build_agent(opts: &DownloadOptions) -> Result<ureq::Agent, DownloadError> {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(opts.timeout)
        .timeout_read(opts.timeout)
        .timeout_write(opts.timeout)
        // 关闭连接池：失败语义可预测（不依赖 ureq 的连接复用自动重试）
        .max_idle_connections(0)
        .user_agent("aibox-net/0.0.1");
    if let Some(proxy) = &opts.proxy {
        let ureq_proxy = proxy.to_ureq().map_err(DownloadError::BadProxy)?;
        builder = builder.proxy(ureq_proxy);
    }
    Ok(builder.build())
}

/// 单通道下载（HEAD 取总长 → 分段 Range → 断点续传 → 段级重试 2 次 → 放弃通道）。
fn download_one(
    agent: &ureq::Agent,
    ch: &ChannelUrl,
    part: &Path,
    opts: &DownloadOptions,
    progress: &mut dyn FnMut(DownloadProgress),
) -> Result<ChannelDone, ChannelError> {
    let segment_size = opts.segment_size.max(1);
    let total = head_total(agent, &ch.url).map_err(ChannelError::Channel)?;

    // 断点续传：已有 .part 且未超总长 → 从断点继续；陈旧（超长）→ 重下
    let mut start = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if start > total {
        let _ = fs::remove_file(part);
        start = 0;
    }
    let resumed_from = start;

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .open(part)
        .map_err(|e| ChannelError::Fatal(DownloadError::Io {
            context: format!("打开断点文件 {}", part.display()),
            source: e,
        }))?;

    let total_segments = total.div_ceil(segment_size).max(1) as usize;
    let speed_guard = slow_guard_for(total, opts.min_segment_bps);
    let mut state = DownState {
        agent,
        ch,
        file: &mut file,
        written: start,
        range_mode: None,
        total,
        segment_size,
        resumed_from,
        total_segments,
        gets: 0,
        speed_guard,
        progress,
    };

    let mut retries_left = opts.segment_retries;
    let mut current_segment: Option<u64> = None;
    // 段级测速累计（本段跨多次尝试）
    let mut seg_bytes: u64 = 0;
    let mut seg_elapsed = Duration::ZERO;

    while state.written < total {
        let seg_idx = state.written / segment_size;
        if current_segment != Some(seg_idx) {
            current_segment = Some(seg_idx);
            retries_left = opts.segment_retries; // 新段：重置重试预算
            seg_bytes = 0;
            seg_elapsed = Duration::ZERO;
        }
        let seg_end = (state.written + segment_size).min(total) - 1;
        let before = state.written;
        let attempt_started = Instant::now();
        state.gets += 1;
        match state.attempt(seg_end).map_err(ChannelError::Fatal)? {
            AttemptResult::SegmentDone => {}
            AttemptResult::Restart => {
                // 已回退 written=0；range_mode=Some(false)，下轮整包单流
                seg_bytes = 0;
                seg_elapsed = Duration::ZERO;
            }
            AttemptResult::TooSlow {
                bps,
                threshold,
                detail,
            } => {
                // 慢速通道：立即放弃该通道并降级下一通道
                // （不消耗段重试预算——重试一个慢通道毫无意义，只会再卡 30s）
                return Err(ChannelError::TooSlow {
                    bps,
                    threshold,
                    detail,
                });
            }
            AttemptResult::Failed(reason) => {
                if retries_left == 0 {
                    return Err(ChannelError::Channel(format!(
                        "段重试 {} 次仍失败：{reason}",
                        opts.segment_retries
                    )));
                }
                retries_left -= 1;
                // 继续当前段：.part 已有部分字节 → 下一请求从断点续（Range）
            }
        }
        // 段级测速判定（跨尝试累计；样本不足 1 MiB 则跳过，避免误伤）
        if let Some(guard) = speed_guard {
            seg_bytes = seg_bytes.saturating_add(state.written.saturating_sub(before));
            seg_elapsed += attempt_started.elapsed();
            if segment_speed_too_slow(
                seg_bytes,
                seg_elapsed,
                Some(guard.threshold),
                guard.segment_min_bytes,
            ) {
                let bps = observed_bps(seg_bytes, seg_elapsed).unwrap_or(0);
                return Err(ChannelError::TooSlow {
                    bps,
                    threshold: guard.threshold,
                    detail: format!(
                        "通道过慢：{bps} B/s < {} B/s（本段累计 {seg_bytes} 字节 / {:.1}s）",
                        guard.threshold,
                        seg_elapsed.as_secs_f64()
                    ),
                });
            }
        }
    }

    Ok(ChannelDone {
        total,
        resumed_from,
        get_requests: state.gets,
    })
}

/// HEAD 取总长度；HEAD 不可用时回退 `GET Range: bytes=0-0` 解析 Content-Range。
fn head_total(agent: &ureq::Agent, url: &str) -> Result<u64, String> {
    let head = agent
        .head(url)
        .set("Range", "bytes=0-0")
        .set("Connection", "close")
        .call();
    if let Ok(resp) = head {
        if let Some(cl) = resp
            .header("content-length")
            .and_then(|v| v.trim().parse::<u64>().ok())
        {
            return Ok(cl);
        }
    }
    // 回退：GET 首字节，解析 `Content-Range: bytes 0-0/TOTAL`
    let resp = agent
        .get(url)
        .set("Range", "bytes=0-0")
        .set("Connection", "close")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            ureq::Error::Transport(t) => t.to_string(),
        })?;
    if let Some(cr) = resp.header("content-range") {
        if let Some(total) = cr
            .rsplit_once('/')
            .and_then(|(_, t)| t.trim().parse::<u64>().ok())
        {
            return Ok(total);
        }
    }
    if let Some(cl) = resp
        .header("content-length")
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        return Ok(cl);
    }
    Err("no Content-Length/Content-Range from server".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestHttpServer;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// 每测试独立临时目录（不引 tempfile 依赖）。
    fn tmp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aibox-net-dl-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create tmp dir");
        dir
    }

    fn deterministic_body(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| ((i * 31 + seed as usize) % 251) as u8)
            .collect()
    }

    fn opts(segment_size: u64) -> DownloadOptions {
        DownloadOptions {
            segment_size,
            segment_retries: SEGMENT_RETRIES,
            timeout: Duration::from_secs(5),
            proxy: None,
            expected_size: None,
            min_segment_bps: Some(DEFAULT_MIN_SEGMENT_BPS),
        }
    }

    fn opts_with_size(segment_size: u64, expected_size: Option<u64>) -> DownloadOptions {
        DownloadOptions {
            expected_size,
            ..opts(segment_size)
        }
    }

    fn no_progress() -> impl FnMut(DownloadProgress) {
        |_| {}
    }

    /// 两个「端口无人监听」地址（连接必败）。
    fn refused_pair() -> (String, String) {
        let one = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let a = l.local_addr().unwrap();
            drop(l);
            a.to_string()
        };
        let two = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let a = l.local_addr().unwrap();
            drop(l);
            a.to_string()
        };
        (one, two)
    }

    #[test]
    fn segment_size_default_is_8mb() {
        assert_eq!(SEGMENT_SIZE, 8 * 1024 * 1024);
        assert_eq!(SEGMENT_RETRIES, 2);
        assert_eq!(DownloadOptions::default().segment_size, SEGMENT_SIZE);
    }

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // 100 万个 'a'（NIST 向量，覆盖多块压缩路径）
        assert_eq!(
            sha256_hex(&vec![b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// 多分段下载 + S1 校验 + 进度单调。
    #[test]
    fn download_multi_segment_with_progress() {
        let dir = tmp_dir("multi");
        let body = deterministic_body(5000, 7);
        let server = TestHttpServer::serve(body.clone());
        let dest = dir.join("file.bin");
        let expected = sha256_hex(&body);

        let mut events: Vec<DownloadProgress> = Vec::new();
        let receipt = download_fallback(
            &[ChannelUrl::new(ChannelType::Direct, server.url("/f.bin"))],
            &dest,
            &expected,
            &opts(1024),
            &mut |p| events.push(p),
        )
        .expect("download must succeed");

        assert_eq!(receipt.channel, ChannelType::Direct);
        assert_eq!(receipt.bytes, 5000);
        assert_eq!(receipt.resumed_from, 0);
        assert_eq!(receipt.sha256, expected);
        assert_eq!(receipt.get_requests, 5, "5000B / 1024B = 5 段");

        // 落盘内容逐字节一致（S1 核心断言）
        assert_eq!(fs::read(&dest).expect("read dest"), body);
        assert!(!part_path(&dest).exists(), ".part 应已清理");

        // 每段 Range 起点
        assert_eq!(
            server.get_range_starts(),
            vec![Some(0), Some(1024), Some(2048), Some(3072), Some(4096)]
        );

        // 进度：单调不减、末值 == 总长、通道正确
        assert!(!events.is_empty());
        let mut last = 0u64;
        for e in &events {
            assert!(e.downloaded >= last, "进度必须单调: {e:?}");
            assert_eq!(e.total, 5000);
            assert_eq!(e.channel, ChannelType::Direct);
            assert!(!e.resumed);
            last = e.downloaded;
        }
        assert_eq!(last, 5000);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 断点续传：已有 .part → 从断点请求，不重下前段。
    #[test]
    fn download_resumes_from_existing_part() {
        let dir = tmp_dir("resume");
        let body = deterministic_body(5000, 9);
        let server = TestHttpServer::serve(body.clone());
        let dest = dir.join("file.bin");
        let part = part_path(&dest);
        // 模拟上次中断：前 2500 字节已落盘
        fs::write(&part, &body[..2500]).expect("seed part");
        let expected = sha256_hex(&body);

        let receipt = download_fallback(
            &[ChannelUrl::new(ChannelType::Mirror, server.url("/f.bin"))],
            &dest,
            &expected,
            &opts(1024),
            &mut no_progress(),
        )
        .expect("resumed download must succeed");

        assert_eq!(receipt.resumed_from, 2500, "从断点 2500 续传");
        // 任何 GET 都不得早于 2500 起（未重下已完成字节）
        let starts = server.get_range_starts();
        assert!(!starts.is_empty());
        for s in &starts {
            assert!(s.unwrap_or(0) >= 2500, "不得重下断点前数据: {starts:?}");
        }
        assert_eq!(fs::read(&dest).unwrap(), body);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 完整 .part（上次已下完但没校验）→ 直接校验落盘，零 GET。
    #[test]
    fn download_complete_part_skips_network_get() {
        let dir = tmp_dir("partdone");
        let body = deterministic_body(2048, 3);
        let server = TestHttpServer::serve(body.clone());
        let dest = dir.join("file.bin");
        fs::write(part_path(&dest), &body).unwrap();
        let expected = sha256_hex(&body);

        let receipt = download_fallback(
            &[ChannelUrl::new(ChannelType::Direct, server.url("/f.bin"))],
            &dest,
            &expected,
            &opts(1024),
            &mut no_progress(),
        )
        .unwrap();
        assert_eq!(receipt.get_requests, 0, "已完成的 .part 不再发 GET");
        assert_eq!(receipt.resumed_from, 2048);
        assert_eq!(fs::read(&dest).unwrap(), body);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 单段重试 2 次仍失败 → 整包降级到下一通道（跨源从头下载）。
    #[test]
    fn segment_retries_exhausted_falls_back_to_next_channel() {
        let dir = tmp_dir("fallback");
        let body = deterministic_body(3000, 11);
        let bad = TestHttpServer::serve(body.clone());
        let good = TestHttpServer::serve(body.clone());
        // 坏通道：连续 3 次 GET 半截体（1 次首发 + 2 次重试）
        bad.inject_truncate_get(3, 100);
        let dest = dir.join("file.bin");
        let expected = sha256_hex(&body);

        let receipt = download_fallback(
            &[
                ChannelUrl::new(ChannelType::Proxy, bad.url("/f.bin")),
                ChannelUrl::new(ChannelType::Direct, good.url("/f.bin")),
            ],
            &dest,
            &expected,
            &opts(1024),
            &mut no_progress(),
        )
        .expect("fallback must succeed");

        assert_eq!(receipt.channel, ChannelType::Direct, "降级到第二通道");
        assert_eq!(
            bad.get_range_starts().len(),
            3,
            "单段 1+2 次尝试后放弃该通道"
        );
        assert_eq!(
            good.get_range_starts(),
            vec![Some(0), Some(1024), Some(2048)],
            "跨源从头下载"
        );
        assert_eq!(fs::read(&dest).unwrap(), body);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 单段第 1 次失败、第 2 次重试成功 → 不降级。
    #[test]
    fn segment_retry_succeeds_in_place() {
        let dir = tmp_dir("retryok");
        let body = deterministic_body(3000, 13);
        let server = TestHttpServer::serve(body.clone());
        // 第 1 个 GET 半截体 → 第 2 个 GET（从断点续）成功
        server.inject_truncate_get(1, 300);
        let dest = dir.join("file.bin");
        let expected = sha256_hex(&body);

        let receipt = download_fallback(
            &[ChannelUrl::new(ChannelType::Direct, server.url("/f.bin"))],
            &dest,
            &expected,
            &opts(1024),
            &mut no_progress(),
        )
        .expect("retry in place must succeed");

        assert_eq!(receipt.channel, ChannelType::Direct, "同通道重试成功，不降级");
        let starts = server.get_range_starts();
        // 段0 两次尝试（0 → 断点300），段1、段2 各一次
        assert_eq!(starts.len(), 4, "失败1次 + 重试1次 + 其余2段: {starts:?}");
        assert_eq!(starts[0], Some(0));
        assert_eq!(starts[1], Some(300), "重试从断点续");
        assert!(starts[1].unwrap() > 0 && starts[1].unwrap() < 1024, "{starts:?}");
        assert_eq!(fs::read(&dest).unwrap(), body);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 连接被掐断（drop）→ 传输错误同样触发段重试。
    #[test]
    fn dropped_connection_retries_then_succeeds() {
        let dir = tmp_dir("drop");
        let body = deterministic_body(1500, 17);
        let server = TestHttpServer::serve(body.clone());
        server.inject_drop_get(1);
        let dest = dir.join("file.bin");
        let expected = sha256_hex(&body);

        let receipt = download_fallback(
            &[ChannelUrl::new(ChannelType::Direct, server.url("/f.bin"))],
            &dest,
            &expected,
            &opts(1024),
            &mut no_progress(),
        )
        .expect("dropped connection must be retried");
        assert_eq!(receipt.channel, ChannelType::Direct);
        assert_eq!(fs::read(&dest).unwrap(), body);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 503 → 段失败计数，重试耗尽后降级。
    #[test]
    fn http_error_status_falls_back_after_retries() {
        let dir = tmp_dir("503");
        let body = deterministic_body(2048, 19);
        let bad = TestHttpServer::serve(body.clone());
        let good = TestHttpServer::serve(body.clone());
        bad.inject_fail_status_get(3);
        let dest = dir.join("file.bin");
        let expected = sha256_hex(&body);

        let receipt = download_fallback(
            &[
                ChannelUrl::new(ChannelType::Mirror, bad.url("/f.bin")),
                ChannelUrl::new(ChannelType::Direct, good.url("/f.bin")),
            ],
            &dest,
            &expected,
            &opts(1024),
            &mut no_progress(),
        )
        .expect("503 channel must fall back");
        assert_eq!(receipt.channel, ChannelType::Direct);
        assert_eq!(bad.get_range_starts().len(), 3, "1+2 次尝试全部 503");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 全通道失败 → 结构化 NET_ALL_CHANNELS_DEAD + 每通道原因表。
    #[test]
    fn all_channels_dead_returns_structured_error() {
        let dir = tmp_dir("dead");
        let (dead1, dead2) = refused_pair();
        let dest = dir.join("file.bin");
        let err = download_fallback(
            &[
                ChannelUrl::new(ChannelType::Proxy, format!("http://{dead1}/f.bin")),
                ChannelUrl::new(ChannelType::Direct, format!("http://{dead2}/f.bin")),
            ],
            &dest,
            &"00".repeat(32),
            &opts(1024),
            &mut no_progress(),
        )
        .unwrap_err();

        assert_eq!(err.code(), "NET_ALL_CHANNELS_DEAD");
        let report = err.channel_report();
        assert_eq!(report.len(), 2, "每通道一条失败原因: {report:?}");
        assert_eq!(report[0].0, "PROXY");
        assert_eq!(report[1].0, "DIRECT");
        let low = report[0].1.to_lowercase();
        assert!(
            low.contains("fail") || low.contains("connect") || low.contains("refused"),
            "原因应为连接失败: {:?}",
            report[0]
        );
        assert!(err.human_message().contains("当前网络无法访问任何下载源"));
        assert!(!dest.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// S1：产物被篡改 → 中止（NET_HASH_MISMATCH），不降级、不放行。
    #[test]
    fn hash_mismatch_aborts_without_fallback() {
        let dir = tmp_dir("tamper");
        let body = deterministic_body(2048, 23);
        let tampered = TestHttpServer::serve(deterministic_body(2048, 99)); // 内容被换
        let clean = TestHttpServer::serve(body.clone());
        let dest = dir.join("file.bin");
        let expected = sha256_hex(&body); // 预期哈希来自独立源

        let err = download_fallback(
            &[
                ChannelUrl::new(ChannelType::Mirror, tampered.url("/f.bin")),
                ChannelUrl::new(ChannelType::Direct, clean.url("/f.bin")),
            ],
            &dest,
            &expected,
            &opts(1024),
            &mut no_progress(),
        )
        .unwrap_err();

        assert_eq!(err.code(), "NET_HASH_MISMATCH");
        assert!(err.human_message().contains("校验失败"));
        assert!(
            matches!(
                &err,
                DownloadError::HashMismatch {
                    channel: ChannelType::Mirror,
                    ..
                }
            ),
            "{err:?}"
        );
        assert_eq!(
            clean.request_count(),
            0,
            "哈希不符必须中止，绝不尝试下一通道（C 档语义）"
        );
        assert!(!dest.exists(), "篡改产物不得落盘");
        assert!(!part_path(&dest).exists(), "篡改产物 .part 必须清理");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 空文件（total=0）：零 GET、空哈希向量。
    #[test]
    fn empty_file_download() {
        let dir = tmp_dir("empty");
        let server = TestHttpServer::serve(Vec::new());
        let dest = dir.join("empty.bin");
        let expected = sha256_hex(b"");
        let receipt = download_fallback(
            &[ChannelUrl::new(ChannelType::Direct, server.url("/e"))],
            &dest,
            &expected,
            &opts(1024),
            &mut no_progress(),
        )
        .unwrap();
        assert_eq!(receipt.bytes, 0);
        assert_eq!(receipt.get_requests, 0);
        assert_eq!(fs::read(&dest).unwrap(), Vec::<u8>::new());
        let _ = fs::remove_dir_all(&dir);
    }

    /// 无通道 → NET_NO_CHANNELS。
    #[test]
    fn no_channels_error() {
        let dir = tmp_dir("nochan");
        let err = download_fallback(
            &[],
            &dir.join("x"),
            &"00".repeat(32),
            &opts(1024),
            &mut no_progress(),
        )
        .unwrap_err();
        assert_eq!(err.code(), "NET_NO_CHANNELS");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 断点文件超过总长（文件已变更）→ 丢弃重下。
    #[test]
    fn stale_part_longer_than_total_restarts() {
        let dir = tmp_dir("stale");
        let body = deterministic_body(1000, 29);
        let server = TestHttpServer::serve(body.clone());
        let dest = dir.join("file.bin");
        fs::write(part_path(&dest), vec![0xAA; 5000]).unwrap(); // 陈旧超长
        let expected = sha256_hex(&body);

        let receipt = download_fallback(
            &[ChannelUrl::new(ChannelType::Direct, server.url("/f"))],
            &dest,
            &expected,
            &opts(4096),
            &mut no_progress(),
        )
        .unwrap();
        assert_eq!(receipt.resumed_from, 0, "陈旧断点必须重下");
        assert_eq!(fs::read(&dest).unwrap(), body);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 服务器不支持 Range（恒 200）→ 整包单流仍能完成。
    #[test]
    fn server_without_range_support_still_downloads() {
        use std::io::{Read as _, Write as _};
        let dir = tmp_dir("norange");
        let body = deterministic_body(3000, 31);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        {
            let body = body.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let mut stream = match stream {
                        Ok(s) => s,
                        Err(_) => break,
                    };
                    let body = body.clone();
                    std::thread::spawn(move || {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let mut buf = [0u8; 4096];
                        let mut req = Vec::new();
                        loop {
                            match stream.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    req.extend_from_slice(&buf[..n]);
                                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        let text = String::from_utf8_lossy(&req);
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(head.as_bytes());
                        if !text.starts_with("HEAD") {
                            let _ = stream.write_all(&body);
                        }
                        let _ = stream.flush();
                    });
                }
            });
        }
        let dest = dir.join("file.bin");
        let expected = sha256_hex(&body);
        let receipt = download_fallback(
            &[ChannelUrl::new(ChannelType::Direct, format!("http://{addr}/f"))],
            &dest,
            &expected,
            &opts(1024), // 请求了分段，但服务器恒 200
            &mut no_progress(),
        )
        .expect("no-range server must still complete");
        assert_eq!(fs::read(&dest).unwrap(), body);
        assert_eq!(receipt.bytes, 3000);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 产物三分判定矩阵（纯函数，离线）。
    #[test]
    fn classify_product_three_way_matrix() {
        let html = b"<!DOCTYPE html><html><body>502 Bad Gateway</body></html>";
        let xml = b"<?xml version=\"1.0\"?><error>quota</error>";
        let json = br#"{"message":"upstream timeout"}"#;
        let exe = b"MZ\x90\x00 this is a real installer payload";
        let sha_ok = true;

        // 1) 哈希相符 → 放行（哪怕长得像文本）
        assert_eq!(classify_product(html, 100, Some(999), sha_ok), ProductVerdict::HashOk);
        // 2) 故障页（HTML/XML/JSON 错误）→ 通道故障（降级）
        for page in [html.as_slice(), xml.as_slice(), json.as_slice()] {
            assert!(matches!(
                classify_product(page, 100, None, false),
                ProductVerdict::ChannelFault(_)
            ));
        }
        // 3) 大小不符（截断）→ 通道故障（降级）
        assert!(matches!(
            classify_product(exe, 500, Some(1000), false),
            ProductVerdict::ChannelFault(_)
        ));
        // 4) 大小相符但哈希不符 → 真篡改 → 中止（安全红线）
        assert_eq!(
            classify_product(exe, 1000, Some(1000), false),
            ProductVerdict::Tamper
        );
        // 5) 无 sizeBytes 信息 + 哈希不符 + 非故障页 → 保守中止（不放松安全）
        assert_eq!(classify_product(exe, 1000, None, false), ProductVerdict::Tamper);
        // 6) BOM + 空白前缀的故障页也识别
        let bom = b"\xEF\xBB\xBF\n<html><body>expired</body></html>";
        assert!(matches!(
            classify_product(bom, 64, None, false),
            ProductVerdict::ChannelFault(_)
        ));
    }

    /// 故障页（HTML 网关错误页）→ 判通道故障 → 降级下一通道成功。
    #[test]
    fn html_fault_page_falls_back_to_next_channel() {
        let dir = tmp_dir("faultpage");
        let body = deterministic_body(4096, 5);
        let bad = TestHttpServer::serve(
            b"<!DOCTYPE html><html><head><title>502 Bad Gateway</title></head></html>".to_vec(),
        );
        let good = TestHttpServer::serve(body.clone());
        let dest = dir.join("file.bin");
        let expected = sha256_hex(&body);

        let receipt = download_fallback(
            &[
                ChannelUrl::new(ChannelType::Proxy, bad.url("/f.bin")),
                ChannelUrl::new(ChannelType::Direct, good.url("/f.bin")),
            ],
            &dest,
            &expected,
            &opts(1024),
            &mut no_progress(),
        )
        .expect("fault page must fall back to next channel");

        assert_eq!(receipt.channel, ChannelType::Direct, "降级到第二通道");
        assert!(bad.request_count() >= 1, "坏通道至少试过一次");
        assert_eq!(fs::read(&dest).unwrap(), body);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 截断（大小 ≠ 期望 sizeBytes）→ 判通道故障 → 降级下一通道。
    #[test]
    fn truncated_size_falls_back_to_next_channel() {
        let dir = tmp_dir("trunc");
        let full = deterministic_body(1000, 7);
        let short = deterministic_body(500, 9); // 网关截断：只有半截
        let bad = TestHttpServer::serve(short.clone());
        let good = TestHttpServer::serve(full.clone());
        let dest = dir.join("file.bin");
        let expected = sha256_hex(&full);

        let receipt = download_fallback(
            &[
                ChannelUrl::new(ChannelType::Proxy, bad.url("/f.bin")),
                ChannelUrl::new(ChannelType::Direct, good.url("/f.bin")),
            ],
            &dest,
            &expected,
            &opts_with_size(256, Some(1000)),
            &mut no_progress(),
        )
        .expect("truncated download must fall back");

        assert_eq!(receipt.channel, ChannelType::Direct);
        assert_eq!(fs::read(&dest).unwrap(), full);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 同长度篡改（sizeBytes 相符）+ 哈希不符 → 真篡改中止，绝不降级（红线）。
    #[test]
    fn tamper_with_matching_size_still_aborts() {
        let dir = tmp_dir("tamper-size");
        let body = deterministic_body(2048, 23);
        let tampered = TestHttpServer::serve(deterministic_body(2048, 99));
        let clean = TestHttpServer::serve(body.clone());
        let dest = dir.join("file.bin");
        let expected = sha256_hex(&body);

        let err = download_fallback(
            &[
                ChannelUrl::new(ChannelType::Mirror, tampered.url("/f.bin")),
                ChannelUrl::new(ChannelType::Direct, clean.url("/f.bin")),
            ],
            &dest,
            &expected,
            &opts_with_size(1024, Some(2048)),
            &mut no_progress(),
        )
        .unwrap_err();

        assert_eq!(err.code(), "NET_HASH_MISMATCH");
        assert_eq!(clean.request_count(), 0, "真篡改必须中止，不降级");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 段 Content-Length 完整性校验：声明长度 ≠ 实际读到 → 该段重试，耗尽后降级/报错。
    #[test]
    fn content_length_mismatch_fails_segment() {
        let dir = tmp_dir("clen");
        let body = deterministic_body(2048, 11);
        let bad = TestHttpServer::serve(body.clone());
        // 所有 GET：头声明全长、体只发 100 字节后断开
        bad.inject_truncate_get(100, 100);
        let dest = dir.join("file.bin");
        let expected = sha256_hex(&body);

        let err = download_fallback(
            &[ChannelUrl::new(ChannelType::Direct, bad.url("/f.bin"))],
            &dest,
            &expected,
            &opts(1024),
            &mut no_progress(),
        )
        .unwrap_err();

        assert_eq!(err.code(), "NET_ALL_CHANNELS_DEAD");
        let report = err.channel_report();
        assert!(
            report[0].1.contains("content-length mismatch"),
            "失败原因应标明段 Content-Length 不符: {:?}",
            report[0]
        );
        assert_eq!(bad.get_range_starts().len(), 3, "1 + 2 次段重试");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 慢速熔断纯判定矩阵（离线，无需网络）。
    #[test]
    fn slow_segment_speed_pure_judgement() {
        const MIB: u64 = 1024 * 1024;
        // 1) 未配置阈值 → 永不熔断（保持旧行为）
        assert!(!segment_speed_too_slow(
            MIB,
            Duration::from_secs(60),
            None,
            MIB
        ));
        // 2) 样本太小（< 1 MiB）→ 跳过判定（测速噪音大，避免误伤小段）
        assert!(!segment_speed_too_slow(
            16 * 1024,
            Duration::from_secs(60),
            Some(32 * 1024),
            MIB
        ));
        // 3) 实测 1 MiB/60s ≈ 17.1 KB/s < 32 KiB/s → 熔断
        assert!(segment_speed_too_slow(
            MIB,
            Duration::from_secs(60),
            Some(32 * 1024),
            MIB
        ));
        // 4) 实测 1 MiB/s ≥ 32 KiB/s → 不熔断
        assert!(!segment_speed_too_slow(
            MIB,
            Duration::from_secs(1),
            Some(32 * 1024),
            MIB
        ));
        // 5) 耗时为 0 → 无法测速 → 不熔断
        assert!(!segment_speed_too_slow(
            MIB,
            Duration::ZERO,
            Some(32 * 1024),
            MIB
        ));
        // 速率换算
        assert_eq!(observed_bps(MIB, Duration::from_secs(2)), Some(524_288));
        assert_eq!(observed_bps(1024, Duration::ZERO), None);
    }

    /// 默认阈值 = 32 KiB/s；整包 < 8 MiB 不熔断（不误伤小文件）。
    #[test]
    fn slow_guard_defaults_and_small_file_gate() {
        assert_eq!(DEFAULT_MIN_SEGMENT_BPS, 32 * 1024);
        assert_eq!(
            DownloadOptions::default().min_segment_bps,
            Some(DEFAULT_MIN_SEGMENT_BPS)
        );
        assert!(!slow_guard_enabled(5 * 1024 * 1024, Some(32 * 1024)));
        assert!(slow_guard_enabled(9 * 1024 * 1024, Some(32 * 1024)));
        assert!(!slow_guard_enabled(100 * 1024 * 1024, None));

        // 小文件仍走旧路径（不熔断、不降级）
        let dir = tmp_dir("smallfile");
        let body = deterministic_body(4096, 41);
        let server = TestHttpServer::serve(body.clone());
        let dest = dir.join("small.bin");
        let expected = sha256_hex(&body);
        let receipt = download_fallback(
            &[ChannelUrl::new(ChannelType::Direct, server.url("/s.bin"))],
            &dest,
            &expected,
            &opts(1024),
            &mut no_progress(),
        )
        .expect("small file must download");
        assert_eq!(receipt.bytes, 4096);
        assert_eq!(fs::read(&dest).unwrap(), body);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 慢速通道（直连 18 KB/s 场景）→ 立即放弃并降级到下一通道。
    ///
    /// 注意：**不触发安全阀**——第二个通道正常成功了，就没必要回头重试第一个慢通道。
    #[test]
    fn slow_channel_is_abandoned_and_falls_back() {
        let dir = tmp_dir("slowfallback");
        // 慢通道：声明 9 MiB，每 8 KiB 间隔 1s（≈ 8 KiB/s ≪ 32 KiB/s 阈值）
        let (slow_url, slow_gets) = slow_server(9 * 1024 * 1024, 8 * 1024, Duration::from_secs(1));
        // 快通道：本地全速
        let good_body = deterministic_body(9 * 1024 * 1024, 43);
        let good = TestHttpServer::serve(good_body.clone());
        let dest = dir.join("big.bin");
        let expected = sha256_hex(&good_body);

        let started = Instant::now();
        let receipt = download_fallback(
            &[
                ChannelUrl::new(ChannelType::Direct, format!("{slow_url}/big.bin")),
                ChannelUrl::new(ChannelType::Mirror, good.url("/big.bin")),
            ],
            &dest,
            &expected,
            &opts(2 * 1024 * 1024),
            &mut no_progress(),
        )
        .expect("must fall back to the fast channel");
        let elapsed = started.elapsed();

        assert_eq!(
            receipt.channel,
            ChannelType::Mirror,
            "慢通道应被放弃，降级到快通道"
        );
        assert_eq!(
            slow_gets.load(Ordering::SeqCst),
            1,
            "慢通道只试 1 次（熔断不烧重试预算；第二通道成功 → 不触发安全阀）"
        );
        assert!(
            elapsed < Duration::from_secs(20),
            "熔断应快速生效（目标 10-20s 量级），实际 {elapsed:?}"
        );
        assert_eq!(fs::read(&dest).unwrap().len(), good_body.len());
        let _ = fs::remove_dir_all(&dir);
    }

    /// 换源原因透传（慢）：慢通道被放弃 → **下一个通道的首个**进度事件带「过慢」原因（只带一次）。
    ///
    /// **为什么测这个**：UI 要靠它区分「线路慢」与「线路坏」——
    /// 若原因丢失/重复，用户看到的提示就会说错。
    #[test]
    fn switch_reason_reports_slow_channel_fallback() {
        let dir = tmp_dir("switchslow");
        let (slow_url, _slow_gets) = slow_server(9 * 1024 * 1024, 8 * 1024, Duration::from_secs(1));
        let good_body = deterministic_body(9 * 1024 * 1024, 47);
        let good = TestHttpServer::serve(good_body.clone());
        let dest = dir.join("big.bin");
        let expected = sha256_hex(&good_body);

        let mut events: Vec<DownloadProgress> = Vec::new();
        let receipt = download_fallback(
            &[
                ChannelUrl::new(ChannelType::Direct, format!("{slow_url}/big.bin")),
                ChannelUrl::new(ChannelType::Mirror, good.url("/big.bin")),
            ],
            &dest,
            &expected,
            &opts(2 * 1024 * 1024),
            &mut |p| events.push(p),
        )
        .expect("must fall back to the fast channel");
        assert_eq!(receipt.channel, ChannelType::Mirror);

        // 首个（慢）通道自己的进度事件不带换源原因（它不是换源换来的）
        for e in events.iter().filter(|e| e.channel == ChannelType::Direct) {
            assert!(e.switch_reason.is_none(), "首个通道不应带换源原因: {e:?}");
        }
        // 换源后的通道：首个进度事件带「过慢」原因，之后不再重复带
        let mirror: Vec<&DownloadProgress> =
            events.iter().filter(|e| e.channel == ChannelType::Mirror).collect();
        assert!(!mirror.is_empty(), "换源后的通道应有进度事件");
        match &mirror[0].switch_reason {
            Some(ChannelFailureKind::TooSlow { .. }) => {}
            other => panic!("换源后首个进度事件应带 TooSlow 原因，实际 {other:?}"),
        }
        for e in &mirror[1..] {
            assert!(e.switch_reason.is_none(), "换源原因只发一次: {e:?}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// 换源原因透传（真故障）：通道连不上 → 下一个通道首个进度事件带「故障」原因。
    #[test]
    fn switch_reason_reports_fault_fallback() {
        let dir = tmp_dir("switchfault");
        let (dead, _dead2) = refused_pair();
        let body = deterministic_body(5000, 51);
        let good = TestHttpServer::serve(body.clone());
        let dest = dir.join("f.bin");
        let expected = sha256_hex(&body);

        let mut events: Vec<DownloadProgress> = Vec::new();
        let receipt = download_fallback(
            &[
                ChannelUrl::new(ChannelType::Direct, format!("http://{dead}/a.bin")),
                ChannelUrl::new(ChannelType::Mirror, good.url("/f.bin")),
            ],
            &dest,
            &expected,
            &opts(1024),
            &mut |p| events.push(p),
        )
        .expect("must fall back after a dead channel");
        assert_eq!(receipt.channel, ChannelType::Mirror);
        assert!(!events.is_empty(), "应有进度事件");
        assert_eq!(
            events[0].switch_reason,
            Some(ChannelFailureKind::Fault),
            "换源后首个进度事件应带 Fault 原因"
        );
        for e in &events[1..] {
            assert!(e.switch_reason.is_none(), "换源原因只发一次: {e:?}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// 不换源：全程不带换源原因（UI 不会误报「已换源」）。
    #[test]
    fn no_switch_reason_without_fallback() {
        let dir = tmp_dir("noswitch");
        let body = deterministic_body(5000, 53);
        let server = TestHttpServer::serve(body.clone());
        let dest = dir.join("f.bin");
        let expected = sha256_hex(&body);

        let mut events: Vec<DownloadProgress> = Vec::new();
        download_fallback(
            &[ChannelUrl::new(ChannelType::Direct, server.url("/f.bin"))],
            &dest,
            &expected,
            &opts(1024),
            &mut |p| events.push(p),
        )
        .expect("download must succeed");
        assert!(!events.is_empty());
        for e in &events {
            assert!(e.switch_reason.is_none(), "未换源不应带原因: {e:?}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// 换源原因码（UI 按它选文案；不能靠匹配人话字符串）。
    #[test]
    fn channel_failure_reason_codes() {
        assert_eq!(
            ChannelFailureKind::TooSlow {
                bps: 1024,
                threshold: 32 * 1024
            }
            .reason_code(),
            "slow-channel"
        );
        assert_eq!(ChannelFailureKind::Fault.reason_code(), "download-fallback");
    }

    /// 全部通道都慢 → 触发安全阀（不限速重试）；重试仍失败 → NET_ALL_CHANNELS_DEAD，
    /// 失败原因标明「过慢」，且错误里能看到「已用不限速方式重试过」。
    #[test]
    fn all_slow_channels_report_slow_reason() {
        let dir = tmp_dir("allslow");
        // 前 2 个 GET 慢速吐（两个通道各触发一次熔断）；之后的 GET 直接断连
        // （模拟安全阀的不限速重试也遇到真故障 → 快速失败，不让测试干等）
        let (slow_url, slow_gets) = slow_server_with(
            9 * 1024 * 1024,
            8 * 1024,
            Duration::from_secs(1),
            u32::MAX,
            Some(2),
        );
        let dest = dir.join("big.bin");
        let err = download_fallback(
            &[
                ChannelUrl::new(ChannelType::Direct, format!("{slow_url}/a.bin")),
                ChannelUrl::new(ChannelType::Mirror, format!("{slow_url}/b.bin")),
            ],
            &dest,
            &"00".repeat(32),
            &opts(2 * 1024 * 1024),
            &mut no_progress(),
        )
        .unwrap_err();

        assert_eq!(err.code(), "NET_ALL_CHANNELS_DEAD");
        let report = err.channel_report();
        assert_eq!(report.len(), 2, "每通道一条失败原因: {report:?}");
        for (name, reason) in &report {
            assert!(
                reason.contains("过慢"),
                "{name} 的失败原因应标明过慢: {reason}"
            );
        }
        // 安全阀已触发并失败 → 错误里必须能看出「已用不限速方式重试过」
        let valve = err.slow_valve().expect("所有通道都过慢 → 应触发安全阀");
        assert_eq!(valve.channel, ChannelType::Direct, "安全阀重试第一个通道");
        assert!(
            err.human_message().contains("不限速"),
            "人话报错应说明已用不限速方式重试过: {}",
            err.human_message()
        );
        // 2 次熔断尝试 + 1 次安全阀不限速重试（首发 + 2 次段重试）
        assert_eq!(
            slow_gets.load(Ordering::SeqCst),
            5,
            "2 次熔断 + 1 次安全阀重试（首发 + 2 次段重试）"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 安全阀触发条件纯判定矩阵（离线，无需网络）。
    #[test]
    fn slow_valve_decision_matrix() {
        let too_slow = ChannelFailure {
            channel: ChannelType::Direct,
            url: "http://x/a".into(),
            reason: "通道过慢：1024 B/s < 32768 B/s".into(),
            kind: ChannelFailureKind::TooSlow {
                bps: 1024,
                threshold: 32 * 1024,
            },
        };
        let fault = ChannelFailure {
            channel: ChannelType::Mirror,
            url: "http://x/b".into(),
            reason: "HTTP 503".into(),
            kind: ChannelFailureKind::Fault,
        };

        // 1) 空失败列表 → 不触发（没有失败就谈不上熔断）
        assert!(!all_failures_too_slow(&[]));
        assert!(SlowValve::new().plan(&[]).is_none());

        // 2) 全部过慢 → 触发（重试第一个通道）
        assert!(all_failures_too_slow(&[too_slow.clone()]));
        assert!(all_failures_too_slow(&[too_slow.clone(), too_slow.clone()]));
        let mut valve = SlowValve::new();
        assert_eq!(valve.plan(&[too_slow.clone(), too_slow.clone()]), Some(0));
        assert!(valve.used());

        // 3) 安全阀至多触发一次（第二次 plan → None，不循环）
        assert_eq!(valve.plan(&[too_slow.clone()]), None);

        // 4) 混合（过慢 + 真故障）→ 不触发（真故障重试没意义）
        assert!(!all_failures_too_slow(&[too_slow.clone(), fault.clone()]));
        let mut valve2 = SlowValve::new();
        assert_eq!(valve2.plan(&[too_slow.clone(), fault.clone()]), None);
        assert!(!valve2.used(), "未触发就不算已用");

        // 5) 全真故障 → 不触发
        assert!(!all_failures_too_slow(&[fault.clone(), fault.clone()]));
        assert!(SlowValve::new().plan(&[fault.clone()]).is_none());
    }

    /// 只有一条慢通道（用户只有 3G、没有 CN 网关兜底）→ 熔断后走安全阀：
    /// 关闭熔断、不限速地把包下完，而不是直接 NET_ALL_CHANNELS_DEAD。
    #[test]
    fn single_slow_channel_completes_via_slow_valve() {
        let dir = tmp_dir("valve-ok");
        let total = 9 * 1024 * 1024;
        // 第一个 GET 慢速吐（触发熔断）；之后的 GET 全速（模拟通道恢复 / 便于测试收尾）
        let (slow_url, slow_gets) =
            slow_server_with(total, 8 * 1024, Duration::from_secs(1), 1, None);
        let dest = dir.join("big.bin");
        let body = vec![0xA5u8; total];
        let expected = sha256_hex(&body);

        let started = Instant::now();
        let receipt = download_fallback(
            &[ChannelUrl::new(
                ChannelType::Direct,
                format!("{slow_url}/big.bin"),
            )],
            &dest,
            &expected,
            &opts(2 * 1024 * 1024),
            &mut no_progress(),
        )
        .expect("安全阀应把慢通道下完，而不是 AllChannelsDead");
        let elapsed = started.elapsed();

        assert_eq!(receipt.bytes, total as u64);
        assert_eq!(receipt.sha256, expected, "S1 校验仍必须生效");
        assert_eq!(fs::read(&dest).unwrap().len(), total);
        // 1 次熔断尝试 + 安全阀不限速重试（5 段：段0 续传 + 段1..4）= 6 次 GET；不循环
        assert_eq!(
            slow_gets.load(Ordering::SeqCst),
            6,
            "安全阀只触发一次（熔断 1 次 + 不限速重试 5 段）"
        );
        assert!(elapsed < Duration::from_secs(60), "实际 {elapsed:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 混合失败（一个过慢 + 一个真故障 503）→ **不触发**安全阀（保持原语义）。
    #[test]
    fn mixed_failures_do_not_trigger_slow_valve() {
        let dir = tmp_dir("mixed");
        let (slow_url, slow_gets) = slow_server(9 * 1024 * 1024, 8 * 1024, Duration::from_secs(1));
        // 真故障通道：所有 GET 都 503
        let broken = TestHttpServer::serve(deterministic_body(2048, 51));
        broken.inject_fail_status_get(3);
        let dest = dir.join("big.bin");
        let err = download_fallback(
            &[
                ChannelUrl::new(ChannelType::Direct, format!("{slow_url}/big.bin")),
                ChannelUrl::new(ChannelType::Mirror, broken.url("/big.bin")),
            ],
            &dest,
            &"00".repeat(32),
            &opts(2 * 1024 * 1024),
            &mut no_progress(),
        )
        .unwrap_err();

        assert_eq!(err.code(), "NET_ALL_CHANNELS_DEAD");
        assert!(
            err.slow_valve().is_none(),
            "混合失败不得触发安全阀（真故障重试没意义）"
        );
        let report = err.channel_report();
        assert_eq!(report.len(), 2, "每通道一条失败原因: {report:?}");
        assert!(report[0].1.contains("过慢"), "{:?}", report[0]);
        assert!(report[1].1.contains("503"), "{:?}", report[1]);
        assert_eq!(
            slow_gets.load(Ordering::SeqCst),
            1,
            "过慢通道不得被安全阀重试（只有全过慢才兜底）"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 起一个「慢速」HTTP 服务器：响应头声明全长，但每 `chunk` 字节间隔 `delay` 才吐一次。
    /// 返回 (base_url, GET 次数计数器)。
    fn slow_server(total: usize, chunk: usize, delay: Duration) -> (String, Arc<AtomicU32>) {
        slow_server_with(total, chunk, delay, u32::MAX, None)
    }

    /// 可注入故障的慢速服务器：
    ///
    /// - `slow_gets`：前 N 个 GET 慢速吐（`u32::MAX` = 永远慢速）；之后的 GET **全速**返回
    ///   （模拟「通道慢下来又恢复」/ 让安全阀的不限速重试能快速收尾）；
    /// - `drop_from`：从第 N 个 GET（0-based）起直接断连（模拟真故障，
    ///   让「安全阀重试也失败」的场景快速收敛，不让测试干等）。
    ///
    /// 返回 (base_url, GET 次数计数器)。
    fn slow_server_with(
        total: usize,
        chunk: usize,
        delay: Duration,
        slow_gets: u32,
        drop_from: Option<u32>,
    ) -> (String, Arc<AtomicU32>) {
        use std::io::{Read as _, Write as _};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let gets = Arc::new(AtomicU32::new(0));
        let gets2 = Arc::clone(&gets);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let gets = Arc::clone(&gets2);
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
                    let mut req = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        match stream.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                req.extend_from_slice(&buf[..n]);
                                if req.windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    if req.is_empty() {
                        return;
                    }
                    let text = String::from_utf8_lossy(&req);
                    let mut range: Option<String> = None;
                    for line in text.lines() {
                        if let Some((k, v)) = line.split_once(':') {
                            if k.trim().eq_ignore_ascii_case("range") {
                                range = Some(v.trim().to_string());
                            }
                        }
                    }
                    if text.starts_with("HEAD") {
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                        );
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.flush();
                        return;
                    }
                    let get_idx = gets.fetch_add(1, Ordering::SeqCst);
                    if let Some(from) = drop_from {
                        if get_idx >= from {
                            return; // 直接断连（模拟真故障）
                        }
                    }
                    let is_slow = get_idx < slow_gets;
                    let (start, end) = match range
                        .as_deref()
                        .and_then(crate::testutil::parse_range)
                    {
                        Some((s, e)) => (s, e.min(total as u64 - 1)),
                        None => (0, total as u64 - 1),
                    };
                    let slice_len = (end - start + 1) as usize;
                    let head = if range.is_some() {
                        format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {slice_len}\r\nContent-Range: bytes {start}-{end}/{total}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                        )
                    } else {
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {slice_len}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                        )
                    };
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.flush();
                    let body = vec![0xA5u8; slice_len];
                    if !is_slow {
                        // 全速返回（安全阀不限速重试 / 通道恢复）
                        if stream.write_all(&body).is_err() {
                            return;
                        }
                        let _ = stream.flush();
                        return;
                    }
                    let mut sent = 0usize;
                    while sent < slice_len {
                        let n = chunk.min(slice_len - sent);
                        if stream.write_all(&body[sent..sent + n]).is_err() {
                            return; // 客户端已断开（熔断放弃通道）
                        }
                        let _ = stream.flush();
                        sent += n;
                        if sent < slice_len {
                            std::thread::sleep(delay);
                        }
                    }
                });
            }
        });
        (format!("http://{addr}"), gets)
    }
}
