//! D3 hosts 审计：
//! 读 hosts 文件，标记关键域条目并分类（加速器残留 / 屏蔽条目 / 用户正常自定义）。
//! **绝不自动修改**（清理属 R3 显式授权 + 备份）。

use serde_json::json;

use crate::report::{loc, DiagCard, DiagCode, DiagStatus, Fix, FixLevel};
use crate::transport::DoctorDeps;

/// 关键域（与 D2 同基线）。
pub const D3_CRITICAL_DOMAINS: [&str; 5] = [
    "github.com",
    "raw.githubusercontent.com",
    "registry.npmjs.org",
    "npmmirror.com",
    "cdn.jsdelivr.net",
];

/// hosts 条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostsEntry {
    pub line_no: usize,
    pub ip: String,
    pub names: Vec<String>,
    pub comment: Option<String>,
    pub raw: String,
}

impl HostsEntry {
    pub fn hosts(&self) -> &[String] {
        &self.names
    }
}

/// 解析 hosts 内容（纯函数）：支持 `#` 行注释、行内注释、Tab/多空格、IPv6、空行。
pub fn parse_hosts(content: &str) -> Vec<HostsEntry> {
    let mut out = Vec::new();
    for (i, raw_line) in content.lines().enumerate() {
        let line_no = i + 1;
        // 行内注释剥离
        let (code, comment) = match raw_line.split_once('#') {
            Some((c, cm)) => (c, Some(cm.trim().to_string())),
            None => (raw_line, None),
        };
        let code = code.trim();
        if code.is_empty() {
            continue;
        }
        let mut parts = code.split_whitespace();
        let Some(ip) = parts.next() else { continue };
        let names: Vec<String> = parts.map(|s| s.to_string()).collect();
        if names.is_empty() {
            continue; // 只有 IP 没有域名的行不成条目
        }
        out.push(HostsEntry {
            line_no,
            ip: ip.to_string(),
            names,
            comment: comment.filter(|c| !c.is_empty()),
            raw: raw_line.to_string(),
        });
    }
    out
}

/// 关键域条目分类（三分类）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriticalClass {
    /// 加速器/代理工具残留（程序写入特征）
    AcceleratorResidue,
    /// 屏蔽条目（0.0.0.0 / 127.0.0.1 指向本地）
    BlockEntry,
    /// 用户正常自定义（有注释/明确人工痕迹）
    UserCustom,
}

impl CriticalClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            CriticalClass::AcceleratorResidue => "accelerator_residue",
            CriticalClass::BlockEntry => "block_entry",
            CriticalClass::UserCustom => "user_custom",
        }
    }

    pub fn summary(&self) -> crate::report::Localized {
        match self {
            CriticalClass::AcceleratorResidue => loc(
                "疑似加速器/代理工具残留条目",
                "Likely accelerator/proxy tool residue",
            ),
            CriticalClass::BlockEntry => loc(
                "屏蔽条目（指向本机/空地址，会阻断访问）",
                "Blocking entry (points to local/empty address, blocks access)",
            ),
            CriticalClass::UserCustom => loc(
                "用户正常自定义条目",
                "Normal user-defined entry",
            ),
        }
    }
}

/// 单条审计结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryVerdict {
    pub entry: HostsEntry,
    /// 是否关键域条目。
    pub critical: bool,
    pub class: Option<CriticalClass>,
    pub reason: String,
}

/// 分类规则（可注入：关键词/特征 IP/无注释条目的处理）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifyRules {
    /// 加速器/代理工具关键词（命中注释或域名即判残留；大小写不敏感）。
    pub keywords: Vec<String>,
    /// 已知加速器/劫持特征 IP。
    pub accelerator_ips: Vec<String>,
    /// 关键域条目**无注释**时是否判为加速器残留（程序写入通常不留注释）。
    pub unnamed_critical_is_residue: bool,
}

impl Default for ClassifyRules {
    fn default() -> Self {
        ClassifyRules {
            keywords: vec![
                "加速".to_string(),
                "accelerator".to_string(),
                "proxy".to_string(),
                "clash".to_string(),
                "v2ray".to_string(),
                "shadowsocks".to_string(),
                "xunlei".to_string(),
                "thunder".to_string(),
                "steam".to_string(),
                "netch".to_string(),
                "hysteria".to_string(),
                "trojan".to_string(),
                "watt toolkit".to_string(),
            ],
            accelerator_ips: vec![],
            unnamed_critical_is_residue: true,
        }
    }
}

/// 屏蔽 IP（指向本机/丢弃）。
pub const BLOCK_IPS: [&str; 4] = ["0.0.0.0", "127.0.0.1", "::", "::1"];

/// 单条分类（纯函数）。
pub fn classify_entry(entry: &HostsEntry, rules: &ClassifyRules) -> EntryVerdict {
    let critical = entry
        .names
        .iter()
        .any(|n| D3_CRITICAL_DOMAINS.iter().any(|d| d.eq_ignore_ascii_case(n)));

    if !critical {
        return EntryVerdict {
            entry: entry.clone(),
            critical: false,
            class: None,
            reason: "非关键域条目".to_string(),
        };
    }

    // 1) 屏蔽条目（最严重优先）
    if BLOCK_IPS.iter().any(|b| b.eq_ignore_ascii_case(&entry.ip)) {
        return EntryVerdict {
            entry: entry.clone(),
            critical: true,
            class: Some(CriticalClass::BlockEntry),
            reason: format!("关键域指向丢弃地址 {}", entry.ip),
        };
    }

    // 2) 加速器特征：关键词（注释/域名）或特征 IP
    let haystack = format!(
        "{} {} {}",
        entry.comment.clone().unwrap_or_default(),
        entry.names.join(" "),
        entry.ip
    )
    .to_lowercase();
    if let Some(kw) = rules
        .keywords
        .iter()
        .find(|k| haystack.contains(&k.to_lowercase()))
    {
        return EntryVerdict {
            entry: entry.clone(),
            critical: true,
            class: Some(CriticalClass::AcceleratorResidue),
            reason: format!("命中关键词「{kw}」"),
        };
    }
    if rules
        .accelerator_ips
        .iter()
        .any(|a| a.eq_ignore_ascii_case(&entry.ip))
    {
        return EntryVerdict {
            entry: entry.clone(),
            critical: true,
            class: Some(CriticalClass::AcceleratorResidue),
            reason: format!("命中特征 IP {}", entry.ip),
        };
    }

    // 3) 有注释 = 人工痕迹 → 用户自定义；无注释 = 程序写入 → 残留（可配置）
    if entry.comment.is_some() {
        EntryVerdict {
            entry: entry.clone(),
            critical: true,
            class: Some(CriticalClass::UserCustom),
            reason: "带注释，视为用户自定义".to_string(),
        }
    } else if rules.unnamed_critical_is_residue {
        EntryVerdict {
            entry: entry.clone(),
            critical: true,
            class: Some(CriticalClass::AcceleratorResidue),
            reason: "关键域且无注释（程序写入特征）".to_string(),
        }
    } else {
        EntryVerdict {
            entry: entry.clone(),
            critical: true,
            class: Some(CriticalClass::UserCustom),
            reason: "关键域无注释，按用户自定义处理".to_string(),
        }
    }
}

/// 执行 D3（依赖注入：hosts 来源；路径可注入，绝不写文件）。
pub fn run_with(deps: &DoctorDeps, rules: &ClassifyRules) -> DiagCard {
    let location = deps.hosts.location();
    let read = deps.hosts.read();

    let (status, summary, verdicts, notes) = match read {
        Err(e) => (
            DiagStatus::Warn,
            loc(
                format!("hosts 文件无法读取：{e}"),
                format!("hosts file unreadable: {e}"),
            ),
            vec![],
            vec![format!("读取失败：{e}")],
        ),
        Ok(content) => {
            let entries = parse_hosts(&content);
            let verdicts: Vec<EntryVerdict> = entries
                .iter()
                .map(|e| classify_entry(e, rules))
                .collect();
            let critical: Vec<&EntryVerdict> =
                verdicts.iter().filter(|v| v.critical).collect();
            let residue = critical
                .iter()
                .filter(|v| v.class == Some(CriticalClass::AcceleratorResidue))
                .count();
            let blocks = critical
                .iter()
                .filter(|v| v.class == Some(CriticalClass::BlockEntry))
                .count();

            let status = if blocks > 0 {
                DiagStatus::Fail
            } else if residue > 0 {
                DiagStatus::Warn
            } else {
                DiagStatus::Ok
            };
            let summary = if blocks > 0 {
                loc(
                    format!("发现 {blocks} 条关键域屏蔽条目、{residue} 条加速器残留（不会自动修改）"),
                    format!(
                        "{blocks} blocking and {residue} residue entries on key domains (never auto-modified)"
                    ),
                )
            } else if residue > 0 {
                loc(
                    format!("发现 {residue} 条疑似加速器残留（不会自动修改）"),
                    format!("{residue} suspected accelerator residue entries (never auto-modified)"),
                )
            } else if critical.is_empty() {
                loc(
                    "hosts 中无关键域条目，审计通过",
                    "No key-domain entries in hosts: audit passed",
                )
            } else {
                loc(
                    format!("hosts 关键域条目均为用户自定义（{} 条）", critical.len()),
                    format!(
                        "{} key-domain entries, all user-defined",
                        critical.len()
                    ),
                )
            };
            let mut notes = vec![format!("共解析 {} 条条目", entries.len())];
            if content.trim().is_empty() {
                notes.push("hosts 文件为空".to_string());
            }
            (status, summary, verdicts, notes)
        }
    };

    let mut fixes = Vec::new();
    let has_suspect = verdicts.iter().any(|v| {
        matches!(
            v.class,
            Some(CriticalClass::AcceleratorResidue) | Some(CriticalClass::BlockEntry)
        )
    });
    if has_suspect {
        fixes.push(Fix::new(
            FixLevel::R3,
            "netdoctor.fix.clean_hosts",
            loc(
                "备份后清理 hosts 可疑条目（需显式授权，一键可还原）",
                "Back up then remove suspicious hosts entries (explicit authorization, one-click revert)",
            ),
            "R3：先写 hosts.aibox.bak.<时间戳> 备份，双层确认后修改；修改历史入 SQLite",
        ));
    }

    DiagCard {
        item: DiagCode::D3,
        status,
        title: DiagCode::D3.title(),
        summary_key: format!("netdoctor.D3.{}", {
            if verdicts.iter().any(|v| v.class == Some(CriticalClass::BlockEntry)) {
                "block_entry"
            } else if verdicts
                .iter()
                .any(|v| v.class == Some(CriticalClass::AcceleratorResidue))
            {
                "accelerator_residue"
            } else if verdicts.iter().any(|v| v.critical) {
                "user_custom"
            } else {
                "clean"
            }
        }),
        summary,
        detail: json!({
            "location": location,
            "entries": verdicts.iter().map(|v| json!({
                "line": v.entry.line_no,
                "ip": v.entry.ip,
                "names": v.entry.names,
                "comment": v.entry.comment,
                "critical": v.critical,
                "class": v.class.map(|c| c.as_str()),
                "reason": v.reason,
            })).collect::<Vec<_>>(),
            "notes": notes,
            "auto_modify": false,
        }),
        timestamp_ms: (deps.now_ms)(),
        fixes,
    }
}

/// 默认规则执行 D3。
pub fn run(deps: &DoctorDeps) -> DiagCard {
    run_with(deps, &ClassifyRules::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::DiagStatus;
    use crate::transport::HostsSource;

    fn entry(line_no: usize, ip: &str, names: &[&str], comment: Option<&str>) -> HostsEntry {
        HostsEntry {
            line_no,
            ip: ip.to_string(),
            names: names.iter().map(|s| s.to_string()).collect(),
            comment: comment.map(|s| s.to_string()),
            raw: String::new(),
        }
    }

    #[test]
    fn parse_hosts_handles_comments_tabs_and_ipv6() {
        let content = "\
# comment only
127.0.0.1\tlocalhost
::1\tlocalhost ip6-localhost
20.205.243.166   github.com  # github 直连加速
0.0.0.0 raw.githubusercontent.com  block it
10.0.0.1 my.lan   my custom host

1.2.3.4
";
        let entries = parse_hosts(content);
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].ip, "127.0.0.1");
        assert_eq!(entries[0].names, vec!["localhost"]);
        assert_eq!(entries[0].line_no, 2);
        assert_eq!(entries[1].ip, "::1");
        assert_eq!(entries[2].comment.as_deref(), Some("github 直连加速"));
        assert_eq!(entries[2].names, vec!["github.com"]);
        assert_eq!(entries[3].line_no, 5);
        // 只有 IP 无域名 → 不成条目
        assert!(!entries.iter().any(|e| e.names.is_empty()));
        // 行号连续对应原始行（空行/纯注释行/无域名行被跳过）
        assert_eq!(entries.last().unwrap().line_no, 6);
    }

    #[test]
    fn classify_block_entry_wins() {
        let rules = ClassifyRules::default();
        let e = entry(1, "0.0.0.0", &["github.com"], Some("加速"));
        let v = classify_entry(&e, &rules);
        assert!(v.critical);
        assert_eq!(v.class, Some(CriticalClass::BlockEntry));
        // 屏蔽优先于关键词
        let e2 = entry(2, "127.0.0.1", &["raw.githubusercontent.com"], None);
        assert_eq!(
            classify_entry(&e2, &rules).class,
            Some(CriticalClass::BlockEntry)
        );
    }

    #[test]
    fn classify_accelerator_residue_by_keyword_or_ip() {
        let rules = ClassifyRules::default();
        // 注释命中关键词
        let e = entry(1, "1.2.3.4", &["github.com"], Some("网游加速器残留"));
        assert_eq!(
            classify_entry(&e, &rules).class,
            Some(CriticalClass::AcceleratorResidue)
        );
        // 特征 IP（注入规则）
        let mut rules2 = ClassifyRules::default();
        rules2.accelerator_ips = vec!["1.2.3.4".into()];
        let e2 = entry(2, "1.2.3.4", &["registry.npmjs.org"], None);
        assert_eq!(
            classify_entry(&e2, &rules2).class,
            Some(CriticalClass::AcceleratorResidue)
        );
        // 无注释关键域 → 程序写入特征 → 残留（默认）
        let e3 = entry(3, "140.82.113.3", &["github.com"], None);
        assert_eq!(
            classify_entry(&e3, &rules).class,
            Some(CriticalClass::AcceleratorResidue)
        );
        // 关闭该规则 → 降级为用户自定义
        let mut rules3 = ClassifyRules::default();
        rules3.unnamed_critical_is_residue = false;
        assert_eq!(
            classify_entry(&e3, &rules3).class,
            Some(CriticalClass::UserCustom)
        );
    }

    #[test]
    fn classify_user_custom_and_non_critical() {
        let rules = ClassifyRules::default();
        // 带注释的关键域 → 用户自定义
        let e = entry(1, "140.82.113.3", &["github.com"], Some("我自己配的解析"));
        assert_eq!(classify_entry(&e, &rules).class, Some(CriticalClass::UserCustom));
        // 非关键域 → 不标记
        let e2 = entry(2, "192.168.1.1", &["nas.lan"], None);
        let v = classify_entry(&e2, &rules);
        assert!(!v.critical);
        assert_eq!(v.class, None);
        // 大小写不敏感
        let e3 = entry(3, "1.2.3.4", &["GitHub.COM"], None);
        let v3 = classify_entry(&e3, &rules);
        assert!(v3.critical);
    }

    struct StringHosts(String, String);
    impl HostsSource for StringHosts {
        fn read(&self) -> Result<String, String> {
            Ok(self.0.clone())
        }
        fn location(&self) -> String {
            self.1.clone()
        }
    }

    struct MissingHosts;
    impl HostsSource for MissingHosts {
        fn read(&self) -> Result<String, String> {
            Err("permission denied".into())
        }
        fn location(&self) -> String {
            "/etc/hosts".into()
        }
    }

    #[test]
    fn d3_run_flags_suspects_without_modifying() {
        let hosts = "\
127.0.0.1 localhost
0.0.0.0 raw.githubusercontent.com
140.82.113.3 github.com # 我自己加的
";
        let deps = DoctorDeps {
            hosts: Box::new(StringHosts(hosts.into(), "/tmp/fake-hosts".into())),
            ..DoctorDeps::default()
        };
        let card = run(&deps);
        assert_eq!(card.item, DiagCode::D3);
        assert_eq!(card.status, DiagStatus::Fail); // 有屏蔽条目
        assert_eq!(card.summary_key, "netdoctor.D3.block_entry");
        assert_eq!(card.detail["location"], "/tmp/fake-hosts");
        assert_eq!(card.detail["auto_modify"], false);
        let entries = card.detail["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[1]["class"], "block_entry");
        assert_eq!(entries[2]["class"], "user_custom");
        // 清理只能是 R3（需授权）
        assert_eq!(card.fixes.len(), 1);
        assert_eq!(card.fixes[0].level, FixLevel::R3);
        assert_eq!(card.fixes[0].level.mode(), "authorize");
    }

    #[test]
    fn d3_run_clean_hosts() {
        let deps = DoctorDeps {
            hosts: Box::new(StringHosts("127.0.0.1 localhost\n".into(), "/etc/hosts".into())),
            ..DoctorDeps::default()
        };
        let card = run(&deps);
        assert_eq!(card.status, DiagStatus::Ok);
        assert_eq!(card.summary_key, "netdoctor.D3.clean");
        assert!(card.fixes.is_empty());
    }

    #[test]
    fn d3_run_unreadable_hosts_is_warn() {
        let deps = DoctorDeps {
            hosts: Box::new(MissingHosts),
            ..DoctorDeps::default()
        };
        let card = run(&deps);
        assert_eq!(card.status, DiagStatus::Warn);
        assert!(card.fixes.is_empty());
        assert!(card.summary.en.contains("unreadable"));
    }
}
