//! Lần mở gần nhất của ứng dụng, đọc từ UserAssist của người dùng hiện tại.
//!
//! UserAssist là nơi Explorer ghi lại chương trình và lối tắt người dùng mở từ
//! Start, thanh tác vụ, màn hình nền hay Explorer. Nó không ghi chương trình chạy
//! từ dòng lệnh, tự khởi động hay do chương trình khác mở, và người dùng có thể
//! tắt nó. Vì vậy "không thấy" không có nghĩa là "chưa từng dùng".
//!
//! Chỉ đọc registry, không cần quyền quản trị.

use std::path::{Path, PathBuf};

use super::uninstall::{AppInfo, AppKind};
use crate::reg;
use crate::winutil::{self, Known};

const USER_ASSIST: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\UserAssist";
const EXECUTABLES: &str = "{CEBFF5CD-ACE2-4F4F-9178-9926F41749EA}";
const SHORTCUTS: &str = "{F4E57C4B-2036-45F0-A9AB-443BCFE33D9F}";
const RECORD_SIZE: usize = 72;
const NAME_LIMIT: usize = 2048;
const MAX_VALUES: u32 = 4096;
const FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;

/// Giây giữa mốc FILETIME (1601) và mốc Unix (1970).
const FILETIME_UNIX_OFFSET: u64 = 11_644_473_600;

/// Một mục UserAssist đã giải mã.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub target: Target,
    /// Giây Unix.
    pub last_used: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Path(PathBuf),
    /// AppUserModelID của ứng dụng Store: `<PackageFamilyName>!<App>`.
    App(String),
}

/// Tên giá trị UserAssist được mã hóa ROT13 (chỉ chữ cái).
pub fn rot13(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a') + 13) % 26 + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A') + 13) % 26 + b'A') as char,
            _ => c,
        })
        .collect()
}

/// Thời điểm chạy cuối (giây Unix) trong dữ liệu UserAssist dạng Windows 7 trở lên
/// (72 byte, FILETIME ở vị trí 60). `None` nếu chưa từng chạy hoặc không hợp lệ.
pub fn last_run(bytes: &[u8]) -> Option<u64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    last_run_at(bytes, now.as_secs())
}

fn last_run_at(bytes: &[u8], now: u64) -> Option<u64> {
    if bytes.len() != RECORD_SIZE || u32::from_le_bytes(bytes[4..8].try_into().ok()?) == 0 {
        return None;
    }
    let filetime = u64::from_le_bytes(bytes[60..68].try_into().ok()?);
    let ticks = filetime.checked_sub(FILETIME_UNIX_OFFSET * FILETIME_TICKS_PER_SECOND)?;
    if ticks == 0 || ticks > now.checked_mul(FILETIME_TICKS_PER_SECOND)? {
        return None;
    }
    let seconds = ticks / FILETIME_TICKS_PER_SECOND;
    (seconds > 0).then_some(seconds)
}

/// Thư mục đặc biệt mà UserAssist ghi bằng GUID ở đầu đường dẫn.
fn known_prefix(guid: &str) -> Option<PathBuf> {
    let guid = guid.to_ascii_uppercase();
    let folder = |known| winutil::known_folder(known);
    match guid.as_str() {
        "{6D809377-6AF0-444B-8957-A3773F02200E}" => folder(Known::ProgramFiles),
        "{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}" => folder(Known::ProgramFilesX86),
        "{F38BF404-1D43-42F2-9305-67DE0B28FC23}" => folder(Known::Windows),
        "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}" => {
            folder(Known::Windows).map(|w| w.join("System32"))
        }
        "{D65231B0-B2F1-4857-A4CE-A8E7C6EA7D27}" => {
            folder(Known::Windows).map(|w| w.join("SysWOW64"))
        }
        "{A77F5D77-2E2B-44C3-A6A2-ABA601054A51}" => {
            folder(Known::StartMenu).map(|s| s.join("Programs"))
        }
        "{0139D44E-6AFE-49F2-8690-3DAFCAE6FFB8}" => {
            folder(Known::CommonStartMenu).map(|s| s.join("Programs"))
        }
        "{5CD7AEE2-2219-4A67-B85D-6C9CE15660CB}" => folder(Known::UserProgramFiles),
        _ => None,
    }
}

/// Chuyển tên giá trị (đã giải ROT13) thành đích: đường dẫn hoặc AppUserModelID.
fn resolve(name: &str, prefix: &dyn Fn(&str) -> Option<PathBuf>) -> Option<Target> {
    if name.len() > NAME_LIMIT || name.chars().any(char::is_control) {
        return None;
    }
    let path = if name.starts_with('{') {
        let end = name.find('}')?;
        let rest = name[end + 1..].strip_prefix('\\')?;
        if rest.is_empty() || rest.starts_with(['\\', '/']) || rest.contains(':') {
            return None;
        }
        prefix(&name[..=end])?.join(rest)
    } else if name.len() >= 3 && name.as_bytes()[1] == b':' {
        PathBuf::from(name)
    } else {
        store_family(name)?;
        return Some(Target::App(name.to_string()));
    };
    let raw = path.to_str()?;
    if raw.ends_with(['\\', '/']) {
        return None;
    }
    super::uninstall::usage_path(raw)?;
    Some(Target::Path(path))
}

fn store_family(id: &str) -> Option<&str> {
    if id.len() > NAME_LIMIT {
        return None;
    }
    let (family, app) = id.split_once('!')?;
    let (name, publisher) = family.split_once('_')?;
    let word = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-'))
    };
    (word(name)
        && !publisher.is_empty()
        && publisher.bytes().all(|c| c.is_ascii_alphanumeric())
        && !app.is_empty()
        && app
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_')))
    .then_some(family)
}

fn path_has_extension(path: &Path, extension: &str) -> bool {
    path.to_str().is_some_and(|raw| {
        !raw.ends_with(['\\', '/'])
            && super::uninstall::usage_path(raw).is_some_and(|path| path.ends_with(extension))
    })
}

fn decode_entry(
    bucket: &str,
    version: u32,
    name: &str,
    value: &reg::RawValue,
    now: u64,
    prefix: &dyn Fn(&str) -> Option<PathBuf>,
    shortcut: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Option<Entry> {
    if ![EXECUTABLES, SHORTCUTS].contains(&bucket)
        || version != 5
        || value.vtype != reg::REG_BINARY
        || name.len() > NAME_LIMIT
    {
        return None;
    }
    let last_used = last_run_at(&value.bytes, now)?;
    let target = match resolve(&rot13(name), prefix)? {
        Target::Path(path) if bucket == SHORTCUTS && path_has_extension(&path, ".lnk") => {
            let resolved = shortcut(&path)?;
            if !path_has_extension(&resolved, ".exe") {
                return None;
            }
            Target::Path(resolved)
        }
        Target::Path(path) if bucket == EXECUTABLES && path_has_extension(&path, ".exe") => {
            Target::Path(path)
        }
        Target::App(id) if bucket == EXECUTABLES => Target::App(id),
        _ => return None,
    };
    Some(Entry { target, last_used })
}

fn encrypted_names(no_encrypt: Result<Option<u32>, ()>) -> bool {
    matches!(no_encrypt, Ok(None | Some(0)))
}

/// Đọc mọi mục UserAssist có thời điểm chạy. Lối tắt `.lnk` được đổi sang đích.
#[cfg(windows)]
pub fn entries() -> Vec<Entry> {
    use windows::core::{w, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS,
    };
    use windows::Win32::System::Registry::{
        RegEnumValueW, RegQueryValueExW, HKEY, REG_DWORD, REG_VALUE_TYPE,
    };
    use winreg::{
        enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE},
        RegKey,
    };

    // reg::read cannot distinguish a missing value from an unreadable setting.
    fn dword(key: &RegKey, name: PCWSTR) -> Result<Option<u32>, ()> {
        let mut bytes = [0u8; 4];
        let mut size = bytes.len() as u32;
        let mut kind = REG_VALUE_TYPE::default();
        let result = unsafe {
            RegQueryValueExW(
                HKEY(key.raw_handle()),
                name,
                None,
                Some(&mut kind),
                Some(bytes.as_mut_ptr()),
                Some(&mut size),
            )
        };
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if result != ERROR_SUCCESS || kind != REG_DWORD || size != 4 {
            return Err(());
        }
        Ok(Some(u32::from_le_bytes(bytes)))
    }

    let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return Vec::new();
    };
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    match current_user.open_subkey_with_flags(format!("{USER_ASSIST}\\Settings"), KEY_QUERY_VALUE) {
        Ok(settings) => {
            if !encrypted_names(dword(&settings, w!("NoEncrypt"))) {
                return Vec::new();
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Vec::new(),
    }
    let mut found = Vec::new();
    for guid in [EXECUTABLES, SHORTCUTS] {
        let Ok(bucket) =
            current_user.open_subkey_with_flags(format!("{USER_ASSIST}\\{guid}"), KEY_QUERY_VALUE)
        else {
            continue;
        };
        if dword(&bucket, w!("Version")) != Ok(Some(5)) {
            continue;
        }
        let Ok(count) = bucket.open_subkey_with_flags("Count", KEY_QUERY_VALUE) else {
            continue;
        };
        for index in 0..MAX_VALUES {
            let mut name = [0u16; NAME_LIMIT + 1];
            let mut name_len = name.len() as u32;
            let mut data = [0u8; RECORD_SIZE];
            let mut data_len = data.len() as u32;
            let mut kind = 0u32;
            let result = unsafe {
                RegEnumValueW(
                    HKEY(count.raw_handle()),
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut name_len,
                    None,
                    Some(&mut kind),
                    Some(data.as_mut_ptr()),
                    Some(&mut data_len),
                )
            };
            if result == ERROR_NO_MORE_ITEMS {
                break;
            }
            if result == ERROR_MORE_DATA {
                continue;
            }
            if result != ERROR_SUCCESS {
                break;
            }
            if name_len as usize >= name.len() || data_len as usize != RECORD_SIZE {
                continue;
            }
            let Ok(name) = String::from_utf16(&name[..name_len as usize]) else {
                continue;
            };
            let value = reg::RawValue {
                vtype: kind,
                bytes: data.to_vec(),
            };
            if let Some(entry) = decode_entry(
                guid,
                5,
                &name,
                &value,
                now.as_secs(),
                &known_prefix,
                &winutil::shortcut_target,
            ) {
                found.push(entry);
            }
        }
    }
    found
}

#[cfg(not(windows))]
pub fn entries() -> Vec<Entry> {
    Vec::new()
}

/// Store only: require one exact package family; desktop ownership stays in uninstall.
pub fn assign_store(apps: &mut [AppInfo], entries: &[Entry]) {
    for app in apps.iter_mut().filter(|app| app.kind == AppKind::Store) {
        app.last_used = None;
    }
    for entry in entries {
        let Target::App(id) = &entry.target else {
            continue;
        };
        let Some(family) = store_family(id) else {
            continue;
        };
        let mut matches = apps.iter().enumerate().filter(|(_, app)| {
            app.kind == AppKind::Store
                && app
                    .package
                    .as_ref()
                    .is_some_and(|p| p.family_name.eq_ignore_ascii_case(family))
        });
        let Some((index, _)) = matches.next() else {
            continue;
        };
        if matches.next().is_some() || entry.last_used == 0 {
            continue;
        }
        let app = &mut apps[index];
        app.last_used = Some(
            app.last_used
                .map_or(entry.last_used, |known| known.max(entry.last_used)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_names_and_timestamps() {
        assert_eq!(
            rot13("{6Q809377-6NS0-444O}\\7-Mvc\\7mSZ.rkr"),
            "{6D809377-6AF0-444B}\\7-Zip\\7zFM.exe"
        );
        let bytes = record(1_700_000_000).bytes;
        assert_eq!(last_run_at(&bytes, 1_800_000_000), Some(1_700_000_000));
        assert_eq!(last_run(&bytes), Some(1_700_000_000));
        assert_eq!(last_run(&[0u8; 72]), None);
        assert_eq!(last_run(&[0u8; 16]), None);
    }

    #[test]
    fn resolves_known_folders_paths_and_store_ids() {
        let prefix = |guid: &str| (guid == "{PF}").then(|| PathBuf::from("C:\\Program Files"));
        assert_eq!(
            resolve("{PF}\\7-Zip\\7zFM.exe", &prefix),
            Some(Target::Path(
                PathBuf::from("C:\\Program Files").join("7-Zip\\7zFM.exe")
            ))
        );
        assert_eq!(resolve("{UNKNOWN}\\x.exe", &prefix), None);
        assert_eq!(
            resolve("D:\\Games\\game.exe", &prefix),
            Some(Target::Path(PathBuf::from("D:\\Games\\game.exe")))
        );
        assert_eq!(
            resolve("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", &prefix),
            Some(Target::App(
                "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into()
            ))
        );
        assert_eq!(resolve("UEME_CTLSESSION", &prefix), None);
        assert_eq!(resolve("Microsoft.Windows.Explorer", &prefix), None);
    }

    fn desktop(id: &str, location: Option<&str>) -> AppInfo {
        AppInfo {
            id: id.into(),
            kind: AppKind::Desktop,
            built_in: false,
            removable: true,
            name: id.into(),
            publisher: None,
            version: None,
            install_date: None,
            installed_together: Vec::new(),
            size: None,
            install_location: location.map(str::to_string),
            needs_admin: false,
            installed_at: None,
            companions: Vec::new(),
            last_used: None,
            machine: true,
            key: String::new(),
            uninstall: String::new(),
            msi: false,
            package: None,
            synthetic: false,
        }
    }

    fn store(id: &str, family: &str) -> AppInfo {
        let mut app = desktop(id, None);
        app.kind = AppKind::Store;
        app.package = Some(super::super::store_apps::Package {
            name: id.into(),
            display_name: id.into(),
            publisher: "Vendor".into(),
            version: "1.0.0.0".into(),
            full_name: id.into(),
            family_name: family.into(),
            install_location: None,
            built_in: false,
            removable: true,
        });
        app
    }

    fn store_entry(id: &str, last_used: u64) -> Entry {
        Entry {
            target: Target::App(id.into()),
            last_used,
        }
    }

    #[test]
    fn store_matches_only_exact_unique_family_and_clears_stale_usage() {
        let mut apps = [
            store("calculator", "Microsoft.WindowsCalculator_8wekyb3d8bbwe"),
            store("prefix", "Microsoft.WindowsCalculator_8wekyb3d8bbwe2"),
            store("other", "Other_pub"),
        ];
        for app in &mut apps {
            app.last_used = Some(999);
        }
        assign_store(
            &mut apps,
            &[
                store_entry("microsoft.windowscalculator_8WEKYB3D8BBWE!App", 100),
                store_entry("Other_pub", 500),
                store_entry("Other_pub!", 500),
                store_entry("Other_pub!App!Extra", 500),
            ],
        );
        assert_eq!(apps[0].last_used, Some(100));
        assert_eq!(apps[1].last_used, None);
        assert_eq!(apps[2].last_used, None);
        assign_store(&mut apps, &[]);
        assert!(apps.iter().all(|app| app.last_used.is_none()));
    }

    #[test]
    fn store_rejects_ambiguous_families_regardless_of_inventory_order() {
        let mut apps = [store("one", "Shared_pub"), store("two", "SHARED_PUB")];
        for app in &mut apps {
            app.last_used = Some(999);
        }
        let entries = [store_entry("Shared_pub!App", 100)];
        assign_store(&mut apps, &entries);
        assert!(apps.iter().all(|app| app.last_used.is_none()));
        apps.reverse();
        assign_store(&mut apps, &entries);
        assert!(apps.iter().all(|app| app.last_used.is_none()));
    }

    #[test]
    fn store_keeps_latest_across_app_ids_in_one_family() {
        let mut apps = [store("one", "Shared_pub")];
        assign_store(
            &mut apps,
            &[
                store_entry("Shared_pub!First", 100),
                store_entry("Shared_pub!Second", 300),
                store_entry("Shared_pub!First", 200),
            ],
        );
        assert_eq!(apps[0].last_used, Some(300));
    }

    #[test]
    fn store_assignment_never_touches_desktop_usage_or_uses_paths() {
        let mut desktop_with_package = store("desktop", "Shared_pub");
        desktop_with_package.kind = AppKind::Desktop;
        desktop_with_package.last_used = Some(70);
        let mut apps = [
            desktop("suite", Some("C:\\Apps\\Suite")),
            desktop("child", Some("C:\\Apps\\Suite\\Child")),
            desktop_with_package,
            store("store", "Shared_pub"),
        ];
        apps[3].install_location = Some("C:\\Apps\\Suite\\Child".into());
        assign_store(
            &mut apps,
            &[
                Entry {
                    target: Target::Path("C:\\Apps\\Suite\\Child\\child.exe".into()),
                    last_used: 900,
                },
                store_entry("Shared_pub!App", 100),
            ],
        );
        assert_eq!(
            apps.iter().map(|app| app.last_used).collect::<Vec<_>>(),
            [None, None, Some(70), Some(100)]
        );
        assign_store(&mut apps, &[]);
        assert_eq!(apps[2].last_used, Some(70));
        assert_eq!(apps[3].last_used, None);
    }

    fn record(seconds: u64) -> reg::RawValue {
        let mut bytes = vec![0; RECORD_SIZE];
        bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
        let filetime = (seconds + FILETIME_UNIX_OFFSET) * FILETIME_TICKS_PER_SECOND;
        bytes[60..68].copy_from_slice(&filetime.to_le_bytes());
        reg::RawValue {
            vtype: reg::REG_BINARY,
            bytes,
        }
    }

    #[test]
    fn last_run_rejects_malformed_zero_count_and_future_records() {
        let good = record(100).bytes;
        assert_eq!(last_run_at(&good, 100), Some(100));
        for size in [0, 16, 60, 67, 68, 71, 73, 80, 1612] {
            let mut bytes = good.clone();
            bytes.resize(size, 0);
            assert_eq!(last_run_at(&bytes, 200), None, "{size}");
        }
        let mut bytes = good.clone();
        bytes[4..8].fill(0);
        assert_eq!(last_run_at(&bytes, 200), None);
        let epoch = FILETIME_UNIX_OFFSET * FILETIME_TICKS_PER_SECOND;
        for filetime in [
            0,
            epoch - 1,
            epoch,
            epoch + 1,
            epoch + 200 * FILETIME_TICKS_PER_SECOND + 1,
            u64::MAX,
        ] {
            let mut bytes = good.clone();
            bytes[60..68].copy_from_slice(&filetime.to_le_bytes());
            assert_eq!(last_run_at(&bytes, 200), None, "{filetime}");
        }
        assert_eq!(last_run_at(&good, 0), None);
        assert_eq!(last_run_at(&good, u64::MAX), None);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(last_run(&record(now + 3600).bytes), None);
    }

    #[test]
    fn entry_schema_and_encryption_must_be_known() {
        assert!(encrypted_names(Ok(None)));
        assert!(encrypted_names(Ok(Some(0))));
        for setting in [Ok(Some(1)), Ok(Some(2)), Ok(Some(u32::MAX)), Err(())] {
            assert!(!encrypted_names(setting));
        }
        let name = rot13("C:\\Apps\\Tool\\tool.exe");
        let value = record(100);
        let no_prefix = |_: &str| None;
        let no_shortcut = |_: &Path| panic!("must not resolve shortcuts");
        for (bucket, version, kind) in [
            ("{UNKNOWN}", 5, reg::REG_BINARY),
            (SHORTCUTS, 5, reg::REG_BINARY),
            (EXECUTABLES, 0, reg::REG_BINARY),
            (EXECUTABLES, 3, reg::REG_BINARY),
            (EXECUTABLES, 6, reg::REG_BINARY),
            (EXECUTABLES, 5, reg::REG_SZ),
            (EXECUTABLES, 5, reg::REG_DWORD),
        ] {
            let mut invalid = value.clone();
            invalid.vtype = kind;
            assert!(decode_entry(
                bucket,
                version,
                &name,
                &invalid,
                200,
                &no_prefix,
                &no_shortcut
            )
            .is_none());
        }
        let long = rot13(&format!("C:\\{}\\tool.exe", "x".repeat(NAME_LIMIT)));
        assert!(
            decode_entry(EXECUTABLES, 5, &long, &value, 200, &no_prefix, &no_shortcut).is_none()
        );
        assert!(decode_entry(
            EXECUTABLES,
            5,
            &name,
            &record(201),
            200,
            &no_prefix,
            &no_shortcut
        )
        .is_none());
    }

    #[test]
    fn entries_resolve_known_guid_shortcuts_and_store_ids_without_launching() {
        let prefix = |guid: &str| (guid == "{PF}").then(|| PathBuf::from("C:\\Program Files"));
        let shortcut = |path: &Path| {
            assert_eq!(path, Path::new("C:\\Start\\Tool.lnk"));
            Some(PathBuf::from("C:\\Apps\\Tool\\tool.exe"))
        };
        let value = record(100);
        let executable = decode_entry(
            EXECUTABLES,
            5,
            &rot13("{PF}\\Tool\\tool.exe"),
            &value,
            200,
            &prefix,
            &shortcut,
        )
        .unwrap();
        assert_eq!(
            executable.target,
            Target::Path(PathBuf::from("C:\\Program Files").join("Tool\\tool.exe"))
        );
        let link_name = rot13("C:\\Start\\Tool.lnk");
        let link = decode_entry(SHORTCUTS, 5, &link_name, &value, 200, &prefix, &shortcut).unwrap();
        assert_eq!(link.target, Target::Path("C:\\Apps\\Tool\\tool.exe".into()));
        assert_eq!(link.last_used, 100);
        for target in [
            "C:\\Apps\\Tool\\tool.cmd",
            "C:\\Apps\\Tool\\..\\other.exe",
            "\\\\server\\share\\tool.exe",
            "C:\\Apps\\Tool\\tool.exe\\",
        ] {
            assert!(
                decode_entry(SHORTCUTS, 5, &link_name, &value, 200, &prefix, &|_| Some(
                    target.into()
                ))
                .is_none()
            );
        }
        assert!(decode_entry(SHORTCUTS, 5, &link_name, &value, 200, &prefix, &|_| None).is_none());
        assert!(
            decode_entry(EXECUTABLES, 5, &link_name, &value, 200, &prefix, &shortcut).is_none()
        );
        let id = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
        let store =
            decode_entry(EXECUTABLES, 5, &rot13(id), &value, 200, &prefix, &shortcut).unwrap();
        assert_eq!(store.target, Target::App(id.into()));
        assert!(decode_entry(SHORTCUTS, 5, &rot13(id), &value, 200, &prefix, &shortcut).is_none());
        for invalid in [
            "{PF}tool.exe",
            "{PF}\\\\tool.exe",
            "{PF}\\D:\\tool.exe",
            "{PF}\\..\\tool.exe",
            "C:tool.exe",
            "C:\\Apps\\Tool\\tool.exe\\",
            "Shared_pub!App!Extra",
            "Shared_pub!App\\x",
            "_pub!App",
            "Shared_!App",
        ] {
            assert!(resolve(invalid, &prefix).is_none(), "{invalid}");
        }
    }
}
