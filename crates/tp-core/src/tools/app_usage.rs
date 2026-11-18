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
    if bytes.len() < 68 {
        return None;
    }
    let filetime = u64::from_le_bytes(bytes[60..68].try_into().ok()?);
    let seconds = (filetime / 10_000_000).checked_sub(FILETIME_UNIX_OFFSET)?;
    // Bỏ giá trị rỗng hoặc vô lý (trước 2000).
    (seconds > 946_684_800).then_some(seconds)
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
    if name.starts_with('{') {
        let end = name.find('}')?;
        let rest = name[end + 1..].trim_start_matches('\\');
        return prefix(&name[..=end]).map(|base| Target::Path(base.join(rest)));
    }
    if name.len() >= 3 && name.as_bytes()[1] == b':' {
        return Some(Target::Path(PathBuf::from(name)));
    }
    // AppUserModelID của Store: `Family_publisherId!App`.
    let (family, app) = name.split_once('!')?;
    (family.contains('_') && !app.is_empty() && !family.contains('\\'))
        .then(|| Target::App(name.to_string()))
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_start_matches("\\\\?\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// Đọc mọi mục UserAssist có thời điểm chạy. Lối tắt `.lnk` được đổi sang đích.
#[cfg(windows)]
pub fn entries() -> Vec<Entry> {
    let mut found = Vec::new();
    for guid in reg::subkeys(false, USER_ASSIST) {
        let key = format!("{USER_ASSIST}\\{guid}\\Count");
        for (name, value) in reg::values(false, &key) {
            let Some(last_used) = last_run(&value.bytes) else {
                continue;
            };
            let Some(target) = resolve(&rot13(&name), &known_prefix) else {
                continue;
            };
            let target = match target {
                Target::Path(path)
                    if path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("lnk")) =>
                {
                    match winutil::shortcut_target(&path) {
                        Some(resolved) => Target::Path(resolved),
                        None => continue,
                    }
                }
                other => other,
            };
            found.push(Entry { target, last_used });
        }
    }
    found
}

#[cfg(not(windows))]
pub fn entries() -> Vec<Entry> {
    Vec::new()
}

/// Gán `last_used` cho ứng dụng. Đường dẫn thuộc về ứng dụng có thư mục cài dài
/// nhất chứa nó, để thư mục hãng dùng chung (ví dụ `Program Files\Adobe`) không
/// nhận nhầm lần mở của ứng dụng con.
pub fn assign(apps: &mut [AppInfo], entries: &[Entry]) {
    let roots: Vec<(usize, String)> = apps
        .iter()
        .enumerate()
        .filter(|(_, app)| app.kind == AppKind::Desktop)
        .filter_map(|(index, app)| {
            let root = path_key(Path::new(app.install_location.as_deref()?));
            // Thư mục quá ngắn (gốc ổ đĩa, `C:\Program Files`) không đủ để nhận diện.
            (root.matches('\\').count() >= 2
                || (root.matches('\\').count() == 1 && !generic_root(&root)))
            .then_some((index, root))
        })
        .collect();
    for entry in entries {
        let index = match &entry.target {
            Target::Path(path) => {
                let key = path_key(path);
                roots
                    .iter()
                    .filter(|(_, root)| key == *root || key.starts_with(&format!("{root}\\")))
                    .max_by_key(|(_, root)| root.len())
                    .map(|(index, _)| *index)
            }
            Target::App(id) => {
                let family = id.split('!').next().unwrap_or("").to_lowercase();
                apps.iter().position(|app| {
                    app.package
                        .as_ref()
                        .is_some_and(|p| p.family_name.to_lowercase() == family)
                })
            }
        };
        if let Some(index) = index {
            let app = &mut apps[index];
            app.last_used = Some(
                app.last_used
                    .map_or(entry.last_used, |known| known.max(entry.last_used)),
            );
        }
    }
}

/// `C:\Program Files`, `C:\Users`... : một cấp dưới gốc ổ nhưng dùng chung.
fn generic_root(root: &str) -> bool {
    let name = root.rsplit('\\').next().unwrap_or("");
    [
        "program files",
        "program files (x86)",
        "programdata",
        "users",
        "windows",
        "games",
        "apps",
        "tools",
    ]
    .contains(&name)
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
        let mut bytes = vec![0u8; 72];
        // 2026-11-19 00:00:00 UTC.
        let filetime = (1_795_046_400u64 + FILETIME_UNIX_OFFSET) * 10_000_000;
        bytes[60..68].copy_from_slice(&filetime.to_le_bytes());
        assert_eq!(last_run(&bytes), Some(1_795_046_400));
        assert_eq!(last_run(&[0u8; 72]), None);
        assert_eq!(last_run(&[0u8; 16]), None);
    }

    #[test]
    fn resolves_known_folders_paths_and_store_ids() {
        let prefix = |guid: &str| (guid == "{PF}").then(|| PathBuf::from("C:\\Program Files"));
        assert_eq!(
            resolve("{PF}\\7-Zip\\7zFM.exe", &prefix),
            Some(Target::Path(PathBuf::from(
                "C:\\Program Files\\7-Zip\\7zFM.exe"
            )))
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

    #[test]
    fn assigns_to_longest_install_root_and_keeps_latest() {
        let mut apps = vec![
            desktop("suite", Some("C:\\Program Files\\Adobe")),
            desktop(
                "bridge",
                Some("C:\\Program Files\\Adobe\\Adobe Bridge 2026\\"),
            ),
            desktop("zip", Some("c:/program files/7-zip")),
            desktop("loose", Some("C:\\Program Files")),
            desktop("game", Some("D:\\OldGame")),
            desktop("none", None),
        ];
        let at = |path: &str, last_used| Entry {
            target: Target::Path(PathBuf::from(path)),
            last_used,
        };
        assign(
            &mut apps,
            &[
                at(
                    "C:\\Program Files\\Adobe\\Adobe Bridge 2026\\Bridge.exe",
                    100,
                ),
                at("C:\\Program Files\\7-Zip\\7zFM.exe", 50),
                at("C:\\Program Files\\7-Zip\\7zG.exe", 70),
                at("C:\\Program Files\\7-Zip2\\x.exe", 900),
                at("C:\\Program Files\\Other\\x.exe", 900),
                at("D:\\OldGame\\game.exe", 30),
            ],
        );
        assert_eq!(apps[0].last_used, None);
        assert_eq!(apps[1].last_used, Some(100));
        assert_eq!(apps[2].last_used, Some(70));
        assert_eq!(apps[3].last_used, None);
        assert_eq!(apps[4].last_used, Some(30));
        assert_eq!(apps[5].last_used, None);
    }
}
