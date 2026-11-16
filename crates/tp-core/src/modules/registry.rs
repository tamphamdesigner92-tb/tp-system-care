//! Dọn registry với luật thận trọng: chỉ gỡ các mục trỏ tới tệp chắc chắn không
//! còn trên ổ cứng cố định, ở những nơi mà mục thừa không được Windows dùng tới.
//! Mọi mục bị xóa đều được lưu trong Rescue Center và hoàn tác được.
//!
//! Cố ý không đụng tới: lớp COM/CLSID, liên kết kiểu tệp, khóa Uninstall, dịch vụ
//! — xóa nhầm ở đó có thể làm hỏng ứng dụng.

use std::path::{Path, PathBuf};

use crate::engine::{Action, Finding, Module, ModuleId, ScanContext};
use crate::modules::shortcut::is_missing;
use crate::reg;
use crate::winutil::{self, Known};

pub struct Registry;

const SHARED_DLLS: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\SharedDLLs";
const APP_PATHS: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths";
const MUI_CACHE: &str = "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\Shell\\MuiCache";
const COMPAT_STORE: &str =
    "Software\\Microsoft\\Windows NT\\CurrentVersion\\AppCompatFlags\\Compatibility Assistant\\Store";

/// Tệp chắc chắn đã mất và không nằm trong thư mục Windows (nơi đường dẫn có thể
/// bị chuyển hướng giữa System32 và SysWOW64).
fn orphan(path: &Path, windows: Option<&PathBuf>) -> bool {
    !windows.is_some_and(|w| path.starts_with(w)) && is_missing(path)
}

/// Bỏ ngoặc kép và khoảng trắng quanh một đường dẫn lưu trong registry.
fn clean_path(raw: &str) -> PathBuf {
    PathBuf::from(winutil::expand_env(raw.trim().trim_matches('"')))
}

/// Tên giá trị MuiCache có dạng `<đường dẫn>.FriendlyAppName` hoặc `.ApplicationCompany`.
fn mui_cache_path(name: &str) -> Option<&str> {
    name.strip_suffix(".FriendlyAppName")
        .or_else(|| name.strip_suffix(".ApplicationCompany"))
}

fn value_finding(kind: &'static str, machine: bool, subkey: &str, names: Vec<String>) -> Option<Finding> {
    if names.is_empty() {
        return None;
    }
    Some(Finding {
        id: String::new(),
        module: ModuleId::Registry,
        kind,
        name: None,
        detail: Some(format!("{}\\{}", reg::hive_name(machine), subkey)),
        count: names.len() as u64,
        bytes: 0,
        recommended: true,
        undoable: true,
        needs_admin: machine,
        action: Action::DeleteRegistryValues {
            machine,
            subkey: subkey.to_string(),
            names,
        },
    })
}

impl Module for Registry {
    fn id(&self) -> ModuleId {
        ModuleId::Registry
    }

    fn scan(&self, ctx: &ScanContext) -> Vec<Finding> {
        let windows = winutil::known_folder(Known::Windows);
        let windows = windows.as_ref();
        let mut findings = Vec::new();

        // Bộ đếm DLL dùng chung: tên giá trị là đường dẫn tệp.
        ctx.report(Path::new(SHARED_DLLS));
        let names = reg::values(true, SHARED_DLLS)
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| orphan(&clean_path(name), windows))
            .collect();
        findings.extend(value_finding("registry.sharedDlls", true, SHARED_DLLS, names));
        if ctx.cancelled() {
            return findings;
        }

        // App Paths: mỗi khóa con là một tên .exe, giá trị mặc định là đường dẫn.
        for machine in [true, false] {
            ctx.report(Path::new(APP_PATHS));
            let subkeys: Vec<String> = reg::subkeys(machine, APP_PATHS)
                .into_iter()
                .map(|name| format!("{APP_PATHS}\\{name}"))
                .filter(|subkey| {
                    !reg::has_subkeys(machine, subkey)
                        && reg::read(machine, subkey, "")
                            .and_then(|value| value.as_string())
                            .is_some_and(|path| orphan(&clean_path(&path), windows))
                })
                .collect();
            if !subkeys.is_empty() {
                findings.push(Finding {
                    id: String::new(),
                    module: ModuleId::Registry,
                    kind: "registry.appPaths",
                    name: None,
                    detail: Some(format!("{}\\{}", reg::hive_name(machine), APP_PATHS)),
                    count: subkeys.len() as u64,
                    bytes: 0,
                    recommended: true,
                    undoable: true,
                    needs_admin: machine,
                    action: Action::DeleteRegistryKeys { machine, subkeys },
                });
            }
        }
        if ctx.cancelled() {
            return findings;
        }

        // Bộ nhớ đệm tên ứng dụng của shell.
        ctx.report(Path::new(MUI_CACHE));
        let names = reg::values(false, MUI_CACHE)
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| mui_cache_path(name).is_some_and(|path| orphan(&clean_path(path), windows)))
            .collect();
        findings.extend(value_finding("registry.muiCache", false, MUI_CACHE, names));

        // Danh sách chương trình đã chạy của Trợ lý tương thích: tên giá trị là đường dẫn.
        ctx.report(Path::new(COMPAT_STORE));
        let names = reg::values(false, COMPAT_STORE)
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| orphan(&clean_path(name), windows))
            .collect();
        findings.extend(value_finding("registry.compatStore", false, COMPAT_STORE, names));

        findings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mui_cache_names_are_split() {
        assert_eq!(mui_cache_path("C:\\A\\b.exe.FriendlyAppName"), Some("C:\\A\\b.exe"));
        assert_eq!(mui_cache_path("C:\\A\\b.exe.ApplicationCompany"), Some("C:\\A\\b.exe"));
        assert_eq!(mui_cache_path("LangID"), None);
    }

    #[test]
    fn clean_path_strips_quotes() {
        assert_eq!(clean_path(" \"C:\\A b\\c.exe\" "), PathBuf::from("C:\\A b\\c.exe"));
    }

    #[cfg(windows)]
    #[test]
    fn orphan_ignores_windows_dir_and_existing_files() {
        let windows = winutil::known_folder(Known::Windows);
        let exe = std::env::current_exe().unwrap();
        assert!(!orphan(&exe, windows.as_ref()));
        if let Some(w) = &windows {
            assert!(!orphan(&w.join("System32\\tp-no-such-file.dll"), windows.as_ref()));
            let drive = w.to_string_lossy()[..3].to_string();
            assert!(orphan(Path::new(&format!("{drive}tp-no-such-dir\\x.dll")), windows.as_ref()));
        }
    }
}
