//! Xóa dấu vết riêng tư: tệp mở gần đây, lịch sử hộp Run và thanh địa chỉ
//! Explorer, lịch sử và cookie trình duyệt.
//!
//! Cố ý không đụng tới: Jump List (`AutomaticDestinations` chứa cả mục ghim của
//! Quick Access) và `places.sqlite` của Firefox (chứa cả dấu trang).

use crate::engine::{Action, Finding, Module, ModuleId, ScanContext};
use crate::modules::{chromium_profiles, firefox_profiles, FileSet, Source};
use crate::winutil::{self, Known};

pub struct Privacy;

const EXPLORER: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer";

fn file_finding(kind: &'static str, recommended: bool, set: FileSet) -> Option<Finding> {
    if set.files.is_empty() {
        return None;
    }
    Some(Finding {
        id: String::new(),
        module: ModuleId::Privacy,
        kind,
        name: None,
        detail: set.roots.first().map(|p| p.to_string_lossy().to_string()),
        count: set.files.len() as u64,
        bytes: set.bytes,
        recommended,
        undoable: false,
        needs_admin: false,
        action: Action::DeleteFiles {
            roots: set.roots,
            files: set.files,
        },
    })
}

fn registry_finding(kind: &'static str, key: &str) -> Option<Finding> {
    let subkey = format!("{EXPLORER}\\{key}");
    let count = count_values(&subkey);
    if count == 0 {
        return None;
    }
    Some(Finding {
        id: String::new(),
        module: ModuleId::Privacy,
        kind,
        name: None,
        detail: Some(format!("HKCU\\{subkey}")),
        count,
        bytes: 0,
        recommended: true,
        undoable: false,
        needs_admin: false,
        action: Action::ClearRegistryValues { subkey },
    })
}

impl Module for Privacy {
    fn id(&self) -> ModuleId {
        ModuleId::Privacy
    }

    fn scan(&self, ctx: &ScanContext) -> Vec<Finding> {
        let mut findings = Vec::new();

        if let Some(recent) = winutil::known_folder(Known::Recent) {
            let mut set = FileSet::default();
            set.collect(ctx, &Source::dir(recent).flat(&[]));
            set.files.retain(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")));
            if !set.files.is_empty() {
                set.bytes = set
                    .files
                    .iter()
                    .filter_map(|f| std::fs::metadata(f).ok())
                    .map(|m| m.len())
                    .sum();
            }
            findings.extend(file_finding("privacy.recentDocs", true, set));
        }

        findings.extend(registry_finding("privacy.runHistory", "RunMRU"));
        findings.extend(registry_finding("privacy.typedPaths", "TypedPaths"));

        // Lịch sử và cookie trình duyệt: không chọn sẵn vì xóa cookie sẽ đăng xuất
        // khỏi các trang web, và nhiều người muốn giữ lịch sử duyệt.
        let chromium = chromium_profiles();
        let mut history = FileSet::default();
        let mut cookies = FileSet::default();
        for profile in &chromium {
            if ctx.cancelled() {
                return findings;
            }
            history.add_named(
                ctx,
                profile,
                &["History", "History-journal", "Visited Links", "Top Sites", "Top Sites-journal"],
            );
            cookies.add_named(ctx, &profile.join("Network"), &["Cookies", "Cookies-journal"]);
        }
        for profile in firefox_profiles(Known::RoamingAppData) {
            cookies.add_named(
                ctx,
                &profile,
                &["cookies.sqlite", "cookies.sqlite-wal", "cookies.sqlite-shm"],
            );
        }
        findings.extend(file_finding("privacy.browserHistory", false, history));
        findings.extend(file_finding("privacy.browserCookies", false, cookies));

        findings
    }
}

#[cfg(windows)]
fn count_values(subkey: &str) -> u64 {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(subkey)
        .map(|key| key.enum_values().filter_map(Result::ok).count() as u64)
        .unwrap_or(0)
}

#[cfg(not(windows))]
fn count_values(_subkey: &str) -> u64 {
    0
}

/// Xóa mọi giá trị của khóa `HKCU\<subkey>`; trả về số giá trị đã xóa.
#[cfg(windows)]
pub fn clear_values(subkey: &str) -> Result<u64, String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE};
    use winreg::RegKey;

    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(subkey, KEY_QUERY_VALUE | KEY_SET_VALUE)
        .map_err(|e| e.to_string())?;
    let names: Vec<String> = key
        .enum_values()
        .filter_map(Result::ok)
        .map(|(name, _)| name)
        .collect();
    let mut removed = 0;
    for name in names {
        if key.delete_value(&name).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(not(windows))]
pub fn clear_values(_subkey: &str) -> Result<u64, String> {
    Err("chỉ hỗ trợ Windows".into())
}
