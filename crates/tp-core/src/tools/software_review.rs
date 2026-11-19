//! Read-only inventory; unknown management is protected and byte counts may be incomplete.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Extension {
    pub id: String,
    pub browser: String,
    pub profile: String,
    pub name: String,
    pub version: String,
    pub enabled: Option<bool>,
    pub managed: bool,
    pub permissions: Vec<String>,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionScan {
    pub extensions: Vec<Extension>,
    pub skipped: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledUpdate {
    pub id: String,
    pub title: String,
    pub kb: Vec<String>,
    /// WUA metadata only. Windows may still refuse removal; this is not an action.
    pub uninstallable: bool,
}

/// Return a URL to copy, not launch. The user must select the matching profile.
pub fn extension_manager(browser: &str) -> Result<&'static str, String> {
    match browser {
        "chrome" => Ok("chrome://extensions/"),
        "edge" => Ok("edge://extensions/"),
        "brave" => Ok("brave://extensions/"),
        "firefox" => Ok("about:addons"),
        _ => Err("unsupported_browser".into()),
    }
}

#[derive(Clone, Copy)]
struct Limits {
    profiles: usize,
    entries: usize,
    files: usize,
    extensions: usize,
    versions: usize,
    depth: usize,
    json: usize,
    total_json: usize,
}
const LIMITS: Limits = Limits {
    profiles: 96,
    entries: 30_000,
    files: 12_000,
    extensions: 2_000,
    versions: 16,
    depth: 12,
    json: 2 * 1024 * 1024,
    total_json: 16 * 1024 * 1024,
};

fn plain_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 255
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| {
            c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|' | '/' | '\\')
        })
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return false;
    }
    for prefix in ["COM", "LPT"] {
        if let Some(suffix) = stem.strip_prefix(prefix) {
            if matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            ) {
                return false;
            }
        }
    }
    true
}

fn plain_absolute(path: &Path) -> io::Result<()> {
    let text = path.to_str().ok_or_else(unsafe_path)?;
    if !path.is_absolute()
        || text.len() > 4096
        || path.components().count() > 64
        || text
            .split(['/', '\\'])
            .any(|part| part == "." || part == "..")
    {
        return Err(unsafe_path());
    }
    for part in path.components() {
        match part {
            Component::Normal(name) if name.to_str().is_some_and(plain_name) => {}
            Component::RootDir => {}
            #[cfg(windows)]
            Component::Prefix(prefix) if matches!(prefix.kind(), std::path::Prefix::Disk(_)) => {}
            _ => return Err(unsafe_path()),
        }
    }
    Ok(())
}
fn unsafe_path() -> io::Error {
    io::Error::other("unsafe_or_changed_path")
}

/// Open each ancestor top-down and retain non-write/non-delete-sharing handles.
/// OPEN_REPARSE_POINT applies to the leaf only, so checking only the leaf is NOT
/// sufficient. Parents remain pinned during enumeration, leaf opening and reads.
fn guarded(path: &Path, directory: Option<bool>) -> io::Result<(File, Vec<File>)> {
    guarded_impl(path, directory, true)
}

fn guarded_impl(
    path: &Path,
    directory: Option<bool>,
    single_link: bool,
) -> io::Result<(File, Vec<File>)> {
    plain_absolute(path)?;
    let mut ancestors: Vec<_> = path.ancestors().skip(1).collect();
    ancestors.reverse();
    let mut guards = Vec::with_capacity(ancestors.len());
    for parent in ancestors {
        guards.push(open_verified(parent, Some(true), false)?);
    }
    Ok((open_verified(path, directory, single_link)?, guards))
}

#[cfg(windows)]
fn open_verified(path: &Path, directory: Option<bool>, single_link: bool) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::*;
    let file = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags((FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS).0)
        .open(path)?;
    let handle = HANDLE(file.as_raw_handle());
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(|_| unsafe_path())?;
    let is_dir = info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
    if info.dwVolumeSerialNumber == 0
        || (info.nFileIndexHigh == 0 && info.nFileIndexLow == 0)
        || info.dwFileAttributes
            & (FILE_ATTRIBUTE_REPARSE_POINT.0 | FILE_ATTRIBUTE_DEVICE.0 | FILE_ATTRIBUTE_OFFLINE.0)
            != 0
        || directory.is_some_and(|expected| expected != is_dir)
        || (!is_dir && single_link && info.nNumberOfLinks != 1)
    {
        return Err(unsafe_path());
    }
    let mut resolved = vec![0u16; 32768];
    let len = unsafe {
        GetFinalPathNameByHandleW(handle, &mut resolved, GETFINALPATHNAMEBYHANDLE_FLAGS(0))
    } as usize;
    if len == 0 || len >= resolved.len() {
        return Err(unsafe_path());
    }
    let resolved = String::from_utf16(&resolved[..len]).map_err(|_| unsafe_path())?;
    let requested = path.to_str().ok_or_else(unsafe_path)?.replace('/', "\\");
    if !requested.eq_ignore_ascii_case(resolved.strip_prefix(r"\\?\").unwrap_or(&resolved)) {
        return Err(unsafe_path());
    }
    // Reject mapped network/removable drives as well as UNC/device paths.
    let root = format!("{}\\", &requested[..2]);
    if unsafe { GetDriveTypeW(&windows::core::HSTRING::from(root)) } != 3 {
        return Err(unsafe_path());
    }
    Ok(file)
}

#[cfg(not(windows))]
fn open_verified(_: &Path, _: Option<bool>, _: bool) -> io::Result<File> {
    // No weaker portable implementation that silently follows parent links.
    Err(io::Error::other("unsupported_platform"))
}

struct Scan {
    result: ExtensionScan,
    limits: Limits,
    profiles: usize,
    entries: usize,
    files: usize,
    json_bytes: usize,
}
impl Scan {
    fn new(limits: Limits) -> Self {
        Self {
            result: ExtensionScan::default(),
            limits,
            profiles: 0,
            entries: 0,
            files: 0,
            json_bytes: 0,
        }
    }
    fn skip(&mut self) {
        self.result.skipped += 1;
    }
    fn limit(&mut self) -> String {
        self.result.truncated = true;
        "review_limit".into()
    }
    fn exhausted(&self) -> bool {
        self.entries >= self.limits.entries
            || self.files >= self.limits.files
            || self.result.extensions.len() >= self.limits.extensions
    }
    fn profile(&mut self) -> Result<(), String> {
        if self.profiles >= self.limits.profiles {
            return Err(self.limit());
        }
        self.profiles += 1;
        Ok(())
    }
    fn file(&mut self) -> Result<(), String> {
        if self.files >= self.limits.files {
            return Err(self.limit());
        }
        self.files += 1;
        Ok(())
    }
    fn push(&mut self, extension: Extension) -> Result<(), String> {
        if self.result.extensions.len() >= self.limits.extensions {
            return Err(self.limit());
        }
        self.result.extensions.push(extension);
        Ok(())
    }
    fn names(&mut self, path: &Path, optional: bool) -> Result<Vec<String>, String> {
        let (_dir, _parents) = match guarded(path, Some(true)) {
            Err(e) if optional && e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            other => other.map_err(|e| e.to_string())?,
        };
        let mut names = Vec::new();
        for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
            if self.entries >= self.limits.entries {
                return Err(self.limit());
            }
            self.entries += 1;
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "invalid_filename")?;
            if !plain_name(&name) {
                self.skip();
                continue;
            }
            names.push(name);
        }
        names.sort();
        Ok(names)
    }
    fn json<T: serde::de::DeserializeOwned>(&mut self, path: &Path) -> Result<T, String> {
        self.file()?;
        let (file, _parents) = guarded(path, Some(false)).map_err(|e| e.to_string())?;
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        let remaining = self.limits.total_json.saturating_sub(self.json_bytes);
        let cap = self.limits.json.min(remaining);
        if size > cap as u64 {
            return Err(self.limit());
        }
        let bytes = bounded_read(file, cap).map_err(|error| {
            if error == "output_limit" {
                self.limit()
            } else {
                error
            }
        })?;
        self.json_bytes += bytes.len();
        if bytes.len() as u64 != size {
            return Err("changed_json".into());
        }
        // serde_json also has its default recursion limit (128); never disable it.
        serde_json::from_slice(&bytes).map_err(|_| "invalid_extension_json".into())
    }
    fn measure(&mut self, root: &Path) -> u64 {
        let mut bytes = 0u64;
        let mut pending = vec![(root.to_path_buf(), 0usize)];
        while let Some((path, depth)) = pending.pop() {
            if self.exhausted() {
                self.limit();
                self.skip();
                break;
            }
            let (file, _parents) = match guarded(&path, None) {
                Ok(value) => value,
                Err(_) => {
                    self.skip();
                    continue;
                }
            };
            let metadata = match file.metadata() {
                Ok(value) => value,
                Err(_) => {
                    self.skip();
                    continue;
                }
            };
            if metadata.is_file() {
                if self.file().is_err() {
                    self.skip();
                    break;
                }
                bytes = bytes.saturating_add(metadata.len());
            } else if metadata.is_dir() {
                match self.names(&path, false) {
                    Ok(names) if names.is_empty() => {}
                    Ok(_) if depth >= self.limits.depth => {
                        self.limit();
                        self.skip();
                    }
                    Ok(names) => {
                        pending.extend(names.into_iter().map(|name| (path.join(name), depth + 1)))
                    }
                    Err(_) => self.skip(),
                }
            } else {
                self.skip();
            }
        }
        bytes
    }
}

fn bounded_read(mut reader: impl Read, cap: usize) -> Result<Vec<u8>, String> {
    let mut result = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        // Read at most cap+1, including when the underlying stream has no EOF.
        let want = buf
            .len()
            .min(cap.saturating_sub(result.len()).saturating_add(1));
        let read = reader
            .read(&mut buf[..want])
            .map_err(|_| "output_read_failed")?;
        if read == 0 {
            return Ok(result);
        }
        if result.len() + read > cap {
            return Err("output_limit".into());
        }
        result.extend_from_slice(&buf[..read]);
    }
}

fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn permissions(mut values: Vec<String>) -> Result<Vec<String>, String> {
    if values.len() > 256 || values.iter().any(|v| !text(v, 2048)) {
        return Err("invalid_permissions".into());
    }
    values.sort();
    values.dedup();
    Ok(values)
}
fn chromium_profile(name: &str) -> bool {
    name == "Default"
        || name.strip_prefix("Profile ").is_some_and(|n| {
            !n.is_empty()
                && n.len() <= 10
                && n.bytes().all(|c| c.is_ascii_digit())
                && !n.starts_with('0')
                && n.parse::<u32>().is_ok()
        })
}
fn firefox_profile(name: &str) -> bool {
    name.len() <= 128
        && plain_name(name)
        && name.split_once('.').is_some_and(|(prefix, suffix)| {
            prefix.len() == 8
                && prefix.bytes().all(|c| c.is_ascii_alphanumeric())
                && !suffix.is_empty()
                && !suffix.starts_with('.')
        })
}
fn chromium_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}
fn chromium_version(value: &str) -> Option<[u16; 4]> {
    let mut parts = [0u16; 4];
    let mut count = 0;
    for (index, part) in value.split('.').enumerate() {
        if index >= 4
            || part.is_empty()
            || !part.bytes().all(|b| b.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            return None;
        }
        parts[index] = part.parse().ok()?;
        count += 1;
    }
    (count > 0).then_some(parts)
}
fn version_directory(name: &str) -> Option<([u16; 4], u32)> {
    let (version, generation) = name.split_once('_').unwrap_or((name, "0"));
    if generation.is_empty() || !generation.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((chromium_version(version)?, generation.parse().ok()?))
}
/// Length-delimited hex, not a path or launch argument; stable without hash collisions.
fn opaque_id(browser: &str, profile: &str, native_id: &str) -> String {
    use std::fmt::Write;
    let mut id = String::from("ext1");
    for part in [
        browser.to_owned(),
        profile.to_lowercase(),
        native_id.to_owned(),
    ] {
        id.push('-');
        for byte in part.bytes() {
            let _ = write!(id, "{byte:02x}");
        }
    }
    id
}

#[derive(Deserialize)]
struct Manifest {
    manifest_version: u32,
    name: String,
    version: String,
    #[serde(default)]
    default_locale: Option<String>,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    host_permissions: Vec<String>,
}
fn localized_name(scan: &mut Scan, version: &Path, manifest: &Manifest) -> Option<String> {
    let key = manifest.name.strip_prefix("__MSG_")?.strip_suffix("__")?;
    if key.is_empty()
        || key.len() > 128
        || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    let locale = manifest.default_locale.as_deref()?;
    if locale.is_empty()
        || locale.len() > 32
        || !locale
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        scan.skip();
        return None;
    }
    let path = version.join("_locales").join(locale).join("messages.json");
    let messages: serde_json::Map<String, serde_json::Value> = match scan.json(&path) {
        Ok(messages) => messages,
        Err(_) => {
            scan.skip();
            return None;
        }
    };
    let mut matches = messages
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(key));
    let message = matches.next()?.1.get("message")?.as_str()?;
    if matches.next().is_some() || !text(message, 512) || message.contains('$') {
        scan.skip();
        return None;
    }
    Some(message.to_owned())
}

fn manifest_extension(
    raw: Manifest,
    browser: &str,
    profile: &str,
    id: &str,
    folder: &str,
) -> Result<Extension, String> {
    if !matches!(raw.manifest_version, 2 | 3)
        || !text(&raw.name, 512)
        || chromium_version(&raw.version).is_none()
        || raw.version != folder.split('_').next().unwrap_or("")
        || !chromium_id(id)
        || !chromium_profile(profile)
    {
        return Err("invalid_manifest".into());
    }
    let mut requested = raw.permissions;
    requested.extend(raw.host_permissions);
    Ok(Extension {
        id: opaque_id(browser, profile, id),
        browser: browser.into(),
        profile: profile.into(),
        name: raw.name,
        version: raw.version,
        enabled: None,
        managed: true,
        permissions: permissions(requested)?,
        bytes: 0,
    })
}

fn scan_chromium(
    scan: &mut Scan,
    local: &Path,
    browser: &str,
    relative: &[&str],
) -> Result<(), String> {
    let data = relative
        .iter()
        .fold(local.to_path_buf(), |p, name| p.join(name));
    for profile in scan.names(&data, true)? {
        if !chromium_profile(&profile) {
            continue;
        }
        scan.profile()?;
        let extensions = data.join(&profile).join("Extensions");
        let ids = match scan.names(&extensions, true) {
            Ok(ids) => ids,
            Err(_) => {
                scan.skip();
                continue;
            }
        };
        for id in ids {
            if scan.exhausted() {
                return Err(scan.limit());
            }
            if !chromium_id(&id) {
                scan.skip();
                continue;
            }
            let result = (|| {
                let root = extensions.join(&id);
                let versions = scan.names(&root, false)?;
                if versions.len() > scan.limits.versions {
                    return Err(scan.limit());
                }
                let mut candidates = Vec::new();
                for folder in versions {
                    let key = version_directory(&folder).ok_or("invalid_version_directory")?;
                    candidates.push((key, folder));
                }
                candidates.sort();
                let (_, folder) = candidates.pop().ok_or("missing_version")?;
                let version = root.join(&folder);
                let raw: Manifest = scan.json(&version.join("manifest.json"))?;
                let name = localized_name(scan, &version, &raw);
                let mut extension = manifest_extension(raw, browser, &profile, &id, &folder)?;
                if let Some(name) = name {
                    extension.name = name;
                }
                extension.bytes = scan.measure(&version);
                scan.push(extension)
            })();
            if result.is_err() {
                scan.skip();
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FirefoxInventory {
    schema_version: u32,
    addons: Vec<FirefoxAddon>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FirefoxAddon {
    id: String,
    version: String,
    #[serde(rename = "type")]
    kind: String,
    default_locale: FirefoxLocale,
    #[serde(default, deserialize_with = "optional_bool")]
    active: Option<bool>,
    #[serde(default, deserialize_with = "optional_bool")]
    user_disabled: Option<bool>,
    #[serde(default, deserialize_with = "optional_bool")]
    app_disabled: Option<bool>,
    #[serde(default)]
    user_permissions: Option<FirefoxPermissions>,
}
// Missing flags are unknown; explicit null/string/number is malformed, not false.
fn optional_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    bool::deserialize(d).map(Some)
}
#[derive(Deserialize)]
struct FirefoxLocale {
    name: String,
}
#[derive(Deserialize)]
struct FirefoxPermissions {
    permissions: Vec<String>,
    origins: Vec<String>,
}
fn firefox_extensions(
    raw: FirefoxInventory,
    profile: &str,
    limit: usize,
) -> Result<Vec<Extension>, String> {
    if !firefox_profile(profile) || raw.schema_version == 0 || raw.schema_version > 1000 {
        return Err("invalid_firefox_inventory".into());
    }
    if raw.addons.len() > limit {
        return Err("review_limit".into());
    }
    let mut seen = HashSet::new();
    let mut extensions = Vec::new();
    for addon in raw.addons {
        if !text(&addon.id, 255)
            || !plain_name(&addon.id)
            || !seen.insert(addon.id.clone())
            || !text(&addon.default_locale.name, 512)
            || !text(&addon.version, 128)
            || !text(&addon.kind, 64)
        {
            return Err("invalid_firefox_addon".into());
        }
        if addon.kind != "extension" {
            continue;
        }
        if addon.active == Some(true)
            && (addon.user_disabled == Some(true) || addon.app_disabled == Some(true))
        {
            return Err("inconsistent_firefox_state".into());
        }
        let enabled = match (addon.active, addon.user_disabled, addon.app_disabled) {
            (Some(false), _, _) | (_, Some(true), _) | (_, _, Some(true)) => Some(false),
            (Some(true), Some(false), Some(false)) => Some(true),
            _ => None,
        };
        let mut requested = Vec::new();
        if let Some(p) = addon.user_permissions {
            requested.extend(p.permissions);
            requested.extend(p.origins);
        }
        extensions.push(Extension {
            id: opaque_id("firefox", profile, &addon.id),
            browser: "firefox".into(),
            profile: profile.into(),
            name: addon.default_locale.name,
            version: addon.version,
            enabled,
            managed: true,
            permissions: permissions(requested)?,
            bytes: 0,
        });
    }
    Ok(extensions)
}
fn scan_firefox(scan: &mut Scan, roaming: &Path) -> Result<(), String> {
    let profiles = roaming.join("Mozilla").join("Firefox").join("Profiles");
    for profile in scan.names(&profiles, true)? {
        if !firefox_profile(&profile) {
            scan.skip();
            continue;
        }
        scan.profile()?;
        if scan.exhausted() {
            return Err(scan.limit());
        }
        let result = (|| {
            let path = profiles.join(&profile).join("extensions.json");
            let raw: FirefoxInventory = scan.json(&path)?;
            let remaining = scan
                .limits
                .extensions
                .saturating_sub(scan.result.extensions.len());
            for extension in firefox_extensions(raw, &profile, remaining)? {
                scan.push(extension)?;
            }
            Ok::<_, String>(())
        })();
        if let Err(error) = result {
            if error == "review_limit" {
                scan.limit();
            }
            scan.skip();
        }
    }
    Ok(())
}

fn scan_at(local: &Path, roaming: &Path, limits: Limits) -> Result<ExtensionScan, String> {
    // A redirected/inaccessible current-user root is an error, not an empty scan.
    let _local = guarded(local, Some(true)).map_err(|e| e.to_string())?;
    let _roaming = guarded(roaming, Some(true)).map_err(|e| e.to_string())?;
    let mut scan = Scan::new(limits);
    for (browser, parts) in [
        ("chrome", &["Google", "Chrome", "User Data"][..]),
        ("edge", &["Microsoft", "Edge", "User Data"][..]),
        (
            "brave",
            &["BraveSoftware", "Brave-Browser", "User Data"][..],
        ),
    ] {
        if scan_chromium(&mut scan, local, browser, parts).is_err() {
            scan.skip();
        }
    }
    if scan_firefox(&mut scan, roaming).is_err() {
        scan.skip();
    }
    scan.result.extensions.sort_by(|a, b| {
        a.browser
            .cmp(&b.browser)
            .then(a.profile.cmp(&b.profile))
            .then(a.id.cmp(&b.id))
    });
    Ok(scan.result)
}

#[cfg(windows)]
pub fn extensions() -> Result<ExtensionScan, String> {
    use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_RoamingAppData};
    scan_at(
        &native::known_folder(&FOLDERID_LocalAppData)?,
        &native::known_folder(&FOLDERID_RoamingAppData)?,
        LIMITS,
    )
}
#[cfg(not(windows))]
pub fn extensions() -> Result<ExtensionScan, String> {
    Err("unsupported_platform".into())
}

const MAX_UPDATES: usize = 4096;
const UPDATE_STDOUT: usize = 8 * 1024 * 1024;
const UPDATE_STDERR: usize = 64 * 1024;
const UPDATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
// No input substitution, profiles, online searches, installers or package removal.
const UPDATE_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version 2.0
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
try {
    $session = New-Object -ComObject Microsoft.Update.Session
    $searcher = $session.CreateUpdateSearcher()
    $searcher.Online = $false
    $searcher.IncludePotentiallySupersededUpdates = $true
    $result = $searcher.Search("IsInstalled=1 and Type='Software'")
    if ([int]$result.ResultCode -ne 2 -or $result.Warnings.Count -ne 0) { throw 'incomplete' }
    $count = $result.Updates.Count
    if ($count -gt 4096) { throw 'limit' }
    $items = @(for ($i = 0; $i -lt $count; $i++) {
        $u = $result.Updates.Item($i)
        if (-not $u.IsInstalled -or [int]$u.Type -ne 1) { throw 'unexpected' }
        $title = [string]$u.Title
        if ($title.Length -gt 2048 -or $u.KBArticleIDs.Count -gt 64) { throw 'limit' }
        $kb = @(for ($j = 0; $j -lt $u.KBArticleIDs.Count; $j++) {
            $article = [string]$u.KBArticleIDs.Item($j)
            if ($article -notmatch '^[0-9]{4,10}$') { throw 'invalid_kb' }
            'KB' + $article
        })
        [pscustomobject]@{ id = [string]$u.Identity.UpdateID; title = $title; kb = $kb; uninstallable = [bool]$u.IsUninstallable }
    })
    $json = ConvertTo-Json -InputObject ([pscustomobject]@{ complete = $true; count = $count; updates = $items }) -Depth 5 -Compress
    if ($json.Length -gt 8388608) { throw 'limit' }
    [Console]::Out.Write($json)
    exit 0
} catch {
    [Console]::Error.Write('installed_update_query_failed')
    exit 1
}
"#;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateEnvelope {
    complete: bool,
    count: usize,
    updates: Vec<InstalledUpdate>,
}
fn guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
        && value.bytes().any(|b| b != b'0' && b != b'-')
}
fn parse_updates(bytes: &[u8]) -> Result<Vec<InstalledUpdate>, String> {
    if bytes.len() > UPDATE_STDOUT {
        return Err("update_output_limit".into());
    }
    let raw: UpdateEnvelope = serde_json::from_slice(bytes).map_err(|_| "invalid_update_json")?;
    if !raw.complete || raw.count != raw.updates.len() || raw.count > MAX_UPDATES {
        return Err("incomplete_update_inventory".into());
    }
    let mut seen = HashSet::new();
    let mut updates = raw.updates;
    for update in &mut updates {
        if !guid(&update.id)
            || !seen.insert(update.id.to_ascii_lowercase())
            || !text(&update.title, 2048)
            || update.kb.len() > 64
        {
            return Err("invalid_update".into());
        }
        let mut articles = HashSet::new();
        for kb in &update.kb {
            if !kb.strip_prefix("KB").is_some_and(|n| {
                (4..=10).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit())
            }) || !articles.insert(kb)
            {
                return Err("invalid_update_kb".into());
            }
        }
        update.id.make_ascii_lowercase();
        update.kb.sort();
    }
    updates.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
    Ok(updates)
}

/// Offline WUA installed-software inventory, not an exhaustive CBS package list.
/// Errors, cancellation, timeouts and partial WUA results never become an empty list.
pub fn updates(cancel: &AtomicBool) -> Result<Vec<InstalledUpdate>, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    #[cfg(windows)]
    {
        let bytes = native::query(cancel)?;
        let result = parse_updates(&bytes)?;
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        Ok(result)
    }
    #[cfg(not(windows))]
    Err("unsupported_platform".into())
}

/// Open Windows' installed-updates UI; success means launched, NOT removed.
pub fn update_settings() -> Result<(), String> {
    #[cfg(windows)]
    {
        native::settings()
    }
    #[cfg(not(windows))]
    {
        Err("unsupported_platform".into())
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::{mpsc, Mutex};
    use std::time::{Duration, Instant};
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};

    static QUERY: Mutex<()> = Mutex::new(());
    pub(super) fn known_folder(id: &windows::core::GUID) -> Result<PathBuf, String> {
        unsafe {
            let raw = SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None)
                .map_err(|_| "user_folder_unavailable")?;
            let path = raw
                .to_string()
                .map(PathBuf::from)
                .map_err(|_| "invalid_user_folder".to_string());
            CoTaskMemFree(Some(raw.as_ptr().cast()));
            path
        }
    }
    fn system32() -> Result<PathBuf, String> {
        let mut buffer = [0u16; 32768];
        let len = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
        if len == 0 || len >= buffer.len() {
            return Err("system_directory_unavailable".into());
        }
        let path = PathBuf::from(
            String::from_utf16(&buffer[..len]).map_err(|_| "invalid_system_directory")?,
        );
        plain_absolute(&path).map_err(|e| e.to_string())?;
        // Refuse a redirected 32-bit process rather than falling back to PATH.
        if !path
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("System32"))
        {
            return Err("native_system32_required".into());
        }
        Ok(path)
    }
    struct Process(Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    pub(super) fn query(cancel: &AtomicBool) -> Result<Vec<u8>, String> {
        // Don't queue behind a potentially 90-second query or accumulate readers.
        let _query = QUERY.try_lock().map_err(|_| "update_query_busy")?;
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let system = system32()?;
        let program = system
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        // Windows-protected executables can legitimately have WinSxS hardlinks.
        let _verified = guarded_impl(&program, Some(false), false).map_err(|e| e.to_string())?;
        let mut process = Process(
            Command::new(&program)
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    UPDATE_SCRIPT,
                ])
                .current_dir(&system)
                // Never auto-import a module from the elevated user's profile/PATH.
                .env(
                    "PSModulePath",
                    system.join("WindowsPowerShell/v1.0/Modules"),
                )
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(0x0800_0000)
                .spawn()
                .map_err(|_| "update_query_start_failed")?,
        );
        // Launch has resolved the verified system image; don't pin System32 for
        // the entire WUA search and interfere with unrelated Windows servicing.
        drop(_verified);
        let stdout = process.0.stdout.take().ok_or("missing_stdout")?;
        let stderr = process.0.stderr.take().ok_or("missing_stderr")?;
        let (sender, receiver) = mpsc::sync_channel(2);
        let out_sender = sender.clone();
        // Readers allocate at most their caps. The parent never blocks on join;
        // even pipe EOF after process exit must arrive before the same deadline.
        std::thread::Builder::new()
            .spawn(move || {
                let _ = out_sender.send((true, bounded_read(stdout, UPDATE_STDOUT)));
            })
            .map_err(|_| "update_reader_failed")?;
        std::thread::Builder::new()
            .spawn(move || {
                let _ = sender.send((false, bounded_read(stderr, UPDATE_STDERR)));
            })
            .map_err(|_| "update_reader_failed")?;
        let start = Instant::now();
        let mut output = None;
        let mut error = None;
        let mut status = None;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            if start.elapsed() >= UPDATE_TIMEOUT {
                return Err("update_query_timeout".into());
            }
            if status.is_none() {
                status = process
                    .0
                    .try_wait()
                    .map_err(|_| "update_query_wait_failed")?;
            }
            if status.is_some_and(|s| !s.success()) {
                return Err("installed_update_query_failed".into());
            }
            if status.is_some() && output.is_some() && error.is_some() {
                if !error.as_ref().is_some_and(Vec::is_empty) {
                    return Err("update_query_diagnostics".into());
                }
                return output.ok_or_else(|| "missing_update_output".into());
            }
            match receiver.recv_timeout(Duration::from_millis(25)) {
                Ok((is_stdout, result)) => {
                    let bytes = result?;
                    if is_stdout {
                        output = Some(bytes);
                    } else {
                        error = Some(bytes);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected)
                    if output.is_none() || error.is_none() =>
                {
                    return Err("update_reader_failed".into());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Both readers finished, but the process has not exited yet.
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }
    pub(super) fn settings() -> Result<(), String> {
        let system = system32()?;
        let program = system.join("control.exe");
        // Windows-protected executables can legitimately have WinSxS hardlinks.
        let _verified = guarded_impl(&program, Some(false), false).map_err(|e| e.to_string())?;
        Command::new(&program)
            .args([
                "/name",
                "Microsoft.WindowsUpdate",
                "/page",
                "pageInstalledUpdates",
            ])
            .current_dir(&system)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|_| "update_settings_start_failed".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const EXT: &str = "abcdefghijklmnopabcdefghijklmnop";
    fn manifest() -> Value {
        json!({"manifest_version":3,"name":"Fixture extension","version":"1.2.3","permissions":["storage"],"host_permissions":["https://example.invalid/*"]})
    }
    fn firefox() -> Value {
        json!({"schemaVersion":35,"addons":[{"id":"fixture@example.invalid","type":"extension","version":"1.0","defaultLocale":{"name":"Fixture"},"active":true,"userDisabled":false,"appDisabled":false,"userPermissions":{"permissions":["storage"],"origins":[]},"path":"C:\\outside\\never-read.xpi"}]})
    }
    fn update() -> Value {
        json!({"complete":true,"count":1,"updates":[{"id":"12345678-1234-1234-1234-123456789abc","title":"Fixture update","kb":["KB1234567"],"uninstallable":true}]})
    }
    fn parse_update(value: &Value) -> Result<Vec<InstalledUpdate>, String> {
        parse_updates(&serde_json::to_vec(value).unwrap())
    }

    #[test]
    fn managers_are_only_fixed_urls() {
        for (name, url) in [
            ("chrome", "chrome://extensions/"),
            ("edge", "edge://extensions/"),
            ("brave", "brave://extensions/"),
            ("firefox", "about:addons"),
        ] {
            assert_eq!(extension_manager(name).unwrap(), url);
        }
        for name in [
            "Chrome",
            " chrome",
            "firefox --profile x",
            "chrome://extensions/",
            "",
            "edge\0",
        ] {
            assert!(extension_manager(name).is_err());
        }
    }
    #[test]
    fn identity_is_stable_and_profile_scoped() {
        assert_eq!(
            opaque_id("chrome", "Default", EXT),
            opaque_id("chrome", "default", EXT)
        );
        assert_ne!(
            opaque_id("chrome", "Default", EXT),
            opaque_id("edge", "Default", EXT)
        );
        assert_ne!(
            opaque_id("chrome", "Default", EXT),
            opaque_id("chrome", "Profile 1", EXT)
        );
        assert_ne!(opaque_id("a-b", "c", "d"), opaque_id("a", "b-c", "d"));
    }
    #[test]
    fn strict_names_profiles_versions() {
        for name in [
            "..",
            ".",
            "x/../y",
            "x\\y",
            "file:stream",
            "NUL.json",
            "COM¹",
            "name.",
            "name ",
            "a\0b",
        ] {
            assert!(!plain_name(name), "{name:?}");
        }
        for name in ["Default", "Profile 1", "Profile 123"] {
            assert!(chromium_profile(name));
        }
        for name in [
            "Guest Profile",
            "System Profile",
            "Profile 0",
            "Profile 01",
            "Profile +1",
            "Profile 1/..",
            "Profile 4294967296",
        ] {
            assert!(!chromium_profile(name));
        }
        assert!(firefox_profile("abcdefgh.default-release"));
        for name in [
            "default",
            "abcd.default",
            "abcdefgh.",
            "abcdefgh..x",
            "abcdefgh.x/..",
        ] {
            assert!(!firefox_profile(name));
        }
    }
    #[test]
    fn malformed_manifest_is_not_coerced() {
        let good: Manifest = serde_json::from_value(manifest()).unwrap();
        let item = manifest_extension(good, "chrome", "Default", EXT, "1.2.3_0").unwrap();
        assert_eq!(item.enabled, None);
        assert!(item.managed);
        assert_eq!(item.permissions.len(), 2);
        for (key, value) in [
            ("name", json!(null)),
            ("version", json!(12)),
            ("permissions", json!([true])),
            ("host_permissions", json!(null)),
            ("manifest_version", json!("3")),
        ] {
            let mut raw = manifest();
            raw[key] = value;
            assert!(serde_json::from_value::<Manifest>(raw).is_err());
        }
        for value in ["", "1.2.3.4.5", "01.2", "65536", "1..2", "1/2"] {
            assert!(chromium_version(value).is_none());
        }
        assert!(manifest_extension(
            serde_json::from_value(manifest()).unwrap(),
            "chrome",
            "Default",
            EXT,
            "2.0_0"
        )
        .is_err());
        assert!(serde_json::from_str::<Manifest>(
            r#"{"manifest_version":3,"name":"a","name":"b","version":"1"}"#
        )
        .is_err());
        assert!(permissions(vec!["x".into(); 257]).is_err());
    }
    #[test]
    fn firefox_flags_paths_and_inventory_are_untrusted() {
        let parsed = firefox_extensions(
            serde_json::from_value(firefox()).unwrap(),
            "abcdefgh.default",
            10,
        )
        .unwrap();
        assert_eq!(parsed[0].enabled, Some(true));
        assert_eq!(parsed[0].bytes, 0);
        assert!(parsed[0].managed);
        for value in [json!(null), json!(1), json!("false")] {
            let mut raw = firefox();
            raw["addons"][0]["active"] = value;
            assert!(serde_json::from_value::<FirefoxInventory>(raw).is_err());
        }
        let mut raw = firefox();
        raw["addons"][0].as_object_mut().unwrap().remove("active");
        assert_eq!(
            firefox_extensions(serde_json::from_value(raw).unwrap(), "abcdefgh.default", 10)
                .unwrap()[0]
                .enabled,
            None
        );
        let mut raw = firefox();
        raw["addons"][0]["userDisabled"] = json!(true);
        assert!(
            firefox_extensions(serde_json::from_value(raw).unwrap(), "abcdefgh.default", 10)
                .is_err()
        );
        assert!(firefox_extensions(
            serde_json::from_value(firefox()).unwrap(),
            "abcdefgh.default",
            0
        )
        .is_err());
        let mut raw = firefox();
        let addon = raw["addons"][0].clone();
        raw["addons"].as_array_mut().unwrap().push(addon);
        assert!(
            firefox_extensions(serde_json::from_value(raw).unwrap(), "abcdefgh.default", 10)
                .is_err()
        );
    }
    #[test]
    fn update_json_requires_complete_exact_inventory() {
        assert_eq!(parse_update(&update()).unwrap().len(), 1);
        assert!(
            parse_update(&json!({"complete":true,"count":0,"updates":[]}))
                .unwrap()
                .is_empty()
        );
        for raw in [
            json!([]),
            json!(null),
            json!({}),
            json!({"complete":false,"count":0,"updates":[]}),
            json!({"complete":true,"count":1,"updates":[]}),
        ] {
            assert!(parse_update(&raw).is_err());
        }
        for (field, value) in [
            ("id", json!("not-a-guid")),
            ("title", json!("")),
            ("uninstallable", json!(null)),
            ("uninstallable", json!("true")),
            ("kb", json!(["12345"])),
            ("kb", json!(["KB12345", "KB12345"])),
            ("kb", json!([1])),
        ] {
            let mut raw = update();
            raw["updates"][0][field] = value;
            assert!(parse_update(&raw).is_err());
        }
        let mut raw = update();
        raw["updates"][0]["extra"] = json!(true);
        assert!(parse_update(&raw).is_err());
        let mut raw = update();
        raw["count"] = json!(2);
        let item = raw["updates"][0].clone();
        raw["updates"].as_array_mut().unwrap().push(item);
        assert!(parse_update(&raw).is_err());
        for raw in [
            b"".as_slice(),
            b"{",
            b"[] trailing",
            b"\xff",
            b"{\"complete\":true,\"complete\":true,\"count\":0,\"updates\":[]}",
        ] {
            assert!(parse_updates(raw).is_err());
        }
        assert!(parse_updates(&vec![b' '; UPDATE_STDOUT + 1]).is_err());
        assert_eq!(updates(&AtomicBool::new(true)).unwrap_err(), "cancelled");
    }
    #[test]
    fn output_read_is_bounded_and_errors_are_not_empty() {
        assert_eq!(bounded_read(&b"1234"[..], 4).unwrap(), b"1234");
        assert!(bounded_read(&b"12345"[..], 4).is_err());
        assert!(bounded_read(io::repeat(b'x'), 32).is_err());
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("fixture"))
            }
        }
        assert!(bounded_read(Broken, 10).is_err());
    }
    #[test]
    fn update_script_contract_is_offline_and_fixed() {
        assert!(UPDATE_SCRIPT.contains("$searcher.Online = $false"));
        assert!(UPDATE_SCRIPT.contains("IsInstalled=1 and Type='Software'"));
        assert!(UPDATE_SCRIPT.contains("$result.Warnings.Count -ne 0"));
        assert!(UPDATE_SCRIPT.contains("$result.ResultCode -ne 2"));
        for forbidden in [
            "Remove-",
            "Uninstall()",
            "CreateUpdateInstaller",
            "dism.exe",
            "Start-Process",
        ] {
            assert!(!UPDATE_SCRIPT.contains(forbidden));
        }
    }

    #[cfg(windows)]
    mod fixtures {
        use super::*;
        use std::sync::atomic::AtomicU64;
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        struct Fixture(PathBuf);
        impl Fixture {
            fn new() -> Self {
                let path = std::env::temp_dir().join(format!(
                    "tp-software-review-{}-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos(),
                    SERIAL.fetch_add(1, Ordering::Relaxed)
                ));
                fs::create_dir(&path).unwrap();
                // Windows TEMP may use an 8.3 alias; production rejects aliases.
                // Normalize only this newly created, owned fixture root.
                let resolved = fs::canonicalize(&path).unwrap();
                let text = resolved.to_str().unwrap();
                Self(PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text)))
            }
            fn write(&self, relative: &str, value: &[u8]) -> PathBuf {
                let path = self.0.join(relative);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, value).unwrap();
                path
            }
            fn chromium(&self) -> PathBuf {
                self.write(
                    &format!(
                        "Google/Chrome/User Data/Default/Extensions/{EXT}/1.2.3_0/manifest.json"
                    ),
                    &serde_json::to_vec(&manifest()).unwrap(),
                )
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        #[test]
        fn scans_only_owned_fixtures_and_counts_bytes() {
            let fixture = Fixture::new();
            let manifest = fixture.chromium();
            let resource = fixture.write(
                &format!("Google/Chrome/User Data/Default/Extensions/{EXT}/1.2.3_0/code.js"),
                b"fixture",
            );
            fixture.write(
                "Mozilla/Firefox/Profiles/abcdefgh.default/extensions.json",
                &serde_json::to_vec(&firefox()).unwrap(),
            );
            let result = scan_at(&fixture.0, &fixture.0, LIMITS).unwrap();
            assert_eq!(result.extensions.len(), 2);
            assert_eq!(result.skipped, 0);
            assert!(!result.truncated);
            assert_eq!(
                result.extensions[0].bytes,
                fs::metadata(manifest).unwrap().len() + fs::metadata(resource).unwrap().len()
            );
        }
        #[test]
        fn localized_manifest_names_stay_inside_verified_version() {
            let fixture = Fixture::new();
            let path = fixture.chromium();
            let mut raw = manifest();
            raw["name"] = json!("__MSG_appName__");
            raw["default_locale"] = json!("vi");
            fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
            fixture.write(
                &format!("Google/Chrome/User Data/Default/Extensions/{EXT}/1.2.3_0/_locales/vi/messages.json"),
                &serde_json::to_vec(&json!({"appName":{"message":"Tiện ích thử nghiệm"}})).unwrap(),
            );
            let result = scan_at(&fixture.0, &fixture.0, LIMITS).unwrap();
            assert_eq!(result.extensions[0].name, "Tiện ích thử nghiệm");
            assert_eq!(result.skipped, 0);
            for locale in ["../../outside", "vi/../../outside", "C:\\outside", "..", ""] {
                raw["default_locale"] = json!(locale);
                fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
                let result = scan_at(&fixture.0, &fixture.0, LIMITS).unwrap();
                assert_eq!(result.extensions[0].name, "__MSG_appName__");
                assert!(result.skipped > 0);
            }
        }

        #[test]
        fn latest_version_is_not_claimed_active() {
            let fixture = Fixture::new();
            fixture.chromium();
            let mut newer = manifest();
            newer["version"] = json!("2.0");
            fixture.write(
                &format!("Google/Chrome/User Data/Default/Extensions/{EXT}/2.0_0/manifest.json"),
                &serde_json::to_vec(&newer).unwrap(),
            );
            let result = scan_at(&fixture.0, &fixture.0, LIMITS).unwrap();
            assert_eq!(result.extensions.len(), 1);
            assert_eq!(result.extensions[0].version, "2.0");
            assert_eq!(result.extensions[0].enabled, None);
        }
        #[test]
        fn malformed_and_limits_are_visible() {
            let fixture = Fixture::new();
            let path = fixture.chromium();
            fs::write(&path, b"{").unwrap();
            let result = scan_at(&fixture.0, &fixture.0, LIMITS).unwrap();
            assert!(result.extensions.is_empty());
            assert!(result.skipped > 0);
            fixture.chromium();
            for limits in [
                Limits {
                    profiles: 0,
                    ..LIMITS
                },
                Limits {
                    entries: 0,
                    ..LIMITS
                },
                Limits { files: 0, ..LIMITS },
                Limits { json: 1, ..LIMITS },
                Limits {
                    total_json: 1,
                    ..LIMITS
                },
                Limits {
                    extensions: 0,
                    ..LIMITS
                },
                Limits {
                    versions: 0,
                    ..LIMITS
                },
                Limits { depth: 0, ..LIMITS },
            ] {
                let result = scan_at(&fixture.0, &fixture.0, limits).unwrap();
                assert!(result.skipped > 0);
                assert!(result.truncated);
            }
        }
        #[test]
        fn rejects_aliases_and_never_reads_through_links() {
            let fixture = Fixture::new();
            let manifest = fixture.chromium();
            let alias = fixture.0.join("hardlink.json");
            fs::hard_link(&manifest, &alias).unwrap();
            let result = scan_at(&fixture.0, &fixture.0, LIMITS).unwrap();
            assert!(result.extensions.is_empty());
            assert!(result.skipped > 0);
            assert!(guarded(&fixture.0.join("Google/../Google"), Some(true)).is_err());
            for path in [
                r"C:\x\..\y",
                r"C:\x\.\y",
                r"C:\x\data:stream",
                r"\\server\share\x",
                r"\\?\C:\x",
                r"C:relative",
            ] {
                assert!(plain_absolute(Path::new(path)).is_err());
            }
        }
        // Test-only junction creation, entirely inside an owned fixture. This
        // needs no shell, symlink privilege, dependency or machine configuration.
        fn junction(fixture: &Fixture, path: &Path, target: &Path) {
            use std::os::windows::fs::OpenOptionsExt;
            use std::os::windows::io::AsRawHandle;
            assert!(path.starts_with(&fixture.0) && target.starts_with(&fixture.0));
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn DeviceIoControl(
                    handle: *mut std::ffi::c_void,
                    code: u32,
                    input: *const std::ffi::c_void,
                    input_len: u32,
                    output: *mut std::ffi::c_void,
                    output_len: u32,
                    returned: *mut u32,
                    overlapped: *mut std::ffi::c_void,
                ) -> i32;
            }
            let print: Vec<u16> = target.to_str().unwrap().encode_utf16().collect();
            let substitute: Vec<u16> = format!(r"\??\{}", target.display())
                .encode_utf16()
                .collect();
            let mut data = Vec::new();
            data.extend_from_slice(&0xa0000003u32.to_le_bytes());
            for value in [
                (8 + (substitute.len() + print.len() + 2) * 2) as u16,
                0,
                0,
                (substitute.len() * 2) as u16,
                ((substitute.len() + 1) * 2) as u16,
                (print.len() * 2) as u16,
            ] {
                data.extend_from_slice(&value.to_le_bytes());
            }
            for value in substitute.into_iter().chain([0]).chain(print).chain([0]) {
                data.extend_from_slice(&value.to_le_bytes());
            }
            fs::create_dir(path).unwrap();
            let file = fs::OpenOptions::new()
                .access_mode(0x40000000)
                .share_mode(7)
                .custom_flags(0x02200000)
                .open(path)
                .unwrap();
            let mut returned = 0;
            assert_ne!(
                unsafe {
                    DeviceIoControl(
                        file.as_raw_handle(),
                        0x900a4,
                        data.as_ptr().cast(),
                        data.len() as u32,
                        std::ptr::null_mut(),
                        0,
                        &mut returned,
                        std::ptr::null_mut(),
                    )
                },
                0,
                "{}",
                io::Error::last_os_error()
            );
        }
        #[test]
        fn parent_junction_and_redirected_root_are_rejected() {
            let fixture = Fixture::new();
            fixture.write(
                "target/Chrome/User Data/Default/Extensions/fixture",
                b"not read",
            );
            let alias = fixture.0.join("Google");
            junction(&fixture, &alias, &fixture.0.join("target"));
            assert!(guarded(&alias.join("Chrome"), Some(true)).is_err());
            assert!(scan_at(&alias, &fixture.0, LIMITS).is_err());
            let scan = scan_at(&fixture.0, &fixture.0, LIMITS).unwrap();
            assert!(scan.extensions.is_empty());
            assert!(scan.skipped > 0);
            fs::remove_dir(&alias).unwrap(); // Only the owned junction, never its target.
            assert!(fixture.0.join("target/Chrome").is_dir());
        }
        #[test]
        fn extension_tree_junction_is_not_counted_or_followed() {
            let fixture = Fixture::new();
            let path = fixture.chromium();
            fixture.write("outside/data", b"not part of the extension");
            let alias = path.parent().unwrap().join("linked");
            junction(&fixture, &alias, &fixture.0.join("outside"));
            let scan = scan_at(&fixture.0, &fixture.0, LIMITS).unwrap();
            assert_eq!(scan.extensions.len(), 1);
            assert_eq!(scan.extensions[0].bytes, fs::metadata(path).unwrap().len());
            assert!(scan.skipped > 0);
            fs::remove_dir(&alias).unwrap();
        }
        #[test]
        fn locked_parent_cannot_be_renamed_or_replaced() {
            let fixture = Fixture::new();
            let path = fixture.chromium();
            let (_file, _parents) = guarded(&path, Some(false)).unwrap();
            assert!(fs::rename(path.parent().unwrap(), fixture.0.join("renamed")).is_err());
            assert!(fs::write(&path, b"changed").is_err());
        }
    }
}
