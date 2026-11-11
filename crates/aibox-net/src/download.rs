//! 分段下载器（下载执行 + S1 安全红线）：
//!
//! - 8MB 分段 + 断点续传（`.part` 文件按字节续写，Range 请求从断点起）；
//! - 单段重试 2 次失败 → 整包降级到下一通道（跨源从头下载）；
//! - 全通道失败 → 结构化错误 [`DownloadError::AllChannelsDead`]（`NET_ALL_CHANNELS_DEAD`）；
//! - **产物三分判定**（下载完成后）：HTML/JSON 故障页、大小不符（截断）→ 判**通道故障**
//!   降级下一通道；大小相符但 SHA256 不符 → **真篡改**，立即中止（`NET_HASH_MISMATCH`，
//!   S1 零豁免、不降级不放行）；
//! - 进度回调为普通闭包/`FnMut` trait，**不依赖 Tauri**（供上层 emit `download_progress`）。
//!
//! 网络 IO 全部走 `ureq`（rustls，无系统 C 库）；单测用手写本地 HTTP 服务器离线覆盖。

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::channel::ChannelType;
use crate::proxy::ProxyConfig;

/// 默认分段大小：8MB。
pub const SEGMENT_SIZE: u64 = 8 * 1024 * 1024;
/// 单段重试次数（2 次失败 → 整包降级）。
pub const SEGMENT_RETRIES: u32 = 2;
/// 默认单请求超时（连接 + 读）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

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
}

/// 单通道失败原因（全通道失败时逐条汇总给人话报错）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelFailure {
    pub channel: ChannelType,
    pub url: String,
    pub reason: String,
}

/// 结构化下载错误。
#[derive(Debug)]
pub enum DownloadError {
    /// 未提供任何通道（调用方配置错误）
    NoChannels,
    /// 所有通道失败（上层唤起 Network Doctor / E1）
    AllChannelsDead { failures: Vec<ChannelFailure> },
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
            DownloadError::AllChannelsDead { .. } => {
                "当前网络无法访问任何下载源（见通道失败原因表，可运行网络体检）".to_string()
            }
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
            DownloadError::AllChannelsDead { failures } => failures
                .iter()
                .map(|f| (f.channel.as_str().to_string(), f.reason.clone()))
                .collect(),
            _ => Vec::new(),
        }
    }
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::AllChannelsDead { failures } => {
                write!(f, "{} [", self.code())?;
                for (i, fail) in failures.iter().enumerate() {
                    if i > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{}: {}", fail.channel.as_str(), fail.reason)?;
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
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            segment_size: SEGMENT_SIZE,
            segment_retries: SEGMENT_RETRIES,
            timeout: DEFAULT_TIMEOUT,
            proxy: None,
            expected_size: None,
        }
    }
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

    for ch in channels {
        match download_one(&agent, ch, &part, opts, progress) {
            Ok(done) => {
                // 产物三分判定（S1）：故障页/截断 → 通道故障降级；真篡改 → 中止
                let actual = sha256_file(&part).map_err(|e| DownloadError::Io {
                    context: format!("读取下载产物 {}", part.display()),
                    source: e,
                })?;
                let size = fs::metadata(&part)
                    .map(|m| m.len())
                    .unwrap_or(done.total);
                let head = read_file_head(&part, 512)?;
                let sha_matches = actual.eq_ignore_ascii_case(expected_sha256);
                match classify_product(&head, size, opts.expected_size, sha_matches) {
                    ProductVerdict::HashOk => {
                        if dest.exists() {
                            fs::remove_file(dest).map_err(|e| DownloadError::Io {
                                context: format!("覆盖目标文件 {}", dest.display()),
                                source: e,
                            })?;
                        }
                        fs::rename(&part, dest).map_err(|e| DownloadError::Io {
                            context: format!("落盘 {}", dest.display()),
                            source: e,
                        })?;
                        return Ok(DownloadReceipt {
                            channel: ch.channel,
                            url: ch.url.clone(),
                            bytes: done.total,
                            sha256: actual,
                            resumed_from: done.resumed_from,
                            get_requests: done.get_requests,
                        });
                    }
                    ProductVerdict::ChannelFault(reason) => {
                        // 通道坏（故障页/截断）→ 记该通道失败，降级下一通道
                        let _ = fs::remove_file(&part);
                        failures.push(ChannelFailure {
                            channel: ch.channel,
                            url: ch.url.clone(),
                            reason,
                        });
                        continue;
                    }
                    ProductVerdict::Tamper => {
                        // 真篡改（大小相符但哈希不符）→ 立即中止，不降级不放行
                        let _ = fs::remove_file(&part);
                        return Err(DownloadError::HashMismatch {
                            channel: ch.channel,
                            expected: expected_sha256.to_ascii_lowercase(),
                            actual,
                        });
                    }
                }
            }
            Err(ChannelError::Channel(reason)) => {
                failures.push(ChannelFailure {
                    channel: ch.channel,
                    url: ch.url.clone(),
                    reason,
                });
                let _ = fs::remove_file(&part); // 跨源从头下载
            }
            Err(ChannelError::Fatal(e)) => return Err(e),
        }
    }

    Err(DownloadError::AllChannelsDead { failures })
}

/// 单通道错误：通道级（可降级）或致命（本地 IO / 配置）。
enum ChannelError {
    Channel(String),
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
            });
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
        progress,
    };

    let mut retries_left = opts.segment_retries;
    let mut current_segment: Option<u64> = None;

    while state.written < total {
        let seg_idx = state.written / segment_size;
        if current_segment != Some(seg_idx) {
            current_segment = Some(seg_idx);
            retries_left = opts.segment_retries; // 新段：重置重试预算
        }
        let seg_end = (state.written + segment_size).min(total) - 1;
        state.gets += 1;
        match state.attempt(seg_end).map_err(ChannelError::Fatal)? {
            AttemptResult::SegmentDone => {}
            AttemptResult::Restart => {
                // 已回退 written=0；range_mode=Some(false)，下轮整包单流
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
}
