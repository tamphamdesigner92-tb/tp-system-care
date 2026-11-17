//! Conservative duplicate discovery and recycle-only removal.
//!
//! Fingerprints are filters, never evidence of equality. Every returned copy has
//! been compared byte-for-byte under read locks. Private proofs must be retained
//! by the caller; serialized results are not deletion capabilities.
//!
//! Limits: 100,000 visited entries, 20,000 candidates, depth 64, 100,000 exact
//! comparisons, 32 GiB of reads, 2,000 groups and 10,000 returned files. `skipped`
//! counts observed rejected entries/candidates (a rejected directory counts once),
//! not the unknown contents of unvisited subtrees. Cancellation is cooperative
//! between directory entries and 64 KiB reads, not an interruption of Windows
//! shell operations. No file is selected automatically.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions, ReadDir};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use serde::Serialize;

use super::verified_recycle::recovery_title;
use crate::rescue::Rescue;
use crate::winutil::{self, Known};

const CHUNK: usize = 64 * 1024;
const MAX_ENTRIES: usize = 100_000;
const MAX_FILES: usize = 20_000;
const MAX_DEPTH: usize = 64;
const MAX_COMPARISONS: usize = 100_000;
const MAX_READ_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const MAX_GROUPS: usize = 2_000;
const MAX_RESULT_FILES: usize = 10_000;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateFile {
    pub id: String,
    pub path: String,
    /// Seconds since the Unix epoch; the private proof retains full precision.
    pub modified: u64,
    #[serde(skip)]
    proof: Proof,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroup {
    pub id: String,
    /// Size of one copy, not the sum of the copies.
    pub size: u64,
    pub files: Vec<DuplicateFile>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Scan {
    pub groups: Vec<DuplicateGroup>,
    pub files_checked: u64,
    pub skipped: u64,
    pub cancelled: bool,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Removal {
    pub done: u64,
    pub failed: u64,
    /// Bytes recycled, not immediately freed disk space.
    pub bytes: u64,
    pub record_id: Option<u64>,
    pub removed_ids: Vec<String>,
    pub cancelled: bool,
    pub recovery: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Identity {
    volume: u64,
    file: [u8; 16],
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    identity: Identity,
    size: u64,
    modified: u128,
    created: u128,
    changed: i128,
    attributes: u32,
}

#[derive(Clone, Debug)]
struct Proof {
    path: PathBuf,
    root: PathBuf,
    stamp: Stamp,
    fingerprint: Option<u64>,
}

#[derive(Debug)]
enum CheckError {
    Cancelled,
    Limit,
    Invalid(String),
}

type Checked<T> = Result<T, CheckError>;

impl From<std::io::Error> for CheckError {
    fn from(error: std::io::Error) -> Self {
        Self::Invalid(error.to_string())
    }
}

fn invalid(message: &str) -> CheckError {
    CheckError::Invalid(message.into())
}

fn poll(cancel: &AtomicBool) -> Checked<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(CheckError::Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Limits {
    entries: usize,
    files: usize,
    groups: usize,
    result_files: usize,
    comparisons: usize,
    reads: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            entries: MAX_ENTRIES,
            files: MAX_FILES,
            groups: MAX_GROUPS,
            result_files: MAX_RESULT_FILES,
            comparisons: MAX_COMPARISONS,
            reads: MAX_READ_BYTES,
        }
    }
}

struct Budget {
    reads: u64,
    comparisons: usize,
}

impl Budget {
    fn new(limits: Limits) -> Self {
        Self {
            reads: limits.reads,
            comparisons: limits.comparisons,
        }
    }

    fn charge(&mut self, bytes: usize) -> Checked<()> {
        self.reads = self
            .reads
            .checked_sub(bytes as u64)
            .ok_or(CheckError::Limit)?;
        Ok(())
    }

    fn comparison(&mut self) -> Checked<()> {
        self.comparisons = self.comparisons.checked_sub(1).ok_or(CheckError::Limit)?;
        Ok(())
    }
}

// Component boundaries, including Windows' ordinal (not locale-dependent) case
// comparison. In particular, `C:\WindowsElsewhere` is not under `C:\Windows`.
#[cfg(windows)]
fn name_eq(left: &OsStr, right: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CompareStringOrdinal(
            left: *const u16,
            left_len: i32,
            right: *const u16,
            right_len: i32,
            ignore_case: i32,
        ) -> i32;
    }
    let left: Vec<u16> = left.encode_wide().collect();
    let right: Vec<u16> = right.encode_wide().collect();
    if left.len() > i32::MAX as usize || right.len() > i32::MAX as usize {
        return false;
    }
    unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        ) == 2
    }
}

#[cfg(not(windows))]
fn name_eq(left: &OsStr, right: &OsStr) -> bool {
    left == right
}

fn within(path: &Path, parent: &Path) -> bool {
    let mut parts = path.components();
    parent.components().all(|part| {
        parts
            .next()
            .is_some_and(|next| name_eq(next.as_os_str(), part.as_os_str()))
    })
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.components().count() == right.components().count() && within(left, right)
}

// Only ordinary local drive paths are accepted on Windows. Strip the canonical
// verbatim drive prefix, but never turn UNC/device namespaces into local paths.
fn plain_path(path: &Path) -> Checked<PathBuf> {
    #[cfg(windows)]
    {
        use std::path::Prefix;
        match path.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(_) => Ok(path.to_path_buf()),
                Prefix::VerbatimDisk(drive) => {
                    let mut result = PathBuf::from(format!("{}:\\", drive as char));
                    for part in path.components().skip(1) {
                        if let Component::Normal(name) = part {
                            result.push(name);
                        } else if !matches!(part, Component::RootDir) {
                            return Err(invalid("invalid_path"));
                        }
                    }
                    Ok(result)
                }
                _ => Err(invalid("non_local_path")),
            },
            _ => Err(invalid("invalid_path")),
        }
    }
    #[cfg(not(windows))]
    {
        Ok(path.to_path_buf())
    }
}

const SYSTEM_NAMES: &[&str] = &[
    "windows",
    "winnt",
    "windows.old",
    "program files",
    "program files (x86)",
    "programdata",
    "appdata",
    "application data",
    "local settings",
    "documents and settings",
    "tp rescue",
    "$recycle.bin",
    "system volume information",
    "$winreagent",
    "$windows.~bt",
    "$windows.~ws",
    "$mft",
    "$mftmirr",
    "$logfile",
    "$bitmap",
    "$boot",
    "$badclus",
    "$secure",
    "$upcase",
    "$extend",
    "recovery",
    "config.msi",
    "msocache",
    "boot",
    "bootmgr",
    "bootnxt",
    "bootsect.bak",
    "ntldr",
    "ntdetect.com",
    "io.sys",
    "msdos.sys",
    "pagefile.sys",
    "hiberfil.sys",
    "swapfile.sys",
    "dumpstack.log",
    "dumpstack.log.tmp",
];

fn protected_name(name: &OsStr) -> bool {
    SYSTEM_NAMES.iter().any(|system| name_eq(name, OsStr::new(system)))
        // Unix fixture scans must also exercise Windows' case-insensitive names.
        || SYSTEM_NAMES.contains(&name.to_string_lossy().to_ascii_lowercase().as_str())
}

fn lexical(path: &Path) -> Checked<PathBuf> {
    if !path.is_absolute() {
        return Err(invalid("invalid_path"));
    }
    // Check the original spelling before normalization can erase `.` or `..`.
    let text = path.to_str().ok_or_else(|| invalid("non_unicode_path"))?;
    if text.contains('\0') || text.split(['/', '\\']).any(|p| p == "." || p == "..") {
        return Err(invalid("invalid_path"));
    }
    let path = plain_path(path)?;
    if path.components().count() > MAX_DEPTH + 3 {
        return Err(invalid("path_too_deep"));
    }
    let names: Vec<_> = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    if names.windows(2).any(|pair| {
        name_eq(pair[0], OsStr::new("TP System Care")) && name_eq(pair[1], OsStr::new("Rescue"))
    }) {
        return Err(invalid("protected_path"));
    }
    for part in path.components() {
        if let Component::Normal(name) = part {
            let name = name.to_str().ok_or_else(|| invalid("non_unicode_path"))?;
            if protected_name(OsStr::new(name)) {
                return Err(invalid("protected_path"));
            }
            #[cfg(windows)]
            {
                let stem = name.split('.').next().unwrap_or("").to_ascii_lowercase();
                if name.contains([':', '*', '?', '"', '<', '>', '|'])
                    || name.ends_with(['.', ' '])
                    || matches!(
                        stem.as_str(),
                        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
                    )
                    || (stem.len() == 4
                        && (stem.starts_with("com") || stem.starts_with("lpt"))
                        && stem.as_bytes()[3].is_ascii_digit())
                {
                    return Err(invalid("invalid_path"));
                }
            }
        } else if matches!(part, Component::ParentDir | Component::CurDir) {
            return Err(invalid("invalid_path"));
        }
    }
    Ok(path)
}

#[derive(Clone)]
struct Policy {
    blocked: Vec<PathBuf>,
}

impl Policy {
    fn new() -> Checked<Self> {
        let mut blocked = Vec::new();
        for known in [
            Known::Windows,
            Known::ProgramFiles,
            Known::ProgramFilesX86,
            Known::ProgramData,
            Known::LocalAppData,
            Known::RoamingAppData,
            Known::UserProgramFiles,
        ] {
            let folder = winutil::known_folder(known);
            #[cfg(windows)]
            if folder.is_none() {
                return Err(invalid("protected_folders_unavailable"));
            }
            if let Some(folder) = folder {
                add_blocked(&mut blocked, &folder);
            }
        }
        // Covers redirected LocalLow as well as the standard AppData layout.
        #[cfg(windows)]
        {
            use windows::Win32::System::Com::CoTaskMemFree;
            use windows::Win32::UI::Shell::{
                FOLDERID_LocalAppDataLow, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG,
            };
            unsafe {
                let raw =
                    SHGetKnownFolderPath(&FOLDERID_LocalAppDataLow, KNOWN_FOLDER_FLAG(0), None)
                        .map_err(|e| CheckError::Invalid(e.to_string()))?;
                let folder = raw.to_string();
                CoTaskMemFree(Some(raw.as_ptr() as *const _));
                add_blocked(
                    &mut blocked,
                    &PathBuf::from(folder.map_err(|e| CheckError::Invalid(e.to_string()))?),
                );
            }
        }
        let exe = std::env::current_exe()?;
        add_blocked(&mut blocked, &exe);
        // The default Rescue is normally already inside a blocked AppData tree.
        add_blocked(&mut blocked, Rescue::default_location().dir());
        Ok(Self { blocked })
    }

    fn check(&self, path: &Path) -> Checked<PathBuf> {
        let path = lexical(path)?;
        if self.blocked.iter().any(|blocked| within(&path, blocked)) {
            return Err(invalid("protected_path"));
        }
        #[cfg(windows)]
        if !winutil::on_fixed_drive(&path) {
            return Err(invalid("non_fixed_drive"));
        }
        for parent in path.ancestors() {
            let metadata = fs::symlink_metadata(parent)?;
            if directory_unsafe(&metadata, parent) {
                return Err(invalid("alias_or_system"));
            }
        }
        let canonical = plain_path(&fs::canonicalize(&path)?)?;
        // Reject short-name aliases, junctions and all alternate spellings that
        // actually resolve to a different path (case and separators are benign).
        if !same_path(&path, &canonical) {
            return Err(invalid("path_alias"));
        }
        lexical(&canonical)?;
        if self
            .blocked
            .iter()
            .any(|blocked| within(&canonical, blocked))
        {
            return Err(invalid("protected_path"));
        }
        Ok(canonical)
    }
}

fn add_blocked(blocked: &mut Vec<PathBuf>, path: &Path) {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        return;
    };
    if let Ok(path) = plain_path(&absolute) {
        // Rescue::at also accepts a relative, not-yet-created directory. Protect
        // its absolute lexical location before the first history flush creates it.
        let mut normalized = PathBuf::new();
        for part in path.components() {
            match part {
                Component::CurDir => {}
                Component::ParentDir => {
                    normalized.pop();
                }
                _ => normalized.push(part.as_os_str()),
            }
        }
        blocked.push(normalized);
        if let Ok(canonical) = fs::canonicalize(&path).and_then(|p| {
            plain_path(&p).map_err(|_| std::io::Error::other("invalid protected path"))
        }) {
            blocked.push(canonical);
        }
    }
}

fn alias_or_system(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_SYSTEM | FILE_ATTRIBUTE_REPARSE_POINT.
        metadata.file_attributes() & (0x4 | 0x400) != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn directory_unsafe(metadata: &fs::Metadata, path: &Path) -> bool {
    // NTFS volume roots commonly carry HIDDEN|SYSTEM (e.g. 0x16). They are
    // necessary ancestry, not Windows-owned subdirectories. Reparse roots and
    // symlinks are still forbidden; ordinary descendants keep the SYSTEM ban.
    #[cfg(windows)]
    if path.parent().is_none() && metadata.is_dir() {
        use std::os::windows::fs::MetadataExt;
        return metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0;
    }
    #[cfg(not(windows))]
    let _ = path;
    alias_or_system(metadata)
}

fn stamp(file: &File) -> Checked<Stamp> {
    let meta = file.metadata()?;
    if !meta.is_file() || alias_or_system(&meta) {
        return Err(invalid("not_plain_file"));
    }
    let modified = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid("invalid_file_time"))?
        .as_nanos();
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            FileBasicInfo, FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
            BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FILE_ID_INFO,
        };
        let handle = HANDLE(file.as_raw_handle());
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        let mut basic = FILE_BASIC_INFO::default();
        let mut identity = FILE_ID_INFO::default();
        unsafe {
            GetFileInformationByHandle(handle, &mut info)
                .map_err(|e| CheckError::Invalid(e.to_string()))?;
            GetFileInformationByHandleEx(
                handle,
                FileBasicInfo,
                &mut basic as *mut _ as _,
                std::mem::size_of_val(&basic) as u32,
            )
            .map_err(|e| CheckError::Invalid(e.to_string()))?;
            GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                &mut identity as *mut _ as _,
                std::mem::size_of_val(&identity) as u32,
            )
            .map_err(|e| CheckError::Invalid(e.to_string()))?;
        }
        if info.nNumberOfLinks != 1 || basic.FileAttributes & (0x4 | 0x400 | 0x10) != 0 {
            return Err(invalid("linked_or_system_file"));
        }
        let size = (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow);
        if size != meta.len() || basic.LastWriteTime < 0 || basic.CreationTime < 0 {
            return Err(invalid("unstable_metadata"));
        }
        Ok(Stamp {
            identity: Identity {
                volume: identity.VolumeSerialNumber,
                file: identity.FileId.Identifier,
            },
            size,
            modified,
            created: basic.CreationTime as u128,
            changed: basic.ChangeTime as i128,
            attributes: basic.FileAttributes,
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return Err(invalid("linked_file"));
        }
        let mut id = [0; 16];
        id[..8].copy_from_slice(&meta.ino().to_le_bytes());
        Ok(Stamp {
            identity: Identity {
                volume: meta.dev(),
                file: id,
            },
            size: meta.len(),
            modified,
            created: meta
                .created()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |t| t.as_nanos()),
            changed: i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec()),
            attributes: meta.mode(),
        })
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = modified;
        Err(invalid("unsupported_identity"))
    }
}

#[derive(Clone, Copy)]
enum LockMode {
    Read,
    Inspect,
    Recycle,
}

struct LockedFile {
    file: File,
    // Pin ancestry objects; shell deletion never uses this user-controlled namespace.
    _parents: Vec<File>,
    proof: Proof,
}

fn read_handle(path: &Path, mode: LockMode) -> Checked<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_DELETE,
            FILE_SHARE_READ,
        };
        let share = match mode {
            LockMode::Read => FILE_SHARE_READ.0,
            LockMode::Inspect | LockMode::Recycle => FILE_SHARE_READ.0 | FILE_SHARE_DELETE.0,
        };
        if matches!(mode, LockMode::Recycle) {
            options.access_mode(0x80010000); // GENERIC_READ | DELETE for handle-bound transfer.
        }
        options
            .share_mode(share)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_SEQUENTIAL_SCAN.0);
    }
    #[cfg(not(windows))]
    let _ = mode;
    Ok(options.open(path)?)
}

fn lock_parents(path: &Path, cancel: &AtomicBool) -> Checked<Vec<File>> {
    let mut handles = Vec::new();
    let parents: Vec<_> = path
        .parent()
        .ok_or_else(|| invalid("invalid_path"))?
        .ancestors()
        .collect();
    for parent in parents.into_iter().rev() {
        poll(cancel)?;
        let meta = fs::symlink_metadata(parent)?;
        if !meta.is_dir() || directory_unsafe(&meta, parent) {
            return Err(invalid("directory_alias_or_system"));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows::Win32::Storage::FileSystem::{
                FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
                FILE_SHARE_WRITE,
            };
            let handle = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
                .open(parent)?;
            let meta = handle.metadata()?;
            if !meta.is_dir() || directory_unsafe(&meta, parent) {
                return Err(invalid("directory_alias_or_system"));
            }
            handles.push(handle);
        }
    }
    Ok(handles)
}

impl LockedFile {
    fn open(
        path: &Path,
        root: &Path,
        policy: &Policy,
        mode: LockMode,
        cancel: &AtomicBool,
    ) -> Checked<Self> {
        poll(cancel)?;
        let path = lexical(path)?;
        let parents = lock_parents(&path, cancel)?;
        let path = policy.check(&path)?;
        if !within(&path, root) || !same_path(&policy.check(root)?, root) {
            return Err(invalid("outside_scan_root"));
        }
        let file = read_handle(&path, mode)?;
        let stamp = stamp(&file)?;
        let proof = Proof {
            path,
            root: root.to_path_buf(),
            stamp,
            fingerprint: None,
        };
        let locked = Self {
            file,
            _parents: parents,
            proof,
        };
        locked.recheck(policy, cancel)?;
        Ok(locked)
    }

    fn verified(
        copy: &DuplicateFile,
        policy: &Policy,
        mode: LockMode,
        cancel: &AtomicBool,
    ) -> Checked<Self> {
        if copy.path
            != copy
                .proof
                .path
                .to_str()
                .ok_or_else(|| invalid("non_unicode_path"))?
            || copy.id != file_id(&copy.proof.stamp.identity)
            || copy.modified != (copy.proof.stamp.modified / 1_000_000_000) as u64
            || copy.proof.fingerprint.is_none()
        {
            return Err(invalid("invalid_proof"));
        }
        let mut locked = Self::open(&copy.proof.path, &copy.proof.root, policy, mode, cancel)?;
        if locked.proof.stamp != copy.proof.stamp {
            return Err(invalid("changed_file"));
        }
        locked.proof.fingerprint = copy.proof.fingerprint;
        Ok(locked)
    }

    fn recheck(&self, policy: &Policy, cancel: &AtomicBool) -> Checked<()> {
        poll(cancel)?;
        if stamp(&self.file)? != self.proof.stamp
            || !same_path(&policy.check(&self.proof.path)?, &self.proof.path)
            || !within(&self.proof.path, &self.proof.root)
            || !same_path(&policy.check(&self.proof.root)?, &self.proof.root)
        {
            return Err(invalid("changed_file"));
        }
        // Check the object at the name, not just the still-open old handle.
        let at_name = read_handle(&self.proof.path, LockMode::Inspect)?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::Foundation::HANDLE;
            super::verified_recycle::default_stream_only(
                &self.proof.path,
                HANDLE(self.file.as_raw_handle()),
            )
            .map_err(CheckError::Invalid)?;
            if !same_path(
                &super::verified_recycle::handle_path(HANDLE(self.file.as_raw_handle()))
                    .map_err(CheckError::Invalid)?,
                &self.proof.path,
            ) {
                return Err(invalid("replaced_file"));
            }
        }
        if stamp(&at_name)? != self.proof.stamp {
            return Err(invalid("replaced_file"));
        }
        Ok(())
    }
}

fn file_id(identity: &Identity) -> String {
    let mut id = format!("df-{:016x}-", identity.volume);
    for byte in identity.file {
        use std::fmt::Write;
        let _ = write!(id, "{byte:02x}");
    }
    id
}

fn read_chunk(
    reader: &mut impl Read,
    mut output: &mut [u8],
    budget: &mut Budget,
    cancel: &AtomicBool,
) -> Checked<()> {
    poll(cancel)?;
    budget.charge(output.len())?;
    // Limit individual reads even for a reader that returns partial blocks.
    while !output.is_empty() {
        poll(cancel)?;
        match reader.read(output) {
            Ok(0) => return Err(invalid("unexpected_eof")),
            Ok(count) => output = &mut output[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    poll(cancel)
}

fn eof(reader: &mut impl Read, budget: &mut Budget, cancel: &AtomicBool) -> Checked<()> {
    poll(cancel)?;
    budget.charge(1)?;
    let mut tail = [0];
    loop {
        poll(cancel)?;
        match reader.read(&mut tail) {
            Ok(0) => return poll(cancel),
            Ok(_) => return Err(invalid("unexpected_growth")),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn fingerprint_reader(
    reader: &mut impl Read,
    size: u64,
    budget: &mut Budget,
    cancel: &AtomicBool,
) -> Checked<u64> {
    let mut hash = 0xcbf29ce484222325u64; // Streaming FNV-1a, only a filter.
    let mut buffer = [0; CHUNK];
    let mut remaining = size;
    while remaining != 0 {
        let count = remaining.min(CHUNK as u64) as usize;
        read_chunk(reader, &mut buffer[..count], budget, cancel)?;
        for byte in &buffer[..count] {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        remaining -= count as u64;
    }
    eof(reader, budget, cancel)?;
    Ok(hash)
}

fn fingerprint(
    locked: &mut LockedFile,
    policy: &Policy,
    budget: &mut Budget,
    cancel: &AtomicBool,
) -> Checked<u64> {
    locked.recheck(policy, cancel)?;
    locked.file.seek(SeekFrom::Start(0))?;
    let hash = fingerprint_reader(&mut locked.file, locked.proof.stamp.size, budget, cancel)?;
    locked.recheck(policy, cancel)?;
    Ok(hash)
}

fn compare_readers(
    left: &mut impl Read,
    right: &mut impl Read,
    size: u64,
    budget: &mut Budget,
    cancel: &AtomicBool,
) -> Checked<bool> {
    let mut a = [0; CHUNK];
    let mut b = [0; CHUNK];
    let mut remaining = size;
    while remaining != 0 {
        let count = remaining.min(CHUNK as u64) as usize;
        read_chunk(left, &mut a[..count], budget, cancel)?;
        read_chunk(right, &mut b[..count], budget, cancel)?;
        if a[..count] != b[..count] {
            return Ok(false);
        }
        remaining -= count as u64;
    }
    eof(left, budget, cancel)?;
    eof(right, budget, cancel)?;
    Ok(true)
}

fn exact(
    left: &mut LockedFile,
    right: &mut LockedFile,
    policy: &Policy,
    budget: &mut Budget,
    cancel: &AtomicBool,
) -> Checked<bool> {
    budget.comparison()?;
    left.recheck(policy, cancel)?;
    right.recheck(policy, cancel)?;
    if left.proof.stamp.identity == right.proof.stamp.identity {
        return Err(invalid("same_file_identity"));
    }
    if left.proof.stamp.size != right.proof.stamp.size {
        return Ok(false);
    }
    left.file.seek(SeekFrom::Start(0))?;
    right.file.seek(SeekFrom::Start(0))?;
    let equal = compare_readers(
        &mut left.file,
        &mut right.file,
        left.proof.stamp.size,
        budget,
        cancel,
    )?;
    left.recheck(policy, cancel)?;
    right.recheck(policy, cancel)?;
    Ok(equal)
}

/// Scan a caller-approved root. Files below `min_size` are not candidates.
/// Root validation failures return an error; individual unreadable/unsafe files
/// are skipped. Only local fixed drives are accepted on Windows.
pub fn scan(
    root: &Path,
    min_size: u64,
    cancel: &AtomicBool,
    progress: &dyn Fn(&Path, usize),
) -> Result<Scan, String> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(Scan {
            cancelled: true,
            ..Scan::default()
        });
    }
    let policy = Policy::new().map_err(error_text)?;
    scan_with(
        root,
        min_size,
        cancel,
        progress,
        &policy,
        Limits::default(),
        &|hash| hash,
    )
}

fn error_text(error: CheckError) -> String {
    match error {
        CheckError::Cancelled => "cancelled".into(),
        CheckError::Limit => "limit".into(),
        CheckError::Invalid(message) => message,
    }
}

// Inject only the bucket key, not the actual stored fingerprint, for collision
// tests. Directory enumeration deliberately does not sort/buffer whole folders.
fn scan_with(
    root: &Path,
    min_size: u64,
    cancel: &AtomicBool,
    progress: &dyn Fn(&Path, usize),
    policy: &Policy,
    limits: Limits,
    bucket_key: &dyn Fn(u64) -> u64,
) -> Result<Scan, String> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(Scan {
            cancelled: true,
            ..Scan::default()
        });
    }
    let root = policy.check(root).map_err(error_text)?;
    if !fs::symlink_metadata(&root)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("not_directory".into());
    }
    let mut result = Scan::default();
    let mut candidates = Vec::new();
    let mut identities = HashSet::new();
    let mut visited = 0;
    let root_locks = match lock_parents(&root.join("__tp_directory_guard__"), cancel) {
        Ok(locks) => locks,
        Err(CheckError::Cancelled) => {
            result.cancelled = true;
            return Ok(result);
        }
        Err(error) => return Err(error_text(error)),
    };
    policy.check(&root).map_err(error_text)?;
    let mut stack: Vec<(PathBuf, ReadDir, Vec<File>)> = vec![(
        root.clone(),
        fs::read_dir(&root).map_err(|e| e.to_string())?,
        root_locks,
    )];
    while !stack.is_empty() {
        if cancel.load(Ordering::Relaxed) {
            result.cancelled = true;
            break;
        }
        let next = stack.last_mut().unwrap().1.next();
        let Some(entry) = next else {
            stack.pop();
            continue;
        };
        if visited == limits.entries {
            result.truncated = true;
            result.skipped += 1; // At least this observed entry was not examined.
            break;
        }
        visited += 1;
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                result.skipped += 1;
                continue;
            }
        };
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(_) => {
                result.skipped += 1;
                continue;
            }
        };
        if kind.is_file() {
            result.files_checked += 1;
            progress(&entry.path(), result.files_checked as usize);
        }
        if cancel.load(Ordering::Relaxed) {
            result.cancelled = true;
            break;
        }
        let path = match policy.check(&entry.path()) {
            Ok(path) => path,
            Err(error) => {
                if matches!(error, CheckError::Invalid(ref message) if message == "path_too_deep") {
                    result.truncated = true;
                }
                result.skipped += 1;
                continue;
            }
        };
        if kind.is_dir() {
            if stack.len() >= MAX_DEPTH {
                result.truncated = true;
                result.skipped += 1;
                continue;
            }
            // Ancestry handles prevent a directory swap while opening read_dir.
            let locks = match lock_parents(&path.join("__tp_directory_guard__"), cancel) {
                Ok(locks) => locks,
                Err(CheckError::Cancelled) => {
                    result.cancelled = true;
                    break;
                }
                Err(_) => {
                    result.skipped += 1;
                    continue;
                }
            };
            match policy
                .check(&path)
                .and_then(|_| fs::read_dir(&path).map_err(CheckError::from))
            {
                Ok(entries) => stack.push((path, entries, locks)),
                Err(_) => result.skipped += 1,
            }
        } else if kind.is_file() {
            let locked = match LockedFile::open(&path, &root, policy, LockMode::Read, cancel) {
                Ok(locked) => locked,
                Err(CheckError::Cancelled) => {
                    result.cancelled = true;
                    break;
                }
                Err(_) => {
                    result.skipped += 1;
                    continue;
                }
            };
            if locked.proof.stamp.size < min_size {
                continue;
            }
            if candidates.len() == limits.files {
                result.truncated = true;
                result.skipped += 1;
                break;
            }
            if !identities.insert(locked.proof.stamp.identity.clone()) {
                result.skipped += 1;
                continue;
            }
            candidates.push(DuplicateFile {
                id: file_id(&locked.proof.stamp.identity),
                path: locked.proof.path.to_str().unwrap().to_string(),
                modified: (locked.proof.stamp.modified / 1_000_000_000) as u64,
                proof: locked.proof,
            });
        } else {
            result.skipped += 1;
        }
    }
    if result.cancelled {
        return Ok(result);
    }
    // Deterministic IDs derive from identity; deterministic grouping/order is
    // independent of read_dir's enumeration order below the traversal cap.
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    let mut sizes: BTreeMap<u64, Vec<DuplicateFile>> = BTreeMap::new();
    for file in candidates {
        sizes.entry(file.proof.stamp.size).or_default().push(file);
    }
    let mut budget = Budget::new(limits);
    let mut buckets: BTreeMap<(u64, u64), Vec<DuplicateFile>> = BTreeMap::new();
    let mut pending: Vec<_> = sizes
        .into_values()
        .filter(|files| files.len() > 1)
        .flatten()
        .collect();
    // Pop from the back without retaining a second copy of the candidate list.
    pending.reverse();
    while let Some(mut file) = pending.pop() {
        let checked = LockedFile::open(&file.proof.path, &root, policy, LockMode::Read, cancel)
            .and_then(|mut locked| {
                if locked.proof.stamp != file.proof.stamp {
                    return Err(invalid("changed_file"));
                }
                fingerprint(&mut locked, policy, &mut budget, cancel)
            });
        match checked {
            Ok(hash) => {
                file.proof.fingerprint = Some(hash);
                buckets
                    .entry((file.proof.stamp.size, bucket_key(hash)))
                    .or_default()
                    .push(file);
            }
            Err(CheckError::Cancelled) => {
                result.cancelled = true;
                return Ok(result);
            }
            Err(CheckError::Limit) => {
                result.truncated = true;
                result.skipped += 1 + pending.len() as u64;
                break;
            }
            Err(_) => result.skipped += 1,
        }
    }
    let mut pending: Vec<_> = buckets
        .into_values()
        .filter(|files| files.len() > 1)
        .collect();
    pending.reverse();
    'buckets: while let Some(mut bucket) = pending.pop() {
        let mut builders: Vec<Vec<DuplicateFile>> = Vec::new();
        bucket.reverse();
        while let Some(file) = bucket.pop() {
            let mut matched = None;
            let mut rejected = false;
            for (index, group) in builders.iter().enumerate() {
                let checked = LockedFile::verified(&group[0], policy, LockMode::Read, cancel)
                    .and_then(|mut representative| {
                        let mut candidate =
                            LockedFile::verified(&file, policy, LockMode::Read, cancel)?;
                        exact(
                            &mut representative,
                            &mut candidate,
                            policy,
                            &mut budget,
                            cancel,
                        )
                    });
                match checked {
                    Ok(true) => {
                        matched = Some(index);
                        break;
                    }
                    Ok(false) => {}
                    Err(CheckError::Cancelled) => {
                        result.cancelled = true;
                        break 'buckets;
                    }
                    Err(CheckError::Limit) => {
                        result.truncated = true;
                        result.skipped += 1
                            + bucket.len() as u64
                            + pending.iter().map(|b| b.len() as u64).sum::<u64>()
                            + builders.iter().map(|b| b.len() as u64).sum::<u64>();
                        break 'buckets;
                    }
                    Err(_) => {
                        result.skipped += 1;
                        rejected = true;
                        break;
                    }
                }
            }
            if !rejected {
                if let Some(index) = matched {
                    builders[index].push(file);
                } else {
                    builders.push(vec![file]);
                }
            }
        }
        for files in builders.into_iter().filter(|files| files.len() > 1) {
            result.groups.push(DuplicateGroup {
                id: format!("dg-{}", files[0].id),
                size: files[0].proof.stamp.size,
                files,
            });
        }
    }
    result.groups.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.files[0].path.cmp(&b.files[0].path))
    });
    let mut returned = 0;
    result.groups.retain(|group| {
        if returned >= limits.result_files || returned + group.files.len() > limits.result_files {
            result.truncated = true;
            result.skipped += group.files.len() as u64;
            false
        } else {
            returned += group.files.len();
            true
        }
    });
    if result.groups.len() > limits.groups {
        result.truncated = true;
        result.skipped += result.groups[limits.groups..]
            .iter()
            .map(|g| g.files.len() as u64)
            .sum::<u64>();
        result.groups.truncate(limits.groups);
    }
    result.cancelled |= cancel.load(Ordering::Relaxed);
    Ok(result)
}

/// Recycle explicitly selected copies, preserving an unchanged, locked keeper
/// in every affected group. Selection errors have no side effects. Per-file
/// safety/recycle failures are counted; cancelled, unattempted IDs are not.
/// History is flushed before the first operation and after each attempt. If a
/// post-operation history write fails, stop and return an error containing the
/// serialized partial Removal, including every successfully removed ID.
pub fn remove(
    groups: &[DuplicateGroup],
    ids: &[String],
    rescue: &Rescue,
    cancel: &AtomicBool,
) -> Result<Removal, String> {
    #[cfg(windows)]
    {
        selection(groups, ids)?;
        if !cancel.load(Ordering::Relaxed) && !winutil::is_elevated() {
            return Err("needs_admin".into());
        }
    }
    remove_with(groups, ids, rescue, cancel, &recycle_only)
}

type Persist<'a> = dyn Fn(Option<&str>) -> Result<(), String> + 'a;
type Recycler<'a> = dyn Fn(&Path, &LockedFile, &LockedFile, &Policy, &AtomicBool, &Persist<'_>) -> Result<(), String>
    + 'a;

fn selection<'a>(
    groups: &'a [DuplicateGroup],
    ids: &[String],
) -> Result<Vec<Vec<&'a DuplicateFile>>, String> {
    if ids.is_empty() {
        return Err("empty_selection".into());
    }
    if ids.len() > MAX_RESULT_FILES || groups.len() > MAX_GROUPS {
        return Err("invalid_selection".into());
    }
    let mut lookup = BTreeMap::new();
    let mut group_ids = HashSet::new();
    let mut count = 0;
    for (index, group) in groups.iter().enumerate() {
        count += group.files.len();
        if count > MAX_RESULT_FILES
            || group.files.len() < 2
            || !group_ids.insert(&group.id)
            || group.id != format!("dg-{}", group.files[0].id)
        {
            return Err("invalid_groups".into());
        }
        for file in &group.files {
            if file.proof.stamp.size != group.size
                || file.proof.fingerprint.is_none()
                || lookup.insert(file.id.as_str(), (index, file)).is_some()
            {
                return Err("invalid_groups".into());
            }
        }
    }
    let mut selected = vec![Vec::new(); groups.len()];
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id.as_str()) {
            return Err("duplicate_id".into());
        }
        let &(group, file) = lookup.get(id.as_str()).ok_or("unknown_id")?;
        selected[group].push(file);
    }
    if groups
        .iter()
        .zip(&selected)
        .any(|(group, selected)| selected.len() == group.files.len())
    {
        return Err("keep_one".into());
    }
    Ok(selected)
}

fn save_removal(rescue: &Rescue, report: &Removal) -> Result<(), String> {
    rescue.add_tool_record(
        report.record_id.ok_or("missing_record_id")?,
        &recovery_title("duplicateFiles", &report.recovery),
        report.done,
        report.failed,
        report.bytes,
        Vec::new(),
    )
}

fn remove_with(
    groups: &[DuplicateGroup],
    ids: &[String],
    rescue: &Rescue,
    cancel: &AtomicBool,
    recycler: &Recycler<'_>,
) -> Result<Removal, String> {
    let selected = selection(groups, ids)?;
    let mut report = Removal::default();
    if cancel.load(Ordering::Relaxed) {
        report.cancelled = true;
        return Ok(report);
    }
    let mut policy = Policy::new().map_err(error_text)?;
    add_blocked(&mut policy.blocked, rescue.dir());
    report.record_id = Some(rescue.new_record_id());
    save_removal(rescue, &report)?; // No deletion if history is initially unwritable.
    let mut budget = Budget::new(Limits::default());
    for (group, targets) in groups.iter().zip(selected) {
        if targets.is_empty() {
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let mut keeper = None;
        for copy in &group.files {
            if targets.iter().any(|target| target.id == copy.id) {
                continue;
            }
            let checked = LockedFile::verified(copy, &policy, LockMode::Read, cancel).and_then(
                |mut locked| {
                    let hash = fingerprint(&mut locked, &policy, &mut budget, cancel)?;
                    if Some(hash) != copy.proof.fingerprint {
                        return Err(invalid("changed_keeper"));
                    }
                    Ok(locked)
                },
            );
            match checked {
                Ok(locked) => {
                    keeper = Some(locked);
                    break;
                }
                Err(CheckError::Cancelled) => {
                    report.cancelled = true;
                    break;
                }
                Err(_) => {}
            }
        }
        if report.cancelled {
            break;
        }
        // Keep this handle (and its ancestry locks) alive through all deletions.
        for target in targets {
            if cancel.load(Ordering::Relaxed) {
                report.cancelled = true;
                break;
            }
            let outcome = (|| -> Checked<()> {
                let keeper = keeper
                    .as_mut()
                    .ok_or_else(|| invalid("no_unchanged_keeper"))?;
                let mut locked = LockedFile::verified(target, &policy, LockMode::Recycle, cancel)?;
                if !exact(keeper, &mut locked, &policy, &mut budget, cancel)? {
                    return Err(invalid("content_changed"));
                }
                keeper.recheck(&policy, cancel)?;
                locked.recheck(&policy, cancel)?;
                poll(cancel)?;
                let recovery = std::cell::RefCell::new(None::<String>);
                let persist = |detail: Option<&str>| -> Result<(), String> {
                    *recovery.borrow_mut() = detail.map(str::to_owned);
                    if let Some(detail) = detail {
                        let mut pending = report.clone();
                        pending.recovery.push(detail.into());
                        save_removal(rescue, &pending)?;
                    }
                    Ok(())
                };
                let recycled = recycler(
                    &locked.proof.path,
                    &locked,
                    keeper,
                    &policy,
                    cancel,
                    &persist,
                );
                if let Some(detail) = recovery.into_inner() {
                    report.recovery.push(detail);
                }
                recycled.map_err(|error| {
                    if error == "cancelled" && cancel.load(Ordering::Relaxed) {
                        CheckError::Cancelled
                    } else {
                        CheckError::Invalid(error)
                    }
                })?;
                // Test adapters must move the verified object, not merely claim success.
                #[cfg(test)]
                {
                    #[cfg(windows)]
                    {
                        use std::os::windows::io::AsRawHandle;
                        use windows::Win32::Foundation::HANDLE;
                        if same_path(
                            &super::verified_recycle::handle_path(HANDLE(
                                locked.file.as_raw_handle(),
                            ))
                            .map_err(CheckError::Invalid)?,
                            &locked.proof.path,
                        ) {
                            return Err(invalid("recycle_not_confirmed"));
                        }
                    }
                    #[cfg(unix)]
                    if stamp(&locked.file)? == locked.proof.stamp {
                        return Err(invalid("recycle_not_confirmed"));
                    }
                }
                Ok(())
            })();
            match outcome {
                Ok(()) => {
                    report.done += 1;
                    report.bytes = report.bytes.saturating_add(group.size);
                    report.removed_ids.push(target.id.clone());
                }
                Err(CheckError::Cancelled) => {
                    report.cancelled = true;
                    break;
                }
                Err(_) => report.failed += 1,
            }
            if let Err(error) = save_removal(rescue, &report) {
                let partial = serde_json::to_string(&report).map_err(|e| e.to_string())?;
                return Err(format!(
                    "history_failed: {error}; partial_removal={partial}"
                ));
            }
        }
        if report.cancelled {
            break;
        }
    }
    report.cancelled |= cancel.load(Ordering::Relaxed);
    if let Err(error) = save_removal(rescue, &report) {
        let partial = serde_json::to_string(&report).map_err(|e| e.to_string())?;
        return Err(format!(
            "history_failed: {error}; partial_removal={partial}"
        ));
    }
    Ok(report)
}

#[cfg(not(windows))]
fn recycle_only(
    _: &Path,
    _: &LockedFile,
    _: &LockedFile,
    _: &Policy,
    _: &AtomicBool,
    _: &Persist<'_>,
) -> Result<(), String> {
    Err("only_windows".into())
}

#[cfg(windows)]
fn recycle_only(
    path: &Path,
    target: &LockedFile,
    keeper: &LockedFile,
    policy: &Policy,
    cancel: &AtomicBool,
    persist: &Persist<'_>,
) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    let parent = target
        ._parents
        .last()
        .ok_or("original_parent_unavailable")?;
    super::verified_recycle::recycle_verified(
        HANDLE(target.file.as_raw_handle()),
        HANDLE(parent.as_raw_handle()),
        path,
        cancel,
        &|restoring| {
            let check_cancel = if restoring {
                &AtomicBool::new(false)
            } else {
                cancel
            };
            keeper.recheck(policy, check_cancel).map_err(error_text)?;
            if restoring {
                policy
                    .check(path.parent().ok_or("invalid_parent")?)
                    .map_err(error_text)?;
            } else {
                target.recheck(policy, check_cancel).map_err(error_text)?;
            }
            Ok(())
        },
        persist,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::AtomicUsize;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            // temp_dir is usually inside AppData on Windows, deliberately not
            // a valid production scan root. These isolated fixtures live only
            // under this crate; never enumerate their parent or an actual drive.
            let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            loop {
                let root = parent.join(format!(
                    ".tp-duplicate-fixture-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&root) {
                    Ok(()) => return Self { root },
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("cannot create duplicate fixture: {e}"),
                }
            }
        }

        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            path
        }

        fn scan(&self) -> Scan {
            scan(&self.root, 0, &AtomicBool::new(false), &|_, _| {}).unwrap()
        }

        fn rescue(&self) -> Rescue {
            Rescue::at(self.root.join("fixture-history"))
        }

        fn recycle(&self, path: &Path) -> Result<(), String> {
            assert!(within(
                path,
                &fs::canonicalize(&self.root)
                    .and_then(|p| {
                        plain_path(&p).map_err(|_| std::io::Error::other("fixture path"))
                    })
                    .unwrap()
            ));
            let bin = self.root.join("fixture-bin");
            fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
            fs::rename(path, bin.join(path.file_name().unwrap())).map_err(|e| e.to_string())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).expect("remove only the owned duplicate fixture");
        }
    }

    fn id_named(group: &DuplicateGroup, name: &str) -> String {
        group
            .files
            .iter()
            .find(|file| Path::new(&file.path).file_name() == Some(OsStr::new(name)))
            .unwrap()
            .id
            .clone()
    }

    fn never_recycle(
        _: &Path,
        _: &LockedFile,
        _: &LockedFile,
        _: &Policy,
        _: &AtomicBool,
        _: &Persist<'_>,
    ) -> Result<(), String> {
        panic!("unexpected recycle callback")
    }

    #[test]
    fn identical_contents_only_with_deterministic_private_proofs() {
        let f = Fixture::new();
        let bytes = vec![17; CHUNK + 19];
        f.write("a.bin", &bytes);
        f.write("sub/b.bin", &bytes);
        let mut different = bytes.clone();
        different[CHUNK + 1] ^= 1;
        f.write("different.bin", &different);
        f.write("small.bin", b"x");
        let first = f.scan();
        let second = f.scan();
        assert_eq!(first.files_checked, 4);
        assert_eq!(first.skipped, 0);
        assert!(!first.cancelled && !first.truncated);
        assert_eq!(first.groups.len(), 1);
        assert_eq!(first.groups[0].size, bytes.len() as u64);
        assert_eq!(first.groups[0].files.len(), 2);
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&second).unwrap()
        );
        let file = serde_json::to_value(&first.groups[0].files[0]).unwrap();
        assert_eq!(file.as_object().unwrap().len(), 3);
        assert!(file.get("proof").is_none());
        assert!(scan(
            &f.root,
            bytes.len() as u64 + 1,
            &AtomicBool::new(false),
            &|_, _| {}
        )
        .unwrap()
        .groups
        .is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn named_streams_are_not_duplicate_data_and_invalidate_old_proofs() {
        let f = Fixture::new();
        f.write("a", b"same");
        let b = f.write("b", b"same");
        let groups = f.scan().groups;
        fs::write(format!("{}:private", b.display()), b"unique ADS data").unwrap();
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        let file = read_handle(&b, LockMode::Read).unwrap();
        assert_eq!(
            super::super::verified_recycle::default_stream_only(&b, HANDLE(file.as_raw_handle()))
                .unwrap_err(),
            "named_stream_refused"
        );
        drop(file);
        assert!(f.scan().groups.is_empty());
        let report = remove_with(
            &groups,
            &[id_named(&groups[0], "b")],
            &f.rescue(),
            &AtomicBool::new(false),
            &never_recycle,
        )
        .unwrap();
        assert_eq!((report.done, report.failed), (0, 1));
        assert!(b.exists());
        assert_eq!(
            fs::read(format!("{}:private", b.display())).unwrap(),
            b"unique ADS data"
        );
        let keeper_fixture = Fixture::new();
        let keeper = keeper_fixture.write("a", b"same");
        keeper_fixture.write("b", b"same");
        let groups = keeper_fixture.scan().groups;
        fs::write(format!("{}:private", keeper.display()), b"keeper-only data").unwrap();
        let report = remove_with(
            &groups,
            &[id_named(&groups[0], "b")],
            &keeper_fixture.rescue(),
            &AtomicBool::new(false),
            &never_recycle,
        )
        .unwrap();
        assert_eq!((report.done, report.failed), (0, 1));
        assert!(keeper_fixture.root.join("b").exists());
    }

    #[test]
    fn recreated_source_name_does_not_negate_verified_object_success() {
        let f = Fixture::new();
        f.write("a", b"same");
        f.write("b", b"same");
        let groups = f.scan().groups;
        let ids = [id_named(&groups[0], "b")];
        let report = remove_with(
            &groups,
            &ids,
            &f.rescue(),
            &AtomicBool::new(false),
            &|path, _, _, _, _, _| {
                f.recycle(path)?;
                fs::write(path, b"new unrelated object").map_err(|e| e.to_string())
            },
        )
        .unwrap();
        assert_eq!((report.done, report.failed, report.bytes), (1, 0, 4));
        assert_eq!(report.removed_ids, ids);
        assert_eq!(fs::read(f.root.join("b")).unwrap(), b"new unrelated object");
    }

    #[cfg(windows)]
    #[test]
    fn native_fixture_transfer_keeps_keeper_locked_and_counts_recreated_name() {
        if !winutil::is_elevated() {
            eprintln!("restricted staging fixture requires elevation; not exercised");
            return;
        }
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        let f = Fixture::new();
        f.write("a", b"same");
        f.write("b", b"same");
        let groups = f.scan().groups;
        let ids = [id_named(&groups[0], "b")];
        let report = remove_with(
            &groups,
            &ids,
            &f.rescue(),
            &AtomicBool::new(false),
            &|original, locked, keeper, _, cancel, persist| {
                super::super::verified_recycle::fixture_recycle(
                    HANDLE(locked.file.as_raw_handle()),
                    HANDLE(locked._parents.last().unwrap().as_raw_handle()),
                    original,
                    &f.root,
                    cancel,
                    &|_| Ok(()),
                    persist,
                    &|staged, _| {
                        assert!(OpenOptions::new()
                            .write(true)
                            .open(&keeper.proof.path)
                            .is_err());
                        assert!(fs::remove_file(&keeper.proof.path).is_err());
                        fs::rename(staged, f.root.join("fixture-recycled-by-handle"))
                            .map_err(|e| e.to_string())?;
                        fs::write(original, b"new source object").map_err(|e| e.to_string())
                    },
                )
            },
        )
        .unwrap();
        assert_eq!((report.done, report.failed), (1, 0));
        assert_eq!(report.removed_ids, ids);
        assert_eq!(report.recovery.len(), 1);
        assert!(report.recovery[0].starts_with("recycled: "));
        assert!(f.rescue().history().unwrap()[0]
            .title
            .as_ref()
            .unwrap()
            .contains("recycle_location="));
    }

    #[test]
    fn retained_recovery_is_durable_and_not_a_fake_undo() {
        let f = Fixture::new();
        f.write("a", b"same");
        f.write("b", b"same");
        let groups = f.scan().groups;
        let rescue = f.rescue();
        let detail = format!(
            "recovery_pending: staging={}; recycle_error=fixture failure",
            f.root.join("owned-staging/payload").display()
        );
        let report = remove_with(
            &groups,
            &[id_named(&groups[0], "b")],
            &rescue,
            &AtomicBool::new(false),
            &|_, _, _, _, _, persist| {
                persist(Some(&detail))?;
                Err(detail.clone())
            },
        )
        .unwrap();
        assert_eq!((report.done, report.failed), (0, 1));
        assert_eq!(report.recovery, [detail.clone()]);
        let history = rescue.history().unwrap();
        assert!(history[0].title.as_ref().unwrap().contains("owned-staging"));
        assert!(history[0].undo.is_empty());
    }

    #[test]
    fn same_size_different_contents_are_not_duplicates() {
        let f = Fixture::new();
        f.write("a", b"ABCD");
        f.write("b", b"ABCE");
        assert!(f.scan().groups.is_empty());
    }

    #[test]
    fn forced_fingerprint_collisions_still_require_exact_bytes() {
        let f = Fixture::new();
        f.write("a1", b"AAAA");
        f.write("a2", b"AAAA");
        f.write("b1", b"BBBB");
        f.write("b2", b"BBBB");
        f.write("unique", b"CCCC");
        let policy = Policy::new().unwrap();
        let result = scan_with(
            &f.root,
            0,
            &AtomicBool::new(false),
            &|_, _| {},
            &policy,
            Limits::default(),
            &|_| 0,
        )
        .unwrap();
        assert_eq!(result.groups.len(), 2);
        for group in &result.groups {
            assert_eq!(group.files.len(), 2);
            let a = fs::read(&group.files[0].path).unwrap();
            let b = fs::read(&group.files[1].path).unwrap();
            assert_eq!(a, b);
        }
    }

    #[test]
    fn empty_unknown_duplicate_and_keep_one_are_rejected_before_history() {
        let f = Fixture::new();
        f.write("a", b"same");
        f.write("b", b"same");
        let groups = f.scan().groups;
        let a = id_named(&groups[0], "a");
        let b = id_named(&groups[0], "b");
        let rescue = f.rescue();
        let cancel = AtomicBool::new(false);
        for (ids, error) in [
            (vec![], "empty_selection"),
            (vec!["unknown".into()], "unknown_id"),
            (vec![a.clone(), a.clone()], "duplicate_id"),
            (vec![a, b], "keep_one"),
        ] {
            assert_eq!(
                remove_with(&groups, &ids, &rescue, &cancel, &never_recycle).unwrap_err(),
                error
            );
        }
        assert!(!rescue.dir().exists());
    }

    #[test]
    fn success_recycles_only_selected_ids_and_upserts_history_without_undo() {
        let f = Fixture::new();
        f.write("a", b"same");
        f.write("b", b"same");
        f.write("c", b"same");
        let groups = f.scan().groups;
        let ids = vec![id_named(&groups[0], "b"), id_named(&groups[0], "c")];
        let rescue = f.rescue();
        let report = remove_with(
            &groups,
            &ids,
            &rescue,
            &AtomicBool::new(false),
            &|path, _, _, _, _, _| f.recycle(path),
        )
        .unwrap();
        assert_eq!((report.done, report.failed, report.bytes), (2, 0, 8));
        assert_eq!(report.removed_ids, ids);
        assert!(f.root.join("a").exists());
        assert!(!f.root.join("b").exists() && !f.root.join("c").exists());
        let history = rescue.history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(Some(history[0].id), report.record_id);
        assert_eq!(history[0].title.as_deref(), Some("duplicateFiles"));
        assert_eq!(
            (history[0].done, history[0].failed, history[0].bytes),
            (2, 0, 8)
        );
        assert!(history[0].undo.is_empty());
        assert_eq!(history[0].undoable, 0);
    }

    #[test]
    fn changed_target_is_not_recycled_even_at_the_same_size() {
        let f = Fixture::new();
        f.write("a", b"same");
        f.write("b", b"same");
        let groups = f.scan().groups;
        f.write("b", b"diff");
        let report = remove_with(
            &groups,
            &[id_named(&groups[0], "b")],
            &f.rescue(),
            &AtomicBool::new(false),
            &never_recycle,
        )
        .unwrap();
        assert_eq!((report.done, report.failed, report.bytes), (0, 1, 0));
        assert!(report.removed_ids.is_empty());
    }

    #[test]
    fn changed_keeper_is_refused_but_another_unchanged_keeper_is_usable() {
        let f = Fixture::new();
        f.write("a", b"same");
        f.write("b", b"same");
        let first = f.scan().groups;
        f.write("a", b"diff");
        let report = remove_with(
            &first,
            &[id_named(&first[0], "b")],
            &f.rescue(),
            &AtomicBool::new(false),
            &never_recycle,
        )
        .unwrap();
        assert_eq!((report.done, report.failed), (0, 1));
        f.write("a", b"same");
        f.write("c", b"same");
        let second = f.scan().groups;
        f.write("a", b"diff");
        let report = remove_with(
            &second,
            &[id_named(&second[0], "b")],
            &f.rescue(),
            &AtomicBool::new(false),
            &|path, _, _, _, _, _| f.recycle(path),
        )
        .unwrap();
        assert_eq!((report.done, report.failed), (1, 0));
        assert!(f.root.join("c").exists());
    }

    #[test]
    fn replaced_target_and_tampered_public_path_do_not_authorize_deletion() {
        let f = Fixture::new();
        f.write("a", b"same");
        let b = f.write("b", b"same");
        let mut groups = f.scan().groups;
        let target = id_named(&groups[0], "b");
        // Keep the original object alive at a different name, preventing file ID reuse.
        fs::rename(&b, f.root.join("old-b")).unwrap();
        f.write("b", b"same");
        let report = remove_with(
            &groups,
            &[target.clone()],
            &f.rescue(),
            &AtomicBool::new(false),
            &never_recycle,
        )
        .unwrap();
        assert_eq!((report.done, report.failed), (0, 1));
        groups[0]
            .files
            .iter_mut()
            .find(|file| file.id == target)
            .unwrap()
            .path = f.root.join("a").to_str().unwrap().into();
        let report = remove_with(
            &groups,
            &[target],
            &f.rescue(),
            &AtomicBool::new(false),
            &never_recycle,
        )
        .unwrap();
        assert_eq!((report.done, report.failed), (0, 1));
    }

    #[test]
    fn failed_or_unconfirmed_recycle_never_reports_success() {
        let f = Fixture::new();
        f.write("a", b"same");
        f.write("b", b"same");
        let groups = f.scan().groups;
        let ids = [id_named(&groups[0], "b")];
        let rescue = f.rescue();
        for claim_success in [false, true] {
            let report = remove_with(
                &groups,
                &ids,
                &rescue,
                &AtomicBool::new(false),
                &move |_, _, _, _, _, _| {
                    if claim_success {
                        Ok(())
                    } else {
                        Err("fixture failure".into())
                    }
                },
            )
            .unwrap();
            assert_eq!((report.done, report.failed, report.bytes), (0, 1, 0));
            assert!(report.removed_ids.is_empty());
            assert!(f.root.join("b").exists());
        }
        assert!(rescue
            .history()
            .unwrap()
            .iter()
            .all(|record| record.done == 0 && record.failed == 1 && record.undo.is_empty()));
    }

    #[test]
    fn hardlinks_are_not_reclaimable_and_new_links_invalidate_removal() {
        let f = Fixture::new();
        let a = f.write("a", b"same");
        f.write("b", b"same");
        let original = f.scan().groups;
        if fs::hard_link(&a, f.root.join("a-link")).is_err() {
            return;
        }
        let result = f.scan();
        assert!(result.groups.is_empty());
        assert!(result.skipped >= 2);
        let report = remove_with(
            &original,
            &[id_named(&original[0], "b")],
            &f.rescue(),
            &AtomicBool::new(false),
            &never_recycle,
        )
        .unwrap();
        assert_eq!((report.done, report.failed), (0, 1));
    }

    #[test]
    fn symlink_files_directories_and_alias_roots_are_not_followed_if_supported() {
        let f = Fixture::new();
        f.write("plain/a", b"same");
        f.write("plain/b", b"same");
        let file_link = f.root.join("file-link");
        let dir_link = f.root.join("dir-link");
        #[cfg(windows)]
        let (file_ok, dir_ok) = (
            std::os::windows::fs::symlink_file(f.root.join("plain/a"), &file_link).is_ok(),
            std::os::windows::fs::symlink_dir(f.root.join("plain"), &dir_link).is_ok(),
        );
        #[cfg(unix)]
        let (file_ok, dir_ok) = (
            std::os::unix::fs::symlink(f.root.join("plain/a"), &file_link).is_ok(),
            std::os::unix::fs::symlink(f.root.join("plain"), &dir_link).is_ok(),
        );
        #[cfg(not(any(windows, unix)))]
        let (file_ok, dir_ok) = (false, false);
        let result = f.scan();
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].files.len(), 2);
        assert!(result.skipped >= u64::from(file_ok) + u64::from(dir_ok));
        if dir_ok {
            assert!(scan(&dir_link, 0, &AtomicBool::new(false), &|_, _| {}).is_err());
            assert!(scan(
                &dir_link.join("sub"),
                0,
                &AtomicBool::new(false),
                &|_, _| {}
            )
            .is_err());
        }
    }

    #[test]
    fn protected_names_case_boundaries_and_rescue_are_enforced() {
        let f = Fixture::new();
        f.write("a", b"same");
        f.write("b", b"same");
        for name in [
            "WiNdOwS/a",
            "APPDATA/a",
            "Program Files/a",
            "ProgramData/a",
            "$Recycle.Bin/a",
            "TP System Care/Rescue/a",
            "pagefile.sys",
        ] {
            f.write(name, b"same");
        }
        let result = f.scan();
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].files.len(), 2);
        assert!(result.skipped >= 7);
        assert!(scan(
            &f.root.join("WiNdOwS"),
            0,
            &AtomicBool::new(false),
            &|_, _| {}
        )
        .is_err());
        let blocked = f.write("blocked/a", b"safe");
        let sibling = f.write("blockedElsewhere/a", b"safe");
        let policy = Policy {
            blocked: vec![blocked.parent().unwrap().to_path_buf()],
        };
        assert!(policy.check(&blocked).is_err());
        assert!(policy.check(&sibling).is_ok());
        let only = Fixture::new();
        only.write("a", b"same");
        only.write("b", b"same");
        let groups = only.scan().groups;
        let rescue = Rescue::at(only.root.clone());
        let report = remove_with(
            &groups,
            &[id_named(&groups[0], "b")],
            &rescue,
            &AtomicBool::new(false),
            &never_recycle,
        )
        .unwrap();
        assert_eq!((report.done, report.failed), (0, 1));
    }

    #[test]
    fn cancelled_traversal_and_removal_have_no_unrequested_side_effects() {
        let f = Fixture::new();
        f.write("a", b"same");
        f.write("b", b"same");
        let groups = f.scan().groups;
        let cancel = AtomicBool::new(true);
        let result = scan(&f.root, 0, &cancel, &|_, _| {
            panic!("progress after cancellation")
        })
        .unwrap();
        assert!(result.cancelled && result.groups.is_empty());
        let rescue = f.rescue();
        let report = remove_with(
            &groups,
            &[id_named(&groups[0], "b")],
            &rescue,
            &cancel,
            &never_recycle,
        )
        .unwrap();
        assert!(report.cancelled && report.record_id.is_none() && report.removed_ids.is_empty());
        assert!(!rescue.dir().exists());
        cancel.store(false, Ordering::Relaxed);
        let result = scan(&f.root, 0, &cancel, &|_, _| {
            cancel.store(true, Ordering::Relaxed)
        })
        .unwrap();
        assert!(result.cancelled && result.groups.is_empty());
        assert_eq!(result.files_checked, 1);
    }

    #[test]
    fn cancellation_between_recycles_preserves_remaining_selected_copies() {
        let f = Fixture::new();
        for name in ["a", "b", "c"] {
            f.write(name, b"same");
        }
        let groups = f.scan().groups;
        let ids = vec![id_named(&groups[0], "b"), id_named(&groups[0], "c")];
        let cancel = AtomicBool::new(false);
        let report = remove_with(
            &groups,
            &ids,
            &f.rescue(),
            &cancel,
            &|path, _, _, _, _, _| {
                f.recycle(path)?;
                cancel.store(true, Ordering::Relaxed);
                Ok(())
            },
        )
        .unwrap();
        assert!(report.cancelled);
        assert_eq!((report.done, report.failed, report.bytes), (1, 0, 4));
        assert_eq!(report.removed_ids, vec![ids[0].clone()]);
        assert!(f.root.join("a").exists() && f.root.join("c").exists());
    }

    #[test]
    fn bounded_entries_candidates_groups_and_read_work_are_reported() {
        let f = Fixture::new();
        for name in ["a", "b", "c"] {
            f.write(name, b"same");
        }
        let policy = Policy::new().unwrap();
        for limits in [
            Limits {
                entries: 1,
                ..Limits::default()
            },
            Limits {
                files: 1,
                ..Limits::default()
            },
            Limits {
                groups: 0,
                ..Limits::default()
            },
            Limits {
                result_files: 1,
                ..Limits::default()
            },
            Limits {
                comparisons: 0,
                ..Limits::default()
            },
            Limits {
                reads: 3,
                ..Limits::default()
            },
        ] {
            let result = scan_with(
                &f.root,
                0,
                &AtomicBool::new(false),
                &|_, _| {},
                &policy,
                limits,
                &|hash| hash,
            )
            .unwrap();
            assert!(result.truncated);
            assert!(result.skipped > 0);
            assert!(result.groups.is_empty());
        }
    }

    #[test]
    fn io_errors_short_reads_and_chunk_cancellation_are_never_equality() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("fixture read failure"))
            }
        }
        let cancel = AtomicBool::new(false);
        assert!(compare_readers(
            &mut Cursor::new(b"same"),
            &mut Broken,
            4,
            &mut Budget::new(Limits::default()),
            &cancel
        )
        .is_err());
        assert!(compare_readers(
            &mut Cursor::new(b"same"),
            &mut Cursor::new(b"sa"),
            4,
            &mut Budget::new(Limits::default()),
            &cancel
        )
        .is_err());
        struct Cancelling<'a> {
            cancel: &'a AtomicBool,
            bytes: Cursor<Vec<u8>>,
        }
        impl Read for Cancelling<'_> {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                let result = self.bytes.read(output);
                self.cancel.store(true, Ordering::Relaxed);
                result
            }
        }
        let mut reader = Cancelling {
            cancel: &cancel,
            bytes: Cursor::new(vec![7; CHUNK * 2]),
        };
        assert!(matches!(
            fingerprint_reader(
                &mut reader,
                (CHUNK * 2) as u64,
                &mut Budget::new(Limits::default()),
                &cancel
            ),
            Err(CheckError::Cancelled)
        ));
    }

    #[test]
    fn unwritable_initial_history_prevents_recycling_and_later_failure_reports_partial_ids() {
        let f = Fixture::new();
        f.write("a", b"same");
        f.write("b", b"same");
        let groups = f.scan().groups;
        let ids = [id_named(&groups[0], "b")];
        let invalid_rescue = Rescue::at(f.write("not-a-directory", b"fixture"));
        assert!(remove_with(
            &groups,
            &ids,
            &invalid_rescue,
            &AtomicBool::new(false),
            &never_recycle
        )
        .is_err());
        let rescue = f.rescue();
        let error = remove_with(
            &groups,
            &ids,
            &rescue,
            &AtomicBool::new(false),
            &|path, _, _, _, _, _| {
                f.recycle(path)?;
                // Deliberately break only this fixture's history storage after the
                // simulated recycle. No unrelated file or real Recycle Bin is used.
                fs::remove_file(rescue.dir().join("history.json")).unwrap();
                fs::remove_dir(rescue.dir()).unwrap();
                fs::write(rescue.dir(), b"fixture").unwrap();
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.starts_with("history_failed:"));
        let partial: RemovalJson =
            serde_json::from_str(error.split("partial_removal=").nth(1).unwrap()).unwrap();
        assert_eq!(partial.done, 1);
        assert_eq!(partial.removed_ids, ids);
    }

    #[test]
    fn cancellation_before_the_shell_operation_is_not_a_failed_deletion() {
        let f = Fixture::new();
        f.write("a", b"same");
        f.write("b", b"same");
        let groups = f.scan().groups;
        let cancel = AtomicBool::new(false);
        let report = remove_with(
            &groups,
            &[id_named(&groups[0], "b")],
            &f.rescue(),
            &cancel,
            &|_, _, _, _, _, _| {
                cancel.store(true, Ordering::Relaxed);
                Err("cancelled".into())
            },
        )
        .unwrap();
        assert!(report.cancelled);
        assert_eq!((report.done, report.failed, report.bytes), (0, 0, 0));
        assert!(report.removed_ids.is_empty() && f.root.join("b").exists());
    }

    #[cfg(windows)]
    #[test]
    fn system_attributes_and_short_path_aliases_are_rejected() {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::{
            GetShortPathNameW, SetFileAttributesW, FILE_FLAGS_AND_ATTRIBUTES,
        };
        let f = Fixture::new();
        f.write("a", b"same");
        f.write("b", b"same");
        let protected = f.write("system-file", b"same");
        unsafe {
            SetFileAttributesW(
                &HSTRING::from(protected.as_os_str()),
                FILE_FLAGS_AND_ATTRIBUTES(0x4),
            )
            .unwrap();
        }
        let result = f.scan();
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].files.len(), 2);
        assert!(result.skipped >= 1);
        // Restore attributes only on the owned fixture so its RAII cleanup is
        // straightforward even on hosts with stricter SYSTEM-file policies.
        unsafe {
            SetFileAttributesW(
                &HSTRING::from(protected.as_os_str()),
                FILE_FLAGS_AND_ATTRIBUTES(0x80),
            )
            .unwrap();
        }
        let long = f.root.join("directory-with-a-long-name");
        fs::create_dir(&long).unwrap();
        let mut buffer = vec![0u16; 32_768];
        let length =
            unsafe { GetShortPathNameW(&HSTRING::from(long.as_os_str()), Some(&mut buffer)) };
        if length != 0 && (length as usize) < buffer.len() {
            let short = PathBuf::from(String::from_utf16(&buffer[..length as usize]).unwrap());
            if !same_path(&short, &long) {
                assert!(scan(&short, 0, &AtomicBool::new(false), &|_, _| {}).is_err());
            }
        }
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RemovalJson {
        done: u64,
        removed_ids: Vec<String>,
    }

    #[cfg(windows)]
    #[test]
    fn keeper_sharing_lock_blocks_write_delete_and_parent_rename() {
        let f = Fixture::new();
        f.write("sub/a", b"same");
        f.write("sub/b", b"same");
        let groups = f.scan().groups;
        let ids = [id_named(&groups[0], "b")];
        let report = remove_with(
            &groups,
            &ids,
            &f.rescue(),
            &AtomicBool::new(false),
            &|path, _, keeper, _, _, _| {
                assert!(OpenOptions::new()
                    .write(true)
                    .open(&keeper.proof.path)
                    .is_err());
                assert!(fs::remove_file(&keeper.proof.path).is_err());
                assert!(OpenOptions::new().write(true).open(path).is_err());
                assert!(fs::rename(f.root.join("sub"), f.root.join("renamed")).is_err());
                f.recycle(path)
            },
        )
        .unwrap();
        assert_eq!((report.done, report.failed), (1, 0));
    }
}
