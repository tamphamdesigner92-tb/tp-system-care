//! Gỡ ứng dụng sạch: liệt kê ứng dụng đã cài, chạy trình gỡ gốc, rồi quét tàn dư.
//!
//! Luật tìm tàn dư cố ý hẹp: chỉ lấy thư mục và khóa registry khớp đúng tên ứng
//! dụng (hoặc `Hãng\Tên`) ở các vị trí cài đặt quen thuộc, cộng với thư mục cài
//! đặt mà ứng dụng tự khai báo. Tên chung chung như "Microsoft" không bao giờ bị
//! lấy. Thư mục dữ liệu trong AppData không được chọn sẵn vì có thể chứa dữ liệu
//! người dùng muốn giữ.
//!
//! Xóa tàn dư: thư mục vào Thùng rác; khóa registry được xuất ra tệp `.reg` trong
//! Rescue Center trước khi xóa nên hoàn tác được.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;

use crate::modules::startup::parse_command;
use crate::reg::{self, RawValue};
use crate::rescue::{self, Rescue, UndoOp};
use crate::winutil::{self, Known};

const UNINSTALL: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall";
const UNINSTALL_WOW64: &str = "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    /// `<HKLM|HKCU>\<khóa Uninstall>`, dùng làm mã.
    pub id: String,
    pub name: String,
    pub publisher: Option<String>,
    pub version: Option<String>,
    /// Dạng `YYYY-MM-DD`.
    pub install_date: Option<String>,
    /// Theo khai báo của ứng dụng (EstimatedSize); có thể thiếu hoặc không chính xác.
    pub size: Option<u64>,
    pub install_location: Option<String>,
    pub needs_admin: bool,
    #[serde(skip)]
    pub machine: bool,
    #[serde(skip)]
    pub key: String,
    #[serde(skip)]
    pub uninstall: String,
    #[serde(skip)]
    pub msi: bool,
}

fn text(values: &[(String, RawValue)], name: &str) -> Option<String> {
    values
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .and_then(|(_, v)| v.as_string())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn number(values: &[(String, RawValue)], name: &str) -> Option<u32> {
    values
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .and_then(|(_, v)| v.as_dword())
}

fn format_date(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    (digits.len() == 8).then(|| format!("{}-{}-{}", &digits[..4], &digits[4..6], &digits[6..]))
}

fn read_app(machine: bool, root: &str, sub: &str) -> Option<AppInfo> {
    let key = format!("{root}\\{sub}");
    let values = reg::values(machine, &key);
    let name = text(&values, "DisplayName")?;
    let uninstall = text(&values, "UninstallString")?;
    // Thành phần hệ thống, bản cập nhật con của ứng dụng khác: không liệt kê.
    if number(&values, "SystemComponent") == Some(1) || text(&values, "ParentKeyName").is_some() {
        return None;
    }
    if text(&values, "ReleaseType").is_some_and(|t| t.to_lowercase().contains("update") || t.eq_ignore_ascii_case("hotfix")) {
        return None;
    }
    Some(AppInfo {
        id: format!("{}\\{key}", reg::hive_name(machine)),
        name,
        publisher: text(&values, "Publisher"),
        version: text(&values, "DisplayVersion"),
        install_date: text(&values, "InstallDate").and_then(|d| format_date(&d)),
        size: number(&values, "EstimatedSize").map(|kb| kb as u64 * 1024),
        install_location: text(&values, "InstallLocation").map(|p| p.trim_matches('"').to_string()),
        needs_admin: machine,
        machine,
        key,
        uninstall,
        msi: number(&values, "WindowsInstaller") == Some(1),
    })
}

/// Ứng dụng đã cài (Win32 và MSI), sắp theo tên. Ứng dụng Store chưa có.
pub fn list() -> Vec<AppInfo> {
    let mut apps: Vec<AppInfo> = Vec::new();
    for (machine, root) in [(true, UNINSTALL), (true, UNINSTALL_WOW64), (false, UNINSTALL)] {
        for sub in reg::subkeys(machine, root) {
            let Some(app) = read_app(machine, root, &sub) else {
                continue;
            };
            // Một ứng dụng có thể đăng ký ở cả hai nhánh 32/64-bit: giữ bản đầy đủ hơn.
            match apps.iter_mut().find(|a| a.name == app.name && a.version == app.version) {
                Some(existing) if existing.install_location.is_none() && app.install_location.is_some() => *existing = app,
                Some(_) => {}
                None => apps.push(app),
            }
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

pub fn still_installed(app: &AppInfo) -> bool {
    !reg::values(app.machine, &app.key).is_empty()
}

/// Tách dòng lệnh gỡ thành chương trình và tham số.
fn split_command(command: &str) -> (String, String) {
    let command = winutil::expand_env(command.trim());
    if let Some(rest) = command.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return (rest[..end].to_string(), rest[end + 1..].trim().to_string());
        }
    }
    let program = parse_command(&command)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let args = command.get(program.len()..).unwrap_or("").trim().to_string();
    (program, args)
}

/// Lệnh gỡ của ứng dụng. Với gói MSI dùng `msiexec /x {mã}`, vì UninstallString
/// của MSI đôi khi là `/I` (mở trình sửa đổi) thay vì gỡ.
fn uninstall_command(app: &AppInfo) -> (String, String) {
    let product = app.key.rsplit('\\').next().unwrap_or("");
    if app.msi && product.starts_with('{') && product.ends_with('}') {
        return ("msiexec.exe".into(), format!("/x {product}"));
    }
    split_command(&app.uninstall)
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UninstallResult {
    pub exit_code: Option<u32>,
    /// Ứng dụng vẫn còn trong danh sách đã cài: trình gỡ bị hủy, hoặc vẫn đang
    /// chạy ở tiến trình con.
    pub still_installed: bool,
}

/// Chạy trình gỡ gốc và chờ nó kết thúc. Dùng ShellExecuteEx để Windows tự hỏi
/// UAC khi trình gỡ cần quyền quản trị.
#[cfg(windows)]
pub fn run_uninstaller(app: &AppInfo, cancel: &AtomicBool) -> Result<UninstallResult, String> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let (program, args) = uninstall_command(app);
    if program.is_empty() {
        return Err("no_uninstaller".into());
    }
    if cancel.load(Ordering::SeqCst) {
        return Ok(UninstallResult {
            exit_code: None,
            still_installed: still_installed(app),
        });
    }
    let file = HSTRING::from(program.as_str());
    let params = HSTRING::from(args.as_str());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: core::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: if args.is_empty() { PCWSTR::null() } else { PCWSTR(params.as_ptr()) },
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    let exit_code = unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        // Người dùng từ chối UAC cũng rơi vào nhánh lỗi này.
        ShellExecuteExW(&mut info).map_err(|e| e.message())?;
        if info.hProcess.is_invalid() {
            None
        } else {
            let mut code = None;
            // Chỉ ngừng chờ, không kết thúc trình gỡ của ứng dụng.
            while !cancel.load(Ordering::SeqCst) {
                match WaitForSingleObject(info.hProcess, 200) {
                    WAIT_OBJECT_0 => {
                        let mut value = 0u32;
                        if GetExitCodeProcess(info.hProcess, &mut value).is_ok() {
                            code = Some(value);
                        }
                        break;
                    }
                    WAIT_TIMEOUT => {}
                    _ => break,
                }
            }
            let _ = CloseHandle(info.hProcess);
            code
        }
    };
    // Trình gỡ dạng NSIS tự chép ra thư mục tạm rồi thoát ngay; chờ thêm một chút
    // để khóa Uninstall kịp biến mất nếu việc gỡ nhanh.
    for _ in 0..10 {
        if cancel.load(Ordering::SeqCst) || !still_installed(app) {
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Ok(UninstallResult {
        exit_code,
        still_installed: still_installed(app),
    })
}

#[cfg(not(windows))]
pub fn run_uninstaller(_app: &AppInfo, _cancel: &AtomicBool) -> Result<UninstallResult, String> {
    Err("chỉ hỗ trợ Windows".into())
}

// ------------------------------------------------------------ Tàn dư

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Leftover {
    pub id: String,
    /// `folder`, `registry` hoặc `uninstallEntry`.
    pub kind: &'static str,
    pub path: String,
    pub bytes: u64,
    pub recommended: bool,
    #[serde(skip)]
    pub target: LeftoverTarget,
}

#[derive(Clone, Debug)]
pub enum LeftoverTarget {
    Folder(PathBuf),
    RegistryKey { machine: bool, subkey: String },
}

/// Tên quá chung: thư mục hay khóa trùng tên này gần như chắc chắn dùng chung.
const GENERIC: &[&str] = &[
    "microsoft", "windows", "common files", "google", "intel", "nvidia", "amd", "adobe", "apple",
    "mozilla", "oracle", "java", "python", "classes", "policies", "clients", "wow6432node", "programs",
    "packages", "temp", "cache", "update", "updater", "setup", "installer", "tools", "app", "apps",
];

/// Các dạng tên để dò: tên đầy đủ, và tên đã bỏ phần phiên bản
/// (ví dụ "7-Zip 23.01 (x64)" → "7-Zip").
fn name_variants(name: &str) -> Vec<String> {
    let mut variants = vec![name.trim().to_string()];
    let words: Vec<&str> = name.split_whitespace().collect();
    let cut = words
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, w)| w.starts_with(|c: char| c.is_ascii_digit() || c == '(' || c == '-') || w.starts_with("v") && w[1..].starts_with(|c: char| c.is_ascii_digit()))
        .map(|(i, _)| i);
    if let Some(cut) = cut {
        variants.push(words[..cut].join(" "));
    }
    variants.dedup();
    variants
        .into_iter()
        .filter(|v| v.chars().count() >= 3 && !GENERIC.contains(&v.to_lowercase().as_str()))
        .collect()
}

/// Thư mục không bao giờ được coi là tàn dư.
fn protected(path: &Path) -> bool {
    let roots = [
        Known::Windows,
        Known::Profile,
        Known::ProgramFiles,
        Known::ProgramFilesX86,
        Known::ProgramData,
        Known::LocalAppData,
        Known::RoamingAppData,
        Known::UserProgramFiles,
        Known::StartMenu,
        Known::CommonStartMenu,
        Known::Desktop,
    ];
    let windows = winutil::known_folder(Known::Windows);
    path.parent().is_none()
        || windows.is_some_and(|w| path.starts_with(w))
        || roots
            .iter()
            .filter_map(|&k| winutil::known_folder(k))
            .any(|root| root == path || root.starts_with(path))
        || std::env::current_exe().is_ok_and(|exe| exe.starts_with(path))
}

/// Đường dẫn mà một ứng dụng khác đang dùng: thư mục cài và nơi đặt trình gỡ.
fn paths_of(app: &AppInfo) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(location) = &app.install_location {
        paths.push(PathBuf::from(winutil::expand_env(location)));
    }
    let (program, _) = uninstall_command(app);
    if Path::new(&program).is_absolute() {
        paths.push(PathBuf::from(program));
    }
    paths
}

/// Thư mục dùng chung: trùng tên hãng hay tên chung chung, hoặc đang chứa một
/// ứng dụng khác còn cài. Ví dụ Adobe After Effects khai báo thư mục cài là
/// `Program Files\Adobe`, nơi còn các ứng dụng Adobe khác.
fn shared(path: &Path, app: &AppInfo, others: &[AppInfo]) -> bool {
    let folder = path
        .file_name()
        .map(|n| n.to_string_lossy().trim().to_lowercase())
        .unwrap_or_default();
    let publisher = app.publisher.as_deref().map(|p| p.trim().to_lowercase()).unwrap_or_default();
    // "Adobe Inc." → thư mục "Adobe" vẫn là thư mục của hãng.
    let vendor = publisher.split_whitespace().next().unwrap_or("");
    GENERIC.contains(&folder.as_str())
        || (!publisher.is_empty() && (folder == publisher || folder == vendor))
        || others
            .iter()
            .filter(|other| other.id != app.id && other.name != app.name)
            .flat_map(paths_of)
            .any(|other| other.starts_with(path))
}

/// Tàn dư thật thường nhỏ. Thư mục còn lớn sau khi gỡ, nằm ngay dưới gốc ổ đĩa
/// (ví dụ `G:\Epic Games` chứa game), hoặc nằm trong AppData (dữ liệu người dùng
/// như phiên đăng nhập Telegram) vẫn được liệt kê nhưng không chọn sẵn.
const PRESELECT_LIMIT: u64 = 500 * 1024 * 1024;

fn safe_to_preselect(path: &Path, bytes: u64) -> bool {
    let under_drive_root = path.parent().is_some_and(|p| p.parent().is_none());
    // Chương trình cài riêng cho người dùng và Start Menu cũng nằm trong AppData
    // nhưng không chứa dữ liệu người dùng.
    let allowed = [Known::UserProgramFiles, Known::StartMenu];
    let in_app_data = [Known::LocalAppData, Known::RoamingAppData]
        .into_iter()
        .filter_map(winutil::known_folder)
        .any(|dir| path.starts_with(dir))
        && !allowed
            .into_iter()
            .filter_map(winutil::known_folder)
            .any(|dir| path.starts_with(dir));
    bytes < PRESELECT_LIMIT && !under_drive_root && !in_app_data
}

/// Quét tàn dư của một ứng dụng (thường gọi sau khi trình gỡ chạy xong).
/// `installed` là danh sách ứng dụng còn cài, dùng để tránh thư mục dùng chung.
pub fn scan_leftovers(app: &AppInfo, installed: &[AppInfo]) -> Vec<Leftover> {
    let mut found: Vec<Leftover> = Vec::new();
    let mut add_folder = |path: PathBuf, recommended: bool| {
        let path = PathBuf::from(path.to_string_lossy().trim_end_matches('\\'));
        if !path.is_dir()
            || protected(&path)
            || shared(&path, app, installed)
            || found.iter().any(|l| matches!(&l.target, LeftoverTarget::Folder(p) if p == &path))
        {
            return;
        }
        let bytes = winutil::dir_size(&path);
        found.push(Leftover {
            id: String::new(),
            kind: "folder",
            path: path.to_string_lossy().to_string(),
            bytes,
            recommended: recommended && safe_to_preselect(&path, bytes),
            target: LeftoverTarget::Folder(path),
        });
    };

    if let Some(location) = &app.install_location {
        add_folder(PathBuf::from(winutil::expand_env(location)), true);
    }

    let names = name_variants(&app.name);
    let publisher = app.publisher.as_deref().map(str::trim).filter(|p| !p.is_empty());
    // Vị trí cài chương trình: chọn sẵn. Vị trí dữ liệu: để người dùng chọn.
    let places = [
        (Known::ProgramFiles, true),
        (Known::ProgramFilesX86, true),
        (Known::UserProgramFiles, true),
        (Known::StartMenu, true),
        (Known::CommonStartMenu, true),
        (Known::RoamingAppData, false),
        (Known::LocalAppData, false),
        (Known::ProgramData, false),
    ];
    for (place, recommended) in places {
        let Some(mut root) = winutil::known_folder(place) else {
            continue;
        };
        if matches!(place, Known::StartMenu | Known::CommonStartMenu) {
            root = root.join("Programs");
        }
        for name in &names {
            add_folder(root.join(name), recommended);
            if let Some(publisher) = publisher {
                add_folder(root.join(publisher).join(name), recommended);
            }
        }
    }

    for (machine, software) in [(false, "Software"), (true, "SOFTWARE"), (true, "SOFTWARE\\WOW6432Node")] {
        for name in &names {
            let mut keys = vec![format!("{software}\\{name}")];
            if let Some(publisher) = publisher {
                keys.push(format!("{software}\\{publisher}\\{name}"));
            }
            for subkey in keys {
                let exists = !reg::values(machine, &subkey).is_empty() || reg::has_subkeys(machine, &subkey);
                if exists && !found.iter().any(|l| l.path.eq_ignore_ascii_case(&format!("{}\\{subkey}", reg::hive_name(machine)))) {
                    found.push(Leftover {
                        id: String::new(),
                        kind: "registry",
                        path: format!("{}\\{subkey}", reg::hive_name(machine)),
                        bytes: 0,
                        recommended: true,
                        target: LeftoverTarget::RegistryKey { machine, subkey },
                    });
                }
            }
        }
    }

    // Mục còn trong danh sách đã cài nhưng trình gỡ đã mất: mục mồ côi.
    if still_installed(app) {
        let (program, _) = uninstall_command(app);
        let missing = !program.eq_ignore_ascii_case("msiexec.exe") && crate::modules::shortcut::is_missing(Path::new(&program));
        if missing {
            found.push(Leftover {
                id: String::new(),
                kind: "uninstallEntry",
                path: format!("{}\\{}", reg::hive_name(app.machine), app.key),
                bytes: 0,
                recommended: true,
                target: LeftoverTarget::RegistryKey {
                    machine: app.machine,
                    subkey: app.key.clone(),
                },
            });
        }
    }

    let app_installed = still_installed(app);
    for (index, leftover) in found.iter_mut().enumerate() {
        leftover.id = format!("leftover:{index}");
        leftover.recommended &= !app_installed;
    }
    found
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveReport {
    pub done: u64,
    pub failed: u64,
    pub bytes: u64,
    pub record_id: Option<u64>,
}

/// Xóa các tàn dư đã chọn và ghi một bản ghi Rescue Center tên `title`.
pub fn remove_leftovers(leftovers: &[Leftover], rescue: &Rescue, title: &str) -> RemoveReport {
    let record_id = rescue.new_record_id();
    let mut report = RemoveReport::default();
    let mut undo = Vec::new();
    for (index, leftover) in leftovers.iter().enumerate() {
        let ok = match &leftover.target {
            LeftoverTarget::Folder(path) => !protected(path) && winutil::recycle(path).is_ok(),
            LeftoverTarget::RegistryKey { machine, subkey } => {
                let key = format!("{}\\{subkey}", reg::hive_name(*machine));
                match rescue::export_registry_key(rescue, record_id, &key, &format!("leftover-{index}.reg")) {
                    Ok(file) => match reg::delete_key(*machine, subkey) {
                        Ok(()) => {
                            undo.push(UndoOp::ImportRegFile { file });
                            true
                        }
                        Err(_) => false,
                    },
                    // Không sao lưu được thì không xóa.
                    Err(_) => false,
                }
            }
        };
        if ok {
            report.done += 1;
            report.bytes += leftover.bytes;
        } else {
            report.failed += 1;
        }
    }
    if report.done > 0 && rescue.add_tool_record(record_id, title, report.done, report.failed, report.bytes, undo).is_ok() {
        report.record_id = Some(record_id);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_variants_strip_versions_and_reject_generic() {
        assert_eq!(name_variants("7-Zip 23.01 (x64)"), vec!["7-Zip 23.01 (x64)", "7-Zip"]);
        assert_eq!(name_variants("Mozilla Firefox (x64 en-US)"), vec!["Mozilla Firefox (x64 en-US)", "Mozilla Firefox"]);
        assert_eq!(name_variants("VLC media player"), vec!["VLC media player"]);
        assert_eq!(name_variants("Zalo v24.10"), vec!["Zalo v24.10", "Zalo"]);
        assert!(name_variants("Google").is_empty());
        assert!(name_variants("Go").is_empty());
    }

    #[test]
    fn splits_uninstall_commands() {
        assert_eq!(
            split_command("\"C:\\Program Files\\App\\uninst.exe\" /S"),
            ("C:\\Program Files\\App\\uninst.exe".to_string(), "/S".to_string())
        );
        assert_eq!(
            split_command("MsiExec.exe /X{1234}"),
            ("MsiExec.exe".to_string(), "/X{1234}".to_string())
        );
        assert_eq!(
            split_command("C:\\No Such\\unins000.exe"),
            ("C:\\No Such\\unins000.exe".to_string(), String::new())
        );
    }

    #[test]
    fn dates_are_normalised() {
        assert_eq!(format_date("20261116").as_deref(), Some("2026-11-16"));
        assert_eq!(format_date("bad"), None);
    }

    #[cfg(windows)]
    #[test]
    fn known_roots_are_protected() {
        for known in [Known::ProgramFiles, Known::RoamingAppData, Known::Windows] {
            let root = winutil::known_folder(known).unwrap();
            assert!(protected(&root));
            assert!(protected(root.parent().unwrap()));
        }
        let windows = winutil::known_folder(Known::Windows).unwrap();
        assert!(protected(&windows.join("System32")));
        let app = winutil::known_folder(Known::ProgramFiles).unwrap().join("Some App");
        assert!(!protected(&app));
    }

    fn app(id: &str, name: &str, publisher: &str, location: &str) -> AppInfo {
        AppInfo {
            id: id.into(),
            name: name.into(),
            publisher: Some(publisher.into()),
            version: None,
            install_date: None,
            size: None,
            install_location: Some(location.into()),
            needs_admin: true,
            machine: true,
            key: format!("SOFTWARE\\Test\\{id}"),
            uninstall: format!("\"{location}\\uninst.exe\""),
            msi: false,
        }
    }

    #[test]
    fn shared_vendor_folders_are_never_leftovers() {
        let effects = app("ae", "Adobe After Effects 2026", "Adobe Inc.", "C:\\Program Files\\Adobe");
        let bridge = app("br", "Adobe Bridge 2026", "Adobe Inc.", "C:\\Program Files\\Adobe\\Adobe Bridge 2026");
        let zip = app("7z", "7-Zip 23.01 (x64)", "Igor Pavlov", "C:\\Program Files\\7-Zip");
        let installed = [effects.clone(), bridge.clone(), zip.clone()];

        // Thư mục chung của hãng, đang chứa một ứng dụng khác còn cài.
        assert!(shared(Path::new("C:\\Program Files\\Adobe"), &effects, &installed));
        // Thư mục riêng của chính ứng dụng thì không tính là dùng chung.
        assert!(!shared(Path::new("C:\\Program Files\\Adobe\\Adobe Bridge 2026"), &bridge, &installed));
        assert!(!shared(Path::new("C:\\Program Files\\7-Zip"), &zip, &installed));
        // Thư mục trùng tên hãng.
        let pavlov = app("ip", "Tool", "Igor Pavlov", "C:\\Program Files\\Igor Pavlov");
        assert!(shared(Path::new("C:\\Program Files\\Igor Pavlov"), &pavlov, &[]));
    }

    #[cfg(windows)]
    #[test]
    fn risky_folders_are_not_preselected() {
        let program_files = winutil::known_folder(Known::ProgramFiles).unwrap();
        assert!(safe_to_preselect(&program_files.join("App"), 10));
        assert!(!safe_to_preselect(&program_files.join("App"), PRESELECT_LIMIT));
        assert!(!safe_to_preselect(Path::new("G:\\Epic Games"), 10));
        let roaming = winutil::known_folder(Known::RoamingAppData).unwrap();
        assert!(!safe_to_preselect(&roaming.join("Telegram Desktop"), 10));
        let programs = winutil::known_folder(Known::UserProgramFiles).unwrap();
        assert!(safe_to_preselect(&programs.join("Zalo"), 10));
    }

    #[cfg(windows)]
    #[test]
    fn installed_app_leftovers_are_not_preselected() {
        let name = format!("TPSystemCareLeftoverTest{}", std::process::id());
        let subkey = format!("Software\\{name}");
        let mut app = app(&name, &name, "Test Vendor", "C:\\TPSystemCareMissingTestApp");
        app.machine = false;
        app.key = format!("{subkey}\\Uninstall");
        reg::write(false, &subkey, "Setting", &RawValue::dword(1)).unwrap();
        reg::write(false, &app.key, "DisplayName", &RawValue::sz(&name)).unwrap();
        let registered = still_installed(&app);
        let installed = scan_leftovers(&app, &[app.clone()]);
        reg::delete_key(false, &app.key).unwrap();
        let removed = scan_leftovers(&app, &[]);
        reg::delete_key(false, &subkey).unwrap();

        assert!(registered);
        assert!(installed.iter().any(|l| l.kind == "registry"));
        assert!(installed.iter().any(|l| l.kind == "uninstallEntry"));
        assert!(installed.iter().all(|l| !l.recommended));
        assert!(removed.iter().any(|l| l.kind == "registry" && l.recommended));
    }

    #[cfg(windows)]
    #[test]
    fn cancelled_wait_does_not_launch_uninstaller() {
        let app = app("cancelled", "Test App", "Test Vendor", "C:\\TPSystemCareMissingTestApp");
        let result = run_uninstaller(&app, &AtomicBool::new(true)).unwrap();
        assert!(result.exit_code.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn installed_apps_have_names_and_uninstallers() {
        let apps = list();
        assert!(!apps.is_empty());
        assert!(apps.iter().all(|a| !a.name.is_empty() && !a.uninstall.is_empty()));
    }
}
