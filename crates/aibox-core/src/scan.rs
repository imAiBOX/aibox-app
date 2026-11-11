//! `scan_system`（SCAN / ScanReport）：OS/磁盘/内存/Node/Git/WSL/
//! 已装应用/端口 18789 占用。**cfg(windows) 与 cfg(unix) 双实现**；解析器为纯函数可单测。
//!
//! `checks[].human` = i18n key（`scan.<id>`，TS 侧翻译；id 清单：
//! os/arch/disk/mem/node/git/wsl/port/installed —— 与 src/locales 既有 key 对齐）。

use std::net::{TcpStream, ToSocketAddrs};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::err::{io_err, CoreError, CoreResult};
use crate::paths;

/// ScanReport。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub os: OsInfo,
    pub disk_free_gb: f64,
    pub mem_total_gb: f64,
    pub node: ToolInfo,
    pub git: ToolInfo,
    pub wsl: WslInfo,
    pub installed_apps: Vec<InstalledApp>,
    pub port18789: PortInfo,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OsInfo {
    pub platform: String,
    pub version: String,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// 满足目标应用 runtime 要求（ok=满足目标应用 runtime）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WslInfo {
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distros: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledApp {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortInfo {
    pub occupied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: String,
    pub level: Level,
    /// i18n key（`scan.<id>`；TS 翻译）
    pub human: String,
}

fn check(id: &str, level: Level) -> Check {
    Check {
        id: id.to_string(),
        level,
        human: format!("scan.{id}"),
    }
}

/// GB 换算（1 位小数）。
fn to_gb(bytes: u64) -> f64 {
    (bytes as f64 / 1_073_741_824.0 * 10.0).round() / 10.0
}

// ── 纯函数解析器（单测锁语义）────────────────────────────────────────────

/// `/proc/meminfo` → MemTotal（kB）。
pub fn parse_meminfo_kb(text: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb);
        }
    }
    None
}

/// `df -kP` 输出 → 可用字节（取第一个含 `NN%` 的数据行，`avail` 字段 ×1024）。
pub fn parse_df_avail_kb(text: &str) -> Option<u64> {
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(cap_idx) = tokens.iter().position(|t| t.ends_with('%') && t.len() > 1) else {
            continue; // 表头行没有 %
        };
        // tokens: [..., blocks, used, avail, cap%, mounted...]
        if cap_idx < 3 {
            continue;
        }
        let avail: u64 = tokens[cap_idx - 1].parse().ok()?;
        let used: u64 = tokens[cap_idx - 2].parse().ok()?;
        let blocks: u64 = tokens[cap_idx - 3].parse().ok()?;
        if used + avail <= blocks + 1 {
            return Some(avail);
        }
    }
    None
}

/// `node --version` → `v26.13.0`。
pub fn parse_node_version(out: &str) -> Option<String> {
    let v = out.trim().lines().last()?.trim();
    if regex::Regex::new(r"^v\d+\.\d+\.\d+")
        .map(|re| re.is_match(v))
        .unwrap_or(false)
    {
        return Some(v.to_string());
    }
    None
}

/// `git --version` → `2.56.0.windows.4` 等。
pub fn parse_git_version(out: &str) -> Option<String> {
    let v = out.trim().lines().last()?.trim();
    v.strip_prefix("git version ").map(|s| s.to_string())
}

/// Node 版本是否满足 OpenClaw runtime（≥24.22.1 <25 / ≥26.12.1 / ≥27）。
pub fn node_ok(version: &str) -> bool {
    let Some(v) = parse_node_version(version) else {
        return false;
    };
    let nums: Vec<u64> = v
        .trim_start_matches('v')
        .split('.')
        .filter_map(|p| p.parse::<u64>().ok())
        .collect();
    let (Some(&major), minor, patch) = (
        nums.first(),
        nums.get(1).copied().unwrap_or(0),
        nums.get(2).copied().unwrap_or(0),
    ) else {
        return false;
    };
    match major {
        24 => minor > 22 || (minor == 22 && patch >= 1),
        26 => minor > 12 || (minor == 12 && patch >= 1),
        27.. => true,
        _ => false, // 22/23/25 及更早：官方明确 unsupported
    }
}

/// `/etc/os-release` → PRETTY_NAME。
pub fn parse_os_release(text: &str) -> String {
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            return v.trim().trim_matches('"').to_string();
        }
    }
    String::new()
}

/// `cmd /C ver` → `10.0.26100`。
pub fn parse_windows_ver(out: &str) -> Option<String> {
    let re = regex::Regex::new(r"Version (\d+\.\d+\.\d+)").ok()?;
    re.captures(out)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Windows 构建号 ≥19045 = Win10 20H2+（最低要求）。
pub fn windows_build_ok(ver: &str) -> bool {
    ver.split('.')
        .nth(2)
        .and_then(|b| b.parse::<u32>().ok())
        .map(|b| b >= 19045)
        .unwrap_or(true) // 解析不了不拦
}

/// `wsl -l -q`（UTF-16LE 输出）→ 发行版列表。
pub fn parse_wsl_distros(bytes: &[u8]) -> Vec<String> {
    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        String::from_utf16(
            &bytes[2..]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.lines()
        .map(|l| l.trim().trim_end_matches('\0').to_string())
        .filter(|l| {
            !l.is_empty()
                && !l.eq_ignore_ascii_case("Windows Subsystem for Linux Distributions:")
                && !l.starts_with("Copyright")
        })
        .collect()
}

/// `ss -ltnp` / `netstat -ano` 行 → 端口监听者（unix：pid+进程名）。
pub fn parse_ss_listener(text: &str, port: u16) -> Option<String> {
    let needle = format!(":{port} ");
    for line in text.lines() {
        if !line.contains(&needle) && !line.trim_end().ends_with(&format!(":{port}")) {
            continue;
        }
        // users:(("node",pid=123,fd=5))
        if let Some(s) = line.find("users:(") {
            let seg = &line[s + 7..];
            if let Some(e) = seg.find(')') {
                let seg = &seg[..e];
                let name = seg
                    .split(',')
                    .next()
                    .unwrap_or("")
                    .trim_matches(|c| c == '"' || c == '(')
                    .to_string();
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
        return Some(String::new());
    }
    None
}

/// `netstat -ano`（Windows）→ 监听该端口的 PID。
pub fn parse_netstat_pid(text: &str, port: u16) -> Option<u32> {
    let needle = format!(":{port}");
    for line in text.lines() {
        let t = line.split_whitespace().collect::<Vec<_>>();
        if t.len() < 5 {
            continue;
        }
        // TCP  127.0.0.1:18789  0.0.0.0:0  LISTENING  1234
        if t[0].eq_ignore_ascii_case("TCP")
            && t[1].ends_with(&needle)
            && t[3].eq_ignore_ascii_case("LISTENING")
        {
            if let Ok(pid) = t[4].parse::<u32>() {
                return Some(pid);
            }
        }
    }
    None
}

/// `tasklist /FI "PID eq N" /NH` → 进程名。
pub fn parse_tasklist(out: &str, pid: u32) -> Option<String> {
    for line in out.lines() {
        let t = line.split_whitespace().collect::<Vec<_>>();
        if t.len() >= 2 && t[1].parse::<u32>().ok() == Some(pid) {
            return Some(t[0].to_string());
        }
    }
    None
}

// ── 实装 ──────────────────────────────────────────────────────────────

fn run_capture(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn os_info() -> OsInfo {
    let arch = std::env::consts::ARCH;
    let arch = match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
    .to_string();

    #[cfg(windows)]
    {
        let version = run_capture("cmd", &["/C", "ver"])
            .and_then(|o| parse_windows_ver(&o))
            .unwrap_or_else(|| "10.0".to_string());
        return OsInfo {
            platform: "windows".into(),
            version,
            arch,
        };
    }
    #[cfg(target_os = "macos")]
    {
        let version = run_capture("sw_vers", &["-productVersion"]).unwrap_or_default();
        return OsInfo {
            platform: "macos".into(),
            version: version.trim().to_string(),
            arch,
        };
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let version = std::fs::read_to_string("/etc/os-release")
            .map(|t| parse_os_release(&t))
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                run_capture("uname", &["-sr"])
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default()
            });
        OsInfo {
            platform: "linux".into(),
            version,
            arch,
        }
    }
}

fn disk_free_bytes() -> u64 {
    let home = paths::home_dir();
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide: Vec<u16> = home
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut free: u64 = 0;
        let mut total_free: u64 = 0;
        let mut total: u64 = 0;
        unsafe {
            if GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total_free, &mut total) != 0 {
                return free;
            }
        }
    }
    // unix：df -kP（macOS/Linux 通用）
    if let Some(kb) =
        run_capture("df", &["-kP", &home.to_string_lossy()]).and_then(|o| parse_df_avail_kb(&o))
    {
        return kb * 1024;
    }
    0
}

fn mem_total_bytes() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        unsafe {
            if GlobalMemoryStatusEx(&mut status) != 0 {
                return status.ullTotalPhys;
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(kb) = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|t| parse_meminfo_kb(&t))
        {
            return kb * 1024;
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(v) =
            run_capture("sysctl", &["-n", "hw.memsize"]).and_then(|o| o.trim().parse::<u64>().ok())
        {
            return v;
        }
    }
    0
}

fn tool_info(program: &str, args: &[&str]) -> ToolInfo {
    match run_capture(program, args) {
        Some(out) => {
            let version = if program == "node" {
                parse_node_version(&out)
            } else if program == "git" {
                parse_git_version(&out)
            } else {
                Some(out.trim().to_string())
            };
            ToolInfo {
                installed: true,
                version,
                ok: None,
                note: None,
            }
        }
        None => ToolInfo {
            installed: false,
            version: None,
            ok: None,
            note: None,
        },
    }
}

fn wsl_info() -> WslInfo {
    #[cfg(windows)]
    {
        let out = Command::new("wsl")
            .args(["-l", "-q"])
            .output()
            .map(|o| o.stdout)
            .ok();
        return match out {
            Some(bytes) if !bytes.is_empty() => {
                let distros = parse_wsl_distros(&bytes);
                if distros.is_empty() {
                    WslInfo {
                        installed: false,
                        distros: None,
                    }
                } else {
                    WslInfo {
                        installed: true,
                        distros: Some(distros),
                    }
                }
            }
            _ => WslInfo {
                installed: false,
                distros: None,
            },
        };
    }
    #[cfg(not(windows))]
    WslInfo {
        installed: false,
        distros: None,
    }
}

pub fn installed_apps() -> Vec<InstalledApp> {
    let path = paths::install_manifest_file();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    parse_install_manifest(&v)
}

/// 安装完成登记：upsert `~/.aibox/install-manifest.json`（保留既有条目/字段）。
/// 返回写入路径；失败不阻塞安装流程（调用方 best-effort）。
pub fn record_install(id: &str, version: Option<&str>) -> CoreResult<String> {
    crate::registry::validate_id(id)?;
    let path = paths::install_manifest_file();
    let mut v: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({"apps": []}));
    upsert_install_manifest(&mut v, id, version);
    if let Some(dir) = path.parent() {
        paths::ensure_dir(dir).map_err(|e| io_err("create install-manifest dir", e))?;
    }
    let text = serde_json::to_string_pretty(&v)
        .map_err(|e| CoreError::new("INSTALL_RECORD", format!("serialize install-manifest: {e}")))?;
    std::fs::write(&path, text)
        .map_err(|e| io_err("write install-manifest", e))?;
    Ok(path.to_string_lossy().into_owned())
}

/// upsert 语义：已有同 id 条目 → 更新 version（保留 path 等字段）；否则追加。
/// 兼容 map 形态旧文件（归一化为 `{apps:[...]}`，不丢既有条目）。
pub fn upsert_install_manifest(v: &mut Value, id: &str, version: Option<&str>) {
    if v.get("apps").and_then(|a| a.as_array()).is_none() {
        let mut apps: Vec<Value> = Vec::new();
        if let Some(map) = v.as_object() {
            for (k, item) in map {
                if k == "apps" {
                    continue;
                }
                let mut e = serde_json::json!({"id": k});
                if let Some(ver) = item.get("version").and_then(|x| x.as_str()) {
                    e["version"] = serde_json::json!(ver);
                }
                if let Some(p) = item.get("path").and_then(|x| x.as_str()) {
                    e["path"] = serde_json::json!(p);
                }
                apps.push(e);
            }
        }
        *v = serde_json::json!({"apps": apps});
    }
    let Some(arr) = v.get_mut("apps").and_then(|a| a.as_array_mut()) else {
        return;
    };
    if let Some(entry) = arr
        .iter_mut()
        .find(|e| e.get("id").and_then(|x| x.as_str()) == Some(id))
    {
        if let Some(ver) = version {
            entry["version"] = serde_json::json!(ver);
        }
    } else {
        let mut e = serde_json::json!({"id": id});
        if let Some(ver) = version {
            e["version"] = serde_json::json!(ver);
        }
        arr.push(e);
    }
}

/// 容错解析 install-manifest：`{apps:[...]}` 或 `{id:{version,path}}` 映射。
pub fn parse_install_manifest(v: &Value) -> Vec<InstalledApp> {
    let mut out = Vec::new();
    if let Some(arr) = v.get("apps").and_then(|a| a.as_array()) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(|x| x.as_str()) {
                out.push(InstalledApp {
                    id: id.to_string(),
                    version: item
                        .get("version")
                        .and_then(|x| x.as_str())
                        .map(String::from),
                    path: item.get("path").and_then(|x| x.as_str()).map(String::from),
                });
            }
        }
        return out;
    }
    if let Some(map) = v.as_object() {
        for (id, item) in map {
            out.push(InstalledApp {
                id: id.clone(),
                version: item
                    .get("version")
                    .and_then(|x| x.as_str())
                    .map(String::from),
                path: item.get("path").and_then(|x| x.as_str()).map(String::from),
            });
        }
    }
    out
}

fn tcp_port_open(port: u16) -> bool {
    let addrs = ("127.0.0.1", port).to_socket_addrs();
    match addrs {
        Ok(mut it) => {
            if let Some(addr) = it.next() {
                return TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok();
            }
            false
        }
        Err(_) => false,
    }
}

fn find_listener_process(port: u16) -> Option<String> {
    #[cfg(windows)]
    {
        let out = run_capture("netstat", &["-ano"])?;
        let pid = parse_netstat_pid(&out, port)?;
        let tl = run_capture("tasklist", &["/FI", &format!("PID eq {pid}"), "/NH"])?;
        let name = parse_tasklist(&tl, pid);
        return Some(name.unwrap_or_else(|| format!("pid {pid}")));
    }
    #[cfg(unix)]
    {
        // ss 优先（iproute2），lsof 兜底；都缺 → None（占用判定回落 TCP 连接）
        if let Some(out) = run_capture("ss", &["-ltnp"]) {
            if let Some(who) = parse_ss_listener(&out, port) {
                return Some(if who.is_empty() {
                    "listener".to_string()
                } else {
                    who
                });
            }
        }
        if let Some(out) = run_capture("lsof", &["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN"]) {
            let mut lines = out.lines().skip(1);
            if let Some(line) = lines.next() {
                let name = line.split_whitespace().next().unwrap_or("").to_string();
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
        None
    }
}

fn port_info() -> PortInfo {
    if let Some(process) = find_listener_process(18789) {
        return PortInfo {
            occupied: true,
            process: Some(process),
        };
    }
    if tcp_port_open(18789) {
        return PortInfo {
            occupied: true,
            process: None,
        };
    }
    PortInfo {
        occupied: false,
        process: None,
    }
}

/// 汇总 `checks[]`（i18n key = `scan.<id>`）。
fn build_checks(
    os: &OsInfo,
    disk_gb: f64,
    mem_gb: f64,
    node: &ToolInfo,
    git: &ToolInfo,
    wsl: &WslInfo,
    port: &PortInfo,
    _apps: &[InstalledApp],
) -> Vec<Check> {
    let os_level = if os.platform == "windows" && !windows_build_ok(&os.version) {
        Level::Warn
    } else {
        Level::Ok
    };
    let arch_ok = matches!(os.arch.as_str(), "x64" | "arm64");
    let disk_level = if disk_gb >= 5.0 {
        Level::Ok
    } else if disk_gb >= 2.0 {
        Level::Warn
    } else {
        Level::Fail
    };
    let mem_level = if mem_gb >= 4.0 {
        Level::Ok
    } else {
        Level::Warn
    };
    let node_level = if node.installed {
        if node.ok.unwrap_or(false) {
            Level::Ok
        } else {
            Level::Warn
        }
    } else {
        Level::Warn // 缺失 = 计划将静默安装（hint 文案安抚）
    };
    let git_level = if git.installed {
        Level::Ok
    } else {
        Level::Warn
    };
    let wsl_level = if wsl.installed {
        Level::Ok
    } else {
        Level::Warn
    };
    let port_level = if port.occupied {
        Level::Warn
    } else {
        Level::Ok
    };

    vec![
        check("os", os_level),
        check("arch", if arch_ok { Level::Ok } else { Level::Warn }),
        check("disk", disk_level),
        check("mem", mem_level),
        check("node", node_level),
        check("git", git_level),
        check("wsl", wsl_level),
        check("port", port_level),
        check("installed", Level::Ok),
    ]
}

/// 执行系统体检（同步；调用方放入 spawn_blocking）。
pub fn scan_system() -> ScanReport {
    let os = os_info();
    let disk_free_gb = to_gb(disk_free_bytes());
    let mem_total_gb = to_gb(mem_total_bytes());

    let mut node = tool_info("node", &["--version"]);
    if node.installed {
        let raw = node.version.clone().unwrap_or_default();
        node.ok = Some(node_ok(&raw));
        if !node.ok.unwrap_or(false) {
            node.note = Some("scan.node.unsupported".into());
        }
    }

    let git = tool_info("git", &["--version"]);
    let wsl = wsl_info();
    let apps = installed_apps();
    let port = port_info();

    let checks = build_checks(
        &os,
        disk_free_gb,
        mem_total_gb,
        &node,
        &git,
        &wsl,
        &port,
        &apps,
    );

    ScanReport {
        os,
        disk_free_gb,
        mem_total_gb,
        node,
        git,
        wsl,
        installed_apps: apps,
        port18789: port,
        checks,
    }
}

/// 供诊断包引用（只读快照）。
pub fn scan_report_value() -> Value {
    serde_json::to_value(scan_system()).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_parser() {
        let sample = "MemTotal:       16384000 kB\nMemFree: 123 kB\n";
        assert_eq!(parse_meminfo_kb(sample), Some(16_384_000));
        assert_eq!(parse_meminfo_kb("nope"), None);
    }

    #[test]
    fn df_parser_skips_header() {
        let sample = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda1 1000000 400000 600000 40% /\n";
        assert_eq!(parse_df_avail_kb(sample), Some(600_000));
        assert_eq!(parse_df_avail_kb("garbage"), None);
    }

    #[test]
    fn node_and_git_version_parsers() {
        assert_eq!(
            parse_node_version("v26.13.0\n").as_deref(),
            Some("v26.13.0")
        );
        assert_eq!(parse_node_version("not installed"), None);
        assert_eq!(
            parse_git_version("git version 2.56.0.windows.4").as_deref(),
            Some("2.56.0.windows.4")
        );
    }

    #[test]
    fn node_policy_matches_appendix_a() {
        assert!(node_ok("v26.13.0"));
        assert!(node_ok("v24.22.1"));
        assert!(node_ok("v24.30.0"));
        assert!(node_ok("v27.1.0"));
        assert!(!node_ok("v26.11.0"), "26.12.1 之前 unsupported");
        assert!(!node_ok("v24.20.0"), "24.22.1 之前 unsupported");
        assert!(!node_ok("v22.19.0"), "22.x unsupported");
        assert!(!node_ok("v25.1.0"), "25.x unsupported");
        assert!(!node_ok("garbage"));
    }

    #[test]
    fn windows_ver_parser_and_build_gate() {
        assert_eq!(
            parse_windows_ver("Microsoft Windows [Version 10.0.26100]").as_deref(),
            Some("10.0.26100")
        );
        assert!(windows_build_ok("10.0.26100"));
        assert!(windows_build_ok("10.0.19045"));
        assert!(!windows_build_ok("10.0.19044"), "20H2 之前不满足");
    }

    #[test]
    fn wsl_utf16_parser() {
        let text = "Ubuntu\r\nDebian\r\n";
        let mut bytes: Vec<u8> = vec![0xFF, 0xFE];
        for u in text.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let d = parse_wsl_distros(&bytes);
        assert_eq!(d, vec!["Ubuntu".to_string(), "Debian".to_string()]);
    }

    #[test]
    fn ss_and_netstat_parsers() {
        let ss = "LISTEN 0 128 127.0.0.1:18789 0.0.0.0:* users:((\"node\",pid=4242,fd=5))\n";
        assert_eq!(parse_ss_listener(ss, 18789).as_deref(), Some("node"));
        let netstat = "  TCP    127.0.0.1:18789     0.0.0.0:0              LISTENING       4242\n";
        assert_eq!(parse_netstat_pid(netstat, 18789), Some(4242));
        assert_eq!(parse_netstat_pid(netstat, 9999), None);
        let tl = "node.exe                   4242 Console                    1     50,000 K\n";
        assert_eq!(parse_tasklist(tl, 4242).as_deref(), Some("node.exe"));
    }

    #[test]
    fn install_manifest_shapes() {
        let v = serde_json::json!({"apps": [{"id": "openclaw", "version": "2026.11.2", "path": "C:/x"}]});
        let apps = parse_install_manifest(&v);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].id, "openclaw");
        let v2 = serde_json::json!({"openclaw": {"version": "1.0"}});
        let apps2 = parse_install_manifest(&v2);
        assert_eq!(apps2[0].id, "openclaw");
        assert_eq!(apps2[0].version.as_deref(), Some("1.0"));
    }

    #[test]
    fn upsert_install_manifest_append_and_update() {
        // 新装 → 追加
        let mut v = serde_json::json!({"apps": []});
        upsert_install_manifest(&mut v, "openclaw", Some("2026.11.2"));
        assert_eq!(v["apps"].as_array().unwrap().len(), 1);
        // 同 id 再登记（版本升级）→ 更新而非重复；无关字段保留
        upsert_install_manifest(&mut v, "openclaw", Some("2026.11.3"));
        assert_eq!(v["apps"].as_array().unwrap().len(), 1);
        assert_eq!(v["apps"][0]["version"], "2026.11.3");
        // 不同 id → 追加第二条
        upsert_install_manifest(&mut v, "cherry-studio", None);
        assert_eq!(v["apps"].as_array().unwrap().len(), 2);
        assert!(v["apps"][1].get("version").is_none(), "无版本不写 null 字段");
        // map 形态旧文件 → 归一化且不丢条目
        let mut legacy = serde_json::json!({"jan": {"version": "1.0", "path": "/opt/jan"}});
        upsert_install_manifest(&mut legacy, "ollama", Some("0.5.0"));
        let apps = legacy["apps"].as_array().unwrap();
        assert_eq!(apps.len(), 2);
        let jan = apps.iter().find(|a| a["id"] == "jan").unwrap();
        assert_eq!(jan["version"], "1.0");
        assert_eq!(jan["path"], "/opt/jan");
        // 幂等：重复 upsert 不产生重复条目
        upsert_install_manifest(&mut legacy, "jan", Some("1.1"));
        assert_eq!(legacy["apps"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn checks_have_i18n_keys_and_levels() {
        let os = OsInfo {
            platform: "linux".into(),
            version: "Ubuntu 24.04".into(),
            arch: "x64".into(),
        };
        let node = ToolInfo {
            installed: true,
            version: Some("v26.13.0".into()),
            ok: Some(true),
            note: None,
        };
        let git = ToolInfo {
            installed: true,
            version: Some("2.56.0".into()),
            ok: None,
            note: None,
        };
        let wsl = WslInfo {
            installed: true,
            distros: Some(vec!["Ubuntu".into()]),
        };
        let port = PortInfo {
            occupied: false,
            process: None,
        };
        let checks = build_checks(&os, 100.0, 16.0, &node, &git, &wsl, &port, &[]);
        assert_eq!(checks.len(), 9);
        for c in &checks {
            assert_eq!(c.human, format!("scan.{}", c.id));
        }
        assert!(checks.iter().all(|c| c.level == Level::Ok));
        // WSL 缺失 → warn（安抚文案，非阻断）
        let no_wsl = WslInfo {
            installed: false,
            distros: None,
        };
        let checks_w = build_checks(&os, 100.0, 16.0, &node, &git, &no_wsl, &port, &[]);
        assert_eq!(checks_w[6].level, Level::Warn, "wsl missing → warn");
        // 低磁盘 + 端口占用 → warn/fail 分级
        let checks2 = build_checks(&os, 3.0, 16.0, &node, &git, &wsl, &port, &[]);
        assert_eq!(checks2[2].level, Level::Warn, "disk 3GB → warn");
        let checks3 = build_checks(&os, 1.0, 16.0, &node, &git, &wsl, &port, &[]);
        assert_eq!(checks3[2].level, Level::Fail, "disk 1GB → fail");
        let busy = PortInfo {
            occupied: true,
            process: Some("node".into()),
        };
        let checks4 = build_checks(&os, 100.0, 16.0, &node, &git, &wsl, &busy, &[]);
        assert_eq!(checks4[6 + 1].level, Level::Warn, "port occupied → warn");
    }

    /// 真实本机扫描冒烟（Linux CI 可跑；只断言结构不依赖具体机器状态）。
    #[test]
    fn scan_system_smoke() {
        let r = scan_system();
        assert_eq!(r.os.platform, std::env::consts::OS);
        assert!(r.disk_free_gb >= 0.0);
        assert!(r.mem_total_gb > 0.0, "mem must be readable on Linux");
        assert_eq!(r.checks.len(), 9);
        assert!(r.checks.iter().any(|c| c.human == "scan.node"));
        // 序列化为 camelCase
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("diskFreeGb").is_some());
        assert!(v.get("memTotalGb").is_some());
        assert!(v.get("port18789").is_some());
        assert!(v.get("installedApps").is_some());
        assert!(v["checks"][0].get("human").is_some());
    }
}
