//! Fixed current-user tweaks and recycle-only browser-cache maintenance.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;

use super::verified_recycle::recovery_title;
use crate::reg::RawValue;
use crate::rescue::{Rescue, UndoOp};
use crate::winutil::{self, Known};

#[derive(Debug)]
struct Spec {
    id: &'static str,
    area: &'static str,
    subkey: &'static str,
    name: &'static str,
    string: bool,
}

static SPECS: [Spec; 7] = [
    Spec {
        id: "minimizeAnimations",
        area: "deep",
        subkey: r"Control Panel\Desktop\WindowMetrics",
        name: "MinAnimate",
        string: true,
    },
    Spec {
        id: "taskbarAnimations",
        area: "deep",
        subkey: r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced",
        name: "TaskbarAnimations",
        string: false,
    },
    Spec {
        id: "backgroundCapture",
        area: "deep",
        subkey: r"Software\Microsoft\Windows\CurrentVersion\GameDVR",
        name: "HistoricalCaptureEnabled",
        string: false,
    },
    Spec {
        id: "chromeBackground",
        area: "browser",
        subkey: r"Software\Policies\Google\Chrome",
        name: "BackgroundModeEnabled",
        string: false,
    },
    Spec {
        id: "edgeBackground",
        area: "browser",
        subkey: r"Software\Policies\Microsoft\Edge",
        name: "BackgroundModeEnabled",
        string: false,
    },
    Spec {
        id: "edgeStartupBoost",
        area: "browser",
        subkey: r"Software\Policies\Microsoft\Edge",
        name: "StartupBoostEnabled",
        string: false,
    },
    Spec {
        id: "braveBackground",
        area: "browser",
        subkey: r"Software\Policies\BraveSoftware\Brave",
        name: "BackgroundModeEnabled",
        string: false,
    },
];

impl Spec {
    fn target(&self) -> RawValue {
        if self.string {
            RawValue::sz("0")
        } else {
            RawValue::dword(0)
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tweak {
    pub id: String,
    pub area: &'static str,
    pub active: bool,
    pub available: bool,
    pub reason: Option<String>,
    /// Restart the browser, or sign out/in for desktop effects. Not immediate.
    pub needs_restart: bool,
    #[serde(skip)]
    previous: Option<RawValue>,
    #[serde(skip)]
    spec: &'static Spec,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReport {
    /// Verified writes only; an already-active selection is a no-op.
    pub done: u64,
    pub failed: u64,
    pub record_id: Option<u64>,
    pub needs_restart: bool,
}

trait Journal {
    fn new_id(&self) -> u64;
    fn save(
        &self,
        id: u64,
        title: &str,
        done: u64,
        failed: u64,
        bytes: u64,
        undo: &[UndoOp],
    ) -> Result<(), String>;
}

impl Journal for Rescue {
    fn new_id(&self) -> u64 {
        self.new_record_id()
    }
    fn save(
        &self,
        id: u64,
        title: &str,
        done: u64,
        failed: u64,
        bytes: u64,
        undo: &[UndoOp],
    ) -> Result<(), String> {
        self.add_tool_record(id, title, done, failed, bytes, undo.to_vec())
    }
}

// NotWritten guarantees that no value mutation was attempted. Uncertain must
// never discard its durable pending undo unless exact readback/rollback proves
// the original value is restored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriteOutcome {
    Written,
    NotWritten,
    Uncertain,
}

trait Registry {
    fn read(&self, spec: &Spec) -> Result<Option<RawValue>, String>;
    fn policy(&self, spec: &Spec) -> Result<bool, String>;
    fn writable(&self, spec: &Spec) -> Result<(), String>;
    fn replace(
        &self,
        spec: &Spec,
        expected: Option<&RawValue>,
        value: Option<&RawValue>,
    ) -> WriteOutcome;
}

// Serialize record allocation and mutation among this module's public tools.
// Main must also serialize other tools/Rescue undo using the same Rescue store.
static MUTATION_LOCK: Mutex<()> = Mutex::new(());

struct NativeRegistry;

#[cfg(windows)]
mod native_registry {
    use super::*;
    use std::borrow::Cow;
    use std::io::ErrorKind;
    use winreg::enums::*;
    use winreg::{RegKey, RegValue};

    fn raw(key: &RegKey, name: &str) -> Result<Option<RawValue>, String> {
        match key.get_raw_value(name) {
            Ok(value) => Ok(Some(RawValue {
                vtype: value.vtype as u32,
                bytes: value.bytes.into_owned(),
            })),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    // No lossy "unknown type -> binary" conversion when rolling back.
    fn value_type(code: u32) -> Option<RegType> {
        Some(match code {
            0 => REG_NONE,
            1 => REG_SZ,
            2 => REG_EXPAND_SZ,
            3 => REG_BINARY,
            4 => REG_DWORD,
            5 => REG_DWORD_BIG_ENDIAN,
            6 => REG_LINK,
            7 => REG_MULTI_SZ,
            8 => REG_RESOURCE_LIST,
            9 => REG_FULL_RESOURCE_DESCRIPTOR,
            10 => REG_RESOURCE_REQUIREMENTS_LIST,
            11 => REG_QWORD,
            _ => return None,
        })
    }

    impl Registry for NativeRegistry {
        fn read(&self, spec: &Spec) -> Result<Option<RawValue>, String> {
            let hive = RegKey::predef(HKEY_CURRENT_USER);
            match hive.open_subkey_with_flags(spec.subkey, KEY_QUERY_VALUE | KEY_WOW64_64KEY) {
                Ok(key) => raw(&key, spec.name),
                Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.to_string()),
            }
        }

        fn policy(&self, spec: &Spec) -> Result<bool, String> {
            if spec.area != "browser" {
                return Ok(false);
            }
            let hive = RegKey::predef(HKEY_LOCAL_MACHINE);
            // Conservatively block the entire policy key, even when this
            // particular value is absent. Never override a managed browser.
            for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
                match hive.open_subkey_with_flags(spec.subkey, KEY_QUERY_VALUE | view) {
                    Ok(_) => return Ok(true),
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
            Ok(false)
        }

        fn writable(&self, spec: &Spec) -> Result<(), String> {
            let hive = RegKey::predef(HKEY_CURRENT_USER);
            match hive.open_subkey_with_flags(spec.subkey, KEY_SET_VALUE | KEY_WOW64_64KEY) {
                Ok(_) => return Ok(()),
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
            // Probe the closest existing ancestor without creating anything.
            let mut parent = spec.subkey;
            while let Some((next, _)) = parent.rsplit_once('\\') {
                parent = next;
                match hive.open_subkey_with_flags(parent, KEY_CREATE_SUB_KEY | KEY_WOW64_64KEY) {
                    Ok(_) => return Ok(()),
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
            Err("registry_not_writable".into())
        }

        fn replace(
            &self,
            spec: &Spec,
            expected: Option<&RawValue>,
            value: Option<&RawValue>,
        ) -> WriteOutcome {
            if self.policy(spec) != Ok(false)
                || self.read(spec).ok().as_ref().map(|v| v.as_ref()) != Some(expected)
            {
                return WriteOutcome::NotWritten;
            }
            let typed = match value {
                Some(value) => match value_type(value.vtype) {
                    Some(vtype) => Some(RegValue {
                        vtype,
                        bytes: Cow::Borrowed(&value.bytes),
                    }),
                    None => return WriteOutcome::NotWritten,
                },
                None => None,
            };
            let hive = RegKey::predef(HKEY_CURRENT_USER);
            let flags = KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY;
            let key = match hive.open_subkey_with_flags(spec.subkey, flags) {
                Ok(key) => key,
                Err(e) if e.kind() == ErrorKind::NotFound && expected.is_none() => {
                    if value.is_none() {
                        return WriteOutcome::Written;
                    }
                    match hive.create_subkey_with_flags(spec.subkey, flags) {
                        Ok((key, _)) => key,
                        Err(_) => return WriteOutcome::NotWritten,
                    }
                }
                Err(_) => return WriteOutcome::NotWritten,
            };
            // Repeat on the SAME opened key after the backup has been flushed.
            // RegSetValueEx does not offer compare-and-swap against other apps.
            if self.policy(spec) != Ok(false)
                || raw(&key, spec.name).ok().as_ref().map(|v| v.as_ref()) != Some(expected)
            {
                return WriteOutcome::NotWritten;
            }
            let result = match typed {
                Some(value) => key.set_raw_value(spec.name, &value),
                None => key.delete_value(spec.name),
            };
            match result {
                Ok(()) => WriteOutcome::Written,
                Err(e) if value.is_none() && e.kind() == ErrorKind::NotFound => {
                    WriteOutcome::Written
                }
                Err(_) => WriteOutcome::Uncertain,
            }
        }
    }
}

#[cfg(not(windows))]
impl Registry for NativeRegistry {
    fn read(&self, _: &Spec) -> Result<Option<RawValue>, String> {
        Err("unsupported_platform".into())
    }
    fn policy(&self, _: &Spec) -> Result<bool, String> {
        Err("unsupported_platform".into())
    }
    fn writable(&self, _: &Spec) -> Result<(), String> {
        Err("unsupported_platform".into())
    }
    fn replace(&self, _: &Spec, _: Option<&RawValue>, _: Option<&RawValue>) -> WriteOutcome {
        WriteOutcome::NotWritten
    }
}

fn availability(registry: &dyn Registry, spec: &Spec) -> Result<(), String> {
    match registry.policy(spec) {
        Ok(false) => {}
        Ok(true) => return Err("machine_policy".into()),
        Err(e) => return Err(format!("machine_policy_unreadable:{e}")),
    }
    registry
        .writable(spec)
        .map_err(|e| format!("registry_not_writable:{e}"))
}

// Rescue's current reg::restore preserves these raw types exactly. Refuse
// other types rather than back them up and later restore them as REG_BINARY.
fn restorable(previous: Option<&RawValue>) -> bool {
    previous.is_none_or(|value| matches!(value.vtype, 0 | 1 | 2 | 3 | 4 | 7 | 11))
}

fn list_with(registry: &dyn Registry) -> Vec<Tweak> {
    SPECS
        .iter()
        .map(|spec| {
            let read = registry.read(spec);
            let reason = match &read {
                Err(e) => Some(format!("registry_unreadable:{e}")),
                Ok(previous) if !restorable(previous.as_ref()) => {
                    Some("unsupported_registry_type".into())
                }
                Ok(_) => availability(registry, spec).err(),
            };
            let previous = read.unwrap_or(None);
            Tweak {
                id: spec.id.into(),
                area: spec.area,
                active: previous.as_ref() == Some(&spec.target()),
                available: reason.is_none(),
                reason,
                needs_restart: true,
                previous,
                spec,
            }
        })
        .collect()
}

pub fn list() -> Result<Vec<Tweak>, String> {
    if !cfg!(windows) {
        return Err("unsupported_platform".into());
    }
    Ok(list_with(&NativeRegistry))
}

fn select<'a, T>(
    items: &'a [T],
    ids: &[String],
    id: impl Fn(&T) -> &str,
) -> Result<Vec<&'a T>, String> {
    if ids.is_empty() {
        return Err("empty_selection".into());
    }
    let mut seen = HashSet::new();
    let mut selected = Vec::with_capacity(ids.len().min(items.len()));
    for wanted in ids {
        if !seen.insert(wanted) {
            return Err(format!("duplicate_id:{wanted}"));
        }
        let mut matches = items.iter().filter(|item| id(item) == wanted);
        let item = matches
            .next()
            .ok_or_else(|| format!("unknown_id:{wanted}"))?;
        if matches.next().is_some() {
            return Err(format!("duplicate_snapshot:{wanted}"));
        }
        selected.push(item);
    }
    Ok(selected)
}

fn save_apply(
    journal: &dyn Journal,
    title: &str,
    report: &ApplyReport,
    undo: &[UndoOp],
) -> Result<(), String> {
    if let Some(id) = report.record_id {
        journal
            .save(id, title, report.done, report.failed, 0, undo)
            .map_err(|e| format!("recovery_pending:{id}:{e}"))?;
    }
    Ok(())
}

/// Apply backend snapshots, never deserialized frontend items. Selection errors
/// abort before mutation; stale/read/write failures count in `failed`. A storage
/// error is `recovery_pending:<id>:<error>` and may follow earlier writes: use
/// that durable Rescue record rather than retrying old snapshots blindly.
pub fn apply(items: &[Tweak], ids: &[String], rescue: &Rescue) -> Result<ApplyReport, String> {
    let _guard = MUTATION_LOCK.lock().map_err(|_| "mutation_lock_poisoned")?;
    apply_with(items, ids, &NativeRegistry, rescue)
}

fn apply_with(
    items: &[Tweak],
    ids: &[String],
    registry: &dyn Registry,
    journal: &dyn Journal,
) -> Result<ApplyReport, String> {
    let selected = select(items, ids, |item| item.id.as_str())?;
    let area = selected[0].spec.area;
    for item in &selected {
        if !SPECS.iter().any(|spec| std::ptr::eq(spec, item.spec))
            || item.id != item.spec.id
            || item.area != item.spec.area
        {
            return Err(format!("unknown_id:{}", item.id));
        }
        if item.spec.area != area {
            return Err("mixed_areas".into());
        }
        if !item.available || !restorable(item.previous.as_ref()) {
            return Err(format!("unavailable:{}", item.id));
        }
        // Policy/ACL changes since list() invalidate the whole selection before
        // any backup or mutation. Read errors are never an absent snapshot.
        availability(registry, item.spec).map_err(|e| format!("unavailable:{}:{e}", item.id))?;
    }
    let title = if area == "deep" {
        "deepOptimization"
    } else {
        "browserTweaks"
    };
    let mut report = ApplyReport::default();
    let mut undo = Vec::new();
    for item in selected {
        let spec = item.spec;
        let target = spec.target();
        let current = registry.read(spec);
        if current.as_ref().ok() != Some(&item.previous) || availability(registry, spec).is_err() {
            report.failed += 1;
            continue;
        }
        if item.previous.as_ref() == Some(&target) {
            continue;
        }
        let id = *report.record_id.get_or_insert_with(|| journal.new_id());
        undo.push(UndoOp::RestoreRegistryValue {
            machine: false,
            subkey: spec.subkey.into(),
            name: spec.name.into(),
            previous: item.previous.clone(),
        });
        // Durable pending recovery is REQUIRED before a write, including the
        // first write. Failure aborts immediately without calling replace().
        save_apply(journal, title, &report, &undo)?;
        let outcome = registry.replace(spec, item.previous.as_ref(), Some(&target));
        match outcome {
            WriteOutcome::Written => {
                if registry.read(spec).ok().as_ref() == Some(&Some(target.clone())) {
                    report.done += 1;
                } else {
                    report.failed += 1;
                }
                // Keep recovery even if verification is unreadable or changed.
                report.needs_restart = true;
            }
            WriteOutcome::NotWritten => {
                report.failed += 1;
                undo.pop();
            }
            WriteOutcome::Uncertain => {
                report.failed += 1;
                let readback = registry.read(spec);
                let restored = if readback.as_ref().ok() == Some(&item.previous) {
                    true
                } else if readback.as_ref().ok() == Some(&Some(target.clone())) {
                    // Roll back only OUR exact target, never an intervening
                    // third-party value. Failed/unverifiable rollback retains
                    // the previously flushed recovery operation.
                    registry.replace(spec, Some(&target), item.previous.as_ref())
                        == WriteOutcome::Written
                        && registry.read(spec).as_ref().ok() == Some(&item.previous)
                } else {
                    false
                };
                if restored {
                    undo.pop();
                } else {
                    report.needs_restart = true;
                }
            }
        }
        // Keep the same ID; never append duplicate recovery records.
        debug_assert_eq!(report.record_id, Some(id));
        save_apply(journal, title, &report, &undo)?;
    }
    save_apply(journal, title, &report, &undo)?;
    Ok(report)
}

// ------------------------------------------------------------------ Caches

#[derive(Debug)]
struct Browser {
    id: &'static str,
    data: &'static [&'static str],
    executable: &'static str,
    firefox: bool,
}

static BROWSERS: [Browser; 4] = [
    Browser {
        id: "chrome",
        data: &["Google", "Chrome", "User Data"],
        executable: "chrome.exe",
        firefox: false,
    },
    Browser {
        id: "edge",
        data: &["Microsoft", "Edge", "User Data"],
        executable: "msedge.exe",
        firefox: false,
    },
    Browser {
        id: "brave",
        data: &["BraveSoftware", "Brave-Browser", "User Data"],
        executable: "brave.exe",
        firefox: false,
    },
    Browser {
        id: "firefox",
        data: &["Mozilla", "Firefox", "Profiles"],
        executable: "firefox.exe",
        firefox: true,
    },
];
const CHROMIUM_CACHES: [&str; 3] = ["Cache", "Code Cache", "GPUCache"];
const FIREFOX_CACHES: [&str; 1] = ["cache2"];

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    device: u64,
    object: u64,
    bytes: u64,
    created: u64,
    modified: u64,
    flags: u32,
    links: u64,
}

impl Stamp {
    // Directory timestamps/length change when their children are recycled.
    fn same_directory(&self, other: &Self) -> bool {
        self.device == other.device
            && self.object == other.object
            && self.created == other.created
            && self.flags == other.flags
    }
}

#[derive(Clone, Debug)]
struct DirectoryProof {
    path: PathBuf,
    stamp: Stamp,
}

#[derive(Clone, Debug)]
struct FileProof {
    path: PathBuf,
    stamp: Stamp,
    parents: Vec<DirectoryProof>,
}

#[derive(Clone, Debug)]
struct CacheProof {
    id: String,
    browser: &'static Browser,
    local: PathBuf,
    profile: String,
    kind: &'static str,
    root: PathBuf,
    ancestors: Vec<DirectoryProof>,
    files: Vec<FileProof>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cache {
    pub id: String,
    pub browser: &'static str,
    pub path: String,
    pub bytes: u64,
    pub files: u64,
    #[serde(skip)]
    proof: CacheProof,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheScan {
    pub caches: Vec<Cache>,
    pub skipped: u64,
    pub cancelled: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheReport {
    /// Files verified recycled, not selected roots.
    pub done: u64,
    pub failed: u64,
    pub bytes: u64,
    pub record_id: Option<u64>,
    pub cancelled: bool,
    pub recovery: Vec<String>,
}

#[derive(Clone, Copy)]
struct Limits {
    profiles: usize,
    discovery: usize,
    entries: usize,
    files: usize,
    depth: usize,
}
const LIMITS: Limits = Limits {
    profiles: 64,
    discovery: 4096,
    entries: 100_000,
    files: 50_000,
    depth: 32,
};

fn join_parts(base: &Path, parts: &[&str]) -> PathBuf {
    parts
        .iter()
        .fold(base.to_path_buf(), |path, part| path.join(part))
}

fn plain_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." || name.ends_with(['.', ' ']) {
        return false;
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|' | '/' | '\\')
    }) {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return false;
    }
    !(stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn plain_absolute(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || path.components().count() > 64 {
        return Err("unsafe_path".into());
    }
    for component in path.components() {
        match component {
            Component::Normal(name) if name.to_str().is_some_and(plain_name) => {}
            Component::RootDir => {}
            #[cfg(windows)]
            Component::Prefix(prefix) if matches!(prefix.kind(), std::path::Prefix::Disk(_)) => {}
            _ => return Err("unsafe_path".into()),
        }
    }
    #[cfg(windows)]
    if !winutil::on_fixed_drive(path) {
        return Err("nonlocal_path".into());
    }
    Ok(())
}

fn same_resolved_path(requested: &Path, resolved: &Path) -> bool {
    #[cfg(windows)]
    {
        let (Some(a), Some(b)) = (requested.to_str(), resolved.to_str()) else {
            return false;
        };
        // Only strip the normal prefix produced by GetFinalPathNameByHandle;
        // UNC/device prefixes and short-name/path aliases are not accepted.
        a.replace('/', "\\")
            .eq_ignore_ascii_case(b.strip_prefix(r"\\?\").unwrap_or(b))
    }
    #[cfg(not(windows))]
    {
        requested == resolved
    }
}

fn no_alias(path: &Path) -> Result<(), String> {
    plain_absolute(path)?;
    let resolved = fs::canonicalize(path).map_err(|e| e.to_string())?;
    if !same_resolved_path(path, &resolved) {
        return Err("path_alias".into());
    }
    Ok(())
}

#[cfg(windows)]
struct FileGuard(windows::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl Drop for FileGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
#[cfg(not(windows))]
struct FileGuard {
    _file: fs::File,
}

#[cfg(windows)]
fn inspect(path: &Path, directory: bool, lock: bool) -> Result<(Stamp, FileGuard), String> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::*;
    plain_absolute(path)?;
    let share = if lock && directory {
        // Data-read access pins ancestor objects; transfer uses the leaf handle.
        FILE_SHARE_READ | FILE_SHARE_WRITE
    } else if lock {
        // DELETE access transfers the verified object without resolving its source name.
        FILE_SHARE_READ | FILE_SHARE_DELETE
    } else {
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
    };
    let handle = unsafe {
        CreateFileW(
            &HSTRING::from(path.as_os_str()),
            if lock && directory {
                0x80000000
            } else if lock {
                0x80010000
            } else {
                FILE_READ_ATTRIBUTES.0
            },
            share,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map_err(|e| e.to_string())?;
    let guard = FileGuard(handle);
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(|e| e.to_string())?;
    if info.dwVolumeSerialNumber == 0 || (info.nFileIndexHigh == 0 && info.nFileIndexLow == 0) {
        return Err("file_identity_unavailable".into());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0) != directory
    {
        return Err("reparse_or_wrong_type".into());
    }
    if !directory
        && (info.nNumberOfLinks != 1
            || info.dwFileAttributes & (FILE_ATTRIBUTE_DEVICE.0 | FILE_ATTRIBUTE_OFFLINE.0) != 0)
    {
        return Err("file_alias_or_offline".into());
    }
    let mut final_path = vec![0u16; 32768];
    let len = unsafe {
        GetFinalPathNameByHandleW(handle, &mut final_path, GETFINALPATHNAMEBYHANDLE_FLAGS(0))
    } as usize;
    if len == 0 || len >= final_path.len() {
        return Err("path_unreadable".into());
    }
    let resolved =
        PathBuf::from(String::from_utf16(&final_path[..len]).map_err(|e| e.to_string())?);
    if !same_resolved_path(path, &resolved) {
        return Err("path_alias".into());
    }
    let time = |t: windows::Win32::Foundation::FILETIME| {
        ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
    };
    Ok((
        Stamp {
            device: info.dwVolumeSerialNumber as u64,
            object: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
            bytes: ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64,
            created: time(info.ftCreationTime),
            modified: time(info.ftLastWriteTime),
            flags: info.dwFileAttributes,
            links: info.nNumberOfLinks as u64,
        },
        guard,
    ))
}

#[cfg(unix)]
fn inspect(path: &Path, directory: bool, _: bool) -> Result<(Stamp, FileGuard), String> {
    use std::os::unix::fs::MetadataExt;
    no_alias(path)?;
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink()
        || (if directory {
            !meta.is_dir()
        } else {
            !meta.is_file() || meta.nlink() != 1
        })
    {
        return Err("alias_or_wrong_type".into());
    }
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if meta.dev() != opened.dev() || meta.ino() != opened.ino() {
        return Err("changed_file".into());
    }
    let modified = (meta.mtime() as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(meta.mtime_nsec() as u64);
    // Do not use ctime as directory identity: child deletion changes it.
    Ok((
        Stamp {
            device: meta.dev(),
            object: meta.ino(),
            bytes: meta.len(),
            created: 0,
            modified,
            flags: meta.mode(),
            links: meta.nlink(),
        },
        FileGuard { _file: file },
    ))
}

#[cfg(not(any(windows, unix)))]
fn inspect(_: &Path, _: bool, _: bool) -> Result<(Stamp, FileGuard), String> {
    Err("unsupported_platform".into())
}

fn directory_proofs(path: &Path) -> Result<Vec<DirectoryProof>, String> {
    no_alias(path)?;
    let mut paths: Vec<_> = path.ancestors().collect();
    paths.reverse();
    let mut proofs = Vec::new();
    for path in paths {
        let (stamp, _) = inspect(path, true, false)?;
        proofs.push(DirectoryProof {
            path: path.to_path_buf(),
            stamp,
        });
    }
    Ok(proofs)
}

fn guard_directories(proofs: &[DirectoryProof]) -> Result<Vec<FileGuard>, String> {
    let mut guards = Vec::with_capacity(proofs.len());
    for proof in proofs {
        let (current, guard) = inspect(&proof.path, true, true)?;
        if !proof.stamp.same_directory(&current) {
            return Err("changed_directory".into());
        }
        guards.push(guard);
    }
    Ok(guards)
}

fn sensitive(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "preferences"
            | "secure preferences"
            | "extensions"
            | "extensions.json"
            | "local state"
            | "cookies"
            | "cookies-journal"
            | "cookies.sqlite"
            | "history"
            | "history-journal"
            | "passwords"
            | "login data"
            | "login data-journal"
            | "logins.json"
            | "key3.db"
            | "key4.db"
            | "places.sqlite"
            | "prefs.js"
            | "user.js"
    )
}

fn profile_name(browser: &Browser, name: &str) -> bool {
    plain_name(name)
        && (browser.firefox
            || name == "Default"
            || name
                .strip_prefix("Profile ")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit())))
}

fn cache_kinds(browser: &Browser) -> &'static [&'static str] {
    if browser.firefox {
        &FIREFOX_CACHES
    } else {
        &CHROMIUM_CACHES
    }
}

#[derive(Default)]
struct Budget {
    discovery: usize,
    profiles: usize,
    entries: usize,
    files: usize,
    exhausted: bool,
}

/// Re-enumerate the same fixed LocalAppData locations as chromium_profiles()
/// and firefox_profiles(Known::LocalAppData). Those convenience helpers silently
/// drop I/O errors and have no bounds, so they cannot provide scan accounting.
pub fn cache_scan(
    cancel: &AtomicBool,
    progress: &dyn Fn(&Path, usize),
) -> Result<CacheScan, String> {
    if !cfg!(windows) {
        return Err("unsupported_platform".into());
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(CacheScan {
            cancelled: true,
            ..Default::default()
        });
    }
    let local = winutil::known_folder(Known::LocalAppData).ok_or("local_app_data_unavailable")?;
    scan_at(&local, cancel, progress, LIMITS)
}

fn scan_at(
    local: &Path,
    cancel: &AtomicBool,
    progress: &dyn Fn(&Path, usize),
    limits: Limits,
) -> Result<CacheScan, String> {
    let mut scan = CacheScan::default();
    if cancel.load(Ordering::Relaxed) {
        scan.cancelled = true;
        return Ok(scan);
    }
    // Never follow a redirected LocalAppData/ancestor junction.
    directory_proofs(local)?;
    let mut budget = Budget::default();
    'browsers: for browser in &BROWSERS {
        let data = join_parts(local, browser.data);
        match fs::symlink_metadata(&data) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                scan.skipped += 1;
                continue;
            }
            Ok(_) => {}
        }
        if directory_proofs(&data).is_err() {
            scan.skipped += 1;
            continue;
        }
        let entries = match fs::read_dir(&data) {
            Ok(entries) => entries,
            Err(_) => {
                scan.skipped += 1;
                continue;
            }
        };
        for entry in entries {
            if cancel.load(Ordering::Relaxed) {
                scan.cancelled = true;
                break 'browsers;
            }
            if budget.discovery >= limits.discovery {
                scan.skipped += 1;
                break 'browsers;
            }
            budget.discovery += 1;
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    scan.skipped += 1;
                    continue;
                }
            };
            let name = match entry.file_name().into_string() {
                Ok(name) => name,
                Err(_) => {
                    scan.skipped += 1;
                    continue;
                }
            };
            if !profile_name(browser, &name) {
                continue;
            }
            if budget.profiles >= limits.profiles {
                scan.skipped += 1;
                break 'browsers;
            }
            budget.profiles += 1;
            let profile = data.join(&name);
            if directory_proofs(&profile).is_err() {
                scan.skipped += 1;
                continue;
            }
            for &kind in cache_kinds(browser) {
                if cancel.load(Ordering::Relaxed) {
                    scan.cancelled = true;
                    break 'browsers;
                }
                let root = profile.join(kind);
                match fs::symlink_metadata(&root) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => {
                        scan.skipped += 1;
                        continue;
                    }
                    Ok(_) => {}
                }
                let ancestors = match directory_proofs(&root) {
                    Ok(proofs) => proofs,
                    Err(_) => {
                        scan.skipped += 1;
                        continue;
                    }
                };
                let id = format!("{}:{}:{}", browser.id, name, kind);
                let mut proof = CacheProof {
                    id: id.clone(),
                    browser,
                    local: local.to_path_buf(),
                    profile: name.clone(),
                    kind,
                    root: root.clone(),
                    ancestors,
                    files: Vec::new(),
                };
                collect_cache(&mut proof, &mut scan, &mut budget, limits, cancel, progress);
                if !proof.files.is_empty() {
                    let bytes = proof
                        .files
                        .iter()
                        .try_fold(0u64, |sum, file| sum.checked_add(file.stamp.bytes));
                    match bytes {
                        Some(bytes) => scan.caches.push(Cache {
                            id,
                            browser: browser.id,
                            path: root.to_string_lossy().into_owned(),
                            bytes,
                            files: proof.files.len() as u64,
                            proof,
                        }),
                        None => scan.skipped += 1,
                    }
                }
                if scan.cancelled || budget.exhausted {
                    break 'browsers;
                }
            }
        }
    }
    scan.cancelled |= cancel.load(Ordering::Relaxed);
    Ok(scan)
}

fn collect_cache(
    proof: &mut CacheProof,
    scan: &mut CacheScan,
    budget: &mut Budget,
    limits: Limits,
    cancel: &AtomicBool,
    progress: &dyn Fn(&Path, usize),
) {
    // The stack is bounded by the entry budget; no eager recursive collection.
    let mut stack = vec![(proof.root.clone(), Vec::<DirectoryProof>::new(), 0usize)];
    while let Some((dir, parents, depth)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            scan.cancelled = true;
            return;
        }
        let root_guards = match guard_directories(&proof.ancestors) {
            Ok(guards) => guards,
            Err(_) => {
                scan.skipped += 1;
                continue;
            }
        };
        let parent_guards = match guard_directories(&parents) {
            Ok(guards) => guards,
            Err(_) => {
                scan.skipped += 1;
                continue;
            }
        };
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => {
                scan.skipped += 1;
                continue;
            }
        };
        for entry in entries {
            if cancel.load(Ordering::Relaxed) {
                scan.cancelled = true;
                return;
            }
            if budget.entries >= limits.entries || budget.files >= limits.files {
                scan.skipped += 1;
                budget.exhausted = true;
                return;
            }
            budget.entries += 1;
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    scan.skipped += 1;
                    continue;
                }
            };
            let path = entry.path();
            if budget.entries == 1 || budget.entries % 128 == 0 {
                progress(&path, budget.entries);
            }
            if cancel.load(Ordering::Relaxed) {
                scan.cancelled = true;
                return;
            }
            let name = entry.file_name();
            if !name
                .to_str()
                .is_some_and(|name| plain_name(name) && !sensitive(name))
            {
                scan.skipped += 1;
                continue;
            }
            let meta = match fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(_) => {
                    scan.skipped += 1;
                    continue;
                }
            };
            if meta.is_dir() {
                if depth >= limits.depth {
                    scan.skipped += 1;
                    continue;
                }
                match inspect(&path, true, false) {
                    Ok((stamp, _)) => {
                        let mut next = parents.clone();
                        next.push(DirectoryProof {
                            path: path.clone(),
                            stamp,
                        });
                        stack.push((path, next, depth + 1));
                    }
                    Err(_) => scan.skipped += 1,
                }
            } else if meta.is_file() {
                match inspect(&path, false, false) {
                    Ok((stamp, _)) => {
                        proof.files.push(FileProof {
                            path,
                            stamp,
                            parents: parents.clone(),
                        });
                        budget.files += 1;
                    }
                    Err(_) => scan.skipped += 1,
                }
            } else {
                scan.skipped += 1;
            }
        }
        drop(parent_guards);
        drop(root_guards);
    }
}

fn validate_cache(cache: &Cache) -> Result<(), String> {
    let proof = &cache.proof;
    if !BROWSERS
        .iter()
        .any(|browser| std::ptr::eq(browser, proof.browser))
        || !profile_name(proof.browser, &proof.profile)
        || !cache_kinds(proof.browser).contains(&proof.kind)
        || proof.root
            != join_parts(&proof.local, proof.browser.data)
                .join(&proof.profile)
                .join(proof.kind)
        || cache.id != proof.id
        || cache.browser != proof.browser.id
        || Path::new(&cache.path) != proof.root
    {
        return Err("invalid_cache_snapshot".into());
    }
    let mut seen = HashSet::new();
    for file in &proof.files {
        let relative = file
            .path
            .strip_prefix(&proof.root)
            .map_err(|_| "invalid_cache_scope")?;
        if relative.as_os_str().is_empty()
            || !relative.components().all(|c| match c {
                Component::Normal(n) => n.to_str().is_some_and(|n| plain_name(n) && !sensitive(n)),
                _ => false,
            })
            || !seen.insert(&file.path)
        {
            return Err("invalid_cache_scope".into());
        }
    }
    Ok(())
}

fn revalidate(proof: &CacheProof, file: &FileProof) -> Result<Vec<FileGuard>, String> {
    let mut guards = guard_directories(&proof.ancestors)?;
    guards.extend(guard_directories(&file.parents)?);
    no_alias(&file.path)?;
    let (stamp, guard) = inspect(&file.path, false, true)?;
    if stamp != file.stamp {
        return Err("changed_file".into());
    }
    guards.push(guard);
    Ok(guards)
}

fn matching_image(browser: &Browser, image: &Path) -> bool {
    image.is_absolute()
        && image
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(browser.executable))
}

#[cfg(windows)]
fn browser_running(browser: &Browser) -> Result<bool, String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER};
    use windows::Win32::System::ProcessStatus::K32EnumProcesses;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let mut pids = vec![0u32; 1024];
    let count = loop {
        let mut needed = 0u32;
        if !unsafe { K32EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut needed) }
            .as_bool()
        {
            return Err("process_enumeration_failed".into());
        }
        if (needed as usize) < pids.len() * 4 {
            break needed as usize / 4;
        }
        if pids.len() >= 65536 {
            return Err("process_enumeration_limit".into());
        }
        pids.resize(pids.len() * 2, 0);
    };
    let mut unreadable = false;
    for &pid in &pids[..count] {
        // 0 = idle and 4 = kernel System, neither is a user browser.
        if pid == 0 || pid == 4 {
            continue;
        }
        let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            Ok(handle) => handle,
            Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
                continue
            } // exited
            Err(_) => {
                unreadable = true;
                continue;
            }
        };
        let mut text = vec![0u16; 32768];
        let mut len = text.len() as u32;
        let result = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(text.as_mut_ptr()),
                &mut len,
            )
        };
        unsafe {
            let _ = CloseHandle(handle);
        }
        if result.is_err() {
            unreadable = true;
            continue;
        }
        let image = match String::from_utf16(&text[..len as usize]) {
            Ok(image) => PathBuf::from(image),
            Err(_) => {
                unreadable = true;
                continue;
            }
        };
        if matching_image(browser, &image) {
            return Ok(true);
        }
    }
    if unreadable {
        Err("process_unreadable".into())
    } else {
        Ok(false)
    }
}

#[cfg(not(windows))]
fn browser_running(_: &Browser) -> Result<bool, String> {
    Err("unsupported_platform".into())
}

type CachePersist<'a> = dyn Fn(Option<&str>) -> Result<(), String> + 'a;
type CacheRecycler<'a> =
    dyn Fn(&CacheProof, &FileProof, &[FileGuard], &CachePersist<'_>) -> Result<(), String> + 'a;

#[cfg(windows)]
fn recycle_only(
    proof: &CacheProof,
    file: &FileProof,
    guards: &[FileGuard],
    cancel: &AtomicBool,
    persist: &CachePersist<'_>,
) -> Result<(), String> {
    let source = guards.last().ok_or("missing_verified_handle")?;
    let parent = guards
        .get(guards.len().checked_sub(2).ok_or("missing_parent_handle")?)
        .ok_or("missing_parent_handle")?;
    super::verified_recycle::recycle_verified(
        source.0,
        parent.0,
        &file.path,
        cancel,
        &|restoring| {
            guard_directories(&proof.ancestors)?;
            guard_directories(&file.parents)?;
            if !restoring {
                if browser_running(proof.browser)? {
                    return Err("browser_running".into());
                }
                revalidate(proof, file)?;
            }
            Ok(())
        },
        persist,
    )
}

#[cfg(not(windows))]
fn recycle_only(
    _: &CacheProof,
    _: &FileProof,
    _: &[FileGuard],
    _: &AtomicBool,
    _: &CachePersist<'_>,
) -> Result<(), String> {
    Err("unsupported_platform".into())
}

fn save_cache(journal: &dyn Journal, report: &CacheReport) -> Result<(), String> {
    if let Some(id) = report.record_id {
        journal
            .save(
                id,
                &recovery_title("browserCache", &report.recovery),
                report.done,
                report.failed,
                report.bytes,
                &[],
            )
            .map_err(|e| format!("history_pending:{id}:{e}"))?;
    }
    Ok(())
}

/// Clear only files in backend scan proofs. Unselected/new/changed files and
/// all directories are untouched. A browser that starts mid-operation stops
/// further work with `browser_running`; prior work remains in `browserCache`
/// history. Cancellation returns the partial report. No Rescue undo is stored.
pub fn cache_clear(
    caches: &[Cache],
    ids: &[String],
    rescue: &Rescue,
    cancel: &AtomicBool,
) -> Result<CacheReport, String> {
    let _guard = MUTATION_LOCK.lock().map_err(|_| "mutation_lock_poisoned")?;
    #[cfg(windows)]
    {
        for cache in select(caches, ids, |cache| cache.id.as_str())? {
            validate_cache(cache)?;
        }
        if !cancel.load(Ordering::Relaxed) && !winutil::is_elevated() {
            return Err("needs_admin".into());
        }
    }
    clear_verified_with(
        caches,
        ids,
        rescue,
        cancel,
        &browser_running,
        &|proof, file, guards, persist| recycle_only(proof, file, guards, cancel, persist),
    )
}

#[cfg(test)]
fn clear_with(
    caches: &[Cache],
    ids: &[String],
    journal: &dyn Journal,
    cancel: &AtomicBool,
    running: &dyn Fn(&Browser) -> Result<bool, String>,
    recycle: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<CacheReport, String> {
    clear_verified_with(caches, ids, journal, cancel, running, &|_, file, _, _| {
        recycle(&file.path)
    })
}

fn clear_verified_with(
    caches: &[Cache],
    ids: &[String],
    journal: &dyn Journal,
    cancel: &AtomicBool,
    running: &dyn Fn(&Browser) -> Result<bool, String>,
    recycle: &CacheRecycler<'_>,
) -> Result<CacheReport, String> {
    let selected = select(caches, ids, |cache| cache.id.as_str())?;
    for cache in &selected {
        validate_cache(cache)?;
    }
    let mut report = CacheReport::default();
    if cancel.load(Ordering::Relaxed) {
        report.cancelled = true;
        return Ok(report);
    }
    // Refuse the entire selection before touching ANY file if a matching
    // browser is running, even if its selected cache happens to be unlocked.
    let mut checked = HashSet::new();
    for cache in &selected {
        if checked.insert(cache.proof.browser.id) && running(cache.proof.browser)? {
            return Err("browser_running".into());
        }
    }
    for cache in selected {
        for file in &cache.proof.files {
            if cancel.load(Ordering::Relaxed) {
                report.cancelled = true;
                break;
            }
            match running(cache.proof.browser) {
                Ok(false) => {}
                result => {
                    if report.failed > 0 && report.record_id.is_none() {
                        report.record_id = Some(journal.new_id());
                    }
                    save_cache(journal, &report)?;
                    return Err(match result {
                        Ok(true) => "browser_running".into(),
                        Err(e) => e,
                        Ok(false) => unreachable!(),
                    });
                }
            }
            let guards = match revalidate(&cache.proof, file) {
                Ok(guards) => guards,
                Err(_) => {
                    report.failed += 1;
                    continue;
                }
            };
            if cancel.load(Ordering::Relaxed) {
                report.cancelled = true;
                break;
            }
            report.record_id.get_or_insert_with(|| journal.new_id());
            save_cache(journal, &report)?;
            // Recheck after the potentially slow durable history flush.
            if running(cache.proof.browser)? {
                return Err("browser_running".into());
            }
            let final_guards = match revalidate(&cache.proof, file) {
                Ok(guards) => guards,
                Err(_) => {
                    report.failed += 1;
                    save_cache(journal, &report)?;
                    continue;
                }
            };
            if cancel.load(Ordering::Relaxed) {
                report.cancelled = true;
                break;
            }
            let recovery = std::cell::RefCell::new(None::<String>);
            let persist = |detail: Option<&str>| -> Result<(), String> {
                *recovery.borrow_mut() = detail.map(str::to_owned);
                if let Some(detail) = detail {
                    let mut pending = report.clone();
                    pending.recovery.push(detail.into());
                    save_cache(journal, &pending)?;
                }
                Ok(())
            };
            let outcome = recycle(&cache.proof, file, &final_guards, &persist);
            if let Some(detail) = recovery.into_inner() {
                report.recovery.push(detail);
            }
            match outcome {
                Ok(()) => {
                    report.done += 1;
                    report.bytes += file.stamp.bytes;
                }
                Err(error) if error == "cancelled" && cancel.load(Ordering::Relaxed) => {
                    report.cancelled = true;
                    break;
                }
                Err(_) => report.failed += 1,
            }
            drop(final_guards);
            drop(guards);
            save_cache(journal, &report)?;
        }
        if report.cancelled {
            break;
        }
    }
    report.cancelled |= cancel.load(Ordering::Relaxed);
    // Record validation failures as well, but never pretend they are undoable.
    if report.failed > 0 && report.record_id.is_none() {
        report.record_id = Some(journal.new_id());
    }
    save_cache(journal, &report)?;
    Ok(report)
}

// --------------------------------------------------------------------- DNS
const DNS_TIMEOUT: Duration = Duration::from_secs(15);
const DNS_ARGS: [&str; 1] = ["/flushdns"];

fn dns_command(windows_dir: &Path) -> Result<PathBuf, String> {
    plain_absolute(windows_dir)?;
    Ok(windows_dir.join("System32").join("ipconfig.exe"))
}

pub fn flush_dns() -> Result<(), String> {
    if !cfg!(windows) {
        return Err("unsupported_platform".into());
    }
    let windows_dir =
        winutil::known_folder(Known::Windows).ok_or("windows_directory_unavailable")?;
    let program = dns_command(&windows_dir)?;
    // No PATH search, command interpreter or user-provided arguments.
    winutil::run_hidden(
        program.to_str().ok_or("unsafe_path")?,
        &DNS_ARGS,
        DNS_TIMEOUT,
        None,
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::atomic::AtomicU64;

    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                ".tp-tuning-fixture-{}-{}-{nonce}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            #[cfg(windows)]
            let root = {
                let resolved = fs::canonicalize(&root).unwrap();
                PathBuf::from(
                    resolved
                        .to_str()
                        .unwrap()
                        .strip_prefix(r"\\?\")
                        .unwrap_or(resolved.to_str().unwrap()),
                )
            };
            Self { root }
        }
        fn file(&self, relative: &str, content: &[u8]) -> PathBuf {
            let path = self.root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, content).unwrap();
            path
        }
        fn scan(&self) -> CacheScan {
            scan_at(&self.root, &AtomicBool::new(false), &|_, _| {}, LIMITS).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[derive(Default)]
    struct MemoryJournal {
        undo: RefCell<Vec<UndoOp>>,
        saves: Cell<usize>,
        fail_at: Cell<Option<usize>>,
        title: RefCell<String>,
        totals: Cell<(u64, u64, u64)>,
        events: Rc<RefCell<Vec<&'static str>>>,
        change_on_save: RefCell<Option<Box<dyn Fn()>>>,
    }
    impl Journal for MemoryJournal {
        fn new_id(&self) -> u64 {
            7
        }
        fn save(
            &self,
            _: u64,
            title: &str,
            done: u64,
            failed: u64,
            bytes: u64,
            undo: &[UndoOp],
        ) -> Result<(), String> {
            let n = self.saves.get() + 1;
            self.saves.set(n);
            if self.fail_at.get() == Some(n) {
                return Err("fixture_flush_failure".into());
            }
            self.events.borrow_mut().push("backup");
            *self.undo.borrow_mut() = undo.to_vec();
            *self.title.borrow_mut() = title.into();
            self.totals.set((done, failed, bytes));
            if let Some(change) = self.change_on_save.borrow_mut().take() {
                change();
            }
            Ok(())
        }
    }

    struct FakeRegistry {
        values: Rc<RefCell<HashMap<&'static str, RawValue>>>,
        read_error: Cell<bool>,
        policy: Cell<bool>,
        policy_error: Cell<bool>,
        write_mode: Cell<u8>,
        writes: Cell<usize>,
        events: Rc<RefCell<Vec<&'static str>>>,
    }
    impl FakeRegistry {
        fn new(journal: &MemoryJournal) -> Self {
            Self {
                values: Rc::new(RefCell::new(HashMap::new())),
                read_error: Cell::new(false),
                policy: Cell::new(false),
                policy_error: Cell::new(false),
                write_mode: Cell::new(0),
                writes: Cell::new(0),
                events: journal.events.clone(),
            }
        }
    }
    impl Registry for FakeRegistry {
        fn read(&self, spec: &Spec) -> Result<Option<RawValue>, String> {
            if self.read_error.get() {
                Err("fixture_access_denied".into())
            } else {
                Ok(self.values.borrow().get(spec.id).cloned())
            }
        }
        fn policy(&self, spec: &Spec) -> Result<bool, String> {
            if spec.area != "browser" {
                return Ok(false);
            }
            if self.policy_error.get() {
                Err("fixture_policy_denied".into())
            } else {
                Ok(self.policy.get())
            }
        }
        fn writable(&self, _: &Spec) -> Result<(), String> {
            Ok(())
        }
        fn replace(
            &self,
            spec: &Spec,
            expected: Option<&RawValue>,
            value: Option<&RawValue>,
        ) -> WriteOutcome {
            if self.read(spec).ok().as_ref().map(|v| v.as_ref()) != Some(expected)
                || self.policy(spec) != Ok(false)
            {
                return WriteOutcome::NotWritten;
            }
            self.events.borrow_mut().push("write");
            self.writes.set(self.writes.get() + 1);
            if self.write_mode.get() == 3 && self.writes.get() > 1 {
                return WriteOutcome::Uncertain;
            }
            match value {
                Some(value) => {
                    self.values.borrow_mut().insert(spec.id, value.clone());
                }
                None => {
                    self.values.borrow_mut().remove(spec.id);
                }
            }
            match self.write_mode.get() {
                1 | 3 if self.writes.get() == 1 => WriteOutcome::Uncertain,
                2 => {
                    self.read_error.set(true);
                    WriteOutcome::Uncertain
                }
                _ => WriteOutcome::Written,
            }
        }
    }
    fn ids(items: &[Tweak], index: usize) -> Vec<String> {
        vec![items[index].id.clone()]
    }
    fn cache_ids(scan: &CacheScan) -> Vec<String> {
        scan.caches.iter().map(|c| c.id.clone()).collect()
    }

    #[test]
    fn registry_specs_are_exact_fixed_hkcu_choices() {
        let expected = [
            (
                "minimizeAnimations",
                "deep",
                r"Control Panel\Desktop\WindowMetrics",
                "MinAnimate",
                crate::reg::REG_SZ,
            ),
            (
                "taskbarAnimations",
                "deep",
                r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced",
                "TaskbarAnimations",
                crate::reg::REG_DWORD,
            ),
            (
                "backgroundCapture",
                "deep",
                r"Software\Microsoft\Windows\CurrentVersion\GameDVR",
                "HistoricalCaptureEnabled",
                crate::reg::REG_DWORD,
            ),
            (
                "chromeBackground",
                "browser",
                r"Software\Policies\Google\Chrome",
                "BackgroundModeEnabled",
                crate::reg::REG_DWORD,
            ),
            (
                "edgeBackground",
                "browser",
                r"Software\Policies\Microsoft\Edge",
                "BackgroundModeEnabled",
                crate::reg::REG_DWORD,
            ),
            (
                "edgeStartupBoost",
                "browser",
                r"Software\Policies\Microsoft\Edge",
                "StartupBoostEnabled",
                crate::reg::REG_DWORD,
            ),
            (
                "braveBackground",
                "browser",
                r"Software\Policies\BraveSoftware\Brave",
                "BackgroundModeEnabled",
                crate::reg::REG_DWORD,
            ),
        ];
        let mut unique = HashSet::new();
        for (spec, (id, area, key, name, kind)) in SPECS.iter().zip(expected) {
            assert_eq!(
                (
                    spec.id,
                    spec.area,
                    spec.subkey,
                    spec.name,
                    spec.target().vtype
                ),
                (id, area, key, name, kind)
            );
            assert!(unique.insert(spec.id));
            assert_eq!(
                spec.target(),
                if kind == crate::reg::REG_SZ {
                    RawValue::sz("0")
                } else {
                    RawValue::dword(0)
                }
            );
        }
        assert_eq!(SPECS.len(), 7);
    }

    #[test]
    fn backup_precedes_write_and_keeps_exact_raw_restore() {
        let journal = MemoryJournal::default();
        let registry = FakeRegistry::new(&journal);
        let previous = RawValue {
            vtype: 3,
            bytes: vec![0, 255, 17, 0, 99],
        };
        registry
            .values
            .borrow_mut()
            .insert(SPECS[0].id, previous.clone());
        let items = list_with(&registry);
        let report = apply_with(&items, &ids(&items, 0), &registry, &journal).unwrap();
        assert_eq!(
            (
                report.done,
                report.failed,
                report.record_id,
                report.needs_restart
            ),
            (1, 0, Some(7), true)
        );
        assert_eq!(&journal.events.borrow()[..2], &["backup", "write"]);
        assert_eq!(&*journal.title.borrow(), "deepOptimization");
        match &journal.undo.borrow()[0] {
            UndoOp::RestoreRegistryValue {
                machine,
                subkey,
                name,
                previous: saved,
            } => {
                assert!(!machine);
                assert_eq!(
                    (subkey.as_str(), name.as_str(), saved),
                    (SPECS[0].subkey, SPECS[0].name, &Some(previous.clone()))
                );
                // Injected undo, never Rescue::undo against a real setting.
                assert_eq!(
                    registry.replace(&SPECS[0], Some(&SPECS[0].target()), saved.as_ref()),
                    WriteOutcome::Written
                );
            }
            _ => panic!("wrong undo"),
        }
        assert_eq!(registry.read(&SPECS[0]).unwrap(), Some(previous));
        let json = serde_json::to_value(&items[0]).unwrap();
        assert!(json.get("previous").is_none() && json.get("spec").is_none());
    }

    #[test]
    fn pending_backup_is_durable_and_upserted_with_real_rescue_fixture() {
        let fixture = Fixture::new();
        let rescue = Rescue::at(fixture.root.join("Rescue"));
        let journal = MemoryJournal::default();
        let registry = FakeRegistry::new(&journal);
        let items = list_with(&registry);
        let report = apply_with(&items, &ids(&items, 3), &registry, &rescue).unwrap();
        let reopened = Rescue::at(rescue.dir().to_path_buf());
        let history = reopened.history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, report.record_id.unwrap());
        assert_eq!(history[0].title.as_deref(), Some("browserTweaks"));
        assert_eq!(history[0].done, 1);
        assert!(matches!(
            history[0].undo[0],
            UndoOp::RestoreRegistryValue {
                machine: false,
                previous: None,
                ..
            }
        ));
    }

    #[test]
    fn corrupt_rescue_blocks_registry_and_cache_mutations() {
        let fixture = Fixture::new();
        let rescue = Rescue::at(fixture.root.join("Rescue"));
        rescue
            .add_tool_record(1, "old recovery", 0, 0, 0, Vec::new())
            .unwrap();
        let history_path = rescue.dir().join("history.json");
        let valid = fs::read(&history_path).unwrap();
        let corrupt = &valid[..valid.len() - 1];
        fs::write(&history_path, corrupt).unwrap();
        let journal = MemoryJournal::default();
        let registry = FakeRegistry::new(&journal);
        let items = list_with(&registry);
        assert!(apply_with(&items, &ids(&items, 0), &registry, &rescue).is_err());
        assert_eq!(registry.writes.get(), 0);
        let cache = fixture.file("Google/Chrome/User Data/Default/Cache/a", b"cache bytes");
        let scan = fixture.scan();
        let recycled = Cell::new(0);
        assert!(clear_with(
            &scan.caches,
            &cache_ids(&scan),
            &rescue,
            &AtomicBool::new(false),
            &|_| Ok(false),
            &|_| {
                recycled.set(recycled.get() + 1);
                Ok(())
            }
        )
        .is_err());
        assert_eq!(recycled.get(), 0);
        assert_eq!(fs::read(cache).unwrap(), b"cache bytes");
        assert_eq!(fs::read(&history_path).unwrap(), corrupt);
        fs::write(&history_path, &valid).unwrap();
        assert_eq!(
            rescue.history().unwrap()[0].title.as_deref(),
            Some("old recovery")
        );
    }

    #[test]
    fn validation_and_read_failures_never_write() {
        let journal = MemoryJournal::default();
        let registry = FakeRegistry::new(&journal);
        let items = list_with(&registry);
        for selection in [
            vec![],
            vec!["not-a-key".into()],
            vec![items[0].id.clone(), items[0].id.clone()],
            vec![items[0].id.clone(), items[3].id.clone()],
        ] {
            assert!(apply_with(&items, &selection, &registry, &journal).is_err());
        }
        registry.read_error.set(true);
        let unavailable = list_with(&registry);
        assert!(!unavailable[0].available);
        assert!(unavailable[0]
            .reason
            .as_ref()
            .unwrap()
            .contains("registry_unreadable"));
        assert!(apply_with(&unavailable, &ids(&unavailable, 0), &registry, &journal).is_err());
        let report = apply_with(&items, &ids(&items, 0), &registry, &journal).unwrap();
        assert_eq!((report.done, report.failed, report.record_id), (0, 1, None));
        assert_eq!(registry.writes.get(), 0);
        registry.read_error.set(false);
        registry.policy.set(true);
        assert!(!list_with(&registry)[3].available);
        assert!(apply_with(&items, &ids(&items, 3), &registry, &journal).is_err());
        registry.policy.set(false);
        registry.policy_error.set(true);
        assert!(!list_with(&registry)[6].available);
        assert_eq!(registry.writes.get(), 0);
        assert_eq!(journal.saves.get(), 0);
    }

    #[test]
    fn stale_value_and_change_after_backup_are_not_overwritten() {
        let journal = MemoryJournal::default();
        let registry = FakeRegistry::new(&journal);
        let items = list_with(&registry);
        let foreign = RawValue::sz("third party");
        registry
            .values
            .borrow_mut()
            .insert(SPECS[0].id, foreign.clone());
        let report = apply_with(&items, &ids(&items, 0), &registry, &journal).unwrap();
        assert_eq!((report.done, report.failed), (0, 1));
        assert_eq!(registry.writes.get(), 0);
        registry.values.borrow_mut().clear();
        let values = registry.values.clone();
        let foreign_copy = foreign.clone();
        *journal.change_on_save.borrow_mut() = Some(Box::new(move || {
            values
                .borrow_mut()
                .insert(SPECS[0].id, foreign_copy.clone());
        }));
        let report = apply_with(&items, &ids(&items, 0), &registry, &journal).unwrap();
        assert_eq!((report.done, report.failed), (0, 1));
        assert_eq!(registry.read(&SPECS[0]).unwrap(), Some(foreign));
        assert_eq!(registry.writes.get(), 0);
        assert!(journal.undo.borrow().is_empty());
    }

    #[test]
    fn backup_failure_aborts_and_failed_writes_rollback_or_retain_recovery() {
        let journal = MemoryJournal::default();
        let registry = FakeRegistry::new(&journal);
        let items = list_with(&registry);
        journal.fail_at.set(Some(1));
        assert!(apply_with(&items, &ids(&items, 0), &registry, &journal)
            .unwrap_err()
            .starts_with("recovery_pending:"));
        assert_eq!(registry.writes.get(), 0);
        for mode in [1, 2, 3] {
            let journal = MemoryJournal::default();
            let registry = FakeRegistry::new(&journal);
            let old = RawValue {
                vtype: 7,
                bytes: vec![1, 0, 2, 0, 0, 0, 0, 0],
            };
            registry
                .values
                .borrow_mut()
                .insert(SPECS[0].id, old.clone());
            registry.write_mode.set(mode);
            let items = list_with(&registry);
            let report = apply_with(&items, &ids(&items, 0), &registry, &journal).unwrap();
            assert_eq!((report.done, report.failed), (0, 1));
            if mode == 1 {
                assert_eq!(registry.read(&SPECS[0]).unwrap(), Some(old));
                assert!(journal.undo.borrow().is_empty());
                assert!(!report.needs_restart);
            } else {
                assert_eq!(journal.undo.borrow().len(), 1);
                assert!(report.needs_restart);
            }
        }
    }

    #[test]
    fn final_flush_failure_retains_pending_and_active_is_exact_not_coerced() {
        let journal = MemoryJournal::default();
        let registry = FakeRegistry::new(&journal);
        journal.fail_at.set(Some(2));
        let items = list_with(&registry);
        assert!(apply_with(&items, &ids(&items, 0), &registry, &journal).is_err());
        assert_eq!(journal.undo.borrow().len(), 1);
        registry.values.borrow_mut().insert(
            SPECS[0].id,
            RawValue {
                vtype: 1,
                bytes: vec![48, 0],
            },
        );
        assert!(!list_with(&registry)[0].active); // missing terminator is not exact REG_SZ "0"
        registry
            .values
            .borrow_mut()
            .insert(SPECS[0].id, RawValue::sz("0"));
        let active = list_with(&registry);
        assert!(active[0].active);
        let noop = apply_with(
            &active,
            &ids(&active, 0),
            &registry,
            &MemoryJournal::default(),
        )
        .unwrap();
        assert_eq!(
            (noop.done, noop.failed, noop.record_id, noop.needs_restart),
            (0, 0, None, false)
        );
    }

    #[test]
    fn cache_scope_excludes_profiles_data_and_roaming_firefox() {
        let fixture = Fixture::new();
        fixture.file("Google/Chrome/User Data/Default/Cache/a", b"aaa");
        fixture.file(
            "Microsoft/Edge/User Data/Profile 1/Code Cache/js/a",
            b"1234",
        );
        fixture.file(
            "BraveSoftware/Brave-Browser/User Data/Default/GPUCache/a",
            b"5",
        );
        fixture.file(
            "Mozilla/Firefox/Profiles/abc.default/cache2/entries/a",
            b"12",
        );
        for path in [
            "Google/Chrome/User Data/Default/Preferences",
            "Google/Chrome/User Data/Default/Secure Preferences",
            "Google/Chrome/User Data/Default/History",
            "Google/Chrome/User Data/Default/Network/Cookies",
            "Google/Chrome/User Data/Default/Extensions/a",
            "Google/Chrome/User Data/Default/Cache/Preferences",
            "Google/Chrome/User Data/Default/Cache/extensions/x",
            "Roaming/Mozilla/Firefox/Profiles/a/cache2/x",
            "Electron/Cache/a",
            "Google/Chrome/User Data/Guest Profile/Cache/a",
        ] {
            fixture.file(path, b"DO NOT TOUCH");
        }
        let scan = fixture.scan();
        assert_eq!(scan.caches.len(), 4);
        assert_eq!(scan.caches.iter().map(|c| c.bytes).sum::<u64>(), 10);
        assert_eq!(scan.caches.iter().map(|c| c.files).sum::<u64>(), 4);
        assert_eq!(scan.skipped, 2);
        for cache in &scan.caches {
            validate_cache(cache).unwrap();
            assert!(serde_json::to_value(cache).unwrap().get("proof").is_none());
        }
    }

    #[test]
    fn hardlinks_and_path_aliases_are_rejected() {
        let fixture = Fixture::new();
        let source = fixture.file("outside", b"private");
        let linked = fixture
            .root
            .join("Google/Chrome/User Data/Default/Cache/linked");
        fs::create_dir_all(linked.parent().unwrap()).unwrap();
        fs::hard_link(&source, &linked).unwrap();
        let scan = fixture.scan();
        assert!(scan.caches.is_empty());
        assert_eq!(scan.skipped, 1);
        for bad in ["..", "a:b", "Cache.", "Cache ", "NUL", "a/b", "a\\b"] {
            assert!(!plain_name(bad));
        }
        assert!(plain_absolute(&fixture.root.join("..").join("other")).is_err());
        #[cfg(unix)]
        {
            let target = fixture
                .root
                .join("Google/Chrome/User Data/Default/Code Cache");
            std::os::unix::fs::symlink(&fixture.root, &target).unwrap();
            assert_eq!(fixture.scan().skipped, 2);
        }
    }

    #[test]
    fn changed_files_and_replaced_roots_never_recycle() {
        let fixture = Fixture::new();
        let path = fixture.file("Google/Chrome/User Data/Default/Cache/a", b"before");
        let scan = fixture.scan();
        fs::write(&path, b"changed length").unwrap();
        let journal = MemoryJournal::default();
        let called = Cell::new(0);
        let report = clear_with(
            &scan.caches,
            &cache_ids(&scan),
            &journal,
            &AtomicBool::new(false),
            &|_| Ok(false),
            &|_| {
                called.set(called.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!((report.done, report.failed, called.get()), (0, 1, 0));
        let scan = fixture.scan();
        let root = path.parent().unwrap();
        fs::rename(root, root.with_file_name("old-cache")).unwrap();
        fs::create_dir(root).unwrap();
        fs::write(&path, b"changed length").unwrap();
        let report = clear_with(
            &scan.caches,
            &cache_ids(&scan),
            &journal,
            &AtomicBool::new(false),
            &|_| Ok(false),
            &|_| {
                called.set(called.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!((report.done, report.failed, called.get()), (0, 1, 0));
    }

    #[test]
    fn active_browser_strict_ids_and_cancel_refuse_before_mutation() {
        let fixture = Fixture::new();
        fixture.file("Google/Chrome/User Data/Default/Cache/a", b"a");
        let scan = fixture.scan();
        let journal = MemoryJournal::default();
        let called = Cell::new(false);
        let recycle = |_: &Path| {
            called.set(true);
            Ok(())
        };
        let selection = cache_ids(&scan);
        assert_eq!(
            clear_with(
                &scan.caches,
                &selection,
                &journal,
                &AtomicBool::new(false),
                &|_| Ok(true),
                &recycle
            )
            .unwrap_err(),
            "browser_running"
        );
        assert_eq!(
            clear_with(
                &scan.caches,
                &selection,
                &journal,
                &AtomicBool::new(false),
                &|_| Err("process_unreadable".into()),
                &recycle
            )
            .unwrap_err(),
            "process_unreadable"
        );
        for ids in [
            vec![],
            vec!["arbitrary/path".into()],
            vec![selection[0].clone(), selection[0].clone()],
        ] {
            assert!(clear_with(
                &scan.caches,
                &ids,
                &journal,
                &AtomicBool::new(false),
                &|_| Ok(false),
                &recycle
            )
            .is_err());
        }
        let report = clear_with(
            &scan.caches,
            &selection,
            &journal,
            &AtomicBool::new(true),
            &|_| panic!("process enumeration on cancellation"),
            &recycle,
        )
        .unwrap();
        assert!(report.cancelled && report.record_id.is_none());
        assert!(!called.get());
        assert_eq!(journal.saves.get(), 0);
        let absolute = fixture.root.join("chrome.exe");
        assert!(matching_image(&BROWSERS[0], &absolute));
        assert!(!matching_image(
            &BROWSERS[0],
            &fixture.root.join("notchrome.exe")
        ));
        assert!(!matching_image(&BROWSERS[0], Path::new("chrome.exe")));
    }

    #[test]
    fn cache_partial_failure_and_cancel_keep_honest_history_without_undo() {
        let fixture = Fixture::new();
        fixture.file("Google/Chrome/User Data/Default/Cache/a", b"123");
        fixture.file("Google/Chrome/User Data/Default/Cache/b", b"123");
        let scan = fixture.scan();
        let journal = MemoryJournal::default();
        let calls = Cell::new(0);
        let report = clear_with(
            &scan.caches,
            &cache_ids(&scan),
            &journal,
            &AtomicBool::new(false),
            &|_| Ok(false),
            &|path| {
                calls.set(calls.get() + 1);
                if calls.get() == 1 {
                    Err("fixture_recycle_failure".into())
                } else {
                    fs::rename(path, fixture.root.join("fixture-recycled"))
                        .map_err(|e| e.to_string())
                }
            },
        )
        .unwrap();
        assert_eq!((report.done, report.failed, report.bytes), (1, 1, 3));
        assert_eq!(journal.totals.get(), (1, 1, 3));
        assert_eq!(&*journal.title.borrow(), "browserCache");
        assert!(journal.undo.borrow().is_empty());
        assert!(fixture
            .root
            .join("Google/Chrome/User Data/Default/Cache")
            .is_dir());
        // A separate cache gives deterministic cancellation after one file.
        fixture.file("Microsoft/Edge/User Data/Default/Cache/a", b"x");
        fixture.file("Microsoft/Edge/User Data/Default/Cache/b", b"x");
        let scan = fixture.scan();
        let edge = scan.caches.iter().find(|c| c.browser == "edge").unwrap();
        let cancel = AtomicBool::new(false);
        let report = clear_with(
            &scan.caches,
            &[edge.id.clone()],
            &journal,
            &cancel,
            &|_| Ok(false),
            &|path| {
                fs::rename(path, fixture.root.join("fixture-recycled-2"))
                    .map_err(|e| e.to_string())?;
                cancel.store(true, Ordering::Relaxed);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            (report.done, report.failed, report.bytes, report.cancelled),
            (1, 0, 1, true)
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_cache_fixture_retains_failed_payload_and_durable_recovery_details() {
        if !winutil::is_elevated() {
            eprintln!("restricted staging fixture requires elevation; not exercised");
            return;
        }
        let fixture = Fixture::new();
        let original = fixture.file("Google/Chrome/User Data/Default/Cache/a", b"cache");
        let private = fixture.file("Google/Chrome/User Data/Default/History", b"private");
        let scan = fixture.scan();
        let journal = MemoryJournal::default();
        let report = clear_verified_with(
            &scan.caches,
            &cache_ids(&scan),
            &journal,
            &AtomicBool::new(false),
            &|_| Ok(false),
            &|proof, file, guards, persist| {
                super::super::verified_recycle::fixture_recycle(
                    guards.last().unwrap().0,
                    guards[guards.len() - 2].0,
                    &file.path,
                    &fixture.root,
                    &AtomicBool::new(false),
                    &|restoring| {
                        guard_directories(&proof.ancestors)?;
                        guard_directories(&file.parents)?;
                        if !restoring {
                            revalidate(proof, file)?;
                        }
                        Ok(())
                    },
                    persist,
                    &|_, _| {
                        fs::write(&original, b"new unrelated cache").unwrap();
                        Err("fixture recycle failure".into())
                    },
                )
            },
        )
        .unwrap();
        assert_eq!((report.done, report.failed, report.bytes), (0, 1, 0));
        assert_eq!(report.recovery.len(), 1);
        assert!(report.recovery[0].contains("fixture recycle failure"));
        assert!(journal.title.borrow().contains("recovery="));
        assert!(journal.undo.borrow().is_empty());
        assert_eq!(fs::read(original).unwrap(), b"new unrelated cache");
        assert_eq!(fs::read(private).unwrap(), b"private");
    }

    #[test]
    fn cache_recovery_flush_precedes_callback_and_survives_failure() {
        let fixture = Fixture::new();
        fixture.file("Google/Chrome/User Data/Default/Cache/a", b"cache");
        let scan = fixture.scan();
        let journal = MemoryJournal::default();
        let report = clear_verified_with(
            &scan.caches,
            &cache_ids(&scan),
            &journal,
            &AtomicBool::new(false),
            &|_| Ok(false),
            &|_, _, _, persist| {
                let detail = format!(
                    "recovery_pending: staging={}; recycle_error=fixture failure",
                    fixture.root.join("staging/payload").display()
                );
                persist(Some(&detail))?;
                assert!(
                    journal.title.borrow().contains("staging/payload")
                        || journal.title.borrow().contains("staging\\\\payload")
                );
                Err(detail)
            },
        )
        .unwrap();
        assert_eq!((report.done, report.failed), (0, 1));
        assert_eq!(report.recovery.len(), 1);
        assert!(journal.title.borrow().contains("fixture failure"));
        assert!(journal.undo.borrow().is_empty());
    }

    #[test]
    fn scan_limits_and_cancel_are_explicit_and_bounded() {
        let fixture = Fixture::new();
        for name in ["a", "b", "c"] {
            fixture.file(
                &format!("Google/Chrome/User Data/Default/Cache/{name}"),
                b"x",
            );
        }
        let limits = Limits { files: 1, ..LIMITS };
        let scan = scan_at(&fixture.root, &AtomicBool::new(false), &|_, _| {}, limits).unwrap();
        assert_eq!(scan.caches.iter().map(|c| c.files).sum::<u64>(), 1);
        assert_eq!(scan.skipped, 1);
        assert!(!scan.cancelled);
        let cancel = AtomicBool::new(false);
        let scan = scan_at(
            &fixture.root,
            &cancel,
            &|_, _| {
                cancel.store(true, Ordering::Relaxed);
            },
            LIMITS,
        )
        .unwrap();
        assert!(scan.cancelled && scan.caches.is_empty());
        assert_eq!(scan.skipped, 0);
    }

    #[test]
    fn history_flush_failure_and_post_flush_change_do_not_recycle() {
        let fixture = Fixture::new();
        let path = fixture.file("Google/Chrome/User Data/Default/Cache/a", b"a");
        let scan = fixture.scan();
        let journal = MemoryJournal::default();
        let calls = Cell::new(0);
        journal.fail_at.set(Some(1));
        assert!(clear_with(
            &scan.caches,
            &cache_ids(&scan),
            &journal,
            &AtomicBool::new(false),
            &|_| Ok(false),
            &|_| {
                calls.set(1);
                Ok(())
            }
        )
        .is_err());
        assert_eq!(calls.get(), 0);
        // Use an injected browser transition rather than touching a real one.
        let journal = MemoryJournal::default();
        let checks = Cell::new(0);
        let result = clear_with(
            &scan.caches,
            &cache_ids(&scan),
            &journal,
            &AtomicBool::new(false),
            &|_| {
                checks.set(checks.get() + 1);
                Ok(checks.get() >= 3)
            },
            &|_| {
                calls.set(1);
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err(), "browser_running");
        assert_eq!(calls.get(), 0);
        assert!(path.is_file());
    }

    #[test]
    fn unscanned_files_and_forged_scopes_are_never_cleared() {
        let fixture = Fixture::new();
        let original = fixture.file("Google/Chrome/User Data/Default/Cache/a", b"cache");
        let private = fixture.file("Google/Chrome/User Data/Default/Preferences", b"private");
        let scan = fixture.scan();
        let fresh = fixture.file("Google/Chrome/User Data/Default/Cache/new", b"fresh");
        let journal = MemoryJournal::default();
        let report = clear_with(
            &scan.caches,
            &cache_ids(&scan),
            &journal,
            &AtomicBool::new(false),
            &|_| Ok(false),
            &|path| {
                assert_eq!(path, original);
                fs::rename(path, fixture.root.join("fixture-recycled-only-enumerated"))
                    .map_err(|e| e.to_string())
            },
        )
        .unwrap();
        assert_eq!((report.done, report.failed, report.bytes), (1, 0, 5));
        assert!(fresh.is_file() && private.is_file() && original.parent().unwrap().is_dir());
        let mut forged = scan.caches.clone();
        forged[0].proof.files[0].path = private;
        assert!(clear_with(
            &forged,
            &cache_ids(&scan),
            &journal,
            &AtomicBool::new(false),
            &|_| panic!("invalid scope must precede process checks"),
            &|_| panic!("invalid scope must not recycle")
        )
        .is_err());
    }

    #[test]
    fn rescue_incompatible_raw_types_are_unavailable_not_lossily_restored() {
        let journal = MemoryJournal::default();
        let registry = FakeRegistry::new(&journal);
        for kind in [5, 6, 8, 9, 10, 99] {
            registry.values.borrow_mut().insert(
                SPECS[0].id,
                RawValue {
                    vtype: kind,
                    bytes: vec![0, 1, 2],
                },
            );
            let items = list_with(&registry);
            assert!(!items[0].available);
            assert_eq!(
                items[0].reason.as_deref(),
                Some("unsupported_registry_type")
            );
            assert!(apply_with(&items, &ids(&items, 0), &registry, &journal).is_err());
        }
        assert_eq!(registry.writes.get(), 0);
        assert_eq!(journal.saves.get(), 0);
    }

    #[test]
    fn dns_command_is_fixed_absolute_and_bounded_without_execution() {
        let fixture = Fixture::new();
        let program = dns_command(&fixture.root).unwrap();
        assert!(program.is_absolute());
        assert_eq!(program, fixture.root.join("System32").join("ipconfig.exe"));
        assert_eq!(DNS_ARGS, ["/flushdns"]);
        assert_eq!(DNS_TIMEOUT, Duration::from_secs(15));
        assert!(dns_command(Path::new("relative/Windows")).is_err());
        assert!(dns_command(&fixture.root.join("..")).is_err());
        // Deliberately never invoke flush_dns(), cache_clear(), list(), apply(),
        // native registry, process enumeration, shell recycle or live browsers.
    }
}
