//! Tiện ích và toolbar trình duyệt (Gỡ sạch đợt 3).
//!
//! Chỉ đọc hồ sơ trình duyệt: không sửa `Preferences`/`Secure Preferences` của
//! Chromium hay `extensions.json` của Firefox, vì trình duyệt ký các tệp đó và coi
//! mọi chỉnh sửa từ bên ngoài là bị can thiệp.
//!
//! Hai cách gỡ:
//! - Tiện ích do phần mềm khác cài qua registry (kiểu toolbar đi kèm bộ cài): xuất
//!   khóa/giá trị ra Rescue Center rồi xóa; trình duyệt tự gỡ tiện ích ở lần mở sau.
//! - Tiện ích người dùng tự cài: mở đúng trang quản lý tiện ích của trình duyệt để
//!   người dùng tự gỡ ở đó.
//!
//! Tiện ích do chính sách (policy) bắt buộc thì chỉ liệt kê, không gỡ.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::reg::{self, RawValue};
use crate::rescue::{self, Rescue, UndoOp};
use crate::winutil::{self, Known};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// Cài từ cửa hàng tiện ích của trình duyệt.
    Store,
    /// Nạp từ thư mục (chế độ nhà phát triển) hoặc dòng lệnh.
    Unpacked,
    /// Do phần mềm khác cài qua registry hoặc tệp cấu hình ngoài.
    External,
    /// Bắt buộc bởi chính sách của máy hoặc tổ chức.
    Policy,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Removal {
    /// Xóa mục registry đã cài tiện ích; hoàn tác được qua Rescue Center.
    Registry,
    /// Mở trang quản lý tiện ích của trình duyệt để người dùng tự gỡ.
    Browser,
    /// Không gỡ được từ ứng dụng (chính sách).
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryRef {
    /// Khóa `...\Extensions\<id>` của Chrome/Edge.
    Key { machine: bool, subkey: String },
    /// Giá trị `<id>` trong khóa `Extensions` của Firefox.
    Value {
        machine: bool,
        subkey: String,
        name: String,
        value: RawValue,
    },
}

impl RegistryRef {
    fn machine(&self) -> bool {
        match self {
            Self::Key { machine, .. } | Self::Value { machine, .. } => *machine,
        }
    }

    fn display(&self) -> String {
        match self {
            Self::Key { machine, subkey } => format!("{}\\{subkey}", reg::hive_name(*machine)),
            Self::Value {
                machine,
                subkey,
                name,
                ..
            } => format!("{}\\{subkey} → {name}", reg::hive_name(*machine)),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Extension {
    /// `<trình duyệt>|<hồ sơ>|<mã tiện ích>`, dùng làm mã ở giao diện.
    pub id: String,
    pub browser: &'static str,
    /// Thư mục hồ sơ; `None` khi tiện ích mới chỉ được đăng ký trong registry.
    pub profile: Option<String>,
    pub extension_id: String,
    pub name: String,
    pub version: Option<String>,
    pub source: Source,
    /// `None` khi không biết (chưa cài vào hồ sơ nào).
    pub enabled: Option<bool>,
    pub removal: Removal,
    /// Vị trí registry sẽ bị xóa, để người dùng xem trước.
    pub registry: Vec<String>,
    pub needs_admin: bool,
    #[serde(skip)]
    pub refs: Vec<RegistryRef>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionScan {
    pub extensions: Vec<Extension>,
    /// Hồ sơ không đọc được (tệp hỏng, bị khóa...).
    pub skipped: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionRemoval {
    pub done: u64,
    pub failed: u64,
    pub record_id: Option<u64>,
    pub errors: Vec<String>,
}

struct Spec {
    id: &'static str,
    /// Thư mục dữ liệu, tính từ LocalAppData (Chromium) hoặc RoamingAppData (Firefox).
    data: &'static [&'static str],
    executable: &'static str,
    scheme: &'static str,
    registry: &'static [&'static str],
    policy: &'static [&'static str],
    firefox: bool,
}

static SPECS: [Spec; 4] = [
    Spec {
        id: "chrome",
        data: &["Google", "Chrome", "User Data"],
        executable: "chrome.exe",
        scheme: "chrome",
        registry: &[
            "SOFTWARE\\Google\\Chrome\\Extensions",
            "SOFTWARE\\WOW6432Node\\Google\\Chrome\\Extensions",
        ],
        policy: &["SOFTWARE\\Policies\\Google\\Chrome\\ExtensionInstallForcelist"],
        firefox: false,
    },
    Spec {
        id: "edge",
        data: &["Microsoft", "Edge", "User Data"],
        executable: "msedge.exe",
        scheme: "edge",
        registry: &[
            "SOFTWARE\\Microsoft\\Edge\\Extensions",
            "SOFTWARE\\WOW6432Node\\Microsoft\\Edge\\Extensions",
        ],
        policy: &["SOFTWARE\\Policies\\Microsoft\\Edge\\ExtensionInstallForcelist"],
        firefox: false,
    },
    Spec {
        id: "brave",
        data: &["BraveSoftware", "Brave-Browser", "User Data"],
        executable: "brave.exe",
        scheme: "brave",
        registry: &[],
        policy: &["SOFTWARE\\Policies\\BraveSoftware\\Brave\\ExtensionInstallForcelist"],
        firefox: false,
    },
    Spec {
        id: "firefox",
        data: &["Mozilla", "Firefox", "Profiles"],
        executable: "firefox.exe",
        scheme: "",
        registry: &[
            "SOFTWARE\\Mozilla\\Firefox\\Extensions",
            "SOFTWARE\\WOW6432Node\\Mozilla\\Firefox\\Extensions",
        ],
        policy: &[],
        firefox: true,
    },
];

/// Địa chỉ cập nhật của cửa hàng Chrome Web Store và Microsoft Edge Add-ons.
const STORE_UPDATE_URLS: [&str; 2] = [
    "https://clients2.google.com/service/update2/crx",
    "https://edge.microsoft.com/extensionwebstorebase/",
];
const PREFERENCES_LIMIT: u64 = 64 * 1024 * 1024;
const MANIFEST_LIMIT: u64 = 2 * 1024 * 1024;

/// Mã tiện ích Chromium: 32 ký tự a–p.
fn chromium_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|c| (b'a'..=b'p').contains(&c))
}

/// Thư mục hồ sơ Chromium: `Default` hoặc `Profile <số>`.
fn chromium_profile(name: &str) -> bool {
    name == "Default"
        || name
            .strip_prefix("Profile ")
            .is_some_and(|n| !n.is_empty() && n.len() <= 6 && n.bytes().all(|c| c.is_ascii_digit()))
}

/// Tên thư mục hồ sơ Firefox: chữ, số, `.`, `-`, `_`.
fn firefox_profile(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
}

fn read_json(path: &Path, limit: u64) -> Result<Value, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > limit {
        return Err("invalid_file".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes);
    serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())
}

fn plain_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}

/// Đường dẫn tương đối an toàn bên trong thư mục `Extensions` của hồ sơ.
fn relative_inside(path: &str) -> Option<PathBuf> {
    let relative = Path::new(path);
    if path.is_empty() || relative.is_absolute() || path.contains(':') {
        return None;
    }
    relative
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
        .then(|| relative.to_path_buf())
}

/// Dịch `__MSG_tên__` theo `_locales/<default_locale>/messages.json`.
fn localize(text: &str, dir: Option<&Path>, manifest: &Value) -> Option<String> {
    let key = text
        .strip_prefix("__MSG_")
        .and_then(|rest| rest.strip_suffix("__"));
    let Some(key) = key else {
        return Some(text.trim().to_string()).filter(|s| !s.is_empty());
    };
    let locale = manifest.get("default_locale")?.as_str()?;
    if !firefox_profile(locale) {
        return None;
    }
    let messages = read_json(
        &dir?.join("_locales").join(locale).join("messages.json"),
        MANIFEST_LIMIT,
    )
    .ok()?;
    messages
        .as_object()?
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .and_then(|(_, entry)| entry.get("message")?.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn merge_settings(target: &mut Map<String, Value>, source: &Map<String, Value>) {
    for (id, value) in source {
        match (target.get_mut(id), value) {
            (Some(Value::Object(existing)), Value::Object(extra)) => {
                for (key, field) in extra {
                    existing.entry(key.clone()).or_insert_with(|| field.clone());
                }
            }
            (None, _) => {
                target.insert(id.clone(), value.clone());
            }
            _ => {}
        }
    }
}

fn disabled(entry: &Value) -> bool {
    match entry.get("disable_reasons") {
        Some(Value::Array(reasons)) => !reasons.is_empty(),
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) != 0,
        _ => entry
            .get("state")
            .and_then(Value::as_i64)
            .is_some_and(|state| state != 1),
    }
}

/// Tiện ích trong một hồ sơ Chromium. Lỗi chỉ khi không đọc được tệp tùy chọn nào.
fn scan_chromium_profile(
    browser: &'static str,
    profile: &str,
    dir: &Path,
) -> Result<Vec<Extension>, String> {
    let mut settings = Map::new();
    let mut read = false;
    for file in ["Secure Preferences", "Preferences"] {
        let path = dir.join(file);
        if !path.exists() {
            continue;
        }
        let json = read_json(&path, PREFERENCES_LIMIT)?;
        read = true;
        if let Some(found) = json
            .pointer("/extensions/settings")
            .and_then(Value::as_object)
        {
            merge_settings(&mut settings, found);
        }
    }
    if !read {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for (extension_id, entry) in &settings {
        if !chromium_id(extension_id) {
            continue;
        }
        let location = entry.get("location").and_then(Value::as_i64).unwrap_or(0);
        // 5 = COMPONENT, 10 = EXTERNAL_COMPONENT: thành phần của chính trình duyệt.
        if location == 5 || location == 10 {
            continue;
        }
        let unpacked = location == 4 || location == 8;
        let ext_dir = entry.get("path").and_then(Value::as_str).and_then(|path| {
            if unpacked {
                let path = PathBuf::from(path);
                path.is_absolute().then_some(path)
            } else {
                relative_inside(path).map(|relative| dir.join("Extensions").join(relative))
            }
        });
        let stored = entry.get("manifest").filter(|m| m.is_object()).cloned();
        let manifest = match stored {
            Some(manifest) => Some(manifest),
            None => ext_dir
                .as_ref()
                .and_then(|d| read_json(&d.join("manifest.json"), MANIFEST_LIMIT).ok()),
        };
        // Mục cũ không còn tệp nào trên đĩa: trình duyệt đã gỡ nhưng chưa dọn tùy chọn.
        let Some(manifest) = manifest else {
            continue;
        };
        let name = manifest
            .get("name")
            .and_then(Value::as_str)
            .and_then(|name| localize(name, ext_dir.as_deref(), &manifest))
            .unwrap_or_else(|| extension_id.clone());
        let store_update = manifest
            .get("update_url")
            .and_then(Value::as_str)
            .is_some_and(|url| {
                STORE_UPDATE_URLS
                    .iter()
                    .any(|prefix| url.starts_with(prefix))
            });
        let source = match location {
            1 if store_update
                || entry.get("from_webstore").and_then(Value::as_bool) == Some(true) =>
            {
                Source::Store
            }
            4 | 8 => Source::Unpacked,
            2 | 3 | 6 => Source::External,
            7 | 9 => Source::Policy,
            _ => Source::Other,
        };
        found.push(Extension {
            id: format!("{browser}|{profile}|{extension_id}"),
            browser,
            profile: Some(profile.to_string()),
            extension_id: extension_id.clone(),
            name,
            version: manifest
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string),
            source,
            enabled: Some(!disabled(entry)),
            removal: Removal::Browser,
            registry: Vec::new(),
            needs_admin: false,
            refs: Vec::new(),
        });
    }
    Ok(found)
}

fn scan_firefox_profile(profile: &str, dir: &Path) -> Result<Vec<Extension>, String> {
    let path = dir.join("extensions.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let json = read_json(&path, PREFERENCES_LIMIT)?;
    let mut found = Vec::new();
    for addon in json
        .get("addons")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if addon.get("type").and_then(Value::as_str) != Some("extension")
            || addon.get("hidden").and_then(Value::as_bool) == Some(true)
            || addon.get("isSystem").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let Some(extension_id) = addon
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 256)
        else {
            continue;
        };
        let source = match addon.get("location").and_then(Value::as_str).unwrap_or("") {
            "app-profile" => Source::Store,
            "winreg-app-user" | "winreg-app-global" => Source::External,
            "app-global" | "app-system-user" => Source::Other,
            _ => continue, // app-builtin, app-system-defaults...: thành phần của Firefox.
        };
        let name = addon
            .pointer("/defaultLocale/name")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| extension_id.to_string());
        let enabled = addon.get("active").and_then(Value::as_bool).or_else(|| {
            addon
                .get("userDisabled")
                .and_then(Value::as_bool)
                .map(|d| !d)
        });
        found.push(Extension {
            id: format!("firefox|{profile}|{extension_id}"),
            browser: "firefox",
            profile: Some(profile.to_string()),
            extension_id: extension_id.to_string(),
            name,
            version: addon
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string),
            source,
            enabled,
            removal: Removal::Browser,
            registry: Vec::new(),
            needs_admin: false,
            refs: Vec::new(),
        });
    }
    Ok(found)
}

/// Mục registry cài tiện ích cho một trình duyệt, theo mã tiện ích.
fn registry_entries(spec: &Spec) -> Vec<(String, RegistryRef)> {
    let mut entries = Vec::new();
    let roots = spec
        .registry
        .iter()
        .map(|root| (true, *root))
        .chain(spec.registry.first().map(|root| (false, *root)));
    for (machine, root) in roots {
        if spec.firefox {
            for (name, value) in reg::values(machine, root) {
                if !name.is_empty() && name.len() <= 256 {
                    entries.push((
                        name.clone(),
                        RegistryRef::Value {
                            machine,
                            subkey: root.to_string(),
                            name,
                            value,
                        },
                    ));
                }
            }
        } else {
            for sub in reg::subkeys(machine, root) {
                let id = sub.to_ascii_lowercase();
                if chromium_id(&id) {
                    entries.push((
                        id,
                        RegistryRef::Key {
                            machine,
                            subkey: format!("{root}\\{sub}"),
                        },
                    ));
                }
            }
        }
    }
    entries
}

/// Mã tiện ích bị chính sách bắt cài (`<mã>;<update url>`).
fn policy_ids(spec: &Spec) -> HashSet<String> {
    let mut ids = HashSet::new();
    for root in spec.policy {
        for machine in [true, false] {
            for (_, value) in reg::values(machine, root) {
                if let Some(text) = value.as_string() {
                    let id = text
                        .split(';')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_ascii_lowercase();
                    if chromium_id(&id) {
                        ids.insert(id);
                    }
                }
            }
        }
    }
    ids
}

/// Gắn mục registry và chính sách vào danh sách tiện ích của một trình duyệt.
fn merge(
    browser: &'static str,
    mut items: Vec<Extension>,
    registry: Vec<(String, RegistryRef)>,
    policy: &HashSet<String>,
) -> Vec<Extension> {
    let mut by_id: BTreeMap<String, Vec<RegistryRef>> = BTreeMap::new();
    for (id, entry) in registry {
        by_id.entry(id).or_default().push(entry);
    }
    let key = |id: &str| {
        if browser == "firefox" {
            id.to_string()
        } else {
            id.to_ascii_lowercase()
        }
    };
    for item in &mut items {
        if let Some(refs) = by_id.get(&key(&item.extension_id)) {
            item.refs = refs.clone();
            item.source = Source::External;
        }
    }
    let known: HashSet<String> = items.iter().map(|item| key(&item.extension_id)).collect();
    for (id, refs) in by_id {
        if !known.contains(&id) {
            items.push(Extension {
                id: format!("{browser}|-|{id}"),
                browser,
                profile: None,
                extension_id: id.clone(),
                name: id,
                version: None,
                source: Source::External,
                enabled: None,
                removal: Removal::Registry,
                registry: Vec::new(),
                needs_admin: false,
                refs,
            });
        }
    }
    for item in &mut items {
        if policy.contains(&key(&item.extension_id)) {
            item.source = Source::Policy;
        }
        item.removal = if item.source == Source::Policy {
            Removal::None
        } else if !item.refs.is_empty() {
            Removal::Registry
        } else {
            Removal::Browser
        };
        item.registry = item.refs.iter().map(RegistryRef::display).collect();
        item.needs_admin = item.refs.iter().any(RegistryRef::machine);
    }
    items
}

fn scan_browser(spec: &Spec, base: &Path, skipped: &mut u64) -> Vec<Extension> {
    let root = spec
        .data
        .iter()
        .fold(base.to_path_buf(), |path, part| path.join(part));
    let mut items = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        let mut dirs: Vec<_> = entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        dirs.sort();
        for name in dirs {
            let dir = root.join(&name);
            let valid = if spec.firefox {
                firefox_profile(&name)
            } else {
                chromium_profile(&name)
            };
            if !valid || !plain_dir(&dir) {
                continue;
            }
            let result = if spec.firefox {
                scan_firefox_profile(&name, &dir)
            } else {
                scan_chromium_profile(spec.id, &name, &dir)
            };
            match result {
                Ok(found) => items.extend(found),
                Err(_) => *skipped += 1,
            }
        }
    }
    items
}

pub fn list() -> Result<ExtensionScan, String> {
    let local = winutil::known_folder(Known::LocalAppData).ok_or("not_found")?;
    let roaming = winutil::known_folder(Known::RoamingAppData).ok_or("not_found")?;
    let mut scan = ExtensionScan::default();
    for spec in &SPECS {
        let base = if spec.firefox { &roaming } else { &local };
        let items = scan_browser(spec, base, &mut scan.skipped);
        scan.extensions.extend(merge(
            spec.id,
            items,
            registry_entries(spec),
            &policy_ids(spec),
        ));
    }
    scan.extensions.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    Ok(scan)
}

/// Xóa mục registry của các tiện ích đã chọn (chỉ loại `Registry`). Mỗi mục được
/// kiểm tra lại ngay trước khi xóa và sao lưu vào Rescue Center.
pub fn remove(
    items: &[Extension],
    ids: &[String],
    rescue: &Rescue,
) -> Result<ExtensionRemoval, String> {
    if ids.is_empty() {
        return Err("not_found".into());
    }
    let mut refs: Vec<(&'static str, RegistryRef)> = Vec::new();
    for id in ids {
        let item = items
            .iter()
            .find(|item| &item.id == id)
            .ok_or("not_found")?;
        if item.removal != Removal::Registry {
            return Err("protected_app".into());
        }
        for entry in &item.refs {
            if !refs.iter().any(|(_, known)| known == entry) {
                refs.push((item.browser, entry.clone()));
            }
        }
    }
    remove_with(&refs, rescue, &|browser| {
        SPECS
            .iter()
            .find(|spec| spec.id == browser)
            .map(registry_entries)
            .unwrap_or_default()
    })
}

fn remove_with(
    refs: &[(&'static str, RegistryRef)],
    rescue: &Rescue,
    current: &dyn Fn(&str) -> Vec<(String, RegistryRef)>,
) -> Result<ExtensionRemoval, String> {
    let record_id = rescue.new_record_id();
    let mut report = ExtensionRemoval::default();
    let mut undo = Vec::new();
    for (index, (browser, entry)) in refs.iter().enumerate() {
        if !current(browser).iter().any(|(_, now)| now == entry) {
            report.failed += 1;
            report
                .errors
                .push(format!("changed_target: {}", entry.display()));
            continue;
        }
        let result = match entry {
            RegistryRef::Key { machine, subkey } => {
                let key = format!("{}\\{subkey}", reg::hive_name(*machine));
                rescue::export_registry_key(
                    rescue,
                    record_id,
                    &key,
                    &format!("extension-{index}.reg"),
                )
                .and_then(|file| {
                    reg::delete_key(*machine, subkey)?;
                    undo.push(UndoOp::ImportRegFile { file });
                    Ok(())
                })
            }
            RegistryRef::Value {
                machine,
                subkey,
                name,
                value,
            } => reg::delete_value(*machine, subkey, name).map(|()| {
                undo.push(UndoOp::RestoreRegistryValue {
                    machine: *machine,
                    subkey: subkey.clone(),
                    name: name.clone(),
                    previous: Some(value.clone()),
                });
            }),
        };
        match result {
            Ok(()) => report.done += 1,
            Err(error) => {
                report.failed += 1;
                report.errors.push(format!("{error}: {}", entry.display()));
            }
        }
    }
    if !undo.is_empty() {
        rescue.add_tool_record(
            record_id,
            "browserExtensions",
            report.done,
            report.failed,
            0,
            undo,
        )?;
        report.record_id = Some(record_id);
    }
    Ok(report)
}

/// Đường dẫn trình duyệt từ khóa App Paths của Windows.
fn browser_executable(spec: &Spec) -> Option<PathBuf> {
    let key = format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{}",
        spec.executable
    );
    [true, false].into_iter().find_map(|machine| {
        let raw = reg::read(machine, &key, "")?.as_string()?;
        let path = PathBuf::from(winutil::expand_env(raw.trim().trim_matches('"')));
        let name_ok = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(spec.executable));
        (path.is_absolute() && name_ok && path.is_file()).then_some(path)
    })
}

fn manage_args(spec: &Spec, item: &Extension) -> Vec<String> {
    if spec.firefox {
        return vec!["about:addons".into()];
    }
    let mut args = Vec::new();
    if let Some(profile) = item.profile.as_deref().filter(|p| chromium_profile(p)) {
        args.push(format!("--profile-directory={profile}"));
    }
    let id = if chromium_id(&item.extension_id) {
        format!("?id={}", item.extension_id)
    } else {
        String::new()
    };
    args.push(format!("{}://extensions/{id}", spec.scheme));
    args
}

/// Mở trang quản lý tiện ích của trình duyệt cho tiện ích đã liệt kê.
pub fn open_manage(item: &Extension) -> Result<(), String> {
    let spec = SPECS
        .iter()
        .find(|spec| spec.id == item.browser)
        .ok_or("not_found")?;
    let program = browser_executable(spec).ok_or("browser_not_found")?;
    std::process::Command::new(program)
        .args(manage_args(spec, item))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ID_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const ID_C: &str = "cccccccccccccccccccccccccccccccc";

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("TPExtensions{name}{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn validates_ids_and_profile_names() {
        assert!(chromium_id(ID_A));
        assert!(!chromium_id("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"));
        assert!(!chromium_id("aaaa"));
        assert!(chromium_profile("Default") && chromium_profile("Profile 12"));
        assert!(
            !chromium_profile("Guest Profile")
                && !chromium_profile("Profile ")
                && !chromium_profile("System Profile")
        );
        assert!(firefox_profile("ab12cd.default-release"));
        assert!(!firefox_profile("..\\x") && !firefox_profile(""));
        assert_eq!(
            relative_inside("abc\\1.0_0"),
            Some(PathBuf::from("abc\\1.0_0"))
        );
        assert!(relative_inside("..\\x").is_none() && relative_inside("C:\\x").is_none());
    }

    #[test]
    fn reads_chromium_profile_with_localized_names_and_skips_components() {
        let fixture = Fixture::new("Chromium");
        let profile = fixture.0.join("Default");
        let ext = profile.join("Extensions").join(ID_A).join("1.0_0");
        write(
            &ext.join("manifest.json"),
            r#"{"name":"__MSG_appName__","version":"1.0","default_locale":"en"}"#,
        );
        write(
            &ext.join("_locales/en/messages.json"),
            r#"{"APPNAME":{"message":"Ad Toolbar"}}"#,
        );
        let secure = format!(
            r#"{{"extensions":{{"settings":{{
                "{ID_A}":{{"location":3,"path":"{ID_A}\\1.0_0"}},
                "{ID_B}":{{"location":1,"from_webstore":true,"manifest":{{"name":"Store Tool","version":"2.0"}},"disable_reasons":[1]}},
                "{ID_C}":{{"location":5,"manifest":{{"name":"Component"}}}},
                "dddddddddddddddddddddddddddddddd":{{"location":1,"path":"dddddddddddddddddddddddddddddddd\\9_0"}}
            }}}}}}"#
        );
        write(&profile.join("Secure Preferences"), &secure);
        write(
            &profile.join("Preferences"),
            &format!(r#"{{"extensions":{{"settings":{{"{ID_B}":{{"state":1}}}}}}}}"#),
        );
        let mut found = scan_chromium_profile("chrome", "Default", &profile).unwrap();
        found.sort_by(|a, b| a.extension_id.cmp(&b.extension_id));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "Ad Toolbar");
        assert_eq!(found[0].source, Source::External);
        assert_eq!(found[0].enabled, Some(true));
        assert_eq!(found[1].name, "Store Tool");
        assert_eq!(found[1].source, Source::Store);
        assert_eq!(found[1].enabled, Some(false));
        assert_eq!(found[1].id, format!("chrome|Default|{ID_B}"));
    }

    #[test]
    fn broken_preferences_are_reported_not_hidden() {
        let fixture = Fixture::new("Broken");
        write(&fixture.0.join("Preferences"), "{not json");
        assert!(scan_chromium_profile("edge", "Default", &fixture.0).is_err());
        assert!(
            scan_chromium_profile("edge", "Default", &fixture.0.join("missing"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn reads_firefox_user_extensions_only() {
        let fixture = Fixture::new("Firefox");
        write(
            &fixture.0.join("extensions.json"),
            r#"{"addons":[
                {"id":"blocker@example","type":"extension","location":"app-profile","active":true,"version":"3","defaultLocale":{"name":"Blocker"}},
                {"id":"bar@vendor","type":"extension","location":"winreg-app-user","active":false,"defaultLocale":{"name":"Vendor Bar"}},
                {"id":"builtin@mozilla.org","type":"extension","location":"app-builtin","active":true},
                {"id":"theme@x","type":"theme","location":"app-profile","active":true}
            ]}"#,
        );
        let found = scan_firefox_profile("abc.default", &fixture.0).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(
            (found[0].name.as_str(), found[0].source, found[0].enabled),
            ("Blocker", Source::Store, Some(true))
        );
        assert_eq!(
            (found[1].name.as_str(), found[1].source, found[1].enabled),
            ("Vendor Bar", Source::External, Some(false))
        );
    }

    fn item(browser: &'static str, extension_id: &str) -> Extension {
        Extension {
            id: format!("{browser}|Default|{extension_id}"),
            browser,
            profile: Some("Default".into()),
            extension_id: extension_id.into(),
            name: extension_id.into(),
            version: None,
            source: Source::Store,
            enabled: Some(true),
            removal: Removal::Browser,
            registry: Vec::new(),
            needs_admin: false,
            refs: Vec::new(),
        }
    }

    #[test]
    fn merge_marks_registry_policy_and_registry_only_entries() {
        let key = |id: &str, machine| RegistryRef::Key {
            machine,
            subkey: format!("SOFTWARE\\Google\\Chrome\\Extensions\\{id}"),
        };
        let registry = vec![
            (ID_A.to_string(), key(ID_A, true)),
            (ID_C.to_string(), key(ID_C, false)),
        ];
        let policy: HashSet<String> = [ID_B.to_string()].into();
        let merged = merge(
            "chrome",
            vec![item("chrome", ID_A), item("chrome", ID_B)],
            registry,
            &policy,
        );
        let a = merged.iter().find(|i| i.extension_id == ID_A).unwrap();
        assert_eq!(
            (a.source, a.removal, a.needs_admin),
            (Source::External, Removal::Registry, true)
        );
        assert_eq!(
            a.registry,
            [format!(
                "HKLM\\SOFTWARE\\Google\\Chrome\\Extensions\\{ID_A}"
            )]
        );
        let b = merged.iter().find(|i| i.extension_id == ID_B).unwrap();
        assert_eq!((b.source, b.removal), (Source::Policy, Removal::None));
        let c = merged.iter().find(|i| i.extension_id == ID_C).unwrap();
        assert_eq!(
            (c.profile.as_deref(), c.removal, c.needs_admin),
            (None, Removal::Registry, false)
        );
    }

    #[test]
    fn remove_rejects_browser_and_policy_items() {
        let rescue = Rescue::at(std::env::temp_dir().join("TPExtensionsUnused"));
        let mut policy = item("chrome", ID_B);
        policy.removal = Removal::None;
        let items = [item("chrome", ID_A), policy];
        assert_eq!(
            remove(&items, &[items[0].id.clone()], &rescue).unwrap_err(),
            "protected_app"
        );
        assert_eq!(
            remove(&items, &[items[1].id.clone()], &rescue).unwrap_err(),
            "protected_app"
        );
        assert_eq!(
            remove(&items, &["other".into()], &rescue).unwrap_err(),
            "not_found"
        );
        assert_eq!(remove(&items, &[], &rescue).unwrap_err(), "not_found");
    }

    #[test]
    fn manage_arguments_target_profile_and_extension() {
        let chrome = &SPECS[0];
        assert_eq!(
            manage_args(chrome, &item("chrome", ID_A)),
            [
                "--profile-directory=Default".to_string(),
                format!("chrome://extensions/?id={ID_A}")
            ]
        );
        let mut odd = item("chrome", "x");
        odd.profile = Some("--evil".into());
        assert_eq!(
            manage_args(chrome, &odd),
            ["chrome://extensions/".to_string()]
        );
        assert_eq!(
            manage_args(&SPECS[3], &item("firefox", "a@b")),
            ["about:addons".to_string()]
        );
    }

    #[cfg(windows)]
    #[test]
    fn registry_removal_backs_up_rechecks_and_undoes() {
        let root = format!("Software\\TPExtensionFixture{}", std::process::id());
        let key_path = format!("{root}\\Chrome\\{ID_A}");
        let value_root = format!("{root}\\Firefox");
        reg::write(
            false,
            &key_path,
            "update_url",
            &RawValue::sz("https://example.invalid/update"),
        )
        .unwrap();
        let ff_value = RawValue::sz("C:\\Fixture\\bar.xpi");
        reg::write(false, &value_root, "bar@vendor", &ff_value).unwrap();
        let dir = std::env::temp_dir().join(format!("TPExtensionRescue{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rescue = Rescue::at(dir.clone());
        let key = RegistryRef::Key {
            machine: false,
            subkey: key_path.clone(),
        };
        let value = RegistryRef::Value {
            machine: false,
            subkey: value_root.clone(),
            name: "bar@vendor".into(),
            value: ff_value.clone(),
        };
        let stale = RegistryRef::Value {
            machine: false,
            subkey: value_root.clone(),
            name: "gone@vendor".into(),
            value: ff_value.clone(),
        };
        let current = |_: &str| {
            let mut now = Vec::new();
            if !reg::values(false, &key_path).is_empty() {
                now.push((
                    ID_A.to_string(),
                    RegistryRef::Key {
                        machine: false,
                        subkey: key_path.clone(),
                    },
                ));
            }
            for (name, raw) in reg::values(false, &value_root) {
                now.push((
                    name.clone(),
                    RegistryRef::Value {
                        machine: false,
                        subkey: value_root.clone(),
                        name,
                        value: raw,
                    },
                ));
            }
            now
        };
        let report = remove_with(
            &[
                ("chrome", key.clone()),
                ("firefox", value.clone()),
                ("firefox", stale),
            ],
            &rescue,
            &current,
        )
        .unwrap();
        let removed = reg::values(false, &key_path).is_empty()
            && reg::read(false, &value_root, "bar@vendor").is_none();
        let record = report.record_id.unwrap();
        rescue.undo(record).unwrap();
        let restored = reg::read(false, &key_path, "update_url").is_some()
            && reg::read(false, &value_root, "bar@vendor") == Some(ff_value);
        reg::delete_key(false, &root).unwrap();
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!((report.done, report.failed), (2, 1));
        assert!(report.errors[0].starts_with("changed_target"));
        assert!(removed && restored);
    }
}
