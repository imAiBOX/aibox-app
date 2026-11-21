//! **系统代理检测**（Windows 注册表只读；免提权、绝不写任何键）。
//!
//! ## 为什么做
//! `aibox-net` 的 `detect_effective_proxy()` 只看 `HTTP_PROXY`/`HTTPS_PROXY` 环境变量，
//! 而 Windows 桌面用户**几乎从不设这两个环境变量** —— 系统代理是在
//! 「设置 → 网络和 Internet → 代理」里配的（WinINET 注册表）。
//! 结果：国内用户开着系统代理（翻墙/公司代理），AiBOX 却直连 → 下载失败或极慢。
//!
//! ## 读什么
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`
//! - `ProxyEnable`（DWORD，1 = 启用）
//! - `ProxyServer`（字符串：`host:port` 或 `http=host:port;https=host:port`）
//! - `AutoConfigURL`（PAC 地址；ureq 不支持 PAC → 只记录不生效）
//!
//! **只读**：本模块只 `open_subkey` + `get_value`，从不 `create_key`/`set_value`。

use aibox_net::proxy::ProxyConfig;
#[cfg(windows)]
use aibox_net::proxy::ProxySource;

/// WinINET 注册表路径（相对 HKCU）。
#[cfg(windows)]
const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

/// 检测系统代理（Windows：WinINET 注册表；其它平台：空）。
///
/// 返回**未做连通性判断**的候选（真正用哪个由 `aibox_net::proxy::decide_proxy` 决定）。
pub fn detect_system_proxies() -> Vec<ProxyConfig> {
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;

        let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(INTERNET_SETTINGS) else {
            return Vec::new();
        };
        let enabled: u32 = key.get_value("ProxyEnable").unwrap_or(0);
        if enabled != 1 {
            return Vec::new();
        }
        let server: String = key.get_value("ProxyServer").unwrap_or_default();
        if server.trim().is_empty() {
            return Vec::new();
        }
        // ProxyServer 形态：
        //   "127.0.0.1:7890"                     → 全部协议
        //   "http=127.0.0.1:7890;https=127.0.0.1:7890" → 按协议
        //   "socks=127.0.0.1:1080"               → socks（ureq 不支持 → parse 返回 None）
        let mut out = Vec::new();
        // Windows 的 `socks=` 即 SOCKS5；`http=`/`https=` 即 HTTP 代理
        let mut push = |raw: &str, scheme: &str| {
            let full = format!("{scheme}://{raw}");
            if let Some(cfg) = ProxyConfig::parse(&full, ProxySource::WinInet) {
                out.push(cfg);
            }
        };
        if server.contains('=') {
            for part in server.split(';') {
                let part = part.trim();
                if let Some((proto, hostport)) = part.split_once('=') {
                    let proto = proto.trim().to_ascii_lowercase();
                    let scheme = match proto.as_str() {
                        "https" | "http" => "http",
                        "socks" | "socks5" => "socks5",
                        "socks4" => "socks4",
                        _ => continue,
                    };
                    // https 优先（下载都是 https）；再退到 http
                    if proto == "https" || proto == "http" || proto.starts_with("socks") {
                        push(hostport.trim(), scheme);
                    }
                }
            }
        } else {
            push(server.trim(), "http");
        }
        out
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_windows_has_no_system_proxy() {
        #[cfg(not(windows))]
        assert!(detect_system_proxies().is_empty());
    }
}
