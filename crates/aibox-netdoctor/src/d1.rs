//! D1 系统代理审计：
//! 逐项列出 env / WinINET / WinHTTP 代理设置；死代理判定 = 设置存在 且
//! （端口无响应 或 CONNECT 转发失败）。**只读，绝不修改任何设置。**

use serde_json::json;
use std::time::Duration;

use crate::report::{loc, DiagCard, DiagCode, DiagStatus, Fix, FixLevel};
use crate::transport::DoctorDeps;

/// 设置来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    /// HTTP(S)_PROXY / ALL_PROXY 等环境变量
    EnvVar,
    /// WinINET（HKCU Internet Settings，免提权读取）
    WinInet,
    /// WinHTTP（netsh winhttp show proxy）
    WinHttp,
}

impl ProxyKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProxyKind::EnvVar => "env",
            ProxyKind::WinInet => "wininet",
            ProxyKind::WinHttp => "winhttp",
        }
    }
}

/// 一条代理设置（位置 + 端口，报告要求输出「位置与端口」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxySetting {
    pub kind: ProxyKind,
    /// 位置描述：环境变量名 / 注册表路径 / netsh 来源。
    pub location: String,
    /// 原始值（报告展示；不含凭据——若有 userinfo 会被剥离）。
    pub raw: String,
    pub host: String,
    pub port: u16,
}

impl ProxySetting {
    pub fn new(
        kind: ProxyKind,
        location: impl Into<String>,
        raw: impl Into<String>,
        host: impl Into<String>,
        port: u16,
    ) -> Self {
        ProxySetting {
            kind,
            location: location.into(),
            raw: raw.into(),
            host: host.into(),
            port,
        }
    }

    pub fn env(key: &str, raw: &str, host: &str, port: u16) -> Self {
        Self::new(ProxyKind::EnvVar, format!("{key} 环境变量"), raw, host, port)
    }
}

/// 代理活性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyHealth {
    /// 端口通且 CONNECT 2xx
    Alive,
    /// 端口无响应或 CONNECT 失败（死代理）
    Dead,
    /// 端口通但未做/无法做 CONNECT（证据不足）
    Unknown,
}

/// 死代理判定（纯函数）：设置存在 且（端口无响应 或 CONNECT 失败）。
pub fn classify_proxy_health(port_open: bool, connect_ok: Option<bool>) -> ProxyHealth {
    if !port_open {
        return ProxyHealth::Dead;
    }
    match connect_ok {
        Some(true) => ProxyHealth::Alive,
        Some(false) => ProxyHealth::Dead,
        None => ProxyHealth::Unknown,
    }
}

/// 解析代理端点（纯函数）。支持：
/// `http://127.0.0.1:7890`、`127.0.0.1:1080`、`socks5://user:pass@host:1080`、
/// `[::1]:8080`、带路径 `http://host:8080/path`。缺端口按 scheme 默认
/// （http→80 / https→443 / socks→1080 / 无 scheme→80）。
pub fn parse_proxy_endpoint(raw: &str) -> Option<(String, u16)> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let (scheme, rest) = if let Some(r) = s.strip_prefix("http://") {
        ("http", r)
    } else if let Some(r) = s.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = s.strip_prefix("socks5h://") {
        ("socks5", r)
    } else if let Some(r) = s.strip_prefix("socks5://") {
        ("socks5", r)
    } else if let Some(r) = s.strip_prefix("socks4://") {
        ("socks4", r)
    } else if let Some(r) = s.strip_prefix("socks://") {
        ("socks", r)
    } else {
        ("http", s)
    };
    // 去路径/查询、去 userinfo
    let rest = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let rest = rest.rsplit_once('@').map(|(_, h)| h).unwrap_or(rest);
    let default_port: u16 = match scheme {
        "https" => 443,
        "socks" | "socks4" | "socks5" => 1080,
        _ => 80,
    };
    // IPv6 字面量 [::1]:8080
    if let Some(rest) = rest.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = if let Some(p) = tail.strip_prefix(':') {
            p.parse::<u16>().ok()?
        } else {
            default_port
        };
        return Some((host.to_string(), port));
    }
    match rest.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() => {
            let port: u16 = port.parse().ok()?;
            Some((host.to_string(), port))
        }
        Some((host, _)) if !host.is_empty() => Some((host.to_string(), default_port)),
        _ => Some((rest.to_string(), default_port)),
    }
}

/// 解析 WinINET `ProxyServer`（纯函数）。支持
/// `http=h:1;https=h:2;socks=h:3` 复合形式与 `h:port` 裸形式。
/// 返回 `(scheme, host, port)` 列表。
pub fn parse_wininet_proxy_server(raw: &str) -> Vec<(String, String, u16)> {
    let mut out = Vec::new();
    for part in raw.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (scheme, value) = match part.split_once('=') {
            Some((k, v)) if !v.trim().is_empty() => (k.trim().to_string(), v.trim()),
            _ => ("http".to_string(), part),
        };
        if let Some((host, port)) = parse_proxy_endpoint(value) {
            out.push((scheme, host, port));
        }
    }
    out
}

/// D1 配置（探测目标与超时可注入；CONNECT 探测目标默认百度——国内环境恒可达，
/// 避免用 github 造成「代理活着但出不去」的误判）。
#[derive(Debug, Clone)]
pub struct D1Config {
    pub tcp_timeout: Duration,
    pub connect_target_host: String,
    pub connect_target_port: u16,
}

impl Default for D1Config {
    fn default() -> Self {
        D1Config {
            tcp_timeout: Duration::from_secs(3),
            connect_target_host: "www.baidu.com".to_string(),
            connect_target_port: 443,
        }
    }
}

/// 单条审计结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyVerdict {
    pub setting: ProxySetting,
    pub health: ProxyHealth,
    pub port_open: bool,
    pub connect_ok: Option<bool>,
    pub detail: String,
}

/// 执行 D1（依赖注入：代理设置来源 + TCP 探测）。
pub fn run_with(deps: &DoctorDeps, cfg: &D1Config) -> DiagCard {
    let settings = deps.proxy_source.settings();
    let no_proxy = deps.proxy_source.no_proxy();
    let notes = deps.proxy_source.notes();

    let mut verdicts = Vec::with_capacity(settings.len());
    for setting in settings {
        let port_open = deps
            .tcp
            .port_open(&setting.host, setting.port, cfg.tcp_timeout);
        let connect_ok = if port_open {
            Some(
                deps.tcp
                    .http_connect(
                        &setting.host,
                        setting.port,
                        &cfg.connect_target_host,
                        cfg.connect_target_port,
                        cfg.tcp_timeout,
                    )
                    .is_ok(),
            )
        } else {
            None
        };
        let health = classify_proxy_health(port_open, connect_ok);
        let detail = match health {
            ProxyHealth::Alive => format!(
                "端口 {}:{} 通，CONNECT 转发成功（活代理）",
                setting.host, setting.port
            ),
            ProxyHealth::Dead if !port_open => format!(
                "位置 {}（{}:{}）端口无响应 → 死代理",
                setting.location, setting.host, setting.port
            ),
            ProxyHealth::Dead => format!(
                "位置 {}（{}:{}）端口通但 CONNECT 转发失败 → 死代理",
                setting.location, setting.host, setting.port
            ),
            ProxyHealth::Unknown => format!(
                "位置 {}（{}:{}）端口通，未做 CONNECT（证据不足）",
                setting.location, setting.host, setting.port
            ),
        };
        verdicts.push(ProxyVerdict {
            setting,
            health,
            port_open,
            connect_ok,
            detail,
        });
    }

    let dead: Vec<&ProxyVerdict> = verdicts
        .iter()
        .filter(|v| v.health == ProxyHealth::Dead)
        .collect();
    let alive = verdicts
        .iter()
        .filter(|v| v.health == ProxyHealth::Alive)
        .count();

    let (status, summary) = if !dead.is_empty() {
        (
            DiagStatus::Fail,
            loc(
                format!(
                    "发现 {} 个死代理（设置存在但端口/转发不通），本进程将忽略并直连",
                    dead.len()
                ),
                format!(
                    "{} dead proxy setting(s) found (configured but unreachable); aibox will ignore them and connect direct",
                    dead.len()
                ),
            ),
        )
    } else if alive > 0 {
        (
            DiagStatus::Ok,
            loc(
                format!("检测到 {alive} 个可用代理设置"),
                format!("{alive} working proxy setting(s) detected"),
            ),
        )
    } else {
        (
            DiagStatus::Ok,
            loc("未检测到代理设置（直连）", "No proxy settings detected (direct)"),
        )
    };

    let mut fixes = Vec::new();
    if !dead.is_empty() {
        // R1：进程内自愈（自动）
        fixes.push(Fix::new(
            FixLevel::R1,
            "netdoctor.fix.ignore_dead_proxy",
            loc(
                "忽略死代理，本进程直连（自动，零系统改动）",
                "Ignore dead proxies and connect direct (automatic, no system change)",
            ),
            "仅影响 aibox 进程内探测与下载；不修改任何系统设置",
        ));
        // R2：用户确认后清理（可还原）
        fixes.push(Fix::new(
            FixLevel::R2,
            "netdoctor.fix.clear_dead_proxy",
            loc(
                "清除用户级代理环境变量 / 关闭 WinINET 代理开关（需确认，先快照可还原）",
                "Clear user-level proxy env vars / turn off WinINET proxy (confirm first, snapshot taken)",
            ),
            "环境变量原值导出快照；WinINET 写 HKCU（免提权）；报告记录修改历史",
        ));
    }

    DiagCard {
        item: DiagCode::D1,
        status,
        title: DiagCode::D1.title(),
        summary_key: if dead.is_empty() {
            "netdoctor.D1.clean".to_string()
        } else {
            "netdoctor.D1.dead_proxy".to_string()
        },
        summary,
        detail: json!({
            "settings": verdicts.iter().map(|v| json!({
                "kind": v.setting.kind.as_str(),
                "location": v.setting.location,
                "host": v.setting.host,
                "port": v.setting.port,
                "health": match v.health {
                    ProxyHealth::Alive => "alive",
                    ProxyHealth::Dead => "dead",
                    ProxyHealth::Unknown => "unknown",
                },
                "port_open": v.port_open,
                "connect_ok": v.connect_ok,
                "detail": v.detail,
            })).collect::<Vec<_>>(),
            "no_proxy": no_proxy,
            "notes": notes,
            "connect_probe_target": format!("{}:{}", cfg.connect_target_host, cfg.connect_target_port),
        }),
        timestamp_ms: (deps.now_ms)(),
        fixes,
    }
}

/// 默认配置执行 D1。
pub fn run(deps: &DoctorDeps) -> DiagCard {
    run_with(deps, &D1Config::default())
}

// ---------------------------------------------------------------------------
// 默认代理来源已在 transport::DefaultProxySource 实现（读 env + cfg(windows) 注册表）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::DiagStatus;
    use crate::transport::{ProxySource, TcpProbe};
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[test]
    fn parse_endpoint_variants() {
        assert_eq!(
            parse_proxy_endpoint("http://127.0.0.1:7890"),
            Some(("127.0.0.1".into(), 7890))
        );
        assert_eq!(
            parse_proxy_endpoint("127.0.0.1:1080"),
            Some(("127.0.0.1".into(), 1080))
        );
        assert_eq!(
            parse_proxy_endpoint("socks5://user:pass@10.0.0.2:1081"),
            Some(("10.0.0.2".into(), 1081))
        );
        assert_eq!(
            parse_proxy_endpoint("http://[::1]:8080"),
            Some(("::1".into(), 8080))
        );
        assert_eq!(
            parse_proxy_endpoint("http://proxy.example.com:8080/path"),
            Some(("proxy.example.com".into(), 8080))
        );
        // 缺端口按 scheme 默认
        assert_eq!(
            parse_proxy_endpoint("http://proxy.example.com"),
            Some(("proxy.example.com".into(), 80))
        );
        assert_eq!(
            parse_proxy_endpoint("socks5://10.0.0.1"),
            Some(("10.0.0.1".into(), 1080))
        );
        // 非法端口 / 空串
        assert_eq!(parse_proxy_endpoint("http://h:notaport"), None);
        assert_eq!(parse_proxy_endpoint("   "), None);
        // userinfo 不进 host
        assert_eq!(
            parse_proxy_endpoint("http://user:pw@h:9"),
            Some(("h".into(), 9))
        );
    }

    #[test]
    fn parse_wininet_server_compound_and_bare() {
        let compound = parse_wininet_proxy_server("http=127.0.0.1:7890;https=127.0.0.1:7890;socks=127.0.0.1:7891");
        assert_eq!(compound.len(), 3);
        assert_eq!(compound[0], ("http".into(), "127.0.0.1".into(), 7890));
        assert_eq!(compound[2], ("socks".into(), "127.0.0.1".into(), 7891));

        let bare = parse_wininet_proxy_server("127.0.0.1:7890");
        assert_eq!(bare, vec![("http".into(), "127.0.0.1".into(), 7890)]);

        assert!(parse_wininet_proxy_server("").is_empty());
        assert!(parse_wininet_proxy_server(";;").is_empty());
    }

    #[test]
    fn dead_proxy_decision_table() {
        // 设置存在且端口无响应 → 死
        assert_eq!(classify_proxy_health(false, None), ProxyHealth::Dead);
        // 端口通但 CONNECT 失败 → 死
        assert_eq!(
            classify_proxy_health(true, Some(false)),
            ProxyHealth::Dead
        );
        // 端口通 + CONNECT 成功 → 活
        assert_eq!(
            classify_proxy_health(true, Some(true)),
            ProxyHealth::Alive
        );
        // 端口通但未做 CONNECT → 未知
        assert_eq!(classify_proxy_health(true, None), ProxyHealth::Unknown);
    }

    /// mock 代理来源：固定设置列表。
    struct FakeSource {
        settings: Vec<ProxySetting>,
        no_proxy: Option<String>,
        notes: Vec<String>,
    }
    impl ProxySource for FakeSource {
        fn settings(&self) -> Vec<ProxySetting> {
            self.settings.clone()
        }
        fn no_proxy(&self) -> Option<String> {
            self.no_proxy.clone()
        }
        fn notes(&self) -> Vec<String> {
            self.notes.clone()
        }
    }

    /// mock TCP：可编程端口表 + CONNECT 结果表。
    struct FakeTcp {
        open: Mutex<HashMap<String, bool>>,
        connect: Mutex<HashMap<String, bool>>,
    }
    impl FakeTcp {
        fn new() -> Self {
            FakeTcp {
                open: Mutex::new(HashMap::new()),
                connect: Mutex::new(HashMap::new()),
            }
        }
        fn key(host: &str, port: u16) -> String {
            format!("{host}:{port}")
        }
    }
    impl TcpProbe for FakeTcp {
        fn port_open(&self, host: &str, port: u16, _t: Duration) -> bool {
            *self
                .open
                .lock()
                .unwrap()
                .get(&Self::key(host, port))
                .unwrap_or(&false)
        }
        fn http_connect(&self, host: &str, port: u16, _th: &str, _tp: u16, _t: Duration) -> Result<(), String> {
            match self.connect.lock().unwrap().get(&Self::key(host, port)) {
                Some(true) => Ok(()),
                Some(false) => Err("CONNECT rejected".into()),
                None => Err("no route".into()),
            }
        }
    }

    #[test]
    fn d1_run_dead_and_alive_proxies() {
        let tcp = FakeTcp::new();
        // 7890 活（端口通 + CONNECT 通）；7891 死（端口无响应）；7892 端口通但 CONNECT 失败
        tcp.open.lock().unwrap().insert("127.0.0.1:7890".into(), true);
        tcp.connect.lock().unwrap().insert("127.0.0.1:7890".into(), true);
        tcp.open.lock().unwrap().insert("127.0.0.1:7891".into(), false);
        tcp.open.lock().unwrap().insert("127.0.0.1:7892".into(), true);
        tcp.connect.lock().unwrap().insert("127.0.0.1:7892".into(), false);

        let deps = DoctorDeps {
            tcp: Box::new(tcp),
            proxy_source: Box::new(FakeSource {
                settings: vec![
                    ProxySetting::env("HTTP_PROXY", "http://127.0.0.1:7890", "127.0.0.1", 7890),
                    ProxySetting::env("HTTPS_PROXY", "http://127.0.0.1:7891", "127.0.0.1", 7891),
                    ProxySetting::new(
                        ProxyKind::WinInet,
                        r"HKCU\...\Internet Settings\ProxyServer",
                        "http=127.0.0.1:7892",
                        "127.0.0.1",
                        7892,
                    ),
                ],
                no_proxy: Some("localhost,127.0.0.1".into()),
                notes: vec!["mock note".into()],
            }),
            ..DoctorDeps::default()
        };

        let card = run(&deps);
        assert_eq!(card.item, DiagCode::D1);
        assert_eq!(card.status, DiagStatus::Fail);
        assert_eq!(card.summary_key, "netdoctor.D1.dead_proxy");
        assert_eq!(card.detail["settings"].as_array().unwrap().len(), 3);
        assert_eq!(card.detail["settings"][0]["health"], "alive");
        assert_eq!(card.detail["settings"][1]["health"], "dead");
        assert_eq!(card.detail["settings"][1]["port"], 7891);
        assert_eq!(card.detail["settings"][2]["health"], "dead");
        assert_eq!(card.detail["no_proxy"], "localhost,127.0.0.1");
        assert_eq!(card.detail["notes"][0], "mock note");

        // 修复分级：R1 自动忽略 + R2 建议清理
        let levels: Vec<&str> = card.fixes.iter().map(|f| f.level.mode()).collect();
        assert_eq!(levels, ["auto", "suggest"]);
        assert!(card.fixes.iter().all(|f| f.level != FixLevel::R3));
    }

    #[test]
    fn d1_run_clean_without_settings() {
        let deps = DoctorDeps {
            proxy_source: Box::new(FakeSource {
                settings: vec![],
                no_proxy: None,
                notes: vec![],
            }),
            ..DoctorDeps::default()
        };
        let card = run(&deps);
        assert_eq!(card.status, DiagStatus::Ok);
        assert_eq!(card.summary_key, "netdoctor.D1.clean");
        assert!(card.fixes.is_empty());
        assert!(card.detail["settings"].as_array().unwrap().is_empty());
    }

    #[test]
    fn d1_run_all_alive_is_ok() {
        let tcp = FakeTcp::new();
        tcp.open.lock().unwrap().insert("127.0.0.1:7890".into(), true);
        tcp.connect.lock().unwrap().insert("127.0.0.1:7890".into(), true);
        let deps = DoctorDeps {
            tcp: Box::new(tcp),
            proxy_source: Box::new(FakeSource {
                settings: vec![ProxySetting::env(
                    "HTTP_PROXY",
                    "http://127.0.0.1:7890",
                    "127.0.0.1",
                    7890,
                )],
                no_proxy: None,
                notes: vec![],
            }),
            ..DoctorDeps::default()
        };
        let card = run(&deps);
        assert_eq!(card.status, DiagStatus::Ok);
        assert!(card.fixes.is_empty());
    }
}
