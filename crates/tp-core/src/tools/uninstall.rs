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

use serde::{Deserialize, Serialize};

use super::{store_apps, uninstall_entries};
use crate::modules::startup::{self, parse_command};
use crate::reg::{self, RawValue};
use crate::rescue::{self, Rescue, UndoOp};
use crate::winutil::{self, Known};

const UNINSTALL: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall";
const UNINSTALL_WOW64: &str = "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AppKind {
    Desktop,
    Store,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UninstallMode {
    Standard,
    Forced,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    /// `<HKLM|HKCU>\<khóa Uninstall>`, dùng làm mã.
    pub id: String,
    pub kind: AppKind,
    pub built_in: bool,
    pub removable: bool,
    pub name: String,
    pub publisher: Option<String>,
    pub version: Option<String>,
    /// Dạng `YYYY-MM-DD`.
    pub install_date: Option<String>,
    /// Theo khai báo của ứng dụng (EstimatedSize); có thể thiếu hoặc không chính xác.
    pub size: Option<u64>,
    pub install_location: Option<String>,
    pub needs_admin: bool,
    /// Lần ghi cuối của khóa Uninstall (giây Unix), gần đúng thời điểm cài.
    pub installed_at: Option<u64>,
    /// Ứng dụng khác được đăng ký trong cùng khoảng thời gian: có thể là phần mềm
    /// đi kèm bộ cài. Chỉ là cảnh báo, không bao giờ tự chọn để gỡ.
    pub companions: Vec<String>,
    #[serde(skip)]
    pub machine: bool,
    #[serde(skip)]
    pub key: String,
    #[serde(skip)]
    pub uninstall: String,
    #[serde(skip)]
    pub msi: bool,
    #[serde(skip)]
    pub package: Option<store_apps::Package>,
    #[serde(skip)]
    pub synthetic: bool,
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
        kind: AppKind::Desktop,
        built_in: false,
        removable: true,
        name,
        publisher: text(&values, "Publisher"),
        version: text(&values, "DisplayVersion"),
        install_date: text(&values, "InstallDate").and_then(|d| format_date(&d)),
        size: number(&values, "EstimatedSize").map(|kb| kb as u64 * 1024),
        install_location: text(&values, "InstallLocation").map(|p| p.trim_matches('"').to_string()),
        needs_admin: machine,
        installed_at: reg::last_write(machine, &key),
        companions: Vec::new(),
        machine,
        key,
        uninstall,
        msi: number(&values, "WindowsInstaller") == Some(1),
        package: None,
        synthetic: false,
    })
}

/// Ứng dụng Win32 và MSI, sắp theo tên.
pub fn desktop_list() -> Vec<AppInfo> {
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
    mark_companions(&mut apps);
    apps
}

/// Hai ứng dụng được coi là cài cùng lúc khi cùng ngày cài và khóa Uninstall được
/// ghi cách nhau không quá 5 phút.
const COMPANION_WINDOW: u64 = 5 * 60;
/// Nhóm lớn hơn thường là cài đặt hàng loạt của nhà sản xuất máy hoặc công ty.
const COMPANION_LIMIT: usize = 6;
/// Thư viện chạy kèm (runtime) thường là phụ thuộc thật của ứng dụng chính.
const RUNTIME_HINTS: &[&str] = &["redistributable", "runtime", ".net", "visual c++", "directx", "webview2", "vc_redist"];

fn runtime(app: &AppInfo) -> bool {
    let name = app.name.to_lowercase();
    RUNTIME_HINTS.iter().any(|hint| name.contains(hint))
}

/// Ghi tên các ứng dụng desktop được cài trong cùng khoảng thời gian.
fn mark_companions(apps: &mut [AppInfo]) {
    let candidates: Vec<(usize, u64, String)> = apps
        .iter()
        .enumerate()
        .filter(|(_, app)| app.kind == AppKind::Desktop && !runtime(app))
        .filter_map(|(index, app)| Some((index, app.installed_at?, app.install_date.clone()?)))
        .collect();
    let mut found: Vec<(usize, Vec<String>)> = Vec::new();
    for (index, time, date) in &candidates {
        let names: Vec<String> = candidates
            .iter()
            .filter(|(other, other_time, other_date)| other != index && other_date == date && other_time.abs_diff(*time) <= COMPANION_WINDOW)
            .map(|(other, _, _)| apps[*other].name.clone())
            .collect();
        if !names.is_empty() && names.len() < COMPANION_LIMIT {
            found.push((*index, names));
        }
    }
    for (index, names) in found {
        apps[index].companions = names;
    }
}

pub fn list() -> Result<Vec<AppInfo>, String> {
    let mut apps = desktop_list();
    for package in store_apps::list()? {
        apps.push(AppInfo {
            id: format!("store:{}", package.full_name),
            kind: AppKind::Store,
            built_in: package.built_in,
            removable: package.removable,
            name: package.display_name.clone(),
            publisher: Some(package.publisher.clone()),
            version: Some(package.version.clone()),
            install_date: None,
            size: None,
            install_location: package.install_location.clone(),
            needs_admin: false,
            installed_at: None,
            companions: Vec::new(),
            machine: false,
            key: String::new(),
            uninstall: String::new(),
            msi: false,
            package: Some(package),
            synthetic: false,
        });
    }
    apps.sort_by_key(|app| app.name.to_lowercase());
    Ok(apps)
}

pub fn still_installed(app: &AppInfo) -> bool {
    if let Some(package) = &app.package {
        return store_apps::is_installed(&package.full_name).unwrap_or(true);
    }
    if app.synthetic { return false; }
    #[cfg(windows)]
    {
        use winreg::{enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE}, RegKey};
        match RegKey::predef(if app.machine { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER }).open_subkey(&app.key) {
            Ok(_) => true,
            Err(error) => error.kind() != std::io::ErrorKind::NotFound,
        }
    }
    #[cfg(not(windows))]
    { !reg::values(app.machine, &app.key).is_empty() }
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

    if !app.removable || app.synthetic {
        return Err("protected_app".into());
    }
    if let Some(package) = &app.package {
        store_apps::uninstall(&package.full_name, cancel)?;
        return Ok(UninstallResult {
            exit_code: Some(0),
            still_installed: still_installed(app),
        });
    }
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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUninstallItem {
    pub id: String,
    pub name: String,
    pub result: Option<UninstallResult>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUninstallReport {
    pub items: Vec<BatchUninstallItem>,
    pub pending: Vec<String>,
    pub cancelled: bool,
}

pub fn run_batch(apps: &[AppInfo], cancel: &AtomicBool, progress: &dyn Fn(usize, usize, &str)) -> BatchUninstallReport {
    batch_with(apps, cancel, progress, |app| run_uninstaller(app, cancel))
}

fn batch_with(
    apps: &[AppInfo],
    cancel: &AtomicBool,
    progress: &dyn Fn(usize, usize, &str),
    run: impl Fn(&AppInfo) -> Result<UninstallResult, String>,
) -> BatchUninstallReport {
    let mut report = BatchUninstallReport::default();
    for (index, app) in apps.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        progress(index + 1, apps.len(), &app.name);
        let (result, error) = match run(app) {
            Ok(result) => (Some(result), None),
            Err(error) => (None, Some(error)),
        };
        let stop = error.is_some() || result.as_ref().is_some_and(|r| r.still_installed);
        report.items.push(BatchUninstallItem { id: app.id.clone(), name: app.name.clone(), result, error });
        // Một số trình gỡ thoát sớm và chuyển việc gỡ sang tiến trình con.
        if stop {
            break;
        }
    }
    report.pending = apps.iter().skip(report.items.len()).map(|a| a.id.clone()).collect();
    report.cancelled = cancel.load(Ordering::SeqCst);
    report
}

pub fn force_target(name: &str, path: &str, installed: &[AppInfo]) -> Result<AppInfo, String> {
    let name = name.trim();
    if name_variants(name).is_empty() || name.chars().any(char::is_control) {
        return Err("invalid_force_target".into());
    }
    let original = PathBuf::from(winutil::expand_env(path.trim().trim_matches('"')));
    if !original.is_absolute() || !original.is_dir() || !winutil::on_fixed_drive(&original) {
        return Err("invalid_force_target".into());
    }
    let canonical = original.canonicalize().map_err(|_| "invalid_force_target".to_string())?;
    if path_key(&original) != path_key(&canonical) || protected(&canonical) {
        return Err("invalid_force_target".into());
    }
    let location = canonical.to_string_lossy().trim_start_matches("\\\\?\\").to_string();
    let app = AppInfo {
        id: format!("forced:{}", path_key(&canonical)),
        kind: AppKind::Desktop,
        built_in: false,
        removable: true,
        name: name.to_string(),
        publisher: None,
        version: None,
        install_date: None,
        size: None,
        install_location: Some(location),
        needs_admin: false,
        installed_at: None,
        companions: Vec::new(),
        machine: false,
        key: String::new(),
        uninstall: String::new(),
        msi: false,
        package: None,
        synthetic: true,
    };
    if shared(&canonical, &app, installed) {
        return Err("shared_force_target".into());
    }
    Ok(app)
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
    Shortcut { path: PathBuf, target: PathBuf },
    Startup { machine: bool, subkey: String, name: String, value: RawValue, approved: String },
    SystemEntry(uninstall_entries::Entry),
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
    if name.contains(['\\', '/', ':']) || name.contains("..") || name.chars().any(char::is_control) {
        return Vec::new();
    }
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
fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").trim_start_matches("\\\\?\\").trim_end_matches('\\').to_lowercase()
}

fn contains_path(root: &Path, path: &Path) -> bool {
    let root = path_key(root);
    let path = path_key(path);
    !root.is_empty() && (path == root || path.starts_with(&format!("{root}\\")))
}

fn protected(path: &Path) -> bool {
    let roots = [
        Known::Windows, Known::Profile, Known::ProgramFiles, Known::ProgramFilesX86,
        Known::ProgramData, Known::LocalAppData, Known::RoamingAppData,
        Known::UserProgramFiles, Known::StartMenu, Known::CommonStartMenu,
        Known::Desktop, Known::PublicDesktop, Known::Startup, Known::CommonStartup,
    ];
    let protected_personal = winutil::known_folder(Known::Profile).is_some_and(|profile| {
        ["Documents", "Downloads", "Pictures", "Music", "Videos", "AppData\\LocalLow"]
            .iter().any(|name| contains_path(path, &profile.join(name)))
    });
    let protected_program = [Known::ProgramFiles, Known::ProgramFilesX86].into_iter().filter_map(winutil::known_folder).any(|root| {
        ["Common Files", "Windows Defender", "Windows Defender Advanced Threat Protection", "Windows NT", "WindowsPowerShell", "Internet Explorer", "Microsoft", "MSBuild", "Reference Assemblies", "Windows Kits", "ModifiableWindowsApps"]
            .iter().any(|name| contains_path(&root.join(name), path) || contains_path(path, &root.join(name)))
    });
    let protected_drive = path.ancestors().last().is_some_and(|root| {
        ["$Recycle.Bin", "System Volume Information", "Recovery", "Boot"].iter().any(|name| contains_path(&root.join(name), path))
    });
    let protected_data = winutil::known_folder(Known::ProgramData).is_some_and(|root| contains_path(&root.join("Microsoft"), path));
    path.parent().is_none() || protected_program || protected_drive || protected_data
        || winutil::known_folder(Known::Windows).is_some_and(|root| contains_path(&root, path))
        || [Known::ProgramFiles, Known::ProgramFilesX86].into_iter().filter_map(winutil::known_folder)
            .any(|root| contains_path(&root.join("WindowsApps"), path))
        || roots.into_iter().filter_map(winutil::known_folder).any(|root| contains_path(path, &root))
        || protected_personal
        || std::env::current_exe().is_ok_and(|exe| contains_path(path, &exe))
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
            .filter(|other| other.id != app.id)
            .flat_map(paths_of)
            .any(|other| contains_path(path, &other) || contains_path(&other, path))
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
    scan_base(app, installed, false)
}

fn scan_base(app: &AppInfo, installed: &[AppInfo], forced: bool) -> Vec<Leftover> {
    let mut found: Vec<Leftover> = Vec::new();
    if let Some(package) = &app.package {
        if let Some(local) = winutil::known_folder(Known::LocalAppData) {
            let path = local.join("Packages").join(&package.family_name);
            if path.is_dir() && !protected(&path) {
                found.push(Leftover {
                    id: "leftover:0".into(), kind: "folder", path: path.to_string_lossy().to_string(),
                    bytes: winutil::dir_size(&path), recommended: false, target: LeftoverTarget::Folder(path),
                });
            }
        }
        return found;
    }
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
    let publisher = app.publisher.as_deref().map(str::trim).filter(|p| !p.is_empty() && !p.contains(['\\', '/', ':']) && !p.contains("..") && !p.chars().any(char::is_control));
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
            if installed.iter().filter(|other| other.id != app.id).any(|other| name_variants(&other.name).iter().any(|variant| variant.eq_ignore_ascii_case(name))) {
                continue;
            }
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
    if app.package.is_none() && !app.synthetic && still_installed(app) {
        let (program, _) = uninstall_command(app);
        let missing = !program.eq_ignore_ascii_case("msiexec.exe") && crate::modules::shortcut::is_missing(Path::new(&program));
        if missing || forced {
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
        leftover.recommended &= !app_installed && !forced;
    }
    found
}

fn safe_path(path: &Path) -> bool {
    path.is_absolute()
        && !path.components().any(|part| matches!(part, std::path::Component::ParentDir))
        && match path.canonicalize() {
            Ok(canonical) => path_key(path) == path_key(&canonical),
            Err(_) => matches!(path.try_exists(), Ok(false)),
        }
}

fn safe_folder(path: &Path) -> bool {
    if !safe_path(path) || protected(path) || !path.is_dir() {
        return false;
    }
    walkdir::WalkDir::new(path).follow_links(false).into_iter().all(|entry| {
        let Ok(entry) = entry else { return false };
        let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else { return false };
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            metadata.file_attributes() & 0x400 == 0
        }
        #[cfg(not(windows))]
        { !metadata.file_type().is_symlink() }
    })
}

fn owned_targets(app: &AppInfo, installed: &[AppInfo]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    if app.kind == AppKind::Store {
        return (Vec::new(), Vec::new());
    }
    let mut roots = Vec::new();
    if let Some(location) = &app.install_location {
        let root = PathBuf::from(winutil::expand_env(location));
        if safe_path(&root) && !protected(&root) && !shared(&root, app, installed) {
            roots.push(root);
        }
    }
    let mut files = Vec::new();
    let (program, _) = uninstall_command(app);
    let file = PathBuf::from(program);
    if let Some(parent) = file.parent() {
        if safe_path(&file) && !protected(&file) && !protected(parent) && !shared(parent, app, installed) {
            files.push(file.clone());
            if !roots.iter().any(|root| contains_path(root, parent)) {
                roots.push(parent.to_path_buf());
            }
        }
    }
    (roots, files)
}

fn owns(target: &Path, roots: &[PathBuf], files: &[PathBuf]) -> bool {
    safe_path(target) && !protected(target)
        && (roots.iter().any(|root| contains_path(root, target))
            || files.iter().any(|file| path_key(file) == path_key(target)))
}

pub fn scan_with_mode(app: &AppInfo, installed: &[AppInfo], mode: UninstallMode) -> Result<Vec<Leftover>, String> {
    if !app.removable || mode == UninstallMode::Forced && app.kind != AppKind::Desktop {
        return Err("protected_app".into());
    }
    if mode == UninstallMode::Standard && still_installed(app) {
        return Err("still_installed".into());
    }
    if app.synthetic {
        let path = app.install_location.as_deref().ok_or("invalid_force_target")?;
        force_target(&app.name, path, installed)?;
    }
    let mut found = scan_base(app, installed, mode == UninstallMode::Forced);
    let (roots, files) = owned_targets(app, installed);
    if roots.is_empty() && files.is_empty() {
        return Ok(found);
    }
    for (machine, subkey, approved) in [
        (false, "Software\\Microsoft\\Windows\\CurrentVersion\\Run", "Run"),
        (true, "Software\\Microsoft\\Windows\\CurrentVersion\\Run", "Run"),
        (true, "Software\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Run", "Run32"),
    ] {
        for (name, value) in reg::values(machine, subkey) {
            let Some(command) = value.as_string() else { continue };
            let Some(target) = parse_command(&command) else { continue };
            if !owns(&target, &roots, &files) { continue; }
            found.push(Leftover {
                id: String::new(), kind: "startup", path: format!("{}\\{subkey}\\{name}", reg::hive_name(machine)),
                bytes: 0, recommended: false,
                target: LeftoverTarget::Startup { machine, subkey: subkey.into(), name, value, approved: format!("{}\\{approved}", startup::APPROVED) },
            });
        }
    }
    for known in [Known::Desktop, Known::PublicDesktop, Known::StartMenu, Known::CommonStartMenu, Known::Startup, Known::CommonStartup] {
        let Some(root) = winutil::known_folder(known) else { continue };
        for entry in walkdir::WalkDir::new(root).follow_links(false).into_iter().filter_map(Result::ok) {
            let path = entry.path();
            if !entry.file_type().is_file() || !path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("lnk")) || !safe_path(path) {
                continue;
            }
            if found.iter().any(|leftover| match &leftover.target {
                LeftoverTarget::Folder(folder) => contains_path(folder, path),
                LeftoverTarget::Shortcut { path: other, .. } => path_key(other) == path_key(path),
                _ => false,
            }) { continue; }
            let Some(target) = winutil::shortcut_target(path) else { continue };
            if !owns(&target, &roots, &files) { continue; }
            found.push(Leftover {
                id: String::new(), kind: "shortcut", path: path.to_string_lossy().to_string(),
                bytes: 0, recommended: false, target: LeftoverTarget::Shortcut { path: path.to_path_buf(), target },
            });
        }
    }
    for entry in uninstall_entries::scan(&roots, &files)? {
        found.push(Leftover {
            id: String::new(), kind: entry.kind, path: format!("{} → {}", entry.path, entry.detail), bytes: 0, recommended: false,
            target: LeftoverTarget::SystemEntry(entry),
        });
    }
    for (index, leftover) in found.iter_mut().enumerate() {
        leftover.id = format!("leftover:{index}");
        leftover.recommended &= mode == UninstallMode::Standard;
    }
    Ok(found)
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
            LeftoverTarget::Folder(path) => safe_folder(path) && winutil::recycle(path).is_ok(),
            LeftoverTarget::Shortcut { path, target } => {
                if safe_path(path) && winutil::shortcut_target(path).is_some_and(|current| path_key(&current) == path_key(target)) {
                    match rescue.quarantine(record_id, path) {
                        Ok(op) => { undo.push(op); true }
                        Err(_) => false,
                    }
                } else { false }
            }
            LeftoverTarget::Startup { machine, subkey, name, value, approved } => {
                remove_startup(*machine, subkey, name, value, approved, rescue, record_id, index, &mut undo).is_ok()
            }
            LeftoverTarget::SystemEntry(entry) => {
                uninstall_entries::remove(entry, rescue, record_id, index, &mut |entry| {
                    let op = UndoOp::RestoreUninstallEntry { entry: entry.clone() };
                    let mut pending = undo.clone();
                    pending.push(op.clone());
                    rescue.add_tool_record(record_id, title, report.done, report.failed, report.bytes, pending)?;
                    undo.push(op);
                    Ok(())
                }).is_ok()
            },
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
    if (report.done > 0 || !undo.is_empty()) && rescue.add_tool_record(record_id, title, report.done, report.failed, report.bytes, undo).is_ok() {
        report.record_id = Some(record_id);
    }
    report
}

fn remove_startup(
    machine: bool, subkey: &str, name: &str, value: &RawValue, approved: &str,
    rescue: &Rescue, record_id: u64, index: usize, undo: &mut Vec<UndoOp>,
) -> Result<(), String> {
    if reg::read(machine, subkey, name).as_ref() != Some(value) {
        return Err("changed_target".into());
    }
    let previous = reg::read(machine, approved, name);
    let ops = vec![
        UndoOp::RestoreRegistryValue { machine, subkey: subkey.into(), name: name.into(), previous: Some(value.clone()) },
        UndoOp::RestoreRegistryValue { machine, subkey: approved.into(), name: name.into(), previous: previous.clone() },
    ];
    let dir = rescue.record_dir(record_id).join("startup");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    std::fs::write(dir.join(format!("{index}.json")), serde_json::to_vec(&ops).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    reg::delete_value(machine, subkey, name)?;
    if previous.is_some() {
        if let Err(error) = reg::delete_value(machine, approved, name) {
            if reg::write(machine, subkey, name, value).is_err() {
                undo.push(ops[0].clone());
            }
            return Err(error);
        }
    }
    undo.extend(ops);
    Ok(())
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
    fn path_matching_requires_boundaries_and_handles_windows_case() {
        assert!(contains_path(Path::new("C:\\Apps\\Tool"), Path::new("c:/apps/TOOL/bin/app.exe")));
        assert!(!contains_path(Path::new("C:\\Apps\\Tool"), Path::new("C:\\Apps\\Toolbox\\app.exe")));
        assert!(name_variants("..\\Windows").is_empty());
        assert!(name_variants("Vendor/App").is_empty());
        assert!(name_variants("App\nName").is_empty());
    }

    #[test]
    fn batch_stops_after_a_still_installed_result() {
        let apps = [app("one", "First App", "Vendor", "C:\\TestOne"), app("two", "Second App", "Vendor", "C:\\TestTwo")];
        let calls = std::cell::Cell::new(0);
        let report = batch_with(&apps, &AtomicBool::new(false), &|_, _, _| {}, |_| {
            calls.set(calls.get() + 1);
            Ok(UninstallResult { exit_code: None, still_installed: true })
        });
        assert_eq!(calls.get(), 1);
        assert_eq!(report.pending, ["two"]);
        assert!(!report.cancelled);
    }

    #[test]
    fn batch_cancellation_keeps_the_queue_pending() {
        let apps = [app("one", "First App", "Vendor", "C:\\TestOne"), app("two", "Second App", "Vendor", "C:\\TestTwo")];
        let cancel = AtomicBool::new(false);
        let report = batch_with(&apps, &cancel, &|_, _, _| {}, |_| {
            cancel.store(true, Ordering::SeqCst);
            Ok(UninstallResult { exit_code: Some(0), still_installed: false })
        });
        assert_eq!(report.items.len(), 1);
        assert_eq!(report.pending, ["two"]);
        assert!(report.cancelled);
        let cancelled = batch_with(&apps, &cancel, &|_, _, _| {}, |_| panic!("must not launch"));
        assert!(cancelled.items.is_empty());
        assert_eq!(cancelled.pending.len(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn force_targets_reject_shared_and_protected_roots() {
        let root = std::env::temp_dir().join(format!("TPForceFixture{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let root = PathBuf::from(root.canonicalize().unwrap().to_string_lossy().trim_start_matches("\\\\?\\"));
        let target = force_target("Fixture App", &root.to_string_lossy(), &[]).unwrap();
        let mut other = target.clone();
        other.id = "another".into();
        let shared_result = force_target("Fixture App", &root.to_string_lossy(), &[other]);
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let parent = app("parent", "Parent App", "Vendor", &root.to_string_lossy());
        let nested_result = force_target("Fixture App", &nested.to_string_lossy(), &[parent]);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(shared_result.unwrap_err(), "shared_force_target");
        assert_eq!(nested_result.unwrap_err(), "shared_force_target");
        let windows = winutil::known_folder(Known::Windows).unwrap();
        assert_eq!(force_target("Fixture App", &windows.to_string_lossy(), &[]).unwrap_err(), "invalid_force_target");
    }

    #[cfg(windows)]
    #[test]
    fn startup_cleanup_preserves_raw_values_and_rejects_changed_targets() {
        let root = format!("Software\\TPStartupFixture{}", std::process::id());
        let run = format!("{root}\\Run");
        let approved = format!("{root}\\Approved");
        let value = RawValue::sz("C:\\Fixture\\app.exe --start");
        let state = RawValue { vtype: reg::REG_BINARY, bytes: vec![3, 0, 0, 0, 8, 7, 6] };
        reg::write(false, &run, "fixture", &value).unwrap();
        reg::write(false, &approved, "fixture", &state).unwrap();
        let dir = std::env::temp_dir().join(format!("TPStartupRescueFixture{}", std::process::id()));
        let rescue = Rescue::at(dir.clone());
        let mut undo = Vec::new();
        remove_startup(false, &run, "fixture", &value, &approved, &rescue, 1, 0, &mut undo).unwrap();
        let removed = reg::read(false, &run, "fixture").is_none() && reg::read(false, &approved, "fixture").is_none();
        rescue.add_tool_record(1, "fixture", 1, 0, 0, undo).unwrap();
        rescue.undo(1).unwrap();
        let restored = reg::read(false, &run, "fixture") == Some(value.clone()) && reg::read(false, &approved, "fixture") == Some(state);
        reg::write(false, &run, "fixture", &RawValue::sz("C:\\Other\\app.exe")).unwrap();
        let changed = remove_startup(false, &run, "fixture", &value, &approved, &rescue, 2, 0, &mut Vec::new());
        reg::delete_key(false, &root).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(removed && restored);
        assert_eq!(changed.unwrap_err(), "changed_target");
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
            kind: AppKind::Desktop,
            built_in: false,
            removable: true,
            name: name.into(),
            publisher: Some(publisher.into()),
            version: None,
            install_date: None,
            size: None,
            install_location: Some(location.into()),
            needs_admin: true,
            installed_at: None,
            companions: Vec::new(),
            machine: true,
            key: format!("SOFTWARE\\Test\\{id}"),
            uninstall: format!("\"{location}\\uninst.exe\""),
            msi: false,
            package: None,
            synthetic: false,
        }
    }

    #[test]
    fn companions_need_same_day_close_time_and_skip_runtimes_and_mass_installs() {
        let at = |id: &str, name: &str, date: &str, time: u64| {
            let mut a = app(id, name, "Vendor", r"C:\Fixture");
            a.install_date = Some(date.into());
            a.installed_at = Some(time);
            a
        };
        let mut apps = vec![
            at("main", "Free Player", "2026-11-18", 1_000),
            at("bar", "Search Bar", "2026-11-18", 1_120),
            at("vc", "Microsoft Visual C++ 2015-2022 Redistributable", "2026-11-18", 1_060),
            at("late", "Later Tool", "2026-11-18", 1_000 + COMPANION_WINDOW + 200),
            at("other", "Other Day", "2026-11-19", 1_010),
        ];
        let mut store = app("store", "Store App", "Vendor", r"C:\Store");
        store.kind = AppKind::Store;
        store.install_date = Some("2026-11-18".into());
        store.installed_at = Some(1_000);
        apps.push(store);
        mark_companions(&mut apps);
        assert_eq!(apps[0].companions, ["Search Bar"]);
        assert_eq!(apps[1].companions, ["Free Player"]);
        assert!(apps[2].companions.is_empty() && apps[3].companions.is_empty() && apps[4].companions.is_empty() && apps[5].companions.is_empty());

        let mut mass: Vec<AppInfo> = (0..=COMPANION_LIMIT as u64).map(|i| at(&format!("m{i}"), &format!("Preload {i}"), "2026-11-18", 5_000 + i)).collect();
        mark_companions(&mut mass);
        assert!(mass.iter().all(|a| a.companions.is_empty()));
    }

    #[test]
    fn shared_vendor_folders_are_never_leftovers() {
        let effects = app("ae", "Adobe After Effects 2026", "Adobe Inc.", "C:\\Program Files\\Adobe");
        let bridge = app("br", "Adobe Bridge 2026", "Adobe Inc.", "C:\\Program Files\\Adobe\\Adobe Bridge 2026");
        let zip = app("7z", "7-Zip 23.01 (x64)", "Igor Pavlov", "C:\\Program Files\\7-Zip");
        let installed = [effects.clone(), bridge.clone(), zip.clone()];

        // Thư mục chung của hãng, đang chứa một ứng dụng khác còn cài.
        assert!(shared(Path::new("C:\\Program Files\\Adobe"), &effects, &installed));
        assert!(shared(Path::new("C:\\Program Files\\Adobe\\Adobe Bridge 2026"), &bridge, &installed));
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
        let apps = desktop_list();
        assert!(!apps.is_empty());
        assert!(apps.iter().all(|a| !a.name.is_empty() && !a.uninstall.is_empty()));
    }
}
