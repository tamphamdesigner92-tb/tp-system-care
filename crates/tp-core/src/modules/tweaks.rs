//! Ba hạng mục dựa trên thiết lập registry: Tối ưu hệ thống, Tăng tốc Internet,
//! Gia cố bảo mật. Mỗi luật so giá trị hiện tại với giá trị đề xuất; sửa là ghi
//! giá trị đề xuất, giá trị cũ được lưu để hoàn tác.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use crate::engine::{Action, Finding, Module, ModuleId, ScanContext};
use crate::reg::{self, RawValue};

/// Khi nào một thiết lập bị coi là cần sửa.
enum When {
    /// Giá trị DWORD hiện tại bằng đúng số này (không có giá trị thì không tính).
    DwordIs(u32),
    /// Giá trị DWORD hiện tại khác số này; `absent` cho biết thiếu giá trị có tính không.
    DwordIsNot { want: u32, absent: bool },
    /// Giá trị chuỗi là số lớn hơn ngưỡng (thiếu giá trị thì dùng `default`).
    NumberAbove { limit: u32, default: u32 },
}

struct Tweak {
    kind: &'static str,
    machine: bool,
    subkey: &'static str,
    name: &'static str,
    when: When,
    /// Giá trị sẽ ghi khi sửa.
    set: Set,
    recommended: bool,
}

enum Set {
    Dword(u32),
    Sz(&'static str),
}

impl Tweak {
    fn needs_fix(&self) -> bool {
        let current = reg::read(self.machine, self.subkey, self.name);
        match self.when {
            When::DwordIs(bad) => current.and_then(|v| v.as_dword()) == Some(bad),
            When::DwordIsNot { want, absent } => match current.and_then(|v| v.as_dword()) {
                Some(value) => value != want,
                None => absent,
            },
            When::NumberAbove { limit, default } => {
                current
                    .and_then(|v| v.as_string())
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .unwrap_or(default)
                    > limit
            }
        }
    }

    fn finding(&self, module: ModuleId) -> Finding {
        Finding {
            id: String::new(),
            module,
            kind: self.kind,
            name: None,
            detail: Some(format!(
                "{}\\{}\\{}",
                reg::hive_name(self.machine),
                self.subkey,
                self.name
            )),
            count: 1,
            bytes: 0,
            recommended: self.recommended,
            undoable: true,
            needs_admin: self.machine,
            action: Action::SetRegistryValue {
                machine: self.machine,
                subkey: self.subkey.to_string(),
                name: self.name.to_string(),
                value: match self.set {
                    Set::Dword(value) => RawValue::dword(value),
                    Set::Sz(value) => RawValue::sz(value),
                },
            },
        }
    }
}

fn scan_tweaks(module: ModuleId, tweaks: &[Tweak], ctx: &ScanContext) -> Vec<Finding> {
    let mut findings = Vec::new();
    for tweak in tweaks {
        if ctx.cancelled() {
            break;
        }
        ctx.report(Path::new(tweak.subkey));
        if tweak.needs_fix() {
            findings.push(tweak.finding(module));
        }
    }
    findings
}

// ---------------------------------------------------------------- Hệ thống

pub struct System;

const SYSTEM: &[Tweak] = &[
    // Độ trễ mở menu con, mặc định 400 ms.
    Tweak {
        kind: "system.menuDelay",
        machine: false,
        subkey: "Control Panel\\Desktop",
        name: "MenuShowDelay",
        when: When::NumberAbove { limit: 100, default: 400 },
        set: Set::Sz("100"),
        recommended: true,
    },
    // Explorer chờ khoảng 10 giây sau khi đăng nhập rồi mới chạy chương trình khởi động.
    Tweak {
        kind: "system.startupDelay",
        machine: false,
        subkey: "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Serialize",
        name: "StartupDelayInMSec",
        when: When::DwordIsNot { want: 0, absent: true },
        set: Set::Dword(0),
        recommended: true,
    },
    // Windows tự cài ứng dụng được quảng bá vào Start Menu.
    Tweak {
        kind: "system.silentApps",
        machine: false,
        subkey: "Software\\Microsoft\\Windows\\CurrentVersion\\ContentDeliveryManager",
        name: "SilentInstalledAppsEnabled",
        when: When::DwordIs(1),
        set: Set::Dword(0),
        recommended: true,
    },
];

impl Module for System {
    fn id(&self) -> ModuleId {
        ModuleId::System
    }

    fn scan(&self, ctx: &ScanContext) -> Vec<Finding> {
        scan_tweaks(ModuleId::System, SYSTEM, ctx)
    }
}

// ---------------------------------------------------------------- Internet

pub struct Internet;

const INTERNET_SETTINGS: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings";

const INTERNET: &[Tweak] = &[
    // Windows giới hạn gói tin mạng khi đang phát đa phương tiện (mặc định 10).
    // Hiệu quả tùy máy nên không chọn sẵn.
    Tweak {
        kind: "internet.throttling",
        machine: true,
        subkey: "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Multimedia\\SystemProfile",
        name: "NetworkThrottlingIndex",
        when: When::DwordIsNot {
            want: 0xFFFF_FFFF,
            absent: true,
        },
        set: Set::Dword(0xFFFF_FFFF),
        recommended: false,
    },
];

/// Tách `host:port` đầu tiên khỏi giá trị ProxyServer, có thể ở dạng
/// `127.0.0.1:8888` hoặc `http=127.0.0.1:8888;https=...`.
fn first_proxy(server: &str) -> Option<&str> {
    let first = server.split(';').next()?.trim();
    let address = first.rsplit('=').next()?.trim();
    let address = address.strip_prefix("http://").unwrap_or(address);
    (!address.is_empty()).then_some(address)
}

/// Proxy trỏ về chính máy này (thường do một phần mềm đã gỡ để lại).
fn local_proxy(address: &str) -> Option<SocketAddr> {
    let (host, _) = address.rsplit_once(':')?;
    if !matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
        return None;
    }
    address.to_socket_addrs().ok()?.next()
}

/// Proxy hệ thống đang bật nhưng trỏ về một cổng trên máy này không còn ai lắng
/// nghe: mọi kết nối qua proxy sẽ thất bại.
fn dead_proxy() -> Option<Finding> {
    let enabled = reg::read(false, INTERNET_SETTINGS, "ProxyEnable")?.as_dword()? == 1;
    let server = reg::read(false, INTERNET_SETTINGS, "ProxyServer")?.as_string()?;
    let address = local_proxy(first_proxy(&server)?)?;
    if !enabled || TcpStream::connect_timeout(&address, Duration::from_millis(700)).is_ok() {
        return None;
    }
    Some(Finding {
        id: String::new(),
        module: ModuleId::Internet,
        kind: "internet.deadProxy",
        name: None,
        detail: Some(server),
        count: 1,
        bytes: 0,
        recommended: true,
        undoable: true,
        needs_admin: false,
        action: Action::SetRegistryValue {
            machine: false,
            subkey: INTERNET_SETTINGS.to_string(),
            name: "ProxyEnable".to_string(),
            value: RawValue::dword(0),
        },
    })
}

impl Module for Internet {
    fn id(&self) -> ModuleId {
        ModuleId::Internet
    }

    fn scan(&self, ctx: &ScanContext) -> Vec<Finding> {
        ctx.report(Path::new(INTERNET_SETTINGS));
        let mut findings: Vec<Finding> = dead_proxy().into_iter().collect();
        findings.extend(scan_tweaks(ModuleId::Internet, INTERNET, ctx));
        findings
    }
}

// ---------------------------------------------------------------- Bảo mật

pub struct Security;

const FIREWALL: &str = "SYSTEM\\CurrentControlSet\\Services\\SharedAccess\\Parameters\\FirewallPolicy";

macro_rules! firewall {
    ($kind:literal, $profile:literal) => {
        Tweak {
            kind: $kind,
            machine: true,
            subkey: concat!(
                "SYSTEM\\CurrentControlSet\\Services\\SharedAccess\\Parameters\\FirewallPolicy\\",
                $profile
            ),
            name: "EnableFirewall",
            when: When::DwordIs(0),
            set: Set::Dword(1),
            recommended: true,
        }
    };
}

const SECURITY: &[Tweak] = &[
    // UAC bị tắt: mọi chương trình chạy thẳng với quyền quản trị.
    Tweak {
        kind: "security.uac",
        machine: true,
        subkey: "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Policies\\System",
        name: "EnableLUA",
        when: When::DwordIs(0),
        set: Set::Dword(1),
        recommended: true,
    },
    firewall!("security.firewallDomain", "DomainProfile"),
    firewall!("security.firewallPrivate", "StandardProfile"),
    firewall!("security.firewallPublic", "PublicProfile"),
    // SMBv1 là giao thức chia sẻ tệp cũ, bị WannaCry khai thác.
    Tweak {
        kind: "security.smb1",
        machine: true,
        subkey: "SYSTEM\\CurrentControlSet\\Services\\LanmanServer\\Parameters",
        name: "SMB1",
        when: When::DwordIs(1),
        set: Set::Dword(0),
        recommended: true,
    },
    // Hai mục dưới là tính năng người dùng có thể đang cần: không chọn sẵn.
    Tweak {
        kind: "security.remoteDesktop",
        machine: true,
        subkey: "SYSTEM\\CurrentControlSet\\Control\\Terminal Server",
        name: "fDenyTSConnections",
        when: When::DwordIs(0),
        set: Set::Dword(1),
        recommended: false,
    },
    Tweak {
        kind: "security.remoteAssistance",
        machine: true,
        subkey: "SYSTEM\\CurrentControlSet\\Control\\Remote Assistance",
        name: "fAllowToGetHelp",
        when: When::DwordIs(1),
        set: Set::Dword(0),
        recommended: false,
    },
    // Ẩn đuôi tệp giúp mã độc giả dạng tài liệu (ví dụ "hoadon.pdf.exe").
    Tweak {
        kind: "security.hiddenExtensions",
        machine: false,
        subkey: "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced",
        name: "HideFileExt",
        when: When::DwordIs(1),
        set: Set::Dword(0),
        recommended: false,
    },
];

impl Module for Security {
    fn id(&self) -> ModuleId {
        ModuleId::Security
    }

    fn scan(&self, ctx: &ScanContext) -> Vec<Finding> {
        ctx.report(Path::new(FIREWALL));
        scan_tweaks(ModuleId::Security, SECURITY, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_server_formats() {
        assert_eq!(first_proxy("127.0.0.1:8888"), Some("127.0.0.1:8888"));
        assert_eq!(first_proxy("http=127.0.0.1:8080;https=127.0.0.1:8081"), Some("127.0.0.1:8080"));
        assert_eq!(first_proxy("http://localhost:3128"), Some("localhost:3128"));
        assert_eq!(first_proxy(""), None);
    }

    #[test]
    fn only_loopback_proxies_are_checked() {
        assert!(local_proxy("127.0.0.1:8888").is_some());
        assert!(local_proxy("proxy.company.local:8080").is_none());
        assert!(local_proxy("10.0.0.5:3128").is_none());
        assert!(local_proxy("127.0.0.1").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn tweak_conditions_follow_registry_value() {
        const KEY: &str = "Software\\TP System Care Test\\tweaks";
        let tweak = |when| Tweak {
            kind: "test",
            machine: false,
            subkey: KEY,
            name: "Value",
            when,
            set: Set::Dword(0),
            recommended: true,
        };
        reg::delete_key(false, KEY).unwrap();

        assert!(!tweak(When::DwordIs(1)).needs_fix());
        assert!(tweak(When::DwordIsNot { want: 0, absent: true }).needs_fix());
        assert!(!tweak(When::DwordIsNot { want: 0, absent: false }).needs_fix());
        assert!(tweak(When::NumberAbove { limit: 100, default: 400 }).needs_fix());

        reg::write(false, KEY, "Value", &RawValue::dword(1)).unwrap();
        assert!(tweak(When::DwordIs(1)).needs_fix());
        assert!(tweak(When::DwordIsNot { want: 0, absent: false }).needs_fix());

        reg::write(false, KEY, "Value", &RawValue::sz("100")).unwrap();
        assert!(!tweak(When::NumberAbove { limit: 100, default: 400 }).needs_fix());
        reg::write(false, KEY, "Value", &RawValue::sz("400")).unwrap();
        assert!(tweak(When::NumberAbove { limit: 100, default: 400 }).needs_fix());

        reg::delete_key(false, KEY).unwrap();
    }
}
