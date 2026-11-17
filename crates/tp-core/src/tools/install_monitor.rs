//! Opt-in, informational installation comparison. Never launches an installer,
//! imports registry data, attributes ownership, or deletes an installation.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA: u32 = 1;
const MAX_SESSION_ID: u64 = 9_007_199_254_740_991;
const MAX_FILES: usize = 10_000;
const MAX_REG_VALUES: usize = 10_000;
const MAX_ENTRIES: usize = 30_000;
const MAX_DIRECTORIES: usize = 4_096;
const MAX_KEYS: usize = 4_096;
const MAX_DEPTH: usize = 24;
const MAX_PATH: usize = 1_024;
const MAX_VALUE_NAME: usize = 512;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_REG_VALUE_BYTES: usize = 64 * 1024;
const MAX_TOTAL_REG_BYTES: usize = 64 * 1024 * 1024;
const MAX_INVENTORY_BYTES: usize = 3 * 1024 * 1024;
const MAX_CHANGES: usize = 5_000;
const MAX_CHANGE_BYTES: usize = 3 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 20 * 1024 * 1024;
const MAX_SESSIONS: usize = 256;
const MAX_WARNINGS: usize = 64;
const MAX_WARNING_BYTES: usize = 2_048;
const LOCK_NAME: &str = ".monitor.lock";
const OMITTED_WARNINGS: &str = "Additional read/omission warnings were truncated.";
const ATTRIBUTION_WARNING: &str = "Informational only: simultaneous changes are not attributed to this app. This is not an ownership manifest or an uninstall/deletion guarantee. User-writable history is schema/safety-validated, not authenticated.";
const FILE_SCOPE_WARNING: &str = "Only regular, single-link files in the exact selected exclusive folder are captured; no ADS, reparse points or outside trees. Limits: 10000 files, 30000 entries, 4096 directories, depth 24, 64 MiB/file, 512 MiB read total, 3 MiB file inventory, 20 MiB/session record, 256 sessions. Fingerprints are non-cryptographic change filters. Cancellation is cooperative between I/O operations.";
const REG_SCOPE_WARNING: &str = "Finite registry scope: HKCU (64-bit view), HKLM64 and HKLM32 Uninstall (depth 8), App Paths (depth 1), Run (depth 0), and optional Software\\<vendor> (depth 12). Differences are value-level; empty keys are not tracked. Only type, size and informational fingerprint are retained, never raw values. Limits: 4096 keys, 10000 values, 64 KiB/value, 64 MiB read total, 3 MiB registry inventory.";
const DIFF_WARNING: &str = "The displayed/persisted diff was truncated (5000 changes or 3 MiB); it is not a complete inventory of changes.";
const INCOMPLETE_WARNING: &str = "Omitted or unreadable data makes this comparison incomplete. Additions require a complete before inventory; removals require a complete after inventory in that domain. Unknown entries are not treated as absent.";

// Also serializes independently constructed Monitor instances in this process.
// Windows additionally uses a crash-released, non-shared file handle per store.
static STORE_MUTEX: Mutex<()> = Mutex::new(());
static NONCE: AtomicU64 = AtomicU64::new(1);

// deserialize_with makes an Option required in JSON (explicit null is allowed).
// Ordinary serde Option fields otherwise silently accept missing members.
fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Summary {
    pub id: u64,
    pub name: String,
    pub root: String,
    pub vendor: String,
    pub started_at: u64,
    #[serde(deserialize_with = "required_option")]
    pub finished_at: Option<u64>,
    pub files_before: u64,
    pub change_count: u64,
    pub complete: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Change {
    pub kind: String,
    pub change: String,
    pub path: String,
    #[serde(deserialize_with = "required_option")]
    pub value_name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    pub session: Summary,
    pub changes: Vec<Change>,
    pub warnings: Vec<String>,
    pub truncated: bool,
}

pub struct Monitor {
    dir: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileId {
    volume: u64,
    index: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileEntry {
    // A validated relative path, using '/' on disk on every platform.
    path: String,
    size: u64,
    #[serde(deserialize_with = "required_option")]
    modified_ns: Option<String>,
    #[serde(deserialize_with = "required_option")]
    file_id: Option<FileId>,
    fingerprint: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegistryEntry {
    path: String,
    value_name: String,
    value_type: u32,
    size: u64,
    fingerprint: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Snapshot {
    files: Vec<FileEntry>,
    registry: Vec<RegistryEntry>,
    files_complete: bool,
    registry_complete: bool,
    truncated: bool,
    warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    schema: u32,
    report: Report,
    before: Snapshot,
    #[serde(deserialize_with = "required_option")]
    after: Option<Snapshot>,
}

struct RegistryCapture {
    entries: Vec<RegistryEntry>,
    complete: bool,
    truncated: bool,
    warnings: Vec<String>,
}

// Tests inject this inventory; no unit test reads or modifies live registry data.
trait Inventory {
    fn capture(&self, vendor: &str, cancel: &AtomicBool) -> Result<RegistryCapture, String>;
}

struct NativeInventory;

impl Inventory for NativeInventory {
    fn capture(&self, vendor: &str, cancel: &AtomicBool) -> Result<RegistryCapture, String> {
        native_registry(vendor, cancel)
    }
}

impl Monitor {
    pub fn default_location() -> Self {
        let base = crate::winutil::known_folder(crate::winutil::Known::LocalAppData)
            .unwrap_or_else(std::env::temp_dir);
        Self::at(base.join("TP System Care").join("InstallMonitor"))
    }

    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn list(&self) -> Result<Vec<Summary>, String> {
        let _process = STORE_MUTEX.try_lock().map_err(|_| "monitor_busy")?;
        let store = self.store(false)?;
        if store.is_none() {
            return Ok(Vec::new());
        }
        Ok(self.load_history(None)?.sessions)
    }

    pub fn begin(
        &self,
        name: &str,
        root: &Path,
        vendor: &str,
        cancel: &AtomicBool,
        progress: &dyn Fn(&Path, usize),
    ) -> Result<Summary, String> {
        self.begin_with(name, root, vendor, cancel, progress, &NativeInventory)
    }

    pub fn finish(
        &self,
        id: u64,
        cancel: &AtomicBool,
        progress: &dyn Fn(&Path, usize),
    ) -> Result<Report, String> {
        self.finish_with(id, cancel, progress, &NativeInventory)
    }

    pub fn detail(&self, id: u64) -> Result<Report, String> {
        check_id(id)?;
        let _process = STORE_MUTEX.try_lock().map_err(|_| "monitor_busy")?;
        let _store = self.store(false)?.ok_or("monitor_not_found")?;
        // Validate every record, including the one-active-baseline invariant.
        let record = self
            .load_history(Some(id))?
            .selected
            .ok_or("monitor_not_found")?;
        if record.after.is_none() {
            return Err("monitor_not_finished".into());
        }
        Ok(record.report)
    }

    fn begin_with(
        &self,
        name: &str,
        root: &Path,
        vendor: &str,
        cancel: &AtomicBool,
        progress: &dyn Fn(&Path, usize),
        inventory: &dyn Inventory,
    ) -> Result<Summary, String> {
        cancelled(cancel)?;
        validate_name(name)?;
        validate_vendor(vendor)?;
        let root = lexical_absolute(root)?;
        require_fixed_drive(&root)?;
        validate_root_policy(&root, &self.dir)?;
        let _process = STORE_MUTEX.try_lock().map_err(|_| "monitor_busy")?;
        let _store = self.store(true)?.ok_or("monitor_store_missing")?;
        let records = self.load_history(None)?.sessions;
        if records.iter().any(|r| r.finished_at.is_none()) {
            return Err("monitor_active".into());
        }
        if records.len() >= MAX_SESSIONS {
            return Err("monitor_history_limit".into());
        }
        let previous = records.first().map_or(0, |r| r.id);
        let id = now_id().max(previous.checked_add(1).ok_or("monitor_id_exhausted")?);
        check_id(id)?;
        let before = capture(&root, vendor, cancel, progress, inventory)?;
        let session = Summary {
            id,
            name: name.to_owned(),
            root: path_string(&root)?,
            vendor: vendor.to_owned(),
            started_at: now_millis()?,
            finished_at: None,
            files_before: before.files.len() as u64,
            change_count: 0,
            complete: false,
        };
        let report = make_report(session, &before, None);
        let record = Record {
            schema: SCHEMA,
            report,
            before,
            after: None,
        };
        self.validate_record(id, &record)?;
        self.persist(&record, false, cancel)?;
        Ok(record.report.session)
    }

    fn finish_with(
        &self,
        id: u64,
        cancel: &AtomicBool,
        progress: &dyn Fn(&Path, usize),
        inventory: &dyn Inventory,
    ) -> Result<Report, String> {
        check_id(id)?;
        cancelled(cancel)?;
        let _process = STORE_MUTEX.try_lock().map_err(|_| "monitor_busy")?;
        let _store = self.store(false)?.ok_or("monitor_not_found")?;
        let mut record = self
            .load_history(Some(id))?
            .selected
            .ok_or("monitor_not_found")?;
        if record.after.is_some() {
            return Err("monitor_already_finished".into());
        }
        // No caller-supplied path or registry scope is accepted at finish.
        let root = PathBuf::from(&record.report.session.root);
        let after = capture(
            &root,
            &record.report.session.vendor,
            cancel,
            progress,
            inventory,
        )?;
        let mut session = record.report.session.clone();
        session.finished_at = Some(now_millis()?.max(session.started_at));
        record.report = make_report(session, &record.before, Some(&after));
        record.after = Some(after);
        self.validate_record(id, &record)?;
        self.persist(&record, true, cancel)?;
        Ok(record.report)
    }

    fn record_path(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn store(&self, create: bool) -> Result<Option<StoreGuard>, String> {
        let path = lexical_absolute(&self.dir)?;
        let directories = safe_ancestors(&path, create)?;
        if !directories.exists {
            return Ok(None);
        }
        let lock_path = path.join(LOCK_NAME);
        if let Ok(metadata) = fs::symlink_metadata(&lock_path) {
            if !metadata.is_file() || is_reparse(&metadata) {
                return Err("monitor_store_unsafe".into());
            }
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        no_follow(&mut options);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let lock = options
            .open(&lock_path)
            .map_err(|e| format!("monitor_store_lock: {e}"))?;
        check_opened(&lock_path, &lock, false)?;
        // Portable mode is only for owned temp fixtures. The process mutex
        // serializes their store access; production Windows locks across processes.
        Ok(Some(StoreGuard {
            _directories: directories,
            _lock: lock,
        }))
    }

    fn load_history(&self, wanted: Option<u64>) -> Result<History, String> {
        // Validate one bounded record at a time. Listing must not retain every
        // before/after snapshot in memory merely to return small summaries.
        let mut records: Vec<Summary> = Vec::new();
        let mut selected = None;
        let mut entries = 0usize;
        for item in fs::read_dir(&self.dir).map_err(|e| format!("monitor_store_read: {e}"))? {
            entries += 1;
            if entries > MAX_SESSIONS * 4 + 1 {
                return Err("monitor_store_entry_limit".into());
            }
            let item = item.map_err(|e| format!("monitor_store_read: {e}"))?;
            let name = item
                .file_name()
                .into_string()
                .map_err(|_| "monitor_corrupt_name")?;
            if name == LOCK_NAME {
                continue;
            }
            if valid_pending_name(&name) {
                // A crash before the atomic commit can leave an uncommitted file.
                // It is not a baseline or history entry and is never promoted.
                let file = open_checked(&item.path(), false)?;
                if file.metadata().map_err(|e| e.to_string())?.len() > MAX_RECORD_BYTES as u64 {
                    return Err("monitor_corrupt_pending".into());
                }
                continue;
            }
            let digits = name.strip_suffix(".json").ok_or("monitor_corrupt_name")?;
            let id = decimal_id(digits).ok_or("monitor_corrupt_id")?;
            if records.len() >= MAX_SESSIONS {
                return Err("monitor_history_limit".into());
            }
            let path = self.record_path(id);
            let mut file = open_checked(&path, false)?;
            let before = stamp(&file)?;
            if before.0 > MAX_RECORD_BYTES as u64 {
                return Err(format!("monitor_corrupt_record: {id}: record size limit"));
            }
            let mut bytes = Vec::new();
            (&mut file)
                .take((MAX_RECORD_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("monitor_record_read: {id}: {e}"))?;
            if bytes.len() > MAX_RECORD_BYTES || stamp(&file)? != before {
                return Err(format!(
                    "monitor_corrupt_record: {id}: unstable or oversized"
                ));
            }
            let record: Record = serde_json::from_slice(&bytes)
                .map_err(|e| format!("monitor_corrupt_record: {id}: {e}"))?;
            self.validate_record(id, &record)
                .map_err(|e| format!("monitor_corrupt_record: {id}: {e}"))?;
            records.push(record.report.session.clone());
            if wanted == Some(id) {
                selected = Some(record);
            }
        }
        records.sort_by(|a, b| b.id.cmp(&a.id));
        if records.iter().filter(|r| r.finished_at.is_none()).count() > 1 {
            return Err("monitor_corrupt_record: multiple active baselines".into());
        }
        if let Some(active) = records.iter().find(|r| r.finished_at.is_none()) {
            if records.first().map(|r| r.id) != Some(active.id) {
                return Err("monitor_corrupt_record: active baseline is not newest".into());
            }
        }
        Ok(History {
            sessions: records,
            selected,
        })
    }

    fn validate_record(&self, expected: u64, record: &Record) -> Result<(), String> {
        let session = &record.report.session;
        check_id(expected)?;
        if record.schema != SCHEMA || session.id != expected {
            return Err("ID/schema mismatch".into());
        }
        validate_name(&session.name)?;
        validate_vendor(&session.vendor)?;
        let root = lexical_absolute(Path::new(&session.root))?;
        if path_string(&root)? != session.root {
            return Err("noncanonical persisted root spelling".into());
        }
        validate_root_policy(&root, &self.dir)?;
        if session.started_at == 0 || session.started_at > 253_402_300_799_999 {
            return Err("invalid start time".into());
        }
        match (session.finished_at, record.after.as_ref()) {
            (None, None) => {}
            (Some(time), Some(_)) if time >= session.started_at && time <= 253_402_300_799_999 => {}
            _ => return Err("invalid completion state".into()),
        }
        validate_snapshot(&record.before, session)?;
        if let Some(after) = &record.after {
            validate_snapshot(after, session)?;
        }
        let expected_report = make_report(session.clone(), &record.before, record.after.as_ref());
        if expected_report != record.report {
            return Err("summary/diff/warnings mismatch".into());
        }
        Ok(())
    }

    fn persist(&self, record: &Record, replace: bool, cancel: &AtomicBool) -> Result<(), String> {
        cancelled(cancel)?;
        let bytes = serde_json::to_vec(record).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err("monitor_record_size_limit".into());
        }
        let id = record.report.session.id;
        let destination = self.record_path(id);
        if replace {
            // Validate the destination again before replacing it. Do not follow
            // a user-writable record symlink or replace any unvalidated record.
            let baseline = self
                .load_history(Some(id))?
                .selected
                .ok_or("monitor_not_found")?;
            if baseline.after.is_some() || baseline.before != record.before {
                return Err("monitor_baseline_changed".into());
            }
            let mut expected_session = record.report.session.clone();
            expected_session.finished_at = None;
            if make_report(expected_session, &record.before, None) != baseline.report {
                return Err("monitor_baseline_changed".into());
            }
        } else {
            // Recheck the catalog after the bounded capture too. Concurrent
            // manual edits to this user-writable store are not trusted.
            let history = self.load_history(None)?;
            if history.sessions.iter().any(|s| s.finished_at.is_none()) {
                return Err("monitor_active".into());
            }
            if history.sessions.len() >= MAX_SESSIONS {
                return Err("monitor_history_limit".into());
            }
            if history.sessions.first().is_some_and(|s| s.id >= id) {
                return Err("monitor_history_changed".into());
            }
            match fs::symlink_metadata(&destination) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                _ => return Err("monitor_record_exists".into()),
            }
        }
        let nonce = now_id()
            .wrapping_add(NONCE.fetch_add(1, Ordering::Relaxed))
            .max(1);
        let temporary = self.dir.join(format!(".{id}.{nonce}.tmp"));
        let mut created = false;
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            no_follow(&mut options);
            let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
            created = true;
            file.write_all(&bytes).map_err(|e| e.to_string())?;
            file.flush().map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            // Cancellation observed before the atomic commit never changes the
            // baseline. A cancellation after this linearization point is late.
            cancelled(cancel)?;
            atomic_move(&temporary, &destination, replace)
        })();
        if created && result.is_err() {
            // Only remove our own uncommitted temporary file, never app files.
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

struct History {
    sessions: Vec<Summary>,
    selected: Option<Record>,
}

struct StoreGuard {
    _directories: DirectoryGuard,
    _lock: File,
}

struct PinnedDirectory {
    path: PathBuf,
    file: File,
    id: FileId,
}

impl PinnedDirectory {
    fn open(path: &Path) -> Result<Self, String> {
        let file = open_checked(path, true)?;
        let id = identity(&file)?.0;
        Ok(Self {
            path: path.to_owned(),
            file,
            id,
        })
    }

    fn validate(&self) -> Result<(), String> {
        check_opened(&self.path, &self.file, true)?;
        if identity(&self.file)?.0 != self.id {
            return Err("monitor_directory_identity_changed".into());
        }
        Ok(())
    }
}

struct DirectoryGuard {
    _handles: Vec<PinnedDirectory>,
    exists: bool,
}

fn validate_parents(parents: &[&PinnedDirectory]) -> Result<(), String> {
    for parent in parents {
        parent.validate()?;
    }
    Ok(())
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("monitor_cancelled".into())
    } else {
        Ok(())
    }
}

fn now_millis() -> Result<u64, String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?;
    u64::try_from(duration.as_millis()).map_err(|_| "monitor_clock_overflow".into())
}

fn now_id() -> u64 {
    now_millis().unwrap_or(1).max(1)
}

fn check_id(id: u64) -> Result<(), String> {
    if id == 0 || id > MAX_SESSION_ID {
        Err("monitor_invalid_id".into())
    } else {
        Ok(())
    }
}

fn decimal_id(text: &str) -> Option<u64> {
    if text.is_empty() || text.len() > 20 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let id = text.parse::<u64>().ok()?;
    (check_id(id).is_ok() && id.to_string() == text).then_some(id)
}

fn valid_pending_name(name: &str) -> bool {
    let Some(inner) = name.strip_prefix('.').and_then(|s| s.strip_suffix(".tmp")) else {
        return false;
    };
    let mut parts = inner.split('.');
    matches!((parts.next(), parts.next(), parts.next()), (Some(id), Some(nonce), None)
        if decimal_id(id).is_some() && decimal_id(nonce).is_some())
}

fn safe_text(text: &str, max: usize, empty: bool) -> bool {
    (empty || !text.is_empty()) && text.len() <= max && !text.chars().any(char::is_control)
}

fn validate_name(name: &str) -> Result<(), String> {
    if !safe_text(name, 128, false) || name.trim() != name {
        return Err("monitor_invalid_name".into());
    }
    Ok(())
}

fn validate_vendor(vendor: &str) -> Result<(), String> {
    if vendor.is_empty() {
        return Ok(());
    }
    let lower = vendor.to_ascii_lowercase();
    let generic = [
        "software",
        "vendor",
        "vendors",
        "app",
        "apps",
        "application",
        "applications",
        "company",
        "companies",
        "common",
        "common files",
        "shared",
        "system",
        "wow6432node",
        "clients",
        "registeredapplications",
        "install",
        "installer",
        "uninstall",
        "app paths",
        "run",
        "runonce",
        "users",
        "default",
        "public",
        "currentversion",
        "services",
        "security",
        "sam",
        "components",
        "network",
    ];
    if !safe_text(vendor, 64, false)
        || vendor.trim() != vendor
        || !vendor
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'_' | b'-'))
        || !vendor.bytes().any(|b| b.is_ascii_alphabetic())
        || lower.starts_with("tp")
        || ["microsoft", "classes", "policies", "windows"]
            .iter()
            .any(|p| lower.starts_with(p))
        || generic.contains(&lower.as_str())
    {
        return Err("monitor_invalid_vendor".into());
    }
    Ok(())
}

fn safe_component(component: &str) -> bool {
    if !safe_text(component, 255, false)
        || component == "."
        || component == ".."
        || component.ends_with(['.', ' '])
        || component.contains(['/', '\\', ':', '~', '<', '>', '"', '|', '?', '*'])
    {
        return false;
    }
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if ["con", "prn", "aux", "nul", "clock$", "conin$", "conout$"].contains(&stem.as_str()) {
        return false;
    }
    if stem.starts_with("com") || stem.starts_with("lpt") {
        let tail = &stem[3..];
        if matches!(
            tail,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        ) {
            return false;
        }
    }
    true
}

#[cfg(any(test, not(windows)))]
fn fixture_temp_root() -> PathBuf {
    let temp = std::env::temp_dir();
    // Fixtures require the real temp path, not an alias.
    let temp = fs::canonicalize(&temp).unwrap_or(temp);
    #[cfg(windows)]
    {
        let raw = temp.to_string_lossy();
        PathBuf::from(raw.strip_prefix(r"\\?\").unwrap_or(&raw))
    }
    #[cfg(not(windows))]
    {
        temp
    }
}

fn path_string(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "monitor_invalid_path_encoding".into())
}

// Do not let Path::components normalize away '..', '.', doubled separators,
// device prefixes, ADS, or short-name spelling before validating the input.
fn lexical_absolute(path: &Path) -> Result<PathBuf, String> {
    let raw = path_string(path)?;
    if raw.len() > MAX_PATH || raw.chars().any(char::is_control) {
        return Err("monitor_invalid_root".into());
    }
    #[cfg(windows)]
    {
        let text = raw.replace('/', "\\");
        let b = text.as_bytes();
        if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
            return Err("monitor_invalid_root: absolute fixed-drive path required".into());
        }
        let rest = &text[3..];
        if !rest.is_empty() && !rest.split('\\').all(safe_component) {
            return Err("monitor_invalid_root: unsafe path component".into());
        }
        Ok(PathBuf::from(text))
    }
    #[cfg(not(windows))]
    {
        if !raw.starts_with('/') || raw.starts_with("//") || raw.contains('\\') {
            return Err("monitor_invalid_root: absolute path required".into());
        }
        if raw != "/" && !raw[1..].split('/').all(safe_component) {
            return Err("monitor_invalid_root: unsafe path component".into());
        }
        // Portable support is deliberately restricted to private temp fixtures.
        // In particular it cannot be used to inventory a developer's home tree.
        let temp = fixture_temp_root();
        let relative = path
            .strip_prefix(&temp)
            .map_err(|_| "monitor_invalid_root: temp fixture only")?;
        relative
            .components()
            .next()
            .ok_or("monitor_invalid_root: temp root")?;
        Ok(PathBuf::from(raw))
    }
}

fn comparable(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    let raw = raw.strip_prefix("//?/").unwrap_or(&raw).to_owned();
    raw.trim_end_matches('/').to_lowercase()
}

fn within(path: &Path, parent: &Path) -> bool {
    let path = comparable(path);
    let parent = comparable(parent);
    path == parent
        || path
            .strip_prefix(&parent)
            .is_some_and(|tail| tail.starts_with('/'))
}

fn overlaps(a: &Path, b: &Path) -> bool {
    within(a, b) || within(b, a)
}

fn protected_component(component: &str) -> bool {
    let lower = component.to_lowercase();
    let compact: String = lower
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    ["tpsystemcare", "tpbackup", "tprescue", "tpquarantine"]
        .iter()
        .any(|prefix| compact.starts_with(prefix))
        || ["microsoft", "classes", "policies", "windows"]
            .iter()
            .any(|p| lower.starts_with(p))
        || [
            "common",
            "common files",
            "shared",
            "system",
            "system32",
            "syswow64",
            "winsxs",
            "installer",
            "packages",
            "windowsapps",
            "modifiablewindowsapps",
            "assembly",
            "msbuild",
            "reference assemblies",
            "dotnet",
            "package cache",
            "installshield installation information",
            "virtualstore",
            "drivers",
            "driverstore",
            "usoshared",
            "usoprivate",
            "windows defender",
            "windows nt",
            "windows kits",
            "internet explorer",
            "$recycle.bin",
            "system volume information",
            "recovery",
            "backup",
            "backups",
            "rescue",
        ]
        .contains(&lower.as_str())
}

fn validate_root_policy(root: &Path, store: &Path) -> Result<(), String> {
    let normalized = comparable(root);
    if root.parent().is_none() || normalized.len() <= 2 {
        return Err("monitor_invalid_root: shared root".into());
    }
    let components: Vec<_> = normalized.split('/').filter(|s| !s.is_empty()).collect();
    if components
        .iter()
        .any(|component| protected_component(component))
    {
        return Err("monitor_invalid_root: protected component".into());
    }
    let last = components.last().copied().unwrap_or("");
    if [
        "program files",
        "program files (x86)",
        "programdata",
        "users",
        "documents and settings",
        "appdata",
        "local",
        "locallow",
        "roaming",
        "programs",
        "temp",
        "tmp",
        "public",
        "default",
        "desktop",
        "documents",
        "downloads",
        "pictures",
        "music",
        "videos",
        "start menu",
        "startup",
        "apps",
        "applications",
        "software",
        "profiles",
        "vendors",
        "tools",
        "cache",
        "caches",
    ]
    .contains(&last)
    {
        return Err("monitor_invalid_root: system/shared/user root".into());
    }
    if overlaps(root, store) {
        return Err("monitor_invalid_root: monitor storage overlap".into());
    }
    if let Ok(executable) = std::env::current_exe() {
        if executable
            .parent()
            .is_some_and(|parent| overlaps(root, parent))
        {
            return Err("monitor_invalid_root: self overlap".into());
        }
    }
    if overlaps(root, &std::env::temp_dir()) && within(&std::env::temp_dir(), root) {
        return Err("monitor_invalid_root: shared temp root".into());
    }
    #[cfg(windows)]
    {
        // DONT_VERIFY resolves even not-yet-created or redirected shell folders;
        // it neither creates them nor enumerates their contents.
        for (path, protect_descendants, profile_container) in protected_locations()? {
            if profile_container
                && root
                    .parent()
                    .is_some_and(|parent| comparable(parent) == comparable(&path))
            {
                return Err("monitor_invalid_root: profile root".into());
            }
            if within(&path, root) || (protect_descendants && within(root, &path)) {
                return Err("monitor_invalid_root: protected root".into());
            }
            for suffix in ["Microsoft", "Common", "Common Files", "TP System Care"] {
                if overlaps(root, &path.join(suffix)) {
                    return Err("monitor_invalid_root: protected shared directory".into());
                }
            }
        }
        let root_text = path_string(root)?;
        let drive = &root_text[..3];
        for suffix in [
            "Windows",
            "WINNT",
            "Program Files/Common Files",
            "Program Files (x86)/Common Files",
            "ProgramData/Microsoft",
            "ProgramData/TP System Care",
        ] {
            if overlaps(root, &Path::new(drive).join(suffix)) {
                return Err("monitor_invalid_root: system directory".into());
            }
        }
        // AppData itself, including other users' conventional profile layouts.
        if let Some(index) = components.iter().position(|c| *c == "users") {
            if components.len() <= index + 2 {
                return Err("monitor_invalid_root: profile root".into());
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn protected_locations() -> Result<Vec<(PathBuf, bool, bool)>, String> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::*;
    let mut paths = Vec::new();
    for (id, protect_descendants) in [
        (FOLDERID_Windows, true),
        (FOLDERID_ProgramData, false),
        (FOLDERID_ProgramFiles, false),
        (FOLDERID_ProgramFilesX86, false),
        (FOLDERID_ProgramFilesCommon, true),
        (FOLDERID_ProgramFilesCommonX86, true),
        (FOLDERID_Profile, false),
        (FOLDERID_UserProfiles, false),
        (FOLDERID_Public, true),
        (FOLDERID_LocalAppData, false),
        (FOLDERID_LocalAppDataLow, false),
        (FOLDERID_RoamingAppData, false),
        (FOLDERID_UserProgramFiles, false),
        (FOLDERID_UserProgramFilesCommon, true),
        (FOLDERID_Desktop, false),
        (FOLDERID_StartMenu, false),
        (FOLDERID_Startup, false),
        (FOLDERID_CommonStartMenu, true),
        (FOLDERID_CommonStartup, true),
    ] {
        let raw = unsafe { SHGetKnownFolderPath(&id, KF_FLAG_DONT_VERIFY, None) }
            .map_err(|e| format!("monitor_invalid_root: cannot resolve protected folder: {e}"))?;
        let text = unsafe { raw.to_string() };
        unsafe {
            CoTaskMemFree(Some(raw.as_ptr() as *const _));
        }
        let path = PathBuf::from(text.map_err(|e| e.to_string())?);
        paths.push((path, protect_descendants, id == FOLDERID_UserProfiles));
    }
    Ok(paths)
}

#[cfg(windows)]
fn is_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0 || metadata.file_type().is_symlink()
}

#[cfg(not(windows))]
fn is_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn no_follow(options: &mut OpenOptions) {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // OPEN_REPARSE_POINT | BACKUP_SEMANTICS; omit SHARE_DELETE.
        options
            .custom_flags(0x0020_0000 | 0x0200_0000)
            .share_mode(1 | 2);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x20000); // O_NOFOLLOW, portable mode is fixture-only.
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    let _ = options;
}

fn open_checked(path: &Path, directory: bool) -> Result<File, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("monitor_path_read: {}: {e}", path.display()))?;
    if is_reparse(&metadata)
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(format!("monitor_unsafe_path: {}", path.display()));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    #[cfg(windows)]
    if directory {
        use std::os::windows::fs::OpenOptionsExt;
        options.access_mode(0x81); // LIST_DIRECTORY pins actual enumeration access, not only attributes.
    }
    let file = options
        .open(path)
        .map_err(|e| format!("monitor_path_open: {}: {e}", path.display()))?;
    check_opened(path, &file, directory)?;
    Ok(file)
}

fn check_opened(path: &Path, file: &File, directory: bool) -> Result<(), String> {
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if is_reparse(&metadata)
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err("monitor_unsafe_opened_path".into());
    }
    let (_, links) = identity(file)?;
    if !directory && links != 1 {
        return Err("monitor_unsafe_path: hard links are not exclusive".into());
    }
    #[cfg(windows)]
    let resolved = {
        use std::os::windows::ffi::OsStringExt;
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED, GETFINALPATHNAMEBYHANDLE_FLAGS,
            VOLUME_NAME_DOS,
        };
        let mut buffer = [0u16; MAX_PATH + 8];
        let length = unsafe {
            GetFinalPathNameByHandleW(
                HANDLE(file.as_raw_handle()),
                &mut buffer,
                GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_NORMALIZED.0 | VOLUME_NAME_DOS.0),
            )
        } as usize;
        if length == 0 || length >= buffer.len() {
            return Err("monitor_opened_path_resolve".into());
        }
        PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]))
    };
    #[cfg(not(windows))]
    let resolved = fs::canonicalize(path).map_err(|e| format!("monitor_path_resolve: {e}"))?;
    // A pathname can be retargeted while the opened object remains outside scope.
    if comparable(&resolved) != comparable(path) {
        return Err("monitor_unsafe_path: alias or redirected opened path".into());
    }
    #[cfg(not(windows))]
    if fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
        || identity(&File::open(path).map_err(|e| e.to_string())?)?.0 != identity(file)?.0
    {
        return Err("monitor_opened_path_identity_changed".into());
    }
    Ok(())
}

fn require_fixed_drive(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    if !crate::winutil::on_fixed_drive(path) {
        return Err("monitor_invalid_root: mounted fixed drive required".into());
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}

fn safe_ancestors(path: &Path, create: bool) -> Result<DirectoryGuard, String> {
    require_fixed_drive(path)?;
    let mut handles: Vec<PinnedDirectory> = Vec::new();
    let mut current = PathBuf::new();
    for component in path.components() {
        for parent in &handles {
            parent.validate()?;
        }
        current.push(component.as_os_str());
        // Windows' drive prefix alone is drive-relative, not a directory.
        if matches!(component, std::path::Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() || is_reparse(&metadata) {
                    return Err(format!("monitor_unsafe_ancestor: {}", current.display()));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if !create {
                    for parent in &handles {
                        parent.validate()?;
                    }
                    return Ok(DirectoryGuard {
                        _handles: handles,
                        exists: false,
                    });
                }
                fs::create_dir(&current).map_err(|e| format!("monitor_store_create: {e}"))?;
            }
            Err(e) => return Err(format!("monitor_ancestor_read: {}: {e}", current.display())),
        }
        handles.push(PinnedDirectory::open(&current)?);
        for parent in &handles {
            parent.validate()?;
        }
    }
    Ok(DirectoryGuard {
        _handles: handles,
        exists: true,
    })
}

#[cfg(windows)]
fn identity(file: &File) -> Result<(FileId, u64), String> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(|e| format!("monitor_file_identity: {e}"))?;
    Ok((
        FileId {
            volume: info.dwVolumeSerialNumber as u64,
            index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        },
        info.nNumberOfLinks as u64,
    ))
}

#[cfg(unix)]
fn identity(file: &File) -> Result<(FileId, u64), String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    Ok((
        FileId {
            volume: metadata.dev(),
            index: metadata.ino(),
        },
        metadata.nlink(),
    ))
}

#[cfg(not(any(windows, unix)))]
fn identity(_: &File) -> Result<(FileId, u64), String> {
    Err("monitor_unsupported_file_identity".into())
}

// Exact, signed nanoseconds from the OS timestamp, not milliseconds or floats.
fn modification_ns(metadata: &fs::Metadata) -> Option<String> {
    let time = metadata.modified().ok()?;
    let nanos = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i128::try_from(duration.as_nanos()).ok()?,
        Err(error) => -i128::try_from(error.duration().as_nanos()).ok()?,
    };
    Some(nanos.to_string())
}

type Stamp = (u64, Option<String>, FileId);

fn stamp(file: &File) -> Result<Stamp, String> {
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    let (id, _) = identity(file)?;
    Ok((metadata.len(), modification_ns(&metadata), id))
}

#[cfg(windows)]
fn atomic_move(source: &Path, destination: &Path, replace: bool) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let flags = if replace {
        MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH
    } else {
        MOVEFILE_WRITE_THROUGH
    };
    unsafe {
        MoveFileExW(
            &HSTRING::from(source.as_os_str()),
            &HSTRING::from(destination.as_os_str()),
            flags,
        )
    }
    .map_err(|e| format!("monitor_commit: {e}"))
}

#[cfg(not(windows))]
fn atomic_move(source: &Path, destination: &Path, replace: bool) -> Result<(), String> {
    if !replace && destination.try_exists().map_err(|e| e.to_string())? {
        return Err("monitor_record_exists".into());
    }
    fs::rename(source, destination).map_err(|e| format!("monitor_commit: {e}"))?;
    // The file was already fsynced. Sync the parent directory where supported.
    if let Some(parent) = destination.parent() {
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
    }
    Ok(())
}

fn push_warning(warnings: &mut Vec<String>, truncated: &mut bool, warning: String) {
    if warnings.iter().any(|old| old == &warning) {
        return;
    }
    if warnings.len() < MAX_WARNINGS && warning.len() <= MAX_WARNING_BYTES {
        warnings.push(warning);
    } else {
        *truncated = true;
        if !warnings.iter().any(|s| s == OMITTED_WARNINGS) {
            if warnings.len() == MAX_WARNINGS {
                warnings.pop();
            }
            warnings.push(OMITTED_WARNINGS.into());
        }
    }
}

fn file_problem(snapshot: &mut Snapshot, warning: String, truncated: bool) {
    snapshot.files_complete = false;
    snapshot.truncated |= truncated;
    push_warning(&mut snapshot.warnings, &mut snapshot.truncated, warning);
}

fn registry_problem(capture: &mut RegistryCapture, warning: String, truncated: bool) {
    capture.complete = false;
    capture.truncated |= truncated;
    push_warning(&mut capture.warnings, &mut capture.truncated, warning);
}

fn relative_file(path: &Path, root: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "monitor_file_outside_root")?;
    let mut names = Vec::new();
    for component in relative.components() {
        let name = component
            .as_os_str()
            .to_str()
            .ok_or("monitor_file_path_encoding")?;
        if !matches!(component, std::path::Component::Normal(_)) || !safe_component(name) {
            return Err("monitor_unsafe_file_component".into());
        }
        names.push(name);
    }
    let result = names.join("/");
    validate_relative(&result, &path_string(root)?)?;
    Ok(result)
}

fn validate_relative(relative: &str, root: &str) -> Result<(), String> {
    if relative.is_empty()
        || relative.len() + root.len() + 1 > MAX_PATH
        || relative.split('/').count() > MAX_DEPTH + 1
        || !relative.split('/').all(safe_component)
    {
        return Err("monitor_invalid_relative_path".into());
    }
    Ok(())
}

fn absolute_file(root: &str, relative: &str) -> String {
    let mut path = PathBuf::from(root);
    for component in relative.split('/') {
        path.push(component);
    }
    // All strings were validated as UTF-8 before persistence.
    path.to_string_lossy().into_owned()
}

// FNV-1a is a stable streaming change filter, not a cryptographic digest.
// Fingerprints are never used as authorization for deleting/restoring anything.
struct Fingerprint(u64);

impl Fingerprint {
    fn new() -> Self {
        Self(0xcbf29ce484222325)
    }
    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= *byte as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
    fn finish(self) -> String {
        format!("{:016x}", self.0)
    }
}

fn fingerprint(bytes: &[u8]) -> String {
    let mut hash = Fingerprint::new();
    hash.update(bytes);
    hash.finish()
}

struct EnumeratedEntry {
    name: std::ffi::OsString,
    directory: bool,
    reparse: bool,
    regular: bool,
    id: Option<FileId>,
}

#[cfg(windows)]
struct DirectoryEntries<'a> {
    directory: &'a PinnedDirectory,
    buffer: Box<[u64]>,
    offset: Option<usize>,
    restarted: bool,
    done: bool,
}

#[cfg(windows)]
fn directory_entries(directory: &PinnedDirectory) -> Result<DirectoryEntries<'_>, String> {
    directory.validate()?;
    Ok(DirectoryEntries {
        directory,
        buffer: vec![0u64; 8192].into_boxed_slice(),
        offset: None,
        restarted: false,
        done: false,
    })
}

#[cfg(windows)]
impl DirectoryEntries<'_> {
    fn next_entry(&mut self) -> Result<Option<EnumeratedEntry>, String> {
        use std::os::windows::ffi::OsStringExt;
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE};
        use windows::Win32::Storage::FileSystem::{
            FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, GetFileInformationByHandleEx,
            FILE_ID_BOTH_DIR_INFO,
        };
        self.directory.validate()?;
        if self.offset.is_none() {
            self.buffer.fill(0);
            let class = if self.restarted {
                FileIdBothDirectoryInfo
            } else {
                FileIdBothDirectoryRestartInfo
            };
            self.restarted = true;
            // Query the pinned object: pathname enumeration can follow an in-place junction.
            let result = unsafe {
                GetFileInformationByHandleEx(
                    HANDLE(self.directory.file.as_raw_handle()),
                    class,
                    self.buffer.as_mut_ptr().cast(),
                    (self.buffer.len() * 8) as u32,
                )
            };
            self.directory.validate()?;
            if let Err(error) = result {
                if error.code() == windows::core::HRESULT::from_win32(ERROR_NO_MORE_FILES.0) {
                    return Ok(None);
                }
                return Err(format!("monitor_directory_query: {error}"));
            }
            self.offset = Some(0);
        }
        let offset = self.offset.unwrap();
        let data = unsafe {
            std::slice::from_raw_parts(self.buffer.as_ptr().cast::<u8>(), self.buffer.len() * 8)
        };
        let header = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
        let record = data.get(offset..).ok_or("monitor_directory_record")?;
        if record.len() < header {
            return Err("monitor_directory_record".into());
        }
        let u32_at = |offset: usize| {
            u32::from_le_bytes(record[offset..offset + 4].try_into().unwrap()) as usize
        };
        let next = u32_at(std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, NextEntryOffset));
        let length = u32_at(std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength));
        let attributes = u32_at(std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileAttributes));
        if length == 0
            || length % 2 != 0
            || length > MAX_PATH * 2
            || header + length > record.len()
            || (next != 0
                && (next % 8 != 0
                    || next < header + length
                    || next > record.len().saturating_sub(header)))
        {
            return Err("monitor_directory_record".into());
        }
        let name: Vec<u16> = record[header..header + length]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let id_offset = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileId);
        let index = u64::from_le_bytes(record[id_offset..id_offset + 8].try_into().unwrap());
        self.offset = if next == 0 { None } else { Some(offset + next) };
        Ok(Some(EnumeratedEntry {
            name: std::ffi::OsString::from_wide(&name),
            directory: attributes & 0x10 != 0,
            reparse: attributes & 0x400 != 0,
            regular: attributes & 0x10 == 0,
            id: Some(FileId {
                volume: self.directory.id.volume,
                index,
            }),
        }))
    }
}

#[cfg(windows)]
impl Iterator for DirectoryEntries<'_> {
    type Item = Result<EnumeratedEntry, String>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.done {
            match self.next_entry() {
                Ok(Some(entry)) if entry.name == "." || entry.name == ".." => continue,
                Ok(Some(entry)) => return Some(Ok(entry)),
                Ok(None) => self.done = true,
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
        None
    }
}

#[cfg(not(windows))]
fn directory_entries(
    directory: &PinnedDirectory,
) -> Result<impl Iterator<Item = Result<EnumeratedEntry, String>>, String> {
    directory.validate()?;
    // Path-based enumeration exists only for owned non-Windows temp fixtures.
    Ok(fs::read_dir(&directory.path)
        .map_err(|e| e.to_string())?
        .map(|item| {
            let item = item.map_err(|e| e.to_string())?;
            let kind = item.file_type().map_err(|e| e.to_string())?;
            Ok(EnumeratedEntry {
                name: item.file_name(),
                directory: kind.is_dir(),
                reparse: kind.is_symlink(),
                regular: kind.is_file(),
                id: None,
            })
        }))
}

fn check_entry_identity(file: &File, expected: Option<&FileId>) -> Result<(), String> {
    if expected.is_some_and(|id| identity(file).map_or(true, |actual| &actual.0 != id)) {
        return Err("monitor_directory_entry_identity_changed".into());
    }
    Ok(())
}

struct FileBudget {
    entries: usize,
    directories: usize,
    bytes: u64,
    inventory_bytes: usize,
    stopped: bool,
}

fn read_file(
    path: &Path,
    relative: String,
    mut file: File,
    parents: &[&PinnedDirectory],
    budget: &mut FileBudget,
    cancel: &AtomicBool,
) -> Result<FileEntry, String> {
    validate_parents(parents)?;
    check_opened(path, &file, false)?;
    let before = stamp(&file)?;
    if before.0 > MAX_FILE_BYTES || before.0 > MAX_TOTAL_FILE_BYTES.saturating_sub(budget.bytes) {
        return Err("monitor_file_read_limit".into());
    }
    let mut hash = Fingerprint::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    // Read exactly the bounded size observed on the opened handle. A growth or
    // short read is detected below, without an unbudgeted extra byte read.
    while bytes < before.0 {
        cancelled(cancel)?;
        validate_parents(parents)?;
        check_opened(path, &file, false)?;
        let capacity = buffer.len().min((before.0 - bytes) as usize);
        let length = file
            .read(&mut buffer[..capacity])
            .map_err(|e| format!("monitor_file_read: {e}"))?;
        if length == 0 {
            break;
        }
        bytes += length as u64;
        budget.bytes += length as u64;
        hash.update(&buffer[..length]);
    }
    validate_parents(parents)?;
    check_opened(path, &file, false)?;
    let after = stamp(&file)?;
    if identity(&file)?.1 != 1 {
        return Err("monitor_unsafe_path: hard links are not exclusive".into());
    }
    if before != after || bytes != before.0 {
        return Err("monitor_file_changed_during_read".into());
    }
    Ok(FileEntry {
        path: relative,
        size: bytes,
        modified_ns: before.1,
        file_id: Some(before.2),
        fingerprint: hash.finish(),
    })
}

fn walk_files(
    directory: &Path,
    parents: &[&PinnedDirectory],
    root: &Path,
    depth: usize,
    snapshot: &mut Snapshot,
    budget: &mut FileBudget,
    cancel: &AtomicBool,
    progress: &dyn Fn(&Path, usize),
) -> Result<(), String> {
    cancelled(cancel)?;
    if depth > MAX_DEPTH || budget.directories >= MAX_DIRECTORIES {
        file_problem(
            snapshot,
            format!("Directory/depth limit; omitted {}", directory.display()),
            true,
        );
        return Ok(());
    }
    budget.directories += 1;
    if let Err(e) = validate_parents(parents) {
        file_problem(snapshot, format!("Changed directory scope: {e}"), false);
        budget.stopped = true;
        return Ok(());
    }
    let mut items = match directory_entries(parents.last().ok_or("monitor_directory_guard")?) {
        Ok(items) => items,
        Err(e) => {
            file_problem(
                snapshot,
                format!("Cannot enumerate {}: {e}", directory.display()),
                false,
            );
            return Ok(());
        }
    };
    while !budget.stopped {
        cancelled(cancel)?;
        if let Err(e) = validate_parents(parents) {
            file_problem(snapshot, format!("Changed directory scope: {e}"), false);
            budget.stopped = true;
            break;
        }
        if budget.entries >= MAX_ENTRIES || snapshot.files.len() >= MAX_FILES {
            file_problem(
                snapshot,
                "File/entry limit; remaining selected tree omitted.".into(),
                true,
            );
            budget.stopped = true;
            break;
        }
        let next = items.next();
        cancelled(cancel)?;
        if let Err(e) = validate_parents(parents) {
            file_problem(snapshot, format!("Changed directory scope: {e}"), false);
            budget.stopped = true;
            break;
        }
        let Some(item) = next else { break };
        budget.entries += 1;
        let item = match item {
            Ok(item) => item,
            Err(e) => {
                file_problem(
                    snapshot,
                    format!(
                        "Late directory entry read failure in {}: {e}",
                        directory.display()
                    ),
                    false,
                );
                break;
            }
        };
        let path = directory.join(&item.name);
        let relative = match relative_file(&path, root) {
            Ok(relative) => relative,
            Err(e) => {
                file_problem(
                    snapshot,
                    format!("Omitted unsafe/long entry in {}: {e}", directory.display()),
                    true,
                );
                continue;
            }
        };
        if item.reparse {
            file_problem(
                snapshot,
                format!("Omitted reparse/symlink entry: {}", path.display()),
                false,
            );
            continue;
        }
        if item.directory {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(protected_component)
            {
                file_problem(
                    snapshot,
                    format!(
                        "Omitted protected/shared/TP backup directory: {}",
                        path.display()
                    ),
                    false,
                );
                continue;
            }
            match PinnedDirectory::open(&path).and_then(|guard| {
                check_entry_identity(&guard.file, item.id.as_ref())?;
                validate_parents(parents)?;
                Ok(guard)
            }) {
                Ok(guard) => {
                    let mut child_parents = parents.to_vec();
                    child_parents.push(&guard);
                    progress(&path, budget.entries);
                    walk_files(
                        &path,
                        &child_parents,
                        root,
                        depth + 1,
                        snapshot,
                        budget,
                        cancel,
                        progress,
                    )?;
                }
                Err(e) => file_problem(
                    snapshot,
                    format!("Omitted directory {}: {e}", path.display()),
                    false,
                ),
            }
        } else if item.regular {
            let opened = open_checked(&path, false).and_then(|file| {
                check_entry_identity(&file, item.id.as_ref())?;
                validate_parents(parents)?;
                Ok(file)
            });
            let result = opened.and_then(|file| {
                progress(&path, budget.entries);
                cancelled(cancel)?;
                read_file(&path, relative, file, parents, budget, cancel)
            });
            match result {
                Ok(entry) => {
                    let size = serde_json::to_vec(&entry).map_err(|e| e.to_string())?.len() + 1;
                    if size + budget.inventory_bytes > MAX_INVENTORY_BYTES {
                        file_problem(
                            snapshot,
                            "File inventory record-size limit; remaining tree omitted.".into(),
                            true,
                        );
                        budget.stopped = true;
                        continue;
                    }
                    budget.inventory_bytes += size;
                    if entry.modified_ns.is_none() {
                        file_problem(
                            snapshot,
                            format!(
                                "Exact modification timestamp unavailable: {}",
                                path.display()
                            ),
                            false,
                        );
                    }
                    snapshot.files.push(entry);
                }
                Err(e) if e == "monitor_cancelled" => return Err(e),
                Err(e) => {
                    let limited = e == "monitor_file_read_limit";
                    file_problem(
                        snapshot,
                        format!("Omitted file {}: {e}", path.display()),
                        limited,
                    );
                }
            }
        } else {
            file_problem(
                snapshot,
                format!("Omitted non-regular entry: {}", path.display()),
                false,
            );
        }
    }
    Ok(())
}

fn capture(
    root: &Path,
    vendor: &str,
    cancel: &AtomicBool,
    progress: &dyn Fn(&Path, usize),
    inventory: &dyn Inventory,
) -> Result<Snapshot, String> {
    cancelled(cancel)?;
    let _ancestors = safe_ancestors(root, false)?;
    let mut snapshot = Snapshot {
        files: Vec::new(),
        registry: Vec::new(),
        files_complete: true,
        registry_complete: true,
        truncated: false,
        warnings: Vec::new(),
    };
    progress(root, 0);
    cancelled(cancel)?;
    let parents: Vec<_> = _ancestors._handles.iter().collect();
    if let Err(e) = validate_parents(&parents) {
        file_problem(
            &mut snapshot,
            format!("Changed directory scope: {e}"),
            false,
        );
    } else if _ancestors.exists {
        walk_files(
            root,
            &parents,
            root,
            0,
            &mut snapshot,
            &mut FileBudget {
                entries: 0,
                directories: 0,
                bytes: 0,
                inventory_bytes: 0,
                stopped: false,
            },
            cancel,
            progress,
        )?;
    }
    // The missing target was proven through safe, pinned existing ancestors;
    // no installer folder is created by taking the before snapshot.
    cancelled(cancel)?;
    let registry = inventory.capture(vendor, cancel)?;
    cancelled(cancel)?;
    snapshot.registry = registry.entries;
    snapshot.registry_complete = registry.complete;
    snapshot.truncated |= registry.truncated;
    for warning in registry.warnings {
        push_warning(&mut snapshot.warnings, &mut snapshot.truncated, warning);
    }
    snapshot
        .files
        .sort_by_key(|entry| entry.path.to_lowercase());
    let mut unique = BTreeSet::new();
    let files = std::mem::take(&mut snapshot.files);
    for entry in files {
        if unique.insert(entry.path.to_lowercase()) {
            snapshot.files.push(entry);
        } else {
            file_problem(
                &mut snapshot,
                "Ambiguous case-insensitive file path omitted; selected tree is incomplete.".into(),
                false,
            );
        }
    }
    snapshot.registry.sort_by_key(registry_identity);
    cancelled(cancel)?;
    Ok(snapshot)
}

fn registry_identity(entry: &RegistryEntry) -> (String, String) {
    (entry.path.to_lowercase(), entry.value_name.to_lowercase())
}

struct Scope {
    hive: &'static str,
    path: String,
    depth: usize,
}

fn scopes(vendor: &str) -> Vec<Scope> {
    let mut result = Vec::new();
    for hive in ["HKCU", "HKLM64", "HKLM32"] {
        for (path, depth) in [
            ("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall", 8),
            ("Software\\Microsoft\\Windows\\CurrentVersion\\App Paths", 1),
            ("Software\\Microsoft\\Windows\\CurrentVersion\\Run", 0),
        ] {
            result.push(Scope {
                hive,
                path: path.into(),
                depth,
            });
        }
        if !vendor.is_empty() {
            result.push(Scope {
                hive,
                path: format!("Software\\{vendor}"),
                depth: 12,
            });
        }
    }
    result
}

fn valid_registry_component(name: &str) -> bool {
    safe_text(name, 255 * 4, false)
        && name.encode_utf16().count() <= 255
        && name != "."
        && name != ".."
        && !name.contains(['\\', '/'])
}

fn validate_registry_path(path: &str, vendor: &str) -> bool {
    if !safe_text(path, MAX_PATH, false) {
        return false;
    }
    scopes(vendor).iter().any(|scope| {
        let prefix = format!("{}\\{}", scope.hive, scope.path);
        if path == prefix {
            return true;
        }
        path.strip_prefix(&(prefix + "\\")).is_some_and(|tail| {
            tail.split('\\').count() <= scope.depth
                && tail.split('\\').all(valid_registry_component)
        })
    })
}

fn validate_fingerprint(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_snapshot(snapshot: &Snapshot, session: &Summary) -> Result<(), String> {
    if snapshot.files.len() > MAX_FILES
        || snapshot.registry.len() > MAX_REG_VALUES
        || snapshot.warnings.len() > MAX_WARNINGS
        || snapshot
            .warnings
            .iter()
            .any(|w| !safe_text(w, MAX_WARNING_BYTES, false))
        || (snapshot.truncated && snapshot.files_complete && snapshot.registry_complete)
        || ((!snapshot.files_complete || !snapshot.registry_complete || snapshot.truncated)
            && snapshot.warnings.is_empty())
    {
        return Err("invalid snapshot bounds/completeness".into());
    }
    let mut files = BTreeSet::new();
    let mut size = 0usize;
    let mut total_file_bytes = 0u64;
    for entry in &snapshot.files {
        total_file_bytes = total_file_bytes
            .checked_add(entry.size)
            .ok_or("file byte count overflow")?;
        validate_relative(&entry.path, &session.root)?;
        if entry.size > MAX_FILE_BYTES
            || !validate_fingerprint(&entry.fingerprint)
            || !files.insert(entry.path.to_lowercase())
        {
            return Err("invalid/duplicate file record".into());
        }
        if let Some(nanos) = &entry.modified_ns {
            if nanos.len() > 40
                || nanos
                    .parse::<i128>()
                    .ok()
                    .is_none_or(|n| n.to_string() != *nanos)
            {
                return Err("invalid modification nanoseconds".into());
            }
        }
        if snapshot.files_complete && (entry.modified_ns.is_none() || entry.file_id.is_none()) {
            return Err("complete snapshot lacks file metadata".into());
        }
        size += serde_json::to_vec(entry).map_err(|e| e.to_string())?.len() + 1;
    }
    if size > MAX_INVENTORY_BYTES || total_file_bytes > MAX_TOTAL_FILE_BYTES {
        return Err("file inventory size/byte limit".into());
    }
    let mut registry = BTreeSet::new();
    let mut total_registry_bytes = 0u64;
    size = 0;
    for entry in &snapshot.registry {
        total_registry_bytes = total_registry_bytes
            .checked_add(entry.size)
            .ok_or("registry byte count overflow")?;
        if !validate_registry_path(&entry.path, &session.vendor)
            || !safe_text(&entry.value_name, MAX_VALUE_NAME, true)
            || entry.value_type > 11
            || entry.value_type == 6
            || entry.size > MAX_REG_VALUE_BYTES as u64
            || !validate_fingerprint(&entry.fingerprint)
            || !registry.insert(registry_identity(entry))
        {
            return Err("invalid/duplicate/out-of-scope registry record".into());
        }
        size += serde_json::to_vec(entry).map_err(|e| e.to_string())?.len() + 1;
    }
    if size > MAX_INVENTORY_BYTES || total_registry_bytes > MAX_TOTAL_REG_BYTES as u64 {
        return Err("registry inventory size/byte limit".into());
    }
    Ok(())
}

fn differences(root: &str, before: &Snapshot, after: &Snapshot) -> (Vec<Change>, bool) {
    let mut changes = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    let mut add = |change: Change| {
        let size = serde_json::to_vec(&change)
            .expect("owned change serializes")
            .len()
            + 1;
        if changes.len() >= MAX_CHANGES || bytes + size > MAX_CHANGE_BYTES {
            truncated = true;
        } else {
            bytes += size;
            changes.push(change);
        }
    };
    let old: BTreeMap<_, _> = before
        .files
        .iter()
        .map(|entry| (entry.path.to_lowercase(), entry))
        .collect();
    let new: BTreeMap<_, _> = after
        .files
        .iter()
        .map(|entry| (entry.path.to_lowercase(), entry))
        .collect();
    let keys: BTreeSet<_> = old.keys().chain(new.keys()).cloned().collect();
    for key in keys {
        let result = match (old.get(&key), new.get(&key)) {
            (None, Some(entry)) if before.files_complete => Some(("added", *entry)),
            (Some(entry), None) if after.files_complete => Some(("removed", *entry)),
            (Some(old), Some(new)) if *old != *new => Some(("modified", *new)),
            _ => None,
        };
        if let Some((change, entry)) = result {
            add(Change {
                kind: "file".into(),
                change: change.into(),
                path: absolute_file(root, &entry.path),
                value_name: None,
            });
        }
    }
    let old: BTreeMap<_, _> = before
        .registry
        .iter()
        .map(|entry| (registry_identity(entry), entry))
        .collect();
    let new: BTreeMap<_, _> = after
        .registry
        .iter()
        .map(|entry| (registry_identity(entry), entry))
        .collect();
    let keys: BTreeSet<_> = old.keys().chain(new.keys()).cloned().collect();
    for key in keys {
        let result = match (old.get(&key), new.get(&key)) {
            (None, Some(entry)) if before.registry_complete => Some(("added", *entry)),
            (Some(entry), None) if after.registry_complete => Some(("removed", *entry)),
            (Some(old), Some(new)) if *old != *new => Some(("modified", *new)),
            _ => None,
        };
        if let Some((change, entry)) = result {
            add(Change {
                kind: "registry".into(),
                change: change.into(),
                path: entry.path.clone(),
                value_name: Some(entry.value_name.clone()),
            });
        }
    }
    (changes, truncated)
}

fn make_report(mut session: Summary, before: &Snapshot, after: Option<&Snapshot>) -> Report {
    let (changes, diff_truncated) = after.map_or_else(
        || (Vec::new(), false),
        |after| differences(&session.root, before, after),
    );
    let truncated = before.truncated || after.is_some_and(|s| s.truncated) || diff_truncated;
    session.files_before = before.files.len() as u64;
    session.change_count = changes.len() as u64;
    session.complete = after.is_some_and(|s| {
        before.files_complete && before.registry_complete && s.files_complete && s.registry_complete
    }) && !truncated;
    let mut warnings = vec![
        ATTRIBUTION_WARNING.into(),
        FILE_SCOPE_WARNING.into(),
        REG_SCOPE_WARNING.into(),
    ];
    for warning in before
        .warnings
        .iter()
        .chain(after.into_iter().flat_map(|s| s.warnings.iter()))
    {
        if !warnings.contains(warning) {
            warnings.push(warning.clone());
        }
    }
    if !before.files_complete
        || !before.registry_complete
        || after.is_some_and(|s| !s.files_complete || !s.registry_complete)
    {
        warnings.push(INCOMPLETE_WARNING.into());
    }
    if diff_truncated {
        warnings.push(DIFF_WARNING.into());
    }
    Report {
        session,
        changes,
        warnings,
        truncated,
    }
}

#[cfg(not(windows))]
fn native_registry(_: &str, cancel: &AtomicBool) -> Result<RegistryCapture, String> {
    cancelled(cancel)?;
    Ok(RegistryCapture {
        entries: Vec::new(), complete: false, truncated: false,
        warnings: vec!["Windows registry is unavailable on this portable fixture platform; registry inventory is incomplete.".into()],
    })
}

#[cfg(windows)]
fn native_registry(vendor: &str, cancel: &AtomicBool) -> Result<RegistryCapture, String> {
    windows_registry::capture(vendor, cancel)
}

#[cfg(windows)]
mod windows_registry {
    use super::*;
    use windows::core::{HSTRING, PWSTR};
    use windows::Win32::Foundation::{FILETIME, WIN32_ERROR};
    use windows::Win32::System::Registry::*;

    const SUCCESS: WIN32_ERROR = WIN32_ERROR(0);
    const NOT_FOUND: WIN32_ERROR = WIN32_ERROR(2);
    const MORE_DATA: WIN32_ERROR = WIN32_ERROR(234);
    const NO_MORE: WIN32_ERROR = WIN32_ERROR(259);

    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    struct Budget {
        keys: usize,
        enumerated_keys: usize,
        values: usize,
        bytes: usize,
        inventory_bytes: usize,
        stopped: bool,
    }

    // OPEN_LINK prevents following registry symbolic links. Only the type of
    // SymbolicLinkValue is queried; its target bytes are never read.
    fn open(parent: HKEY, component: &str, view: REG_SAM_FLAGS) -> Result<Option<Key>, String> {
        let mut raw = HKEY::default();
        let result = unsafe {
            RegOpenKeyExW(
                parent,
                &HSTRING::from(component),
                Some(8),
                KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS | view,
                &mut raw,
            )
        };
        if result == NOT_FOUND {
            return Ok(None);
        }
        if result != SUCCESS {
            return Err(format!("registry open error {}", result.0));
        }
        let key = Key(raw);
        let mut value_type = REG_VALUE_TYPE(0);
        let mut size = 0u32;
        let result = unsafe {
            RegQueryValueExW(
                key.0,
                &HSTRING::from("SymbolicLinkValue"),
                None,
                Some(&mut value_type),
                None,
                Some(&mut size),
            )
        };
        if (result == SUCCESS || result == MORE_DATA) && value_type == REG_LINK {
            return Err("registry symbolic link omitted".into());
        }
        if result != SUCCESS && result != NOT_FOUND && result != MORE_DATA {
            return Err(format!("registry link-check error {}", result.0));
        }
        Ok(Some(key))
    }

    fn key_stamp(key: &Key) -> Result<(u32, u32, u64), String> {
        let mut subkeys = 0;
        let mut values = 0;
        let mut time = FILETIME::default();
        let result = unsafe {
            RegQueryInfoKeyW(
                key.0,
                None,
                None,
                None,
                Some(&mut subkeys),
                None,
                None,
                Some(&mut values),
                None,
                None,
                None,
                Some(&mut time),
            )
        };
        if result != SUCCESS {
            return Err(format!("registry metadata error {}", result.0));
        }
        Ok((
            subkeys,
            values,
            ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64,
        ))
    }

    pub(super) fn capture(vendor: &str, cancel: &AtomicBool) -> Result<RegistryCapture, String> {
        let mut capture = RegistryCapture {
            entries: Vec::new(),
            complete: true,
            truncated: false,
            warnings: Vec::new(),
        };
        let mut budget = Budget {
            keys: 0,
            enumerated_keys: 0,
            values: 0,
            bytes: 0,
            inventory_bytes: 0,
            stopped: false,
        };
        for scope in scopes(vendor) {
            cancelled(cancel)?;
            if budget.stopped {
                break;
            }
            let (hive, view) = match scope.hive {
                "HKCU" => (HKEY_CURRENT_USER, KEY_WOW64_64KEY),
                "HKLM64" => (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY),
                _ => (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY),
            };
            // Open every fixed ancestor without following links, but do not
            // enumerate Software/Microsoft/Windows or any other ancestor.
            let mut chain: Vec<Key> = Vec::new();
            let mut missing = false;
            for component in scope.path.split('\\') {
                let parent = chain.last().map_or(hive, |key| key.0);
                match open(parent, component, view) {
                    Ok(Some(key)) => chain.push(key),
                    Ok(None) => {
                        missing = true;
                        break;
                    }
                    Err(e) => {
                        registry_problem(
                            &mut capture,
                            format!("Omitted {}\\{}: {e}", scope.hive, scope.path),
                            false,
                        );
                        missing = true;
                        break;
                    }
                }
            }
            if missing {
                continue;
            }
            if let Some(key) = chain.last() {
                walk(
                    key,
                    &format!("{}\\{}", scope.hive, scope.path),
                    scope.depth,
                    view,
                    &mut capture,
                    &mut budget,
                    cancel,
                )?;
            }
        }
        capture.entries.sort_by_key(registry_identity);
        // Enumeration races can produce duplicates. Never persist ambiguous
        // entries or silently treat their omitted partners as missing values.
        let mut unique = BTreeSet::new();
        let old = std::mem::take(&mut capture.entries);
        for entry in old {
            if unique.insert(registry_identity(&entry)) {
                capture.entries.push(entry);
            } else {
                registry_problem(
                    &mut capture,
                    "Registry enumeration changed: duplicate value omitted.".into(),
                    false,
                );
            }
        }
        cancelled(cancel)?;
        Ok(capture)
    }

    fn walk(
        key: &Key,
        path: &str,
        depth_left: usize,
        view: REG_SAM_FLAGS,
        capture: &mut RegistryCapture,
        budget: &mut Budget,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        cancelled(cancel)?;
        if budget.keys >= MAX_KEYS {
            registry_problem(
                capture,
                "Registry key limit; remaining scope omitted.".into(),
                true,
            );
            budget.stopped = true;
            return Ok(());
        }
        budget.keys += 1;
        let before = match key_stamp(key) {
            Ok(stamp) => stamp,
            Err(e) => {
                registry_problem(capture, format!("Cannot read {path}: {e}"), false);
                return Ok(());
            }
        };
        let mut value_index = 0u32;
        let mut data = vec![0u8; MAX_REG_VALUE_BYTES];
        loop {
            cancelled(cancel)?;
            if budget.values >= MAX_REG_VALUES {
                registry_problem(
                    capture,
                    "Registry value limit; remaining scope omitted.".into(),
                    true,
                );
                budget.stopped = true;
                break;
            }
            let capacity = data
                .len()
                .min(MAX_TOTAL_REG_BYTES.saturating_sub(budget.bytes));
            if capacity == 0 {
                registry_problem(
                    capture,
                    "Registry byte-read limit; remaining scope omitted.".into(),
                    true,
                );
                budget.stopped = true;
                break;
            }
            let mut name = [0u16; MAX_VALUE_NAME + 1];
            let mut name_length = name.len() as u32;
            let mut data_length = capacity as u32;
            let mut value_type = 0u32;
            let result = unsafe {
                RegEnumValueW(
                    key.0,
                    value_index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut name_length,
                    None,
                    Some(&mut value_type),
                    Some(data.as_mut_ptr()),
                    Some(&mut data_length),
                )
            };
            if result == NO_MORE {
                break;
            }
            budget.values += 1;
            value_index += 1;
            if result == SUCCESS
                && (data_length as usize > capacity || name_length as usize >= name.len())
            {
                data.fill(0);
                registry_problem(
                    capture,
                    format!("Invalid registry enumeration lengths in {path}"),
                    false,
                );
                break;
            }
            // Charge possible partial reads on errors too. Never let repeated
            // oversized or unreadable values evade the global byte-read limit.
            budget.bytes += if result == SUCCESS {
                data_length as usize
            } else {
                capacity
            };
            if result != SUCCESS {
                data.fill(0);
                registry_problem(
                    capture,
                    format!(
                        "Omitted value in {path}: registry enumeration error {}",
                        result.0
                    ),
                    result == MORE_DATA,
                );
                if result != MORE_DATA {
                    break;
                }
                continue;
            }
            let name = match String::from_utf16(&name[..name_length as usize]) {
                Ok(name) if safe_text(&name, MAX_VALUE_NAME, true) => name,
                _ => {
                    data.fill(0);
                    registry_problem(
                        capture,
                        format!("Omitted invalid/long value name in {path}"),
                        true,
                    );
                    continue;
                }
            };
            let length = data_length as usize;
            if value_type > 11 || value_type == 6 {
                data.fill(0);
                registry_problem(
                    capture,
                    format!("Omitted unsupported/link registry value in {path}"),
                    false,
                );
                continue;
            }
            let entry = RegistryEntry {
                path: path.into(),
                value_name: name,
                value_type,
                size: length as u64,
                fingerprint: fingerprint(&data[..length]),
            };
            // Raw secret/license/token bytes are discarded, never serialized or
            // included in error strings. This is not a cryptographic erasure claim.
            data.fill(0);
            let size = serde_json::to_vec(&entry).map_err(|e| e.to_string())?.len() + 1;
            if budget.inventory_bytes + size > MAX_INVENTORY_BYTES {
                registry_problem(
                    capture,
                    "Registry inventory record-size limit; remaining scope omitted.".into(),
                    true,
                );
                budget.stopped = true;
                break;
            }
            budget.inventory_bytes += size;
            capture.entries.push(entry);
        }
        if !budget.stopped && before.0 > 0 {
            if depth_left == 0 {
                registry_problem(
                    capture,
                    format!("Registry depth limit; child keys omitted in {path}"),
                    true,
                );
            } else {
                let mut index = 0u32;
                loop {
                    cancelled(cancel)?;
                    if budget.stopped {
                        break;
                    }
                    if budget.keys >= MAX_KEYS || budget.enumerated_keys >= MAX_KEYS {
                        registry_problem(
                            capture,
                            "Registry key enumeration limit; remaining scope omitted.".into(),
                            true,
                        );
                        budget.stopped = true;
                        break;
                    }
                    let mut name = [0u16; 256];
                    let mut length = name.len() as u32;
                    let result = unsafe {
                        RegEnumKeyExW(
                            key.0,
                            index,
                            Some(PWSTR(name.as_mut_ptr())),
                            &mut length,
                            None,
                            None,
                            None,
                            None,
                        )
                    };
                    if result == NO_MORE {
                        break;
                    }
                    index += 1;
                    budget.enumerated_keys += 1;
                    if result != SUCCESS {
                        registry_problem(
                            capture,
                            format!("Late key enumeration failure in {path}: {}", result.0),
                            result == MORE_DATA,
                        );
                        break;
                    }
                    let component = match String::from_utf16(&name[..length as usize]) {
                        Ok(name) if valid_registry_component(&name) => name,
                        _ => {
                            registry_problem(
                                capture,
                                format!("Invalid child key omitted in {path}"),
                                true,
                            );
                            continue;
                        }
                    };
                    let child_path = format!("{path}\\{component}");
                    if child_path.len() > MAX_PATH {
                        registry_problem(
                            capture,
                            format!("Long registry child path omitted in {path}"),
                            true,
                        );
                        continue;
                    }
                    match open(key.0, &component, view) {
                        Ok(Some(child)) => walk(
                            &child,
                            &child_path,
                            depth_left - 1,
                            view,
                            capture,
                            budget,
                            cancel,
                        )?,
                        Ok(None) => registry_problem(
                            capture,
                            format!("Late registry read: enumerated key disappeared: {child_path}"),
                            false,
                        ),
                        Err(e) => {
                            registry_problem(capture, format!("Omitted {child_path}: {e}"), false)
                        }
                    }
                }
            }
        }
        match key_stamp(key) {
            Ok(after) if after == before => {}
            _ => registry_problem(
                capture,
                format!("Registry key changed or became unreadable during capture: {path}"),
                false,
            ),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        base: PathBuf,
        store: PathBuf,
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let base = fixture_temp_root().join(format!(
                "install-monitor-fixture-{}-{}-{}",
                std::process::id(),
                now_id(),
                NONCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&base).unwrap();
            Self {
                store: base.join("history"),
                root: base.join("ExclusiveApp"),
                base,
            }
        }
        fn monitor(&self) -> Monitor {
            Monitor::at(self.store.clone())
        }
        fn file(&self, name: &str, bytes: &[u8]) {
            fs::create_dir_all(&self.root).unwrap();
            fs::write(self.root.join(name), bytes).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    struct FakeRegistry {
        entries: Vec<RegistryEntry>,
        complete: bool,
        truncated: bool,
        cancel_during: bool,
        fail: bool,
    }

    impl FakeRegistry {
        fn empty() -> Self {
            Self {
                entries: Vec::new(),
                complete: true,
                truncated: false,
                cancel_during: false,
                fail: false,
            }
        }
        fn values(values: &[(&str, &[u8])]) -> Self {
            let mut result = Self::empty();
            result.entries = values
                .iter()
                .map(|(name, bytes)| RegistryEntry {
                    path: "HKCU\\Software\\AcmeDedicated".into(),
                    value_name: (*name).into(),
                    value_type: 3,
                    size: bytes.len() as u64,
                    fingerprint: fingerprint(bytes),
                })
                .collect();
            result
        }
    }

    impl Inventory for FakeRegistry {
        fn capture(&self, _: &str, cancel: &AtomicBool) -> Result<RegistryCapture, String> {
            if self.cancel_during {
                cancel.store(true, Ordering::Release);
            }
            if self.fail {
                return Err("fixture_inventory_failure".into());
            }
            Ok(RegistryCapture {
                entries: self.entries.clone(),
                complete: self.complete,
                truncated: self.truncated,
                warnings: if self.complete && !self.truncated {
                    Vec::new()
                } else {
                    vec!["Injected omitted/late registry reads.".into()]
                },
            })
        }
    }

    fn begin(f: &Fixture, registry: &FakeRegistry) -> Summary {
        f.monitor()
            .begin_with(
                "Fixture App",
                &f.root,
                "AcmeDedicated",
                &AtomicBool::new(false),
                &|_, _| {},
                registry,
            )
            .unwrap()
    }

    fn finish(f: &Fixture, id: u64, registry: &FakeRegistry) -> Report {
        f.monitor()
            .finish_with(id, &AtomicBool::new(false), &|_, _| {}, registry)
            .unwrap()
    }

    // Tests using the public API are serialized with this independent mutex:
    // STORE_MUTEX deliberately uses try_lock rather than blocking callbacks.
    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn missing_before_restart_roundtrip_add_modify_remove_and_history() {
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        assert!(f.monitor().list().unwrap().is_empty());
        assert!(!f.store.exists());
        let first = begin(&f, &FakeRegistry::empty());
        assert_eq!(first.files_before, 0);
        assert!(!f.root.exists());
        assert_eq!(
            f.monitor().detail(first.id).unwrap_err(),
            "monitor_not_finished"
        );
        assert_eq!(f.monitor().list().unwrap(), vec![first.clone()]);
        assert_eq!(
            f.monitor()
                .begin_with(
                    "Another",
                    &f.root,
                    "",
                    &AtomicBool::new(false),
                    &|_, _| {},
                    &FakeRegistry::empty()
                )
                .unwrap_err(),
            "monitor_active"
        );
        f.file("created.bin", b"first");
        let report = finish(
            &f,
            first.id,
            &FakeRegistry::values(&[("License", b"secret-not-persisted")]),
        );
        assert!(report.session.complete);
        assert_eq!(report.changes.len(), 2);
        assert!(report
            .changes
            .iter()
            .any(|c| c.kind == "file" && c.change == "added"));
        let reopened = Monitor::at(f.store.clone());
        assert_eq!(reopened.detail(first.id).unwrap(), report);
        let disk = fs::read(reopened.record_path(first.id)).unwrap();
        assert!(!String::from_utf8_lossy(&disk).contains("secret-not-persisted"));
        let record: Record = serde_json::from_slice(&disk).unwrap();
        assert_eq!(
            serde_json::from_slice::<Record>(&serde_json::to_vec(&record).unwrap()).unwrap(),
            record
        );
        f.file("deleted.bin", b"remove me");
        let second = begin(
            &f,
            &FakeRegistry::values(&[("same", b"before"), ("gone", b"old")]),
        );
        f.file("created.bin", b"different length and data");
        f.file("added.bin", b"new");
        fs::remove_file(f.root.join("deleted.bin")).unwrap();
        let report = finish(
            &f,
            second.id,
            &FakeRegistry::values(&[("same", b"after"), ("new", b"added")]),
        );
        for kind in ["file", "registry"] {
            for change in ["added", "modified", "removed"] {
                assert!(report
                    .changes
                    .iter()
                    .any(|c| c.kind == kind && c.change == change));
            }
        }
        let list = f.monitor().list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, second.id);
        assert!(list.iter().all(|s| s.finished_at.is_some()));
    }

    #[test]
    fn no_change_and_exact_metadata() {
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        f.file("same", b"unchanged");
        let summary = begin(&f, &FakeRegistry::empty());
        let record: Record =
            serde_json::from_slice(&fs::read(f.monitor().record_path(summary.id)).unwrap())
                .unwrap();
        let entry = &record.before.files[0];
        assert_eq!(
            entry.modified_ns,
            modification_ns(&fs::metadata(f.root.join("same")).unwrap())
        );
        assert!(entry.file_id.is_some());
        let report = finish(&f, summary.id, &FakeRegistry::empty());
        assert!(report.changes.is_empty());
        assert!(report.session.complete);
        assert!(!report.truncated);
    }

    #[test]
    fn cancel_begin_finish_and_inventory_errors_preserve_baseline() {
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        f.file("one", b"baseline");
        let cancel = AtomicBool::new(false);
        let result = f.monitor().begin_with(
            "Fixture",
            &f.root,
            "",
            &cancel,
            &|_, _| {
                cancel.store(true, Ordering::Release);
            },
            &FakeRegistry::empty(),
        );
        assert_eq!(result.unwrap_err(), "monitor_cancelled");
        assert!(f.monitor().list().unwrap().is_empty());
        let mut registry = FakeRegistry::empty();
        registry.cancel_during = true;
        cancel.store(false, Ordering::Release);
        assert_eq!(
            f.monitor()
                .begin_with("Fixture", &f.root, "", &cancel, &|_, _| {}, &registry)
                .unwrap_err(),
            "monitor_cancelled"
        );
        let summary = begin(&f, &FakeRegistry::empty());
        let original = fs::read(f.monitor().record_path(summary.id)).unwrap();
        f.file("one", b"changed");
        for during_registry in [false, true] {
            cancel.store(false, Ordering::Release);
            registry.cancel_during = during_registry;
            let result = f.monitor().finish_with(
                summary.id,
                &cancel,
                &|_, count| {
                    if !during_registry && count > 0 {
                        cancel.store(true, Ordering::Release);
                    }
                },
                &registry,
            );
            assert_eq!(result.unwrap_err(), "monitor_cancelled");
            assert_eq!(
                fs::read(f.monitor().record_path(summary.id)).unwrap(),
                original
            );
        }
        registry.cancel_during = false;
        registry.fail = true;
        assert_eq!(
            f.monitor()
                .finish_with(summary.id, &AtomicBool::new(false), &|_, _| {}, &registry)
                .unwrap_err(),
            "fixture_inventory_failure"
        );
        assert_eq!(
            fs::read(f.monitor().record_path(summary.id)).unwrap(),
            original
        );
        assert_eq!(
            f.monitor().detail(summary.id).unwrap_err(),
            "monitor_not_finished"
        );
        assert!(
            finish(&f, summary.id, &FakeRegistry::empty())
                .session
                .complete
        );
    }

    #[test]
    fn late_file_reads_and_registry_omissions_are_not_absence() {
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        f.file("late", b"baseline");
        let summary = begin(&f, &FakeRegistry::values(&[("late", b"baseline")]));
        let mut registry = FakeRegistry::empty();
        registry.complete = false;
        let report = f
            .monitor()
            .finish_with(
                summary.id,
                &AtomicBool::new(false),
                &|path, count| {
                    if count > 0 && path.file_name().is_some_and(|n| n == "late") {
                        File::options()
                            .write(true)
                            .open(path)
                            .unwrap()
                            .set_len(MAX_FILE_BYTES + 1)
                            .unwrap();
                    }
                },
                &registry,
            )
            .unwrap();
        assert!(!report.session.complete);
        assert!(report.changes.is_empty());
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("monitor_file_read_limit")));
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("not treated as absent")));
        assert_eq!(f.monitor().detail(summary.id).unwrap(), report);
    }

    #[test]
    fn incomplete_before_suppresses_uncertain_additions_and_limits_are_disclosed() {
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        f.file("too-large", b"");
        File::options()
            .write(true)
            .open(f.root.join("too-large"))
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        let mut registry = FakeRegistry::empty();
        registry.complete = false;
        registry.truncated = true;
        let summary = begin(&f, &registry);
        f.file("too-large", b"now readable");
        f.file("possibly-new", b"no ownership claim");
        let report = finish(
            &f,
            summary.id,
            &FakeRegistry::values(&[("maybe-new", b"data")]),
        );
        assert!(report.truncated);
        assert!(!report.session.complete);
        assert!(report.changes.is_empty());
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("monitor_file_read_limit")));
    }

    #[test]
    fn protected_nested_backups_and_excess_depth_are_omitted_not_absent() {
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        f.file("safe", b"safe");
        let backups = f.root.join("TPBackups");
        fs::create_dir(&backups).unwrap();
        fs::write(backups.join("private"), b"fixture backup not inventoried").unwrap();
        let mut deep = f.root.clone();
        for _ in 0..MAX_DEPTH + 1 {
            deep.push("n");
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("omitted"), b"depth fixture").unwrap();
        let summary = begin(&f, &FakeRegistry::empty());
        assert_eq!(summary.files_before, 1);
        let report = finish(&f, summary.id, &FakeRegistry::empty());
        assert!(!report.session.complete);
        assert!(report.truncated);
        assert!(report.changes.is_empty());
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("TP backup directory")));
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("Directory/depth limit")));
    }

    #[test]
    fn strict_vendor_root_and_id_validation() {
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        for vendor in [
            "Microsoft",
            "microsoftWhatever",
            "Classes",
            "Policies",
            "Windows",
            "TP System Care",
            "TPVendor",
            "Software",
            "vendor",
            "CurrentVersion",
            "Acme\\Child",
            "Acme/Child",
            "Acme:stream",
            "..",
            " Acme",
            "Acme.",
            "*",
        ] {
            assert!(validate_vendor(vendor).is_err(), "{vendor}");
        }
        assert!(validate_vendor("").is_ok());
        assert!(validate_vendor("AcmeDedicated").is_ok());
        assert!(validate_vendor(&"A".repeat(64)).is_ok());
        assert!(validate_vendor(&"A".repeat(65)).is_err());
        assert!(validate_vendor("AcmeĐ").is_err());
        assert!(validate_name(&"é".repeat(64)).is_ok());
        assert!(validate_name(&"é".repeat(65)).is_err());
        assert!(validate_name("Fixture\u{0085}").is_err());
        assert!(check_id(now_id()).is_ok());
        assert!(now_id() < 10_000_000_000_000);
        assert!(check_id(MAX_SESSION_ID).is_ok());
        assert!(check_id(MAX_SESSION_ID + 1).is_err());
        assert!(check_id(1_794_938_153_330_548_300).is_err());
        assert!(decimal_id("9007199254740992").is_none());
        for root in [
            f.base.join(".."),
            f.base.join("."),
            f.base.join("A:stream"),
            f.base.join("FOO~1"),
            f.base.join("CON.txt"),
            f.base.join("trailing."),
            f.base.join("Microsoft").join("App"),
            f.base.join("Common Files").join("App"),
            f.base.join("TPBackups").join("App"),
            f.base.join("Tools"),
            f.store.clone(),
            f.base.clone(),
        ] {
            assert!(
                f.monitor()
                    .begin_with(
                        "Fixture",
                        &root,
                        "",
                        &AtomicBool::new(false),
                        &|_, _| {},
                        &FakeRegistry::empty()
                    )
                    .is_err(),
                "{}",
                root.display()
            );
        }
        assert!(lexical_absolute(Path::new("relative/path")).is_err());
        assert!(lexical_absolute(Path::new("\\\\server\\share\\App")).is_err());
        assert!(f.monitor().detail(0).is_err());
        assert!(f
            .monitor()
            .finish_with(
                0,
                &AtomicBool::new(false),
                &|_, _| {},
                &FakeRegistry::empty()
            )
            .is_err());
        #[cfg(windows)]
        for root in [
            "C:\\Windows\\MadeUpApp",
            "C:\\Program Files",
            "C:\\ProgramData",
            "C:\\Users",
            "C:\\Users\\MadeUpUser",
            "C:\\Users\\MadeUpUser\\AppData",
            "C:\\Users\\MadeUpUser\\AppData\\Local",
            "C:\\Program Files\\Common Files\\MadeUpApp",
            "C:\\App\\..\\Other",
            "\\\\?\\C:\\App",
            "C:App",
        ] {
            let result =
                lexical_absolute(Path::new(root)).and_then(|p| validate_root_policy(&p, &f.store));
            assert!(result.is_err(), "{root}");
        }
    }

    #[test]
    fn corrupt_records_are_visible_and_finish_cannot_redirect_the_baseline() {
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        let summary = begin(&f, &FakeRegistry::empty());
        let path = f.monitor().record_path(summary.id);
        let original = fs::read(&path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let mut variants = Vec::new();
        let mut bad = value.clone();
        bad["schema"] = 999.into();
        variants.push(bad);
        let mut bad = value.clone();
        bad["report"]["session"]["id"] = 9.into();
        variants.push(bad);
        let mut bad = value.clone();
        bad["report"]["session"]["root"] = f.base.join("..").to_string_lossy().into_owned().into();
        variants.push(bad);
        let mut bad = value.clone();
        bad["report"]["session"]["vendor"] = "Microsoft".into();
        variants.push(bad);
        let mut bad = value.clone();
        bad["report"]["session"]["complete"] = true.into();
        variants.push(bad);
        let mut bad = value.clone();
        bad["report"]["session"]
            .as_object_mut()
            .unwrap()
            .remove("finishedAt");
        variants.push(bad);
        let mut bad = value.clone();
        bad.as_object_mut().unwrap().remove("after");
        variants.push(bad);
        let mut bad = value.clone();
        bad["before"]["unexpected"] = true.into();
        variants.push(bad);
        let mut bad = value.clone();
        bad["before"]["files"] = serde_json::json!([{"path":"../outside","size":1,"modifiedNs":"1","fileId":{"volume":1,"index":1},"fingerprint":"0000000000000000"}]);
        variants.push(bad);
        let mut bad = value.clone();
        bad["before"]["registry"] = serde_json::json!([{"path":"HKLM64\\SAM","valueName":"secret","valueType":3,"size":1,"fingerprint":"0000000000000000"}]);
        variants.push(bad);
        for bad in variants {
            let bytes = serde_json::to_vec(&bad).unwrap();
            fs::write(&path, &bytes).unwrap();
            assert!(f
                .monitor()
                .list()
                .unwrap_err()
                .contains("monitor_corrupt_record"));
            assert!(f
                .monitor()
                .finish_with(
                    summary.id,
                    &AtomicBool::new(false),
                    &|_, _| {},
                    &FakeRegistry::empty()
                )
                .is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        fs::write(&path, b"{broken").unwrap();
        assert!(f.monitor().list().is_err());
        fs::write(&path, &original).unwrap();
        let alias = f.store.join(format!("0{}.json", summary.id));
        fs::write(&alias, &original).unwrap();
        assert!(f.monitor().list().is_err());
        fs::remove_file(&alias).unwrap();
        // A crashed uncommitted write does not become a fake baseline.
        fs::write(f.store.join(format!(".{}.1.tmp", summary.id)), b"partial").unwrap();
        assert_eq!(f.monitor().list().unwrap(), vec![summary.clone()]);
        assert!(
            finish(&f, summary.id, &FakeRegistry::empty())
                .session
                .complete
        );
    }

    #[test]
    fn multiple_baselines_and_oversized_persisted_records_are_rejected() {
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        let summary = begin(&f, &FakeRegistry::empty());
        let mut record: Record =
            serde_json::from_slice(&fs::read(f.monitor().record_path(summary.id)).unwrap())
                .unwrap();
        record.report.session.id = summary.id + 1;
        fs::write(
            f.monitor().record_path(summary.id + 1),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        assert!(f.monitor().list().unwrap_err().contains("multiple active"));
        fs::remove_file(f.monitor().record_path(summary.id + 1)).unwrap();
        File::options()
            .write(true)
            .open(f.monitor().record_path(summary.id))
            .unwrap()
            .set_len(MAX_RECORD_BYTES as u64 + 1)
            .unwrap();
        assert!(f
            .monitor()
            .list()
            .unwrap_err()
            .contains("record size limit"));
    }

    #[test]
    fn fingerprints_are_streaming_and_registry_scope_is_exact() {
        let mut hash = Fingerprint::new();
        hash.update(b"abc");
        hash.update(b"def");
        assert_eq!(hash.finish(), fingerprint(b"abcdef"));
        for path in [
            "HKLM64\\Software",
            "HKCU\\Software\\Microsoft",
            "HKLM32\\SECURITY",
            "HKCU\\Software\\AcmeDedicatedOther",
            "HKCU\\Software\\AcmeDedicated\\..\\Microsoft",
        ] {
            assert!(!validate_registry_path(path, "AcmeDedicated"));
        }
        assert!(validate_registry_path(
            "HKLM32\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
            ""
        ));
        assert!(!validate_registry_path(
            "HKLM32\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\\Child",
            ""
        ));
        assert!(validate_registry_path(
            "HKCU\\Software\\AcmeDedicated\\Child",
            "AcmeDedicated"
        ));
        assert!(!validate_registry_path("HKCU\\Software\\AcmeDedicated", ""));
    }

    #[test]
    fn diff_and_warning_caps_are_explicit() {
        let session = Summary {
            id: 1,
            name: "Fixture".into(),
            root: {
                #[cfg(windows)]
                {
                    "C:\\FixtureApp".into()
                }
                #[cfg(not(windows))]
                {
                    "/tmp/FixtureApp".into()
                }
            },
            vendor: "".into(),
            started_at: 1,
            finished_at: Some(2),
            files_before: 0,
            change_count: 0,
            complete: false,
        };
        let before = Snapshot {
            files: Vec::new(),
            registry: Vec::new(),
            files_complete: true,
            registry_complete: true,
            truncated: false,
            warnings: Vec::new(),
        };
        let mut after = before.clone();
        after.files = (0..MAX_CHANGES + 1)
            .map(|i| FileEntry {
                path: format!("f{i:05}"),
                size: 0,
                modified_ns: Some("1".into()),
                file_id: Some(FileId {
                    volume: 1,
                    index: i as u64,
                }),
                fingerprint: fingerprint(b""),
            })
            .collect();
        let report = make_report(session, &before, Some(&after));
        assert!(report.truncated);
        assert!(!report.session.complete);
        assert_eq!(report.changes.len(), MAX_CHANGES);
        assert!(report.warnings.iter().any(|w| w == DIFF_WARNING));
        for i in 0..MAX_WARNINGS + 2 {
            file_problem(&mut after, format!("Read failure {i}"), false);
        }
        assert_eq!(after.warnings.len(), MAX_WARNINGS);
        assert!(after.truncated);
        assert!(after.warnings.iter().any(|w| w == OMITTED_WARNINGS));
    }

    #[test]
    fn hard_links_are_omitted_and_unsafe_missing_ancestors_are_rejected() {
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        f.file("original", b"not exclusive");
        fs::hard_link(f.root.join("original"), f.base.join("outside-link")).unwrap();
        let summary = begin(&f, &FakeRegistry::empty());
        let report = finish(&f, summary.id, &FakeRegistry::empty());
        assert!(!report.session.complete);
        assert!(report.warnings.iter().any(|w| w.contains("hard links")));
        // An existing regular file cannot be a safe ancestor of a missing target.
        let target = f.base.join("outside-link").join("missing");
        assert!(f
            .monitor()
            .begin_with(
                "Fixture",
                &target,
                "",
                &AtomicBool::new(false),
                &|_, _| {},
                &FakeRegistry::empty()
            )
            .is_err());
    }

    #[cfg(windows)]
    struct FixtureJunction(File);

    #[cfg(windows)]
    impl FixtureJunction {
        fn control(file: &File, code: u32, data: &[u8]) -> io::Result<()> {
            use std::os::windows::io::AsRawHandle;
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
            let mut returned = 0;
            let success = unsafe {
                DeviceIoControl(
                    file.as_raw_handle(),
                    code,
                    data.as_ptr().cast(),
                    data.len() as u32,
                    std::ptr::null_mut(),
                    0,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if success == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }

        fn set(path: &Path, target: &Path) -> io::Result<Self> {
            use std::os::windows::fs::OpenOptionsExt;
            assert!(within(path, &fixture_temp_root()));
            assert!(within(target, &fixture_temp_root()));
            let target = fs::canonicalize(target)?;
            let print = target
                .to_string_lossy()
                .trim_start_matches(r"\\?\")
                .to_owned();
            let substitute: Vec<u16> = format!(r"\??\{print}").encode_utf16().collect();
            let print: Vec<u16> = print.encode_utf16().collect();
            let mut data = Vec::new();
            data.extend_from_slice(&0xa0000003u32.to_le_bytes());
            data.extend_from_slice(
                &((8 + (substitute.len() + print.len() + 2) * 2) as u16).to_le_bytes(),
            );
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&((substitute.len() * 2) as u16).to_le_bytes());
            data.extend_from_slice(&(((substitute.len() + 1) * 2) as u16).to_le_bytes());
            data.extend_from_slice(&((print.len() * 2) as u16).to_le_bytes());
            for value in substitute.into_iter().chain([0]).chain(print).chain([0]) {
                data.extend_from_slice(&value.to_le_bytes());
            }
            let mut options = OpenOptions::new();
            no_follow(&mut options);
            options.access_mode(0x100); // Attribute-only writers can bypass ordinary read/write sharing guards.
            let file = options.open(path)?;
            Self::control(&file, 0x900a4, &data)?;
            Ok(Self(file))
        }
    }

    #[cfg(windows)]
    impl Drop for FixtureJunction {
        fn drop(&mut self) {
            let mut tag = [0u8; 8];
            tag[..4].copy_from_slice(&0xa0000003u32.to_le_bytes());
            let result = Self::control(&self.0, 0x900ac, &tag);
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn pinned_in_place_junction_stops_without_outside_progress_or_diff() {
        use std::cell::RefCell;
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        fs::create_dir(&f.root).unwrap();
        let outside = f.base.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(
            outside.join("OUTSIDE_DECOY_NAME.bin"),
            b"OUTSIDE_SECRET_CONTENT",
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let before = capture(&f.root, "", &cancel, &|_, _| {}, &FakeRegistry::empty()).unwrap();
        assert!(before.files_complete);
        let pinned = PinnedDirectory::open(&f.root).unwrap();
        let mut entries = directory_entries(&pinned).unwrap();
        let junction = RefCell::new(None);
        let seen = RefCell::new(Vec::new());
        let after = capture(
            &f.root,
            "",
            &cancel,
            &|path, count| {
                seen.borrow_mut().push(path.to_owned());
                if count == 0 {
                    *junction.borrow_mut() = Some(FixtureJunction::set(&f.root, &outside).unwrap());
                }
            },
            &FakeRegistry::empty(),
        )
        .unwrap();
        assert!(is_reparse(&fs::symlink_metadata(&f.root).unwrap()));
        assert!(entries.next().unwrap().is_err());
        assert!(!after.files_complete);
        assert!(after.files.is_empty());
        assert_eq!(&*seen.borrow(), &[f.root.clone()]);
        let changes = differences(&path_string(&f.root).unwrap(), &before, &after).0;
        assert!(changes.is_empty());
        let serialized = serde_json::to_string(&after).unwrap();
        assert!(!serialized.contains("OUTSIDE_DECOY_NAME"));
        assert!(!serialized.contains("OUTSIDE_SECRET_CONTENT"));
        drop(junction);
    }

    #[cfg(windows)]
    #[test]
    fn retargeted_ancestor_after_leaf_open_is_rejected_before_any_bytes() {
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        f.file("same.bin", b"inside");
        let outside = f.base.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("same.bin"), b"OUTSIDE_SECRET_CONTENT").unwrap();
        fs::write(outside.join("OUTSIDE_DECOY_NAME.bin"), b"outside").unwrap();
        let ancestors = safe_ancestors(&f.root, false).unwrap();
        let parents: Vec<_> = ancestors._handles.iter().collect();
        let path = f.root.join("same.bin");
        // Deliberately allow unlink to exercise scope checks independently of the rename guard.
        let opened = File::open(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let junction = FixtureJunction::set(&f.root, &outside).unwrap();
        assert!(validate_parents(&parents).is_err());
        let redirected = File::open(&path).unwrap();
        assert!(check_opened(&path, &redirected, false).is_err());
        let mut budget = FileBudget {
            entries: 0,
            directories: 0,
            bytes: 0,
            inventory_bytes: 0,
            stopped: false,
        };
        assert!(read_file(
            &path,
            "same.bin".into(),
            opened,
            &parents,
            &mut budget,
            &AtomicBool::new(false)
        )
        .is_err());
        assert_eq!(budget.bytes, 0);
        drop(redirected);
        drop(junction);
    }

    #[cfg(windows)]
    #[test]
    fn ancestor_retargeted_before_child_open_does_not_hash_decoy_or_fake_removal() {
        use std::cell::RefCell;
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        f.file("same.bin", b"inside");
        let outside = f.base.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("same.bin"), b"OUTSIDE_SECRET_CONTENT").unwrap();
        fs::write(outside.join("OUTSIDE_DECOY_NAME.bin"), b"outside").unwrap();
        let cancel = AtomicBool::new(false);
        let before = capture(&f.root, "", &cancel, &|_, _| {}, &FakeRegistry::empty()).unwrap();
        assert_eq!(before.files.len(), 1);
        fs::remove_file(f.root.join("same.bin")).unwrap();
        let junction = RefCell::new(None);
        let seen = RefCell::new(Vec::new());
        let after = capture(
            &f.root,
            "",
            &cancel,
            &|path, count| {
                seen.borrow_mut().push(path.to_owned());
                if count == 0 {
                    *junction.borrow_mut() = Some(FixtureJunction::set(&f.root, &outside).unwrap());
                }
            },
            &FakeRegistry::empty(),
        )
        .unwrap();
        assert!(!after.files_complete);
        assert!(after.files.is_empty());
        assert!(!seen
            .borrow()
            .iter()
            .any(|path| path.to_string_lossy().contains("OUTSIDE_DECOY_NAME")));
        assert!(differences(&path_string(&f.root).unwrap(), &before, &after)
            .0
            .is_empty());
        assert!(!serde_json::to_string(&after)
            .unwrap()
            .contains(&fingerprint(b"OUTSIDE_SECRET_CONTENT")));
        drop(junction);
    }

    #[cfg(windows)]
    #[test]
    fn pinned_directory_and_leaf_block_namespace_rename_until_released() {
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        f.file("same.bin", b"inside");
        let guard = safe_ancestors(&f.root, false).unwrap();
        let leaf = open_checked(&f.root.join("same.bin"), false).unwrap();
        let moved = f.base.join("renamed");
        assert!(fs::rename(&f.root, &moved).is_err());
        assert!(fs::rename(f.root.join("same.bin"), f.root.join("renamed.bin")).is_err());
        assert!(guard
            ._handles
            .iter()
            .all(|parent| parent.validate().is_ok()));
        drop(leaf);
        fs::rename(f.root.join("same.bin"), f.root.join("renamed.bin")).unwrap();
        drop(guard);
        fs::rename(&f.root, &moved).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn native_directory_batches_preserve_identity_and_streaming_entry_limit() {
        use std::cell::Cell;
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        fs::create_dir(&f.root).unwrap();
        let cancel = AtomicBool::new(false);
        let mut snapshot =
            capture(&f.root, "", &cancel, &|_, _| {}, &FakeRegistry::empty()).unwrap();
        for index in 0..512 {
            f.file(&format!("{index:04}-{}.bin", "x".repeat(120)), b"");
        }
        let guard = safe_ancestors(&f.root, false).unwrap();
        let parents: Vec<_> = guard._handles.iter().collect();
        let pinned = parents.last().unwrap();
        let mut seen = BTreeSet::new();
        for item in directory_entries(pinned).unwrap() {
            let item = item.unwrap();
            let file = open_checked(&f.root.join(&item.name), false).unwrap();
            check_entry_identity(&file, item.id.as_ref()).unwrap();
            assert!(seen.insert(item.name));
        }
        assert_eq!(seen.len(), 512);
        let mut budget = FileBudget {
            entries: MAX_ENTRIES - 1,
            directories: 0,
            bytes: 0,
            inventory_bytes: 0,
            stopped: false,
        };
        let progress_count = Cell::new(0);
        walk_files(
            &f.root,
            &parents,
            &f.root,
            0,
            &mut snapshot,
            &mut budget,
            &cancel,
            &|_, _| progress_count.set(progress_count.get() + 1),
        )
        .unwrap();
        assert_eq!(progress_count.get(), 1);
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(budget.entries, MAX_ENTRIES);
        assert_eq!(budget.bytes, 0);
        assert!(budget.stopped && snapshot.truncated && !snapshot.files_complete);
    }

    #[cfg(windows)]
    #[test]
    fn existing_short_name_alias_is_rejected_when_the_fixture_has_one() {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::GetShortPathNameW;
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        f.file("safe", b"fixture only");
        let mut buffer = vec![0u16; MAX_PATH + 1];
        let length =
            unsafe { GetShortPathNameW(&HSTRING::from(f.root.as_os_str()), Some(&mut buffer)) }
                as usize;
        if length > 0 && length < buffer.len() {
            let short = PathBuf::from(String::from_utf16(&buffer[..length]).unwrap());
            if comparable(&short) != comparable(&f.root) {
                assert!(f
                    .monitor()
                    .begin_with(
                        "Fixture",
                        &short,
                        "",
                        &AtomicBool::new(false),
                        &|_, _| {},
                        &FakeRegistry::empty()
                    )
                    .is_err());
            }
        }
        // 8.3 creation can be disabled; lexical aliases are tested regardless.
        assert!(lexical_absolute(&f.base.join("FOO~1")).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn reparse_fixtures_are_never_followed_without_changing_privileges() {
        use std::os::windows::fs::symlink_dir;
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        let real = f.base.join("real");
        fs::create_dir(&real).unwrap();
        let alias = f.base.join("alias");
        // Windows may disallow fixture symlinks. Do not enable privileges,
        // developer mode, or change any permission/configuration to run a test.
        if symlink_dir(&real, &alias).is_err() {
            return;
        }
        assert!(f
            .monitor()
            .begin_with(
                "Fixture",
                &alias.join("missing"),
                "",
                &AtomicBool::new(false),
                &|_, _| {},
                &FakeRegistry::empty()
            )
            .is_err());
        f.file("safe", b"safe");
        symlink_dir(&real, f.root.join("outside")).unwrap();
        let summary = begin(&f, &FakeRegistry::empty());
        let report = finish(&f, summary.id, &FakeRegistry::empty());
        assert!(!report.session.complete);
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("reparse/symlink")));
        let store_alias = f.base.join("history-alias");
        symlink_dir(&f.store, &store_alias).unwrap();
        assert!(Monitor::at(store_alias).list().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_ancestor_store_and_selected_entries_are_never_followed() {
        use std::os::unix::fs::symlink;
        let _test = TEST_MUTEX.lock().unwrap();
        let f = Fixture::new();
        let real = f.base.join("real");
        fs::create_dir(&real).unwrap();
        let alias = f.base.join("alias");
        symlink(&real, &alias).unwrap();
        assert!(f
            .monitor()
            .begin_with(
                "Fixture",
                &alias.join("missing"),
                "",
                &AtomicBool::new(false),
                &|_, _| {},
                &FakeRegistry::empty()
            )
            .is_err());
        f.file("safe", b"safe");
        symlink(&real, f.root.join("outside")).unwrap();
        let summary = begin(&f, &FakeRegistry::empty());
        let report = finish(&f, summary.id, &FakeRegistry::empty());
        assert!(!report.session.complete);
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("reparse/symlink")));
        let external = f.base.join("other-history");
        fs::create_dir(&external).unwrap();
        let store_alias = f.base.join("history-alias");
        symlink(external, &store_alias).unwrap();
        assert!(Monitor::at(store_alias).list().is_err());
    }
}
