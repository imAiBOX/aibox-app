//! D6 证书异常统计：
//! - **时钟偏差检查**：对关键域 HTTPS 响应取 `Date` 头与本机时间比对，
//!   偏差 > 5min → warn（人话：你电脑的时间不准，可能导致网站说证书过期）；
//! - **证书错误采集**：TLS 握手错误按 [`classify_tls_error`] 分类
//!   （过期 / 主机名不符 / 未知 CA=疑似 MITM / 自签）；
//! - **运行时统计**：[`record_tls_failure`] 进程内收集下载重试期的 TLS 失败
//!   （core 下载路径后续接线；本次提供 API + 单测），[`run`] 报告中展示快照。

use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::report::{loc, DiagCard, DiagCode, DiagStatus, Fix, FixLevel};
use crate::transport::DoctorDeps;

/// 时钟偏差告警阈值（> 5 分钟）。
pub const CLOCK_SKEW_THRESHOLD_MS: i64 = 5 * 60 * 1000;

/// 运行时 TLS 失败记录上限（超出丢最旧，防进程内无界增长）。
const TLS_FAILURE_CAP: usize = 200;

// ── 纯函数：HTTP `Date` 头解析与时钟偏差 ─────────────────────────────────

/// 解析 IMF-fixdate（RFC 7231，`Wed, 21 Oct 2015 07:28:00 GMT`）→ epoch 毫秒。
///
/// 仅支持服务器实际在用的标准格式；RFC 850 / asctime 等遗留格式返回 `None`
/// （该域跳过时钟核对，不影响其他域）。
pub fn parse_http_date_ms(s: &str) -> Option<i64> {
    let mut parts = s.trim().split_whitespace();
    // 星期几（校验存在即可，不参与计算）
    let weekday = parts.next()?;
    if !weekday.ends_with(',') {
        return None;
    }
    let day: i64 = parts.next()?.parse().ok()?;
    let month = parts.next()?;
    let year: i64 = parts.next()?.parse().ok()?;
    let time = parts.next()?;
    let tz = parts.next()?;
    if !tz.eq_ignore_ascii_case("GMT") {
        return None;
    }
    if parts.next().is_some() {
        return None; // 尾部多余 token
    }
    let month_idx = match month {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let (h, m, sec) = {
        let mut it = time.split(':');
        let h: i64 = it.next()?.parse().ok()?;
        let m: i64 = it.next()?.parse().ok()?;
        let sec: i64 = it.next()?.parse().ok()?;
        if it.next().is_some() {
            return None;
        }
        (h, m, sec)
    };
    if !(1..=31).contains(&day) || !(0..=23).contains(&h) || !(0..=59).contains(&m) || !(0..=60).contains(&sec) {
        return None;
    }
    let days = days_from_civil(year, month_idx, day);
    Some((days * 86_400 + h * 3_600 + m * 60 + sec) * 1_000)
}

/// Civil date → epoch 天数（Howard Hinnant 算法，纯整数无依赖）。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 时钟偏差（ms）：本机时间 − 服务器 `Date` 时间。**正 = 本机超前**。
pub fn clock_skew_ms(local_ms: i64, date_ms: i64) -> i64 {
    local_ms - date_ms
}

/// 偏差绝对值是否超过阈值（严格大于）。
pub fn skew_exceeds(skew_ms: i64, threshold_ms: i64) -> bool {
    skew_ms.unsigned_abs() > threshold_ms as u64
}

// ── 纯函数：TLS 错误分类 ────────────────────────────────────────────────

/// TLS / 证书错误分类（D6 统计口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsErrorKind {
    /// 证书过期（notAfter 已过；也可能是本机时钟超前）
    Expired,
    /// 证书尚未生效（notBefore 未到；典型原因：本机时钟落后）
    NotValidYet,
    /// 主机名不符（SNI 与证书 SAN 不匹配 → 可能被换证书）
    HostnameMismatch,
    /// 未知 CA（证书链无法追到可信根 → **疑似中间人/拦截**）
    UnknownCa,
    /// 自签证书（**疑似中间人/拦截**或测试设备）
    SelfSigned,
    /// 其他（连接超时、重置、协议错误等）
    Other,
}

impl TlsErrorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TlsErrorKind::Expired => "expired",
            TlsErrorKind::NotValidYet => "not_valid_yet",
            TlsErrorKind::HostnameMismatch => "hostname_mismatch",
            TlsErrorKind::UnknownCa => "unknown_ca",
            TlsErrorKind::SelfSigned => "self_signed",
            TlsErrorKind::Other => "other",
        }
    }

    /// 是否为疑似中间人特征（UnknownCa / SelfSigned）。
    pub fn is_mitm_suspect(&self) -> bool {
        matches!(self, TlsErrorKind::UnknownCa | TlsErrorKind::SelfSigned)
    }

    pub fn label(&self) -> crate::report::Localized {
        match self {
            TlsErrorKind::Expired => loc("证书过期", "certificate expired"),
            TlsErrorKind::NotValidYet => loc("证书尚未生效（本机时钟可能落后）", "certificate not yet valid (local clock may be behind)"),
            TlsErrorKind::HostnameMismatch => loc("主机名不符（证书与域名不匹配）", "hostname mismatch (certificate does not cover domain)"),
            TlsErrorKind::UnknownCa => loc("未知 CA（疑似中间人/拦截）", "unknown CA (possible MITM/interception)"),
            TlsErrorKind::SelfSigned => loc("自签证书（疑似中间人）", "self-signed certificate (possible MITM)"),
            TlsErrorKind::Other => loc("其他 TLS 错误", "other TLS error"),
        }
    }
}

/// 按错误字符串分类（纯函数；覆盖 rustls 0.23 与常见 OpenSSL 原文，大小写不敏感）。
pub fn classify_tls_error(msg: &str) -> TlsErrorKind {
    let m = msg.to_ascii_lowercase();
    // 顺序敏感：先具体后宽泛（自签常同时含 issuer 相关词）
    if m.contains("not valid yet") || m.contains("not yet valid") || m.contains("notvalidyetyet") {
        TlsErrorKind::NotValidYet
    } else if m.contains("expired") {
        TlsErrorKind::Expired
    } else if m.contains("not valid for name")
        || m.contains("notvalidforname")
        || m.contains("hostname")
        || m.contains("host name")
        || m.contains("does not match")
        || m.contains("doesn't match")
        || m.contains("no subject alternative")
    {
        TlsErrorKind::HostnameMismatch
    } else if m.contains("self signed") || m.contains("self-signed") || m.contains("selfsigned") {
        TlsErrorKind::SelfSigned
    } else if m.contains("unknownissuer")
        || m.contains("unknown issuer")
        || m.contains("unknown ca")
        || m.contains("unable to get local issuer")
        || m.contains("unable to verify")
        || m.contains("untrusted")
    {
        TlsErrorKind::UnknownCa
    } else {
        TlsErrorKind::Other
    }
}

// ── 运行时统计：record_tls_failure（进程内收集，供下载重试期上报）────────

/// 一条运行时 TLS 失败记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsFailure {
    pub domain: String,
    pub error: String,
    pub kind: TlsErrorKind,
    /// 记录时刻（Unix epoch ms，进程系统时钟）。
    pub at_ms: u64,
}

fn failure_log() -> &'static Mutex<Vec<TlsFailure>> {
    static LOG: OnceLock<Mutex<Vec<TlsFailure>>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(Vec::new()))
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 记录一次 TLS 失败（core 下载重试路径后续接线调用；分类自动完成）。
///
/// 进程内收集、上限 [`TLS_FAILURE_CAP`] 条（超出丢最旧）；只读快照经
/// [`tls_failures`] 获取，[`clear_tls_failures`] 清空。
pub fn record_tls_failure(domain: &str, error_msg: &str) {
    let mut log = failure_log().lock().unwrap_or_else(|e| e.into_inner());
    log.push(TlsFailure {
        domain: domain.to_string(),
        error: error_msg.to_string(),
        kind: classify_tls_error(error_msg),
        at_ms: now_epoch_ms(),
    });
    if log.len() > TLS_FAILURE_CAP {
        let overflow = log.len() - TLS_FAILURE_CAP;
        log.drain(..overflow);
    }
}

/// 运行时 TLS 失败快照（按记录顺序）。
pub fn tls_failures() -> Vec<TlsFailure> {
    failure_log()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// 清空运行时 TLS 失败记录（测试 / 上报后重置）。
pub fn clear_tls_failures() {
    failure_log()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

// ── D6 执行 ─────────────────────────────────────────────────────────────

/// 单域 D6 结果。
#[derive(Debug, Clone)]
pub struct DomainTls {
    pub domain: String,
    pub ok: bool,
    pub status: Option<u16>,
    pub error: Option<String>,
    /// 错误分类（仅失败时 Some）。
    pub kind: Option<TlsErrorKind>,
    pub date: Option<String>,
    pub date_ms: Option<i64>,
    /// 本机 − 服务器（正=本机超前；无 Date 为 None）。
    pub skew_ms: Option<i64>,
}

/// 对单域执行 D6 检查（经注入 `TlsProbe`，系统解析路径）。
pub fn probe_domain(domain: &str, deps: &DoctorDeps) -> DomainTls {
    let attempt = deps.tls.head_tls(domain, None, deps.timeout);
    let date_ms = attempt.date.as_deref().and_then(parse_http_date_ms);
    let local = (deps.now_ms)() as i64;
    let skew_ms = date_ms.map(|d| clock_skew_ms(local, d));
    let kind = if attempt.success {
        None
    } else {
        Some(classify_tls_error(
            attempt.error.as_deref().unwrap_or("unknown tls failure"),
        ))
    };
    DomainTls {
        domain: domain.to_string(),
        ok: attempt.success,
        status: attempt.status,
        error: attempt.error,
        kind,
        date: attempt.date,
        date_ms,
        skew_ms,
    }
}

fn empty_domain(domain: &str, error: &str) -> DomainTls {
    DomainTls {
        domain: domain.to_string(),
        ok: false,
        status: None,
        error: Some(error.to_string()),
        kind: Some(TlsErrorKind::Other),
        date: None,
        date_ms: None,
        skew_ms: None,
    }
}

/// 执行 D6（对给定域列表；并发逐域）。`domains` 为空 → `no_domains` warn 卡
/// （仍展示运行时失败快照）。
pub fn run_with_domains<S: AsRef<str>>(domains: &[S], deps: &DoctorDeps) -> DiagCard {
    let results: Vec<DomainTls> = if domains.is_empty() {
        Vec::new()
    } else {
        thread::scope(|s| {
            let handles: Vec<_> = domains
                .iter()
                .map(|d| {
                    let domain = d.as_ref().to_string();
                    let handle = {
                        let domain = domain.clone();
                        s.spawn(move || probe_domain(&domain, deps))
                    };
                    (domain, handle)
                })
                .collect();
            handles
                .into_iter()
                .map(|(domain, handle)| {
                    handle
                        .join()
                        .unwrap_or_else(|_| empty_domain(&domain, "probe thread panicked"))
                })
                .collect()
        })
    };

    aggregate(results, deps)
}

/// 执行 D6（默认关键域 = D2 域列表）。
pub fn run(deps: &DoctorDeps) -> DiagCard {
    run_with_domains(&crate::d2::D2_DOMAINS, deps)
}

fn aggregate(results: Vec<DomainTls>, deps: &DoctorDeps) -> DiagCard {
    // 时钟偏差：取各域 skew 中位数（抗单个异常服务器）
    let mut skews: Vec<i64> = results.iter().filter_map(|r| r.skew_ms).collect();
    skews.sort_unstable();
    let median_skew = if skews.is_empty() {
        None
    } else {
        Some(skews[skews.len() / 2])
    };
    let clock_exceeded = median_skew
        .map(|s| skew_exceeds(s, CLOCK_SKEW_THRESHOLD_MS))
        .unwrap_or(false);

    // 证书错误（实时探测）
    let failed: Vec<&DomainTls> = results.iter().filter(|r| !r.ok).collect();
    let mitm_domains: Vec<&str> = failed
        .iter()
        .filter(|r| r.kind.map(|k| k.is_mitm_suspect()).unwrap_or(false))
        .map(|r| r.domain.as_str())
        .collect();

    // 运行时统计快照（下载重试期 record_tls_failure 的上报；只入明细不改状态——
    // 实时复测才是当前状态的权威来源）
    let runtime = tls_failures();
    let runtime_count = runtime.len();

    let status = if !mitm_domains.is_empty() {
        DiagStatus::Fail
    } else if clock_exceeded || !failed.is_empty() {
        DiagStatus::Warn
    } else if results.is_empty() {
        DiagStatus::Warn
    } else {
        DiagStatus::Ok
    };

    let summary_key;
    let summary;
    if !mitm_domains.is_empty() {
        summary_key = "netdoctor.D6.mitm";
        summary = loc(
            format!(
                "证书链异常（疑似中间人/拦截）：{}",
                mitm_domains.join(", ")
            ),
            format!(
                "Certificate chain anomaly (possible MITM/interception): {}",
                mitm_domains.join(", ")
            ),
        );
    } else if results.is_empty() {
        summary_key = "netdoctor.D6.no_domains";
        summary = loc(
            "未提供检测域（由调用方传入，或走默认关键域）",
            "No domains supplied (caller-provided or default key domains)",
        );
    } else if clock_exceeded {
        let minutes = median_skew.map(|s| s.unsigned_abs() / 60_000).unwrap_or(0);
        summary_key = "netdoctor.D6.clock_skew";
        summary = loc(
            format!(
                "你电脑的时间不准（与网络时间偏差约 {minutes} 分钟），可能导致网站说证书过期"
            ),
            format!(
                "Your computer's clock is off by about {minutes} minutes from network time; websites may report certificates as expired"
            ),
        );
    } else if !failed.is_empty() {
        summary_key = "netdoctor.D6.cert_errors";
        summary = loc(
            format!("{} 个域 TLS 证书校验失败（详见明细）", failed.len()),
            format!(
                "{} domain(s) failed TLS certificate validation (see detail)",
                failed.len()
            ),
        );
    } else {
        summary_key = "netdoctor.D6.ok";
        summary = loc(
            "证书与时钟正常：关键域 TLS 握手与时间均无异常",
            "Certificates and clock healthy: TLS handshakes and time checks passed on all key domains",
        );
    }

    let mut fixes = Vec::new();
    if !failed.is_empty() {
        fixes.push(Fix::new(
            FixLevel::R1,
            "netdoctor.fix.channel_fallback",
            loc(
                "换通道重试：进程内自动降级到其他可用通道（自动）",
                "Retry via another channel: automatic in-process fallback (automatic)",
            ),
            "若 UnknownCa/SelfSigned 持续出现，请检查本机安全软件 TLS 扫描或抓包工具\
             （可能为中间人）；aibox 永不放宽证书校验（S 系红线）",
        ));
    }
    if clock_exceeded {
        fixes.push(Fix::new(
            FixLevel::R3,
            "netdoctor.fix.sync_clock",
            loc(
                "校准系统时间：开启系统自动对时（Windows：设置→时间和语言→自动设置时间）",
                "Sync the system clock: enable automatic time (Windows: Settings → Time & Language → Set time automatically)",
            ),
            "指引性修复：修改系统时间属系统级变更（R3），aibox 不自动执行；\
             校准后重新体检即可消除证书过期误报",
        ));
    }

    let clock_json = json!({
        "checked": results.len(),
        "parsed": skews.len(),
        "median_skew_ms": median_skew,
        "threshold_ms": CLOCK_SKEW_THRESHOLD_MS,
        "exceeded": clock_exceeded,
    });

    DiagCard {
        item: DiagCode::D6,
        status,
        title: DiagCode::D6.title(),
        summary_key: summary_key.to_string(),
        summary,
        detail: json!({
            "domains": results.iter().map(|r| json!({
                "domain": r.domain,
                "ok": r.ok,
                "status": r.status,
                "error": r.error,
                "tls_kind": r.kind.map(|k| k.as_str()),
                "date": r.date,
                "date_ms": r.date_ms,
                "skew_ms": r.skew_ms,
            })).collect::<Vec<_>>(),
            "clock": clock_json,
            "runtime_failure_count": runtime_count,
            "runtime_failures": runtime.iter().take(50).map(|f| json!({
                "domain": f.domain,
                "error": f.error,
                "kind": f.kind.as_str(),
                "at_ms": f.at_ms,
            })).collect::<Vec<_>>(),
            "note": "runtime_failures = 下载重试期 record_tls_failure 上报快照（不参与本卡状态判定，实时复测为准）",
        }),
        timestamp_ms: (deps.now_ms)(),
        fixes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::DiagStatus;
    use crate::transport::{DoctorDeps, TlsAttempt, TlsProbe};
    use std::time::Duration;

    // ── Date 解析 ────────────────────────────────────────────────────────

    #[test]
    fn parse_http_date_cases() {
        // 已知锚点：2015-10-21T07:28:00Z = 1445412480 s
        assert_eq!(
            parse_http_date_ms("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(1_445_412_480_000)
        );
        // 纪元
        assert_eq!(parse_http_date_ms("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        // 闰日（2020-02-29 12:00 UTC）
        assert_eq!(
            parse_http_date_ms("Sat, 29 Feb 2020 12:00:00 GMT"),
            Some(1_582_977_600_000)
        );
        // 非法输入 → None
        assert_eq!(parse_http_date_ms(""), None);
        assert_eq!(parse_http_date_ms("garbage"), None);
        assert_eq!(parse_http_date_ms("Wed, 21 Oct 2015 07:28:00 PST"), None);
        assert_eq!(parse_http_date_ms("21 Oct 2015 07:28:00 GMT"), None); // 缺星期
        assert_eq!(parse_http_date_ms("Wed, 21 Xxx 2015 07:28:00 GMT"), None); // 未知月
        assert_eq!(parse_http_date_ms("Wed, 21 Oct 2015 25:28:00 GMT"), None); // 非法时
        assert_eq!(parse_http_date_ms("Wed, 21 Oct 2015 07:28:00 GMT extra"), None);
        // RFC 850 遗留格式不支持（返回 None，跳过该域核对）
        assert_eq!(parse_http_date_ms("Sunday, 06-Nov-94 08:49:37 GMT"), None);
    }

    #[test]
    fn clock_skew_pure_functions() {
        // 正 = 本机超前
        assert_eq!(clock_skew_ms(1_000_000, 940_000), 60_000);
        assert_eq!(clock_skew_ms(940_000, 1_000_000), -60_000);
        // 严格大于 5 分钟
        assert!(!skew_exceeds(300_000, CLOCK_SKEW_THRESHOLD_MS));
        assert!(skew_exceeds(300_001, CLOCK_SKEW_THRESHOLD_MS));
        assert!(!skew_exceeds(-300_000, CLOCK_SKEW_THRESHOLD_MS));
        assert!(skew_exceeds(-300_001, CLOCK_SKEW_THRESHOLD_MS));
        assert!(!skew_exceeds(0, CLOCK_SKEW_THRESHOLD_MS));
    }

    // ── TLS 错误分类 ─────────────────────────────────────────────────────

    #[test]
    fn classify_tls_error_matrix() {
        // rustls 0.23 原文（io::Error 透传）
        assert_eq!(
            classify_tls_error("TLS 读取失败: invalid peer certificate: Expired"),
            TlsErrorKind::Expired
        );
        assert_eq!(
            classify_tls_error("invalid peer certificate: certificate not valid yet: verification time 1 (UNIX), but certificate is not valid before 2 (1 seconds in future)"),
            TlsErrorKind::NotValidYet
        );
        assert_eq!(
            classify_tls_error("invalid peer certificate: certificate not valid for name \"github.com\"; certificate is only valid for example.com"),
            TlsErrorKind::HostnameMismatch
        );
        assert_eq!(
            classify_tls_error("TLS 读取失败: invalid peer certificate: UnknownIssuer"),
            TlsErrorKind::UnknownCa
        );
        // OpenSSL 常见原文
        assert_eq!(
            classify_tls_error("self signed certificate in certificate chain"),
            TlsErrorKind::SelfSigned
        );
        assert_eq!(
            classify_tls_error("Hostname github.com does not match certificate hostname"),
            TlsErrorKind::HostnameMismatch
        );
        assert_eq!(
            classify_tls_error("unable to get local issuer certificate"),
            TlsErrorKind::UnknownCa
        );
        assert_eq!(
            classify_tls_error("certificate has expired"),
            TlsErrorKind::Expired
        );
        // 非证书错误 → Other
        assert_eq!(classify_tls_error("connection timed out"), TlsErrorKind::Other);
        assert_eq!(
            classify_tls_error("TCP 连接失败 1.2.3.4:443: 拒绝连接"),
            TlsErrorKind::Other
        );
        // MITM 特征判定
        assert!(TlsErrorKind::UnknownCa.is_mitm_suspect());
        assert!(TlsErrorKind::SelfSigned.is_mitm_suspect());
        assert!(!TlsErrorKind::Expired.is_mitm_suspect());
        assert!(!TlsErrorKind::Other.is_mitm_suspect());
        // serde 形状（TS 边界）
        assert_eq!(
            serde_json::to_value(TlsErrorKind::UnknownCa).unwrap(),
            "unknown_ca"
        );
    }

    // ── 运行时统计 API（唯一触碰全局日志的测试，避免并行竞争断言）──────────

    #[test]
    fn record_tls_failure_api() {
        clear_tls_failures();
        assert!(tls_failures().is_empty());

        record_tls_failure("github.com", "invalid peer certificate: UnknownIssuer");
        record_tls_failure("registry.npmjs.org", "connection timed out");
        record_tls_failure("cdn.jsdelivr.net", "self signed certificate in certificate chain");

        let log = tls_failures();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0].domain, "github.com");
        assert_eq!(log[0].kind, TlsErrorKind::UnknownCa);
        assert_eq!(log[1].kind, TlsErrorKind::Other);
        assert_eq!(log[2].kind, TlsErrorKind::SelfSigned);
        assert!(log[0].at_ms > 0);
        // 序列化到 TS
        let v = serde_json::to_value(&log[0]).unwrap();
        assert_eq!(v["kind"], "unknown_ca");
        assert!(v["domain"].is_string());

        clear_tls_failures();
        assert!(tls_failures().is_empty());

        // 上限：超出丢最旧
        for i in 0..(TLS_FAILURE_CAP + 5) {
            record_tls_failure(&format!("d{i}.example"), "timeout");
        }
        let log = tls_failures();
        assert_eq!(log.len(), TLS_FAILURE_CAP);
        assert_eq!(log[0].domain, "d5.example"); // 最旧的 5 条被丢弃
        clear_tls_failures();
    }

    // ── 注入 transport 的完整 run 流程 ────────────────────────────────────

    struct FakeTls {
        date: Option<&'static str>,
        error: Option<&'static str>,
    }
    impl TlsProbe for FakeTls {
        fn head_tls(&self, _host: &str, _ip: Option<std::net::IpAddr>, _t: Duration) -> TlsAttempt {
            match (&self.date, &self.error) {
                (_, Some(e)) => TlsAttempt::fail(*e),
                (Some(d), None) => TlsAttempt::ok(200, Some(d.to_string())),
                (None, None) => TlsAttempt::ok(200, None),
            }
        }
    }

    /// 固定 now_ms = 已知 Date + 10 分钟（构造 +10min 超前偏差）。
    fn deps_with_tls(fake: FakeTls, now_ms: u64) -> DoctorDeps {
        DoctorDeps {
            tls: Box::new(fake),
            now_ms: Box::new(move || now_ms),
            ..DoctorDeps::default()
        }
    }

    const BASE_DATE: &str = "Wed, 21 Oct 2015 07:28:00 GMT"; // 1445412480000 ms

    #[test]
    fn d6_run_clock_skew_warn() {
        // 本机时间 = Date + 10min → 偏差 10min > 5min → warn + 人话 summary
        let local = 1_445_412_480_000 + 10 * 60 * 1_000;
        let deps = deps_with_tls(
            FakeTls { date: Some(BASE_DATE), error: None },
            local,
        );
        let card = run(&deps);
        assert_eq!(card.item, DiagCode::D6);
        assert_eq!(card.status, DiagStatus::Warn);
        assert_eq!(card.summary_key, "netdoctor.D6.clock_skew");
        assert!(card.summary.zh.contains("时间不准"));
        assert!(card.summary.zh.contains("10 分钟"));
        assert_eq!(card.detail["clock"]["exceeded"], true);
        assert_eq!(card.detail["clock"]["median_skew_ms"], 600_000);
        // R3 校时建议
        assert_eq!(card.fixes.len(), 1);
        assert_eq!(card.fixes[0].level, FixLevel::R3);
        assert!(!card.fixes[0].level.is_auto());
        assert_eq!(card.detail["domains"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn d6_run_ok_when_time_matches() {
        let deps = deps_with_tls(
            FakeTls { date: Some(BASE_DATE), error: None },
            1_445_412_480_000, // 与 Date 完全一致 → skew 0
        );
        let card = run_with_domains(&["github.com"], &deps);
        assert_eq!(card.status, DiagStatus::Ok);
        assert_eq!(card.summary_key, "netdoctor.D6.ok");
        assert_eq!(card.detail["clock"]["exceeded"], false);
        assert!(card.fixes.is_empty());
        assert_eq!(card.detail["domains"][0]["skew_ms"], 0);
    }

    #[test]
    fn d6_run_mitm_cert_error_fail() {
        let deps = deps_with_tls(
            FakeTls { date: None, error: Some("TLS 读取失败: invalid peer certificate: UnknownIssuer") },
            1_000,
        );
        let card = run(&deps);
        assert_eq!(card.status, DiagStatus::Fail);
        assert_eq!(card.summary_key, "netdoctor.D6.mitm");
        assert!(card.summary.zh.contains("中间人"));
        assert_eq!(card.detail["domains"][0]["tls_kind"], "unknown_ca");
        // R1 换通道（auto）
        assert_eq!(card.fixes.len(), 1);
        assert!(card.fixes[0].level.is_auto());
    }

    #[test]
    fn d6_run_expired_is_warn_not_fail() {
        let deps = deps_with_tls(
            FakeTls { date: None, error: Some("invalid peer certificate: Expired") },
            1_000,
        );
        let card = run_with_domains(&["github.com"], &deps);
        assert_eq!(card.status, DiagStatus::Warn);
        assert_eq!(card.summary_key, "netdoctor.D6.cert_errors");
        assert_eq!(card.detail["domains"][0]["tls_kind"], "expired");
        assert_eq!(card.fixes.len(), 1);
        assert_eq!(card.fixes[0].level, FixLevel::R1);
    }

    #[test]
    fn d6_run_no_date_is_ok_with_note() {
        // 无 Date 头（TLS 成功）→ 时钟无法核对但不算异常
        let deps = deps_with_tls(FakeTls { date: None, error: None }, 1_000);
        let card = run_with_domains(&["github.com"], &deps);
        assert_eq!(card.status, DiagStatus::Ok);
        assert_eq!(card.detail["clock"]["parsed"], 0);
        assert_eq!(card.detail["clock"]["exceeded"], false);
    }

    #[test]
    fn d6_empty_domains_is_warn() {
        let deps = DoctorDeps::default();
        let card = run_with_domains(&[] as &[&str], &deps);
        assert_eq!(card.status, DiagStatus::Warn);
        assert_eq!(card.summary_key, "netdoctor.D6.no_domains");
    }

    /// 卡片序列化到 TS 边界（形状与 D0-D4 一致）。
    #[test]
    fn d6_card_serializes() {
        let deps = deps_with_tls(
            FakeTls { date: Some(BASE_DATE), error: None },
            1_445_412_480_000,
        );
        let card = run_with_domains(&["github.com"], &deps);
        let v = serde_json::to_value(&card).unwrap();
        assert_eq!(v["item"], "D6");
        assert_eq!(v["status"], "ok");
        assert!(v["summary"]["zh"].is_string());
        assert!(v["summary"]["en"].is_string());
        assert!(v["detail"]["clock"]["threshold_ms"].is_number());
        assert!(v["detail"]["runtime_failures"].is_array());
    }
}
