//! Explicitly selected, reversible removal of narrowly attributable Windows entries.
//! No service is stopped and no task is disabled as a substitute for deletion.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::rescue::Rescue;

const FORMAT: u32 = 1;
const MAX_BACKUP: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Entry {
    pub kind: &'static str,
    pub path: String,
    pub detail: String,
    snapshot: Snapshot,
    ownership: Ownership,
}

/// The same payload is flushed to `undo.json` BEFORE removing the resource.
/// Paths are relative to Rescue, never arbitrary paths supplied to an importer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryUndo {
    version: u32,
    record_id: u64,
    index: usize,
    snapshot: Snapshot,
    ownership: Ownership,
    manifest: PathBuf,
    backup: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    roots: Vec<String>,
    files: Vec<String>,
    windows: String,
    current_sid: String,
    self_exe: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "definition",
    rename_all = "lowercase",
    deny_unknown_fields
)]
enum Snapshot {
    Service(ServiceSnapshot),
    Task(TaskSnapshot),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryValue {
    name: String,
    value_type: String,
    data: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceSnapshot {
    name: String,
    display_name: String,
    binary: String,
    account: String,
    start: u32,
    error_control: u32,
    service_type: u32,
    state: u32,
    dependents: u32,
    dependencies: Vec<String>,
    group: String,
    tag: u32,
    description: String,
    security: String,
    advanced_safe: bool,
    registry_supported: bool,
    registry: Vec<RegistryValue>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Action {
    command: String,
    arguments: String,
    working_directory: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskSnapshot {
    path: String,
    xml: String,
    security: String,
    state: u32,
    user: String,
    sid: String,
    logon: String,
    run_level: String,
    actions: Vec<Action>,
    supported: bool,
}

// Lexical normalization is deliberately NOT canonicalize(): leftovers often have
// missing executables. Reject aliases, devices, UNC, ADS and dot components rather
// than guessing what Windows might execute. Only local, fully qualified .exe paths.
fn windows_path(raw: &str) -> Option<String> {
    if raw != raw.trim()
        || raw
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '%' | '*' | '?' | '<' | '>' | '|'))
    {
        return None;
    }
    let text = raw.replace('/', "\\");
    let b = text.as_bytes();
    if b.len() < 4 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
        return None;
    }
    let tail = text[3..].trim_end_matches('\\');
    if tail.is_empty() {
        return None;
    }
    for part in tail.split('\\') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || part.contains([':', '~'])
        {
            return None;
        }
        let stem = part.split('.').next()?.to_ascii_lowercase();
        if matches!(stem.as_str(), "con" | "prn" | "aux" | "nul" | "clock$")
            || (stem.len() == 4
                && (stem.starts_with("com") || stem.starts_with("lpt"))
                && stem.as_bytes()[3].is_ascii_digit())
        {
            return None;
        }
    }
    Some(format!(
        "{}:\\{}",
        (b[0] as char).to_ascii_lowercase(),
        tail
    ))
}

fn path_equal(a: &str, b: &str) -> bool {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn CompareStringOrdinal(
                a: *const u16,
                a_len: i32,
                b: *const u16,
                b_len: i32,
                ignore_case: i32,
            ) -> i32;
        }
        let a: Vec<u16> = a.encode_utf16().collect();
        let b: Vec<u16> = b.encode_utf16().collect();
        if a.len() > i32::MAX as usize || b.len() > i32::MAX as usize {
            return false;
        }
        // Windows ordinal comparison, not locale-dependent Unicode folding.
        unsafe {
            CompareStringOrdinal(a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32, 1) == 2
        }
    }
    #[cfg(not(windows))]
    {
        // Portable fixtures deliberately avoid guessing non-ASCII Windows rules.
        a == b || (a.is_ascii() && b.is_ascii() && a.eq_ignore_ascii_case(b))
    }
}

fn below(path: &str, root: &str) -> bool {
    let mut components = path.split('\\');
    for expected in root.split('\\') {
        if !components
            .next()
            .is_some_and(|actual| path_equal(actual, expected))
        {
            return false;
        }
    }
    components.next().is_some()
}

fn runner(path: &str) -> bool {
    let name = path.rsplit('\\').next().unwrap_or("").to_ascii_lowercase();
    let name = name.as_str();
    matches!(
        name,
        "cmd.exe"
            | "powershell.exe"
            | "pwsh.exe"
            | "rundll32.exe"
            | "regsvr32.exe"
            | "svchost.exe"
            | "dllhost.exe"
            | "msiexec.exe"
            | "wscript.exe"
            | "cscript.exe"
            | "mshta.exe"
            | "wmic.exe"
            | "reg.exe"
            | "sc.exe"
            | "schtasks.exe"
            | "installutil.exe"
            | "regasm.exe"
            | "regsvcs.exe"
            | "msbuild.exe"
            | "python.exe"
            | "pythonw.exe"
            | "python3.exe"
            | "python3w.exe"
            | "py.exe"
            | "pyw.exe"
            | "node.exe"
            | "java.exe"
            | "javaw.exe"
            | "dotnet.exe"
            | "ruby.exe"
            | "perl.exe"
            | "php.exe"
            | "bash.exe"
            | "sh.exe"
            | "wsl.exe"
            | "conhost.exe"
            | "services.exe"
            | "taskhostw.exe"
            | "taskeng.exe"
            | "scriptrunner.exe"
            | "hh.exe"
            | "forfiles.exe"
            | "psexec.exe"
            | "psexec64.exe"
            | "nssm.exe"
            | "winsw.exe"
    ) || name.starts_with("python")
        || name.starts_with("winsw")
}

fn executable(raw: &str, service_command: bool) -> Option<String> {
    let raw = raw.trim();
    let program = if let Some(rest) = raw.strip_prefix('"') {
        let end = rest.find('"')?;
        let suffix = &rest[end + 1..];
        if !suffix.is_empty() && (!service_command || !suffix.starts_with(char::is_whitespace)) {
            return None;
        }
        &rest[..end]
    } else if service_command {
        // Unquoted spaces are ambiguous even if the entire text happens to end
        // in .exe: CreateProcess can choose C:\Program.exe instead.
        raw.split_whitespace().next()?
    } else {
        raw
    };
    let path = windows_path(program)?;
    (path.to_ascii_lowercase().ends_with(".exe") && !runner(&path)).then_some(path)
}

fn evidence(raw: &str, service_command: bool, owned: &Ownership) -> Option<String> {
    let path = executable(raw, service_command)?;
    let windows = windows_path(&owned.windows)?;
    let self_exe = windows_path(&owned.self_exe)?;
    let self_dir = self_exe.rsplit_once('\\')?.0;
    // Protect Windows and this application's own installation, even if the
    // caller accidentally supplies them as exclusive ownership evidence.
    if path_equal(&path, &windows)
        || below(&path, &windows)
        || path_equal(&path, &self_exe)
        || below(&path, self_dir)
        || path.split('\\').any(|p| {
            p.eq_ignore_ascii_case("system volume information")
                || p.eq_ignore_ascii_case("$recycle.bin")
        })
    {
        return None;
    }
    let file_match = owned
        .files
        .iter()
        .filter_map(|p| windows_path(p))
        .any(|p| path_equal(&p, &path));
    let root_match = owned
        .roots
        .iter()
        .filter_map(|p| windows_path(p))
        .any(|root| {
            let basename = root.rsplit('\\').next().unwrap_or("").to_ascii_lowercase();
            !matches!(
                basename.as_str(),
                "windows"
                    | "winnt"
                    | "system32"
                    | "syswow64"
                    | "program files"
                    | "program files (x86)"
                    | "programdata"
                    | "users"
                    | "common files"
            ) && !path_equal(&root, &windows)
                && !below(&root, &windows)
                && below(&path, &root)
        });
    (file_match || root_match).then_some(path)
}

fn safe_service_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"'))
        && !matches!(
            name.to_ascii_lowercase().as_str(),
            "rpcss"
                | "rpceptmapper"
                | "dcomlaunch"
                | "plugplay"
                | "power"
                | "eventlog"
                | "samss"
                | "lsm"
                | "schedule"
                | "winmgmt"
                | "bfe"
                | "mpssvc"
                | "windefend"
                | "wuauserv"
                | "trustedinstaller"
                | "cryptsvc"
                | "nsi"
                | "dhcp"
                | "dnscache"
                | "lanmanserver"
                | "lanmanworkstation"
                | "gpsvc"
                | "profsvc"
                | "user manager"
                | "usermanager"
                | "securityhealthservice"
                | "sense"
                | "waasmedicsvc"
        )
}

fn builtin_service_account(account: &str) -> bool {
    matches!(
        account.to_ascii_lowercase().as_str(),
        "localsystem"
            | "nt authority\\system"
            | "nt authority\\localservice"
            | "nt authority\\networkservice"
            | "nt authority\\local service"
            | "nt authority\\network service"
    )
}

fn task_path(path: &str) -> Option<(&str, &str)> {
    if !path.starts_with('\\')
        || path.ends_with('\\')
        || path.len() > 1024
        || path
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        || path[1..]
            .split('\\')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.ends_with(['.', ' ']))
    {
        return None;
    }
    let index = path.rfind('\\')?;
    Some((&path[..=index], &path[index + 1..]))
}

fn task_principal(task: &TaskSnapshot, owned: &Ownership) -> bool {
    matches!(
        (task.sid.as_str(), task.logon.as_str()),
        ("S-1-5-18" | "S-1-5-19" | "S-1-5-20", "ServiceAccount")
    ) || (!owned.current_sid.is_empty()
        && task.sid == owned.current_sid
        && task.logon == "InteractiveToken")
}

fn eligible(snapshot: &Snapshot, owned: &Ownership) -> Option<Vec<String>> {
    match snapshot {
        Snapshot::Service(s) => {
            if !safe_service_name(&s.name)
                || s.service_type != 0x10
                || s.state != 1
                || !(2..=4).contains(&s.start)
                || s.error_control > 1
                || s.dependents != 0
                || !s.group.is_empty()
                || s.tag != 0
                || !s.advanced_safe
                || !s.registry_supported
                || !builtin_service_account(&s.account)
                || s.security.is_empty()
            {
                return None;
            }
            Some(vec![evidence(&s.binary, true, owned)?])
        }
        Snapshot::Task(t) => {
            task_path(&t.path)?;
            if t.path.to_lowercase().starts_with("\\microsoft\\")
                || !t.supported
                || !matches!(t.state, 1 | 3)
                || !task_principal(t, owned)
                || t.xml.is_empty()
                || t.xml.len() as u64 > MAX_BACKUP
                || t.security.is_empty()
                || !matches!(t.run_level.as_str(), "LeastPrivilege" | "HighestAvailable")
                || t.actions.is_empty()
            {
                return None;
            }
            // Every action must be directly owned, not just one action. Arguments
            // and working directories are preserved but never ownership evidence.
            t.actions
                .iter()
                .map(|a| evidence(&a.command, false, owned))
                .collect()
        }
    }
}

fn parse_snapshots(json: &str) -> Result<Vec<Snapshot>, String> {
    serde_json::from_str(json.trim_start_matches('\u{feff}').trim())
        .map_err(|e| format!("invalid entry enumeration: {e}"))
}

fn relative_backup(path: &Path) -> bool {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return false;
    }
    // Inspect Windows syntax even when fixture tests run on another OS.
    let text = path.to_string_lossy();
    if text.contains([':', '\\']) || text.starts_with('/') {
        return false;
    }
    text.split('/').all(|p| {
        !p.is_empty()
            && p != "."
            && p != ".."
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            && !p.ends_with('.')
    }) && path.components().all(|p| matches!(p, Component::Normal(_)))
}

fn reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn checked_backup(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if !relative_backup(relative) {
        return Err("unsafe Rescue backup path".into());
    }
    let root_metadata = fs::symlink_metadata(root).map_err(|e| e.to_string())?;
    if !root_metadata.is_dir() || reparse(&root_metadata) {
        return Err("unsafe Rescue root".into());
    }
    let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if reparse(&metadata) {
            return Err("Rescue backup uses a reparse point".into());
        }
    }
    let canonical = path.canonicalize().map_err(|e| e.to_string())?;
    if !canonical.starts_with(&canonical_root)
        || canonical == canonical_root
        || !canonical.is_file()
    {
        return Err("backup is not a regular file inside Rescue".into());
    }
    Ok(canonical)
}

fn locked_backup(root: &Path, relative: &Path) -> Result<(fs::File, Vec<u8>), String> {
    let path = checked_backup(root, relative)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Keep the manifest/backup open during PowerShell execution: another
        // process must not rewrite or replace the already verified payload.
        options.share_mode(1).custom_flags(0x0020_0000); // READ only; OPEN_REPARSE_POINT
    }
    let mut file = options.open(&path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || reparse(&metadata)
        || metadata.len() == 0
        || metadata.len() > MAX_BACKUP
        || checked_backup(root, relative)? != path
    {
        return Err("invalid or changed Rescue backup".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_BACKUP + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_BACKUP {
        return Err("invalid Rescue backup size".into());
    }
    Ok((file, bytes))
}

fn write_new(path: &Path, contents: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(contents)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}

fn expected_paths(record_id: u64, index: usize, task: bool) -> (PathBuf, PathBuf) {
    let dir = format!("{record_id}/uninstall-entries/{index}");
    let backup = if task {
        PathBuf::from(format!("{dir}/task.xml"))
    } else {
        PathBuf::from(format!(
            "{record_id}/registry/uninstall-service-{index}.reg"
        ))
    };
    (PathBuf::from(format!("{dir}/undo.json")), backup)
}

#[cfg(any(windows, test))]
fn task_backup_xml(xml: &str) -> String {
    let xml = xml.strip_prefix('\u{feff}').unwrap_or(xml);
    let Some(rest) = xml.strip_prefix("<?xml") else {
        return xml.into();
    };
    if !rest.starts_with(char::is_whitespace) {
        return xml.into();
    }
    let Some(end) = xml.find("?>") else {
        return xml.into();
    };
    let declaration = &xml[..end];
    let Some(attribute) = declaration.find("encoding") else {
        return xml.into();
    };
    let Some(value) = declaration[attribute + "encoding".len()..]
        .trim_start()
        .strip_prefix('=')
    else {
        return xml.into();
    };
    let value = value.trim_start();
    let Some(quote) = value.chars().next().filter(|c| matches!(c, '\'' | '"')) else {
        return xml.into();
    };
    let Some(length) = value[1..].find(quote) else {
        return xml.into();
    };
    let start = declaration.len() - value.len() + 1;
    format!("{}UTF-8{}", &xml[..start], &xml[start + length..])
}

#[cfg(windows)]
fn targets_unaliased(targets: &[String]) -> Result<bool, String> {
    for target in targets {
        // Missing executables are legitimate leftovers; existing ancestors must
        // not redirect an app-owned spelling into Windows or a shared directory.
        let path = Path::new(target);
        let ancestors: Vec<_> = path.ancestors().collect();
        for component in ancestors.into_iter().rev() {
            match fs::symlink_metadata(component) {
                Ok(metadata) if reparse(&metadata) => return Ok(false),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("cannot verify executable path: {e}")),
            }
        }
    }
    Ok(true)
}

#[cfg(any(windows, test))]
fn ps_quote(text: &str) -> String {
    use std::fmt::Write as _;
    let json = serde_json::to_string(text).expect("string serialization cannot fail");
    let mut scalar = String::with_capacity(json.len());
    for c in json.chars() {
        if c == '\'' || !c.is_ascii() {
            for unit in c.encode_utf16(&mut [0; 2]) {
                write!(scalar, "\\u{unit:04x}").unwrap();
            }
        } else {
            scalar.push(c);
        }
    }
    format!("(ConvertFrom-Json -InputObject '{scalar}')")
}

#[cfg(any(windows, test))]
fn ps_wrapped(script: &str) -> String {
    format!(
        "$ErrorActionPreference='Stop'; $WarningPreference='Stop'; Set-StrictMode -Version Latest; \
         [Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false); \
         try {{ {script} }} catch {{ [Console]::Error.WriteLine($_.Exception.ToString()); exit 1 }}"
    )
}

#[cfg(any(windows, test))]
fn quoted_command_len(text: &str) -> usize {
    // CreateProcess counts the escaped argument, not just the script's UTF-16.
    let mut length = 2;
    let mut slashes = 0;
    for unit in text.encode_utf16() {
        length += 1;
        match unit {
            0x5c => slashes += 1,
            0x22 => {
                length += slashes + 1;
                slashes = 0;
            }
            _ => slashes = 0,
        }
    }
    length + slashes
}

#[cfg(windows)]
fn powershell(script: &str) -> Result<String, String> {
    use crate::winutil::{self, Known};
    use std::time::Duration;
    let windows = winutil::known_folder(Known::Windows).ok_or("cannot locate Windows")?;
    let exe = windows.join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let wrapped = ps_wrapped(script);
    let args = [
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        &wrapped,
    ];
    let command_len = quoted_command_len(&exe.to_string_lossy())
        + args
            .iter()
            .map(|arg| 1 + quoted_command_len(arg))
            .sum::<usize>()
        + 1;
    if command_len > 32_767 {
        return Err("PowerShell command too large".into());
    }
    winutil::run_hidden(&exe.to_string_lossy(), &args, Duration::from_secs(90), None)
}

/// Main must show these entries unchecked. `owned_*` must already be app-exclusive.
#[cfg(windows)]
pub fn scan(owned_roots: &[PathBuf], owned_files: &[PathBuf]) -> Result<Vec<Entry>, String> {
    use crate::winutil::{self, Known};
    if owned_roots.is_empty() && owned_files.is_empty() {
        return Ok(Vec::new());
    }
    let ownership = Ownership {
        roots: owned_roots
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        files: owned_files
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        windows: winutil::known_folder(Known::Windows)
            .ok_or("cannot locate Windows")?
            .to_string_lossy()
            .into_owned(),
        current_sid: powershell("[Security.Principal.WindowsIdentity]::GetCurrent().User.Value")?
            .trim()
            .to_string(),
        self_exe: std::env::current_exe()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned(),
    };
    let snapshots = parse_snapshots(&powershell(&format!(
        "{SCM_SCRIPT}\n{METADATA_SCRIPT}\n{ENUMERATE_SCRIPT}"
    ))?)?;
    let mut entries = Vec::new();
    for snapshot in snapshots {
        let Some(targets) = eligible(&snapshot, &ownership) else {
            continue;
        };
        if !targets_unaliased(&targets)? {
            continue;
        }
        let (kind, path) = match &snapshot {
            Snapshot::Service(s) => ("service", s.name.clone()),
            Snapshot::Task(t) => ("task", t.path.clone()),
        };
        entries.push(Entry {
            kind,
            path,
            detail: targets.join("; "),
            snapshot,
            ownership: ownership.clone(),
        });
    }
    entries.sort_by(|a, b| (a.kind, a.path.to_lowercase()).cmp(&(b.kind, b.path.to_lowercase())));
    Ok(entries)
}

#[cfg(not(windows))]
pub fn scan(_owned_roots: &[PathBuf], _owned_files: &[PathBuf]) -> Result<Vec<Entry>, String> {
    Err("chỉ hỗ trợ Windows".into())
}

#[cfg(windows)]
fn observe(snapshot: &Snapshot) -> Result<Snapshot, String> {
    let selector = match snapshot {
        Snapshot::Service(s) => format!("Read-Service {}", ps_quote(&s.name)),
        Snapshot::Task(t) => format!("Read-Task {}", ps_quote(&t.path)),
    };
    let json = powershell(&format!(
        "{SCM_SCRIPT}\n{METADATA_SCRIPT}\n{selector} | ConvertTo-Json -Depth 32 -Compress"
    ))?;
    serde_json::from_str(json.trim_start_matches('\u{feff}').trim()).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn make_backup_directory(rescue: &Rescue, record_id: u64, index: usize) -> Result<PathBuf, String> {
    fs::create_dir_all(rescue.dir()).map_err(|e| e.to_string())?;
    let mut dir = rescue.dir().to_path_buf();
    let metadata = fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || reparse(&metadata) {
        return Err("unsafe Rescue root".into());
    }
    for component in [record_id.to_string(), "uninstall-entries".into()] {
        dir.push(component);
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.to_string()),
        }
        let metadata = fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
        if !metadata.is_dir() || reparse(&metadata) {
            return Err("unsafe Rescue directory".into());
        }
    }
    dir.push(index.to_string());
    // A repeated index must not replace a previous recovery payload.
    fs::create_dir(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

#[cfg(windows)]
pub fn remove(
    entry: &Entry,
    rescue: &Rescue,
    record_id: u64,
    index: usize,
    before_remove: &mut dyn FnMut(&EntryUndo) -> Result<(), String>,
) -> Result<EntryUndo, String> {
    if eligible(&entry.snapshot, &entry.ownership).is_none() {
        return Err("entry is not safely removable".into());
    }
    let current = observe(&entry.snapshot)?;
    if current != entry.snapshot || eligible(&current, &entry.ownership).is_none() {
        return Err("entry changed since scan; scan again".into());
    }
    let targets = eligible(&current, &entry.ownership).ok_or("entry lost ownership evidence")?;
    if !targets_unaliased(&targets)? {
        return Err("executable path uses a reparse point".into());
    }
    let dir = make_backup_directory(rescue, record_id, index)?;
    let (manifest, backup) = expected_paths(record_id, index, matches!(current, Snapshot::Task(_)));
    match &current {
        Snapshot::Task(task) => {
            write_new(&dir.join("task.xml"), task_backup_xml(&task.xml).as_bytes())?
        }
        Snapshot::Service(service) => {
            let registry_dir = rescue.record_dir(record_id).join("registry");
            match fs::create_dir(&registry_dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.to_string()),
            }
            let metadata = fs::symlink_metadata(&registry_dir).map_err(|e| e.to_string())?;
            if !metadata.is_dir() || reparse(&metadata) {
                return Err("unsafe registry backup directory".into());
            }
            let name = format!("uninstall-service-{index}.reg");
            match fs::symlink_metadata(registry_dir.join(&name)) {
                Ok(_) => return Err("service backup already exists".into()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
            let key = format!(
                "HKLM\\SYSTEM\\CurrentControlSet\\Services\\{}",
                service.name
            );
            crate::rescue::export_registry_key(rescue, record_id, &key, &name)?;
            // Export is archival only: restore never blindly imports a .reg file.
            let path = checked_backup(rescue.dir(), &backup)?;
            OpenOptions::new()
                .write(true)
                .open(path)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
    }
    let (_backup_lock, backup_bytes) = locked_backup(rescue.dir(), &backup)?;
    if let Snapshot::Task(task) = &current {
        if backup_bytes != task_backup_xml(&task.xml).as_bytes() {
            return Err("task XML backup changed".into());
        }
    }
    let undo = EntryUndo {
        version: FORMAT,
        record_id,
        index,
        snapshot: current,
        ownership: entry.ownership.clone(),
        manifest,
        backup,
    };
    let json = serde_json::to_vec_pretty(&undo).map_err(|e| e.to_string())?;
    if json.len() as u64 > MAX_BACKUP {
        return Err("undo payload is too large to restore safely".into());
    }
    write_new(&dir.join("undo.json"), &json)?;
    let (_manifest_lock, persisted) = locked_backup(rescue.dir(), &undo.manifest)?;
    if serde_json::from_slice::<EntryUndo>(&persisted).map_err(|e| e.to_string())? != undo {
        return Err("undo payload changed before deletion".into());
    }
    let payload = checked_backup(rescue.dir(), &undo.manifest)?;
    let script = format!("{SCM_SCRIPT}\n{METADATA_SCRIPT}\n$p=Get-Content -LiteralPath {} -Raw -Encoding UTF8 | ConvertFrom-Json;\n{REMOVE_SCRIPT}", ps_quote(&payload.to_string_lossy()));
    // History must be durable even if deletion succeeds but PowerShell times out.
    before_remove(&undo)?;
    powershell(&script)?;
    Ok(undo)
}

#[cfg(not(windows))]
pub fn remove(
    _entry: &Entry,
    _rescue: &Rescue,
    _record_id: u64,
    _index: usize,
    _before_remove: &mut dyn FnMut(&EntryUndo) -> Result<(), String>,
) -> Result<EntryUndo, String> {
    Err("chỉ hỗ trợ Windows".into())
}

#[cfg(windows)]
pub fn restore(undo: &EntryUndo, rescue_root: &Path) -> Result<(), String> {
    use crate::winutil::{self, Known};
    let mut safety = undo.ownership.clone();
    safety.windows = winutil::known_folder(Known::Windows)
        .ok_or("cannot locate Windows")?
        .to_string_lossy()
        .into_owned();
    safety.self_exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned();
    if undo.version != FORMAT || eligible(&undo.snapshot, &safety).is_none() {
        return Err("unsupported or unsafe entry undo".into());
    }
    let targets = eligible(&undo.snapshot, &safety).ok_or("unsafe undo evidence")?;
    if !targets_unaliased(&targets)? {
        return Err("executable path uses a reparse point".into());
    }
    let expected = expected_paths(
        undo.record_id,
        undo.index,
        matches!(undo.snapshot, Snapshot::Task(_)),
    );
    if (undo.manifest.clone(), undo.backup.clone()) != expected {
        return Err("unexpected entry backup paths".into());
    }
    let (_manifest_lock, payload_bytes) = locked_backup(rescue_root, &undo.manifest)?;
    let persisted: EntryUndo = serde_json::from_slice(&payload_bytes).map_err(|e| e.to_string())?;
    if persisted != *undo {
        return Err("undo payload differs from persisted backup".into());
    }
    let (_backup_lock, backup) = locked_backup(rescue_root, &undo.backup)?;
    if let Snapshot::Task(task) = &undo.snapshot {
        if backup != task_backup_xml(&task.xml).as_bytes() {
            return Err("task XML backup changed".into());
        }
        let sid = powershell("[Security.Principal.WindowsIdentity]::GetCurrent().User.Value")?;
        if task.logon == "InteractiveToken" && sid.trim() != task.sid {
            return Err("task requires the original interactive user".into());
        }
    }
    let manifest = checked_backup(rescue_root, &undo.manifest)?;
    powershell(&format!("{SCM_SCRIPT}\n{METADATA_SCRIPT}\n$p=Get-Content -LiteralPath {} -Raw -Encoding UTF8 | ConvertFrom-Json;\n{RESTORE_SCRIPT}", ps_quote(&manifest.to_string_lossy())))?;
    Ok(())
}

#[cfg(not(windows))]
pub fn restore(_undo: &EntryUndo, _rescue_root: &Path) -> Result<(), String> {
    Err("chỉ hỗ trợ Windows".into())
}

// Native SCM access is kept inside a fixed, in-memory PowerShell Add-Type script:
// no extra windows crate features, external script files, policies or dependencies.
// Advanced features are queried, not inferred from localized sc.exe output.
#[cfg(any(windows, test))]
const SCM_SCRIPT: &str = r#"
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
namespace TPUninstall {
 public sealed class ServiceInfo {
  public string Name, DisplayName, Binary, Account, Group, Description, Security;
  public uint Start, ErrorControl, ServiceType, State, Dependents, Tag;
  public string[] Dependencies;
  public bool AdvancedSafe;
 }
 public static class Scm {
  [StructLayout(LayoutKind.Sequential)] struct Config {
   public uint type,start,error; public IntPtr binary,group; public uint tag;
   public IntPtr dependencies,account,display;
  }
  [StructLayout(LayoutKind.Sequential)] struct Status {
   public uint type,state,accepted,exit,specific,checkpoint,hint;
  }
  [StructLayout(LayoutKind.Sequential)] struct Failure {
   public uint reset; public IntPtr reboot,command; public uint count; public IntPtr actions;
  }
  [StructLayout(LayoutKind.Sequential)] struct Triggers { public uint count; public IntPtr data,reserved; }
  [StructLayout(LayoutKind.Sequential)] struct Description { public IntPtr text; }
  [StructLayout(LayoutKind.Sequential)] struct PreferredNode { public ushort usPreferredNode; public byte fDelete; }
  [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr OpenSCManager(string machine,string database,uint access);
  [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr OpenService(IntPtr scm,string name,uint access);
  [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool QueryServiceConfig(IntPtr s,IntPtr buf,uint size,out uint needed);
  [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool QueryServiceConfig2(IntPtr s,uint level,IntPtr buf,uint size,out uint needed);
  [DllImport("advapi32.dll",SetLastError=true)] static extern bool QueryServiceStatus(IntPtr s,out Status status);
  [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool EnumDependentServices(IntPtr s,uint state,IntPtr buf,uint size,out uint needed,out uint count);
  [DllImport("advapi32.dll",SetLastError=true)] static extern bool QueryServiceObjectSecurity(IntPtr s,uint info,byte[] buf,uint size,out uint needed);
  [DllImport("advapi32.dll",SetLastError=true)] static extern bool SetServiceObjectSecurity(IntPtr s,uint info,byte[] descriptor);
  [DllImport("advapi32.dll",SetLastError=true)] static extern bool DeleteService(IntPtr s);
  [DllImport("advapi32.dll",SetLastError=true)] static extern bool CloseServiceHandle(IntPtr s);
  [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr CreateService(IntPtr scm,string name,string display,uint access,uint type,uint start,uint error,string binary,string group,IntPtr tag,string dependencies,string account,string password);
  [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool ChangeServiceConfig2(IntPtr s,uint level,ref Description desc);
  static Exception Error() { return new Win32Exception(Marshal.GetLastWin32Error()); }
  static string Text(IntPtr p) { return p==IntPtr.Zero ? "" : Marshal.PtrToStringUni(p); }
  static IntPtr Manager(uint rights) { IntPtr p=OpenSCManager(null,null,rights); if(p==IntPtr.Zero) throw Error(); return p; }
  static IntPtr Service(IntPtr m,string name,uint rights) { IntPtr p=OpenService(m,name,rights); if(p==IntPtr.Zero) throw Error(); return p; }
  static IntPtr Info(IntPtr s,uint level) {
   uint size; QueryServiceConfig2(s,level,IntPtr.Zero,0,out size);
   if(Marshal.GetLastWin32Error()!=122 || size==0 || size>1048576) throw Error();
   IntPtr p=Marshal.AllocHGlobal((int)size);
   if(!QueryServiceConfig2(s,level,p,size,out size)) { Exception e=Error(); Marshal.FreeHGlobal(p); throw e; }
   return p;
  }
  static uint Number(IntPtr s,uint level) { IntPtr p=Info(s,level); try{return (uint)Marshal.ReadInt32(p);} finally{Marshal.FreeHGlobal(p);} }
  static bool Advanced(IntPtr s) {
   bool safe=true; IntPtr p=Info(s,2);
   try { Failure f=(Failure)Marshal.PtrToStructure(p,typeof(Failure)); safe=f.reset==0 && f.count==0 && Text(f.reboot)=="" && Text(f.command)==""; }
   finally { Marshal.FreeHGlobal(p); }
   // Delayed auto-start, failure-on-noncrash, service SID and required privileges.
   safe &= Number(s,3)==0; safe &= Number(s,4)==0; safe &= Number(s,5)==0;
   p=Info(s,6); try { safe &= Text(Marshal.ReadIntPtr(p))==""; } finally{Marshal.FreeHGlobal(p);}
   safe &= Number(s,7)==180000; // Default preshutdown timeout.
   p=Info(s,8); try { safe &= ((Triggers)Marshal.PtrToStructure(p,typeof(Triggers))).count==0; } finally{Marshal.FreeHGlobal(p);}
   p=Info(s,9); try { safe &= ((PreferredNode)Marshal.PtrToStructure(p,typeof(PreferredNode))).usPreferredNode==ushort.MaxValue; } finally{Marshal.FreeHGlobal(p);} // fDelete does not establish absent affinity.
   safe &= Number(s,12)==0; // Not launch-protected.
   return safe;
  }
  static ServiceInfo ReadHandle(IntPtr s,string name) {
   uint needed; QueryServiceConfig(s,IntPtr.Zero,0,out needed);
   if(Marshal.GetLastWin32Error()!=122 || needed==0 || needed>1048576) throw Error();
   IntPtr p=Marshal.AllocHGlobal((int)needed); Config c;
   try { if(!QueryServiceConfig(s,p,needed,out needed)) throw Error(); c=(Config)Marshal.PtrToStructure(p,typeof(Config));
    ServiceInfo r=new ServiceInfo(); r.Name=name; r.DisplayName=Text(c.display); r.Binary=Text(c.binary);
    r.Account=Text(c.account); r.Start=c.start; r.ErrorControl=c.error; r.ServiceType=c.type;
    r.Group=Text(c.group); r.Tag=c.tag; List<string> deps=new List<string>();
    IntPtr d=c.dependencies; while(d!=IntPtr.Zero && Marshal.ReadInt16(d)!=0) { string v=Text(d); deps.Add(v); d=IntPtr.Add(d,(v.Length+1)*2); }
    r.Dependencies=deps.ToArray(); Status status; if(!QueryServiceStatus(s,out status)) throw Error(); r.State=status.state;
    uint count; bool ok=EnumDependentServices(s,3,IntPtr.Zero,0,out needed,out count);
    if(!ok) {
     if(Marshal.GetLastWin32Error()!=234 || needed==0 || needed>1048576) throw Error();
     IntPtr dependentBuffer=Marshal.AllocHGlobal((int)needed);
     try { if(!EnumDependentServices(s,3,dependentBuffer,needed,out needed,out count)) throw Error(); }
     finally { Marshal.FreeHGlobal(dependentBuffer); }
    }
    r.Dependents=count;
    IntPtr desc=Info(s,1); try{r.Description=Text(Marshal.ReadIntPtr(desc));} finally{Marshal.FreeHGlobal(desc);}
    QueryServiceObjectSecurity(s,7,null,0,out needed);
    if(Marshal.GetLastWin32Error()!=122 || needed==0 || needed>1048576) throw Error();
    byte[] security=new byte[needed]; if(!QueryServiceObjectSecurity(s,7,security,needed,out needed)) throw Error();
    r.Security=Convert.ToBase64String(security); r.AdvancedSafe=Advanced(s); return r;
   } finally{Marshal.FreeHGlobal(p);}
  }
  public static ServiceInfo Read(string name) {
   IntPtr m=Manager(1); try { IntPtr s=Service(m,name,0x2000d); try{return ReadHandle(s,name);} finally{CloseServiceHandle(s);} } finally{CloseServiceHandle(m);}
  }
  static bool Equal(ServiceInfo a,ServiceInfo b) {
   return a.Name==b.Name && a.DisplayName==b.DisplayName && a.Binary==b.Binary && a.Account==b.Account
    && a.Start==b.Start && a.ErrorControl==b.ErrorControl && a.ServiceType==b.ServiceType && a.State==b.State
    && a.Dependents==b.Dependents && String.Join("\0",a.Dependencies)==String.Join("\0",b.Dependencies)
    && a.Group==b.Group && a.Tag==b.Tag && a.Description==b.Description && a.Security==b.Security && a.AdvancedSafe==b.AdvancedSafe;
  }
  public static void Remove(ServiceInfo expected) {
   IntPtr m=Manager(1); try { IntPtr s=Service(m,expected.Name,0x3000d); try {
    ServiceInfo now=ReadHandle(s,expected.Name);
    if(!Equal(now,expected) || now.State!=1 || now.ServiceType!=16 || now.Dependents!=0 || !now.AdvancedSafe) throw new InvalidOperationException("Service changed or is not stopped/safe");
    if(!DeleteService(s)) throw Error();
   } finally{CloseServiceHandle(s);} } finally{CloseServiceHandle(m);}
  }
  public static void Create(string name,string display,string binary,uint start,uint error,string account,string[] dependencies,string description,string security) {
   IntPtr m=Manager(3); try {
    IntPtr old=OpenService(m,name,1); if(old!=IntPtr.Zero) {CloseServiceHandle(old);throw new InvalidOperationException("Service already exists");}
    if(Marshal.GetLastWin32Error()!=1060) throw Error();
    string deps=dependencies.Length==0 ? null : String.Join("\0",dependencies)+"\0\0";
    IntPtr s=CreateService(m,name,display,0xf01ff,16,start,error,binary,null,IntPtr.Zero,deps,account,null);
    if(s==IntPtr.Zero) throw Error();
    try {
     if(description!="") { Description d=new Description(); d.text=Marshal.StringToHGlobalUni(description); try{if(!ChangeServiceConfig2(s,1,ref d)) throw Error();} finally{Marshal.FreeHGlobal(d.text);} }
     if(!SetServiceObjectSecurity(s,7,Convert.FromBase64String(security))) throw Error();
    } catch(Exception e) { throw new InvalidOperationException("Service was created stopped, but configuration restoration failed; existing service will NOT be overwritten on retry",e); }
    finally{CloseServiceHandle(s);}
   } finally{CloseServiceHandle(m);}
  }
 }
}
'@ -ErrorAction Stop
"#;

#[cfg(any(windows, test))]
const METADATA_SCRIPT: &str = r#"
function Task-Metadata([string]$xml) {
 $settings=New-Object System.Xml.XmlReaderSettings
 $settings.DtdProcessing=[System.Xml.DtdProcessing]::Prohibit; $settings.XmlResolver=$null
 $reader=[System.Xml.XmlReader]::Create((New-Object System.IO.StringReader($xml)),$settings)
 $doc=New-Object System.Xml.XmlDocument; $doc.XmlResolver=$null
 try { $doc.Load($reader) } finally { $reader.Dispose() }
 $ns=New-Object System.Xml.XmlNamespaceManager($doc.NameTable)
 $uri='http://schemas.microsoft.com/windows/2004/02/mit/task'; $ns.AddNamespace('t',$uri)
 if($doc.DocumentElement.LocalName -cne 'Task' -or $doc.DocumentElement.NamespaceURI -cne $uri) { throw 'Invalid task XML namespace' }
 $principals=@($doc.SelectNodes('/t:Task/t:Principals/t:Principal',$ns))
 $containers=@($doc.SelectNodes('/t:Task/t:Actions',$ns))
 $supported=$principals.Count -eq 1 -and $containers.Count -eq 1
 $user=''; $logon=''; $level='LeastPrivilege'; $actions=@()
 if($principals.Count -eq 1) {
  $p=$principals[0]
  $users=@($p.SelectNodes('t:UserId',$ns)); $logs=@($p.SelectNodes('t:LogonType',$ns)); $levels=@($p.SelectNodes('t:RunLevel',$ns))
  $supported=$supported -and $users.Count -eq 1 -and $logs.Count -eq 1 -and $levels.Count -le 1 -and @($p.SelectNodes('t:GroupId',$ns)).Count -eq 0
  if($users.Count -eq 1) {$user=$users[0].InnerText}
  if($logs.Count -eq 1) {$logon=$logs[0].InnerText}
  if($levels.Count -eq 1) {$level=$levels[0].InnerText}
 }
 if($containers.Count -eq 1) {
  $context=$containers[0].GetAttribute('Context')
  if($context -ne '' -and ($principals.Count -ne 1 -or $context -cne $principals[0].GetAttribute('id'))) {$supported=$false}
  foreach($a in $containers[0].ChildNodes) {
   if($a.NodeType -ne [System.Xml.XmlNodeType]::Element) {continue}
   if($a.LocalName -cne 'Exec' -or $a.NamespaceURI -cne $uri) {$supported=$false;continue}
   $commands=@($a.SelectNodes('t:Command',$ns)); $args=@($a.SelectNodes('t:Arguments',$ns)); $dirs=@($a.SelectNodes('t:WorkingDirectory',$ns))
   if($commands.Count -ne 1 -or $args.Count -gt 1 -or $dirs.Count -gt 1) {$supported=$false;continue}
   foreach($child in $a.ChildNodes) { if($child.NodeType -eq [System.Xml.XmlNodeType]::Element -and ($child.NamespaceURI -cne $uri -or $child.LocalName -cnotin @('Command','Arguments','WorkingDirectory'))) {$supported=$false} }
   $arguments=''; $directory=''; if($args.Count -eq 1){$arguments=$args[0].InnerText}; if($dirs.Count -eq 1){$directory=$dirs[0].InnerText}
   $actions+=,[ordered]@{command=$commands[0].InnerText;arguments=$arguments;working_directory=$directory}
  }
 }
 [ordered]@{user=$user;logon=$logon;run_level=$level;actions=@($actions);supported=[bool]$supported}
}
function Resolve-Sid([string]$user) {
 if($user -match '^S-1-') { return (New-Object Security.Principal.SecurityIdentifier($user)).Value }
 $account=New-Object Security.Principal.NTAccount($user)
 $account.Translate([Security.Principal.SecurityIdentifier]).Value
}
function Connect-Scheduler {
 $scheduler=New-Object -ComObject 'Schedule.Service'; $scheduler.Connect(); return $scheduler
}
function No-AuditAcl($descriptor) {
 try {
  if($descriptor -is [byte[]]) {$raw=New-Object Security.AccessControl.RawSecurityDescriptor($descriptor,0)}
  elseif($descriptor -is [string] -and $descriptor.Length -ne 0) {$raw=New-Object Security.AccessControl.RawSecurityDescriptor($descriptor)}
  else {return $false}
  return $null -eq $raw.SystemAcl -or $raw.SystemAcl.Count -eq 0
 } catch {return $false}
}
function Service-SecurityMetadata($key) {
 if($null -eq $key) {throw 'Cannot read service security key'}
 $supported=$key.GetSubKeyNames().Length -eq 0; $values=@()
 foreach($valueName in $key.GetValueNames()) {
  $kind=$key.GetValueKind($valueName)
  $v=$key.GetValue($valueName,$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
  if($valueName -cne 'Security' -or $kind -ne [Microsoft.Win32.RegistryValueKind]::Binary -or !(No-AuditAcl $v)) {$supported=$false}
  $values+=,[ordered]@{name=('Security\'+$valueName);value_type=$kind.ToString();data=(ConvertTo-Json -InputObject $v -Depth 8 -Compress)}
 }
 [ordered]@{supported=[bool]$supported;values=@($values)}
}
function Snapshot-Task($registered) {
 $xml=[string]$registered.Xml; $meta=Task-Metadata $xml; $sid=''
 # Audit state is not part of the restorable descriptor; unknown is not safe.
 try {if(!(No-AuditAcl ($registered.GetSecurityDescriptor(8)))) {$meta.supported=$false}}
 catch {$meta.supported=$false}
 # Password/S4U/group definitions are excluded without trying unavailable
 # domain credentials merely to enumerate them.
 if($meta.supported -and $meta.logon -cin @('ServiceAccount','InteractiveToken')) {$sid=Resolve-Sid $meta.user}
 [ordered]@{kind='task';definition=[ordered]@{
  path=[string]$registered.Path;xml=$xml;security=[string]$registered.GetSecurityDescriptor(7)
  state=[uint32]$registered.State;user=$meta.user;sid=$sid;logon=$meta.logon
  run_level=$meta.run_level;actions=@($meta.actions);supported=$meta.supported
 }}
}
function Read-Task([string]$path) {
 $scheduler=Connect-Scheduler; $scheduler.GetFolder('\').GetTask($path) | ForEach-Object { Snapshot-Task $_ }
}
function Read-Service([string]$name) {
 $s=[TPUninstall.Scm]::Read($name)
 $base=[Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine,[Microsoft.Win32.RegistryView]::Registry64)
 $key=$null; $values=@(); $supported=$true
 try {
  $key=$base.OpenSubKey('SYSTEM\CurrentControlSet\Services\'+$name,$false)
  if($null -eq $key) {throw 'Service registry key missing'}
  foreach($child in $key.GetSubKeyNames()) {
   if($child -cne 'Security') {$supported=$false;continue}
   $securityKey=$key.OpenSubKey($child,$false)
   try {
    $securityMeta=Service-SecurityMetadata $securityKey
    if(!$securityMeta.supported) {$supported=$false}
    $values+=@($securityMeta.values)
   } finally {if($null -ne $securityKey){$securityKey.Dispose()}}
  }
  foreach($valueName in @($key.GetValueNames() | Sort-Object)) {
   $kind=$key.GetValueKind($valueName).ToString()
   if($valueName -cnotin @('Type','Start','ErrorControl','ImagePath','DisplayName','ObjectName','DependOnService','Description')) {$supported=$false}
   $v=$key.GetValue($valueName,$null,[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
   # Direct registry edits can diverge from SCM's cached definition. Do not
   # delete a definition whose raw configuration cannot be recreated by SCM.
   switch -CaseSensitive ($valueName) {
    'Type' {$supported=$supported -and $kind -ceq 'DWord' -and [uint32]$v -eq $s.ServiceType}
    'Start' {$supported=$supported -and $kind -ceq 'DWord' -and [uint32]$v -eq $s.Start}
    'ErrorControl' {$supported=$supported -and $kind -ceq 'DWord' -and [uint32]$v -eq $s.ErrorControl}
    'ImagePath' {$supported=$supported -and $kind -cin @('String','ExpandString') -and [string]$v -ceq $s.Binary}
    'DisplayName' {$supported=$supported -and $kind -ceq 'String' -and [string]$v -ceq $s.DisplayName}
    'ObjectName' {$supported=$supported -and $kind -ceq 'String' -and [string]$v -ceq $s.Account}
    'Description' {$supported=$supported -and $kind -ceq 'String' -and [string]$v -ceq $s.Description}
    'DependOnService' {$supported=$supported -and $kind -ceq 'MultiString' -and (Same @($v) @($s.Dependencies))}
    default {$supported=$false}
   }
   $values+=,[ordered]@{name=$valueName;value_type=$kind;data=(ConvertTo-Json -InputObject $v -Depth 8 -Compress)}
  }
 } finally {if($null -ne $key){$key.Dispose()};$base.Dispose()}
 [ordered]@{kind='service';definition=[ordered]@{
  name=$s.Name;display_name=$s.DisplayName;binary=$s.Binary;account=$s.Account
  start=$s.Start;error_control=$s.ErrorControl;service_type=$s.ServiceType;state=$s.State;dependents=$s.Dependents
  dependencies=@($s.Dependencies);group=$s.Group;tag=$s.Tag;description=$s.Description;security=$s.Security
  advanced_safe=$s.AdvancedSafe;registry_supported=[bool]$supported;registry=@($values)
 }}
}
function Same($a,$b) {
 if($null -eq $a -or $null -eq $b) {return $null -eq $a -and $null -eq $b}
 if($a -is [System.Collections.IDictionary]) {$a=[pscustomobject]$a}
 if($b -is [System.Collections.IDictionary]) {$b=[pscustomobject]$b}
 if($a -is [Array] -or $b -is [Array]) {
  if($a -isnot [Array] -or $b -isnot [Array] -or $a.Count -ne $b.Count) {return $false}
  for($i=0;$i -lt $a.Count;$i++){if(!(Same $a[$i] $b[$i])){return $false}};return $true
 }
 if($a -is [pscustomobject] -or $b -is [pscustomobject]) {
  if($a -isnot [pscustomobject] -or $b -isnot [pscustomobject]) {return $false}
  $names=@($a.PSObject.Properties.Name); $other=@($b.PSObject.Properties.Name)
  if($names.Count -ne $other.Count){return $false}
  foreach($n in $names){if($n -cnotin $other -or !(Same $a.$n $b.$n)){return $false}};return $true
 }
 return [string]::Equals([string]$a,[string]$b,[StringComparison]::Ordinal)
}
"#;

#[cfg(any(windows, test))]
const ENUMERATE_SCRIPT: &str = r#"
$found=New-Object 'System.Collections.Generic.List[object]'
# Never query unsupported driver/share-process configuration levels. Errors
# enumerating or reading any own-process definition abort the whole scan.
foreach($service in @(Get-Service -ErrorAction Stop)) {
 if([int]$service.ServiceType -eq 16 -and [int]$service.Status -eq 1) { $found.Add((Read-Service $service.Name)) }
 $service.Dispose()
}
$scheduler=Connect-Scheduler
function Walk-Tasks($folder) {
 foreach($registered in $folder.GetTasks(1)) { $found.Add((Snapshot-Task $registered)) }
 foreach($child in $folder.GetFolders(0)) { Walk-Tasks $child }
}
Walk-Tasks ($scheduler.GetFolder('\'))
ConvertTo-Json -InputObject @($found.ToArray()) -Depth 32 -Compress
"#;

#[cfg(any(windows, test))]
const REMOVE_SCRIPT: &str = r#"
$d=$p.snapshot.definition
if($p.snapshot.kind -ceq 'service') {
 $now=Read-Service $d.name
 if(!(Same $now $p.snapshot)) {throw 'Service changed since backup'}
 $expected=New-Object TPUninstall.ServiceInfo
 $expected.Name=$d.name; $expected.DisplayName=$d.display_name; $expected.Binary=$d.binary; $expected.Account=$d.account
 $expected.Start=$d.start; $expected.ErrorControl=$d.error_control; $expected.ServiceType=$d.service_type
 $expected.State=$d.state; $expected.Dependents=$d.dependents; $expected.Dependencies=[string[]]@($d.dependencies)
 $expected.Group=$d.group; $expected.Tag=$d.tag; $expected.Description=$d.description; $expected.Security=$d.security; $expected.AdvancedSafe=$d.advanced_safe
 [TPUninstall.Scm]::Remove($expected)
 # DeleteService has succeeded, not merely disabled a service. SCM may retain
 # a deletion-pending definition until other processes close their handles.
 # Return its undo payload anyway; restore safely refuses it until it is gone.
} elseif($p.snapshot.kind -ceq 'task') {
 $now=Read-Task $d.path
 if(!(Same $now $p.snapshot)) {throw 'Task changed since backup'}
 $split=$d.path.LastIndexOf('\'); $folder=$d.path.Substring(0,$split+1); $name=$d.path.Substring($split+1)
 $candidates=@(Get-ScheduledTask -ErrorAction Stop | Where-Object { $_.TaskPath -ceq $folder -and $_.TaskName -ceq $name })
 if($candidates.Count -ne 1) {throw 'Task selection is ambiguous'}
 # Re-read after CIM lookup too. InputObject avoids wildcard task selection.
 if(!(Same (Read-Task $d.path) $p.snapshot)) {throw 'Task changed before unregister'}
 Unregister-ScheduledTask -InputObject $candidates[0] -Confirm:$false -ErrorAction Stop
 $scheduler=Connect-Scheduler
 $remaining=@($scheduler.GetFolder($folder).GetTasks(1) | Where-Object {$_.Path -ceq $d.path})
 if($remaining.Count -ne 0) {throw 'Task remains registered; undo.json is persisted'}
} else {throw 'Unsupported entry kind'}
"#;

#[cfg(any(windows, test))]
const RESTORE_SCRIPT: &str = r#"
$d=$p.snapshot.definition
if($p.snapshot.kind -ceq 'service') {
 # Also refuse an orphaned Services key: CreateService must not adopt it.
 $base=[Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine,[Microsoft.Win32.RegistryView]::Registry64)
 try {$key=$base.OpenSubKey('SYSTEM\CurrentControlSet\Services\'+$d.name,$false); if($null -ne $key){$key.Dispose();throw 'Service or registry definition already exists'}} finally{$base.Dispose()}
 [TPUninstall.Scm]::Create($d.name,$d.display_name,$d.binary,[uint32]$d.start,[uint32]$d.error_control,$d.account,[string[]]@($d.dependencies),$d.description,$d.security)
 $now=Read-Service $d.name
 foreach($field in @('name','display_name','binary','account','start','error_control','service_type','state','dependencies','group','tag','description','security','advanced_safe')) {
  if(!(Same $now.definition.$field $d.$field)) {throw ('Service recreated stopped, but config verification failed: '+$field)}
 }
} elseif($p.snapshot.kind -ceq 'task') {
 $meta=Task-Metadata $d.xml
 if(!$meta.supported -or !(Same $meta.actions $d.actions) -or $meta.user -cne $d.user -or $meta.logon -cne $d.logon -or $meta.run_level -cne $d.run_level -or (Resolve-Sid $d.user) -cne $d.sid) {throw 'Task XML metadata differs from backup'}
 $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
 $builtin=$d.sid -cin @('S-1-5-18','S-1-5-19','S-1-5-20') -and $d.logon -ceq 'ServiceAccount'
 $interactive=$d.sid -ceq $sid -and $d.logon -ceq 'InteractiveToken'
 if(!$builtin -and !$interactive) {throw 'Task principal cannot be restored without a password'}
 $split=$d.path.LastIndexOf('\'); $folder=$d.path.Substring(0,$split+1); $name=$d.path.Substring($split+1)
 $scheduler=Connect-Scheduler
 # Folder must still exist. No new folder or existing resource is overwritten.
 $parent=$scheduler.GetFolder($folder)
 $existing=@($parent.GetTasks(1) | Where-Object { [string]::Equals($_.Path,$d.path,[StringComparison]::OrdinalIgnoreCase) })
 if($existing.Count -ne 0) {throw 'Task already exists'}
 # Creation installs the ACL atomically and suppresses registration triggers only.
 $logonType=3; if($builtin){$logonType=5}
 $registered=$parent.RegisterTask($name,$d.xml,0x32,$d.user,$null,$logonType,$d.security)
 try {
  $now=Snapshot-Task $registered
  foreach($field in @('path','user','sid','logon','run_level','actions','security','supported')) {
   if(!(Same $now.definition.$field $d.$field)) {throw ('Task restore verification failed: '+$field)}
  }
 } catch {throw ('Task was registered; normal scheduled triggers may run; verification failed and retry will not overwrite it: '+$_.Exception.Message)}
} else {throw 'Unsupported entry kind'}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn owned() -> Ownership {
        Ownership {
            roots: vec![r"C:\Program Files\Unique App".into()],
            files: vec![r"D:\Portable\single.exe".into()],
            windows: r"C:\Windows".into(),
            current_sid: "S-1-5-21-1-2-3-1001".into(),
            self_exe: r"C:\Program Files\TP System Care\tp.exe".into(),
        }
    }

    fn service() -> ServiceSnapshot {
        ServiceSnapshot {
            name: "UniqueService".into(),
            display_name: "Unique App service".into(),
            binary: r#""C:\Program Files\Unique App\service.exe" --service"#.into(),
            account: r"NT AUTHORITY\LocalService".into(),
            start: 3,
            error_control: 1,
            service_type: 16,
            state: 1,
            dependents: 0,
            dependencies: vec!["RpcSs".into()],
            group: String::new(),
            tag: 0,
            description: "fixture".into(),
            security: "fixture-security".into(),
            advanced_safe: true,
            registry_supported: true,
            registry: Vec::new(),
        }
    }

    fn task() -> TaskSnapshot {
        TaskSnapshot {
            path: r"\Unique App\Maintenance".into(),
            xml: "<fixture/>".into(),
            security: "D:fixture".into(),
            state: 3,
            user: "S-1-5-18".into(),
            sid: "S-1-5-18".into(),
            logon: "ServiceAccount".into(),
            run_level: "LeastPrivilege".into(),
            actions: vec![Action {
                command: r"C:\Program Files\Unique App\task.exe".into(),
                arguments: "--safe".into(),
                working_directory: String::new(),
            }],
            supported: true,
        }
    }

    #[test]
    fn matches_only_executable_directory_boundaries_or_exact_files() {
        let owned = owned();
        assert!(evidence(r"c:\PROGRAM FILES\UNIQUE APP\service.EXE", false, &owned).is_some());
        assert!(evidence(r"d:\portable\SINGLE.exe", false, &owned).is_some());
        for path in [
            r"C:\Program Files\Unique App2\service.exe",
            r"D:\Portable\single.exe.bak",
            r"D:\Portable\another.exe",
            r"C:\Unrelated\Unique App.exe",
        ] {
            assert!(evidence(path, false, &owned).is_none(), "{path}");
        }
        assert!(evidence(
            r#""C:\Other\other.exe" "C:\Program Files\Unique App\service.exe""#,
            true,
            &owned
        )
        .is_none());
    }

    #[test]
    fn rejects_aliases_ambiguous_commands_runners_and_system_targets() {
        let mut owned = owned();
        owned.roots.push(r"C:\Windows".into());
        owned.files.push(r"C:\Windows\System32\custom.exe".into());
        owned.roots.push(r"C:\Program Files\TP System Care".into());
        for raw in [
            r"C:\Program Files\Unique App\service.exe --service",
            r"C:\Program Files\Unique App\..\other.exe",
            r"C:\PROGRA~1\UNIQUE~1\service.exe",
            r"\\server\share\service.exe",
            r"\\?\C:\app\service.exe",
            r"C:\Program Files\Unique App\service.exe:payload",
            r"%APPROOT%\service.exe",
            r"C:\Windows\System32\custom.exe",
            r"C:\Program Files\TP System Care\worker.exe",
            r"C:\Program Files\Unique App\node.exe",
            r"C:\Program Files\Unique App\powershell.exe",
            r"C:\Program Files\Unique App\python311.exe",
        ] {
            assert!(evidence(raw, true, &owned).is_none(), "{raw}");
        }
        assert!(executable(r"C:\Program Files\Unique App\service.exe --args", false).is_none());
    }

    #[test]
    fn rejects_live_shared_driver_critical_credential_and_advanced_services() {
        let owned = owned();
        assert!(eligible(&Snapshot::Service(service()), &owned).is_some());
        let mut variants = Vec::new();
        let mut s = service();
        s.state = 4;
        variants.push(s);
        let mut s = service();
        s.service_type = 32;
        variants.push(s);
        let mut s = service();
        s.service_type = 1;
        variants.push(s);
        let mut s = service();
        s.dependents = 1;
        variants.push(s);
        let mut s = service();
        s.name = "RpcSs".into();
        variants.push(s);
        let mut s = service();
        s.account = r"DOMAIN\user".into();
        variants.push(s);
        let mut s = service();
        s.account = r"NT SERVICE\UniqueService".into();
        variants.push(s);
        let mut s = service();
        s.start = 0;
        variants.push(s);
        let mut s = service();
        s.error_control = 3;
        variants.push(s);
        let mut s = service();
        s.advanced_safe = false;
        variants.push(s);
        let mut s = service();
        s.registry_supported = false;
        variants.push(s);
        for s in variants {
            assert!(eligible(&Snapshot::Service(s), &owned).is_none());
        }
    }

    #[test]
    fn rejects_unrelated_actions_and_password_s4u_group_principals() {
        let owned = owned();
        assert!(eligible(&Snapshot::Task(task()), &owned).is_some());
        let mut t = task();
        t.sid = owned.current_sid.clone();
        t.logon = "InteractiveToken".into();
        assert!(eligible(&Snapshot::Task(t.clone()), &owned).is_some());
        for logon in [
            "Password",
            "S4U",
            "Group",
            "InteractiveTokenOrPassword",
            "None",
        ] {
            t.logon = logon.into();
            assert!(eligible(&Snapshot::Task(t.clone()), &owned).is_none());
        }
        let mut t = task();
        t.actions.push(Action {
            command: r"C:\Unrelated\app.exe".into(),
            arguments: r"C:\Program Files\Unique App\task.exe".into(),
            working_directory: String::new(),
        });
        assert!(eligible(&Snapshot::Task(t), &owned).is_none());
        let mut t = task();
        t.path = r"\Microsoft\Windows\Maintenance".into();
        assert!(eligible(&Snapshot::Task(t), &owned).is_none());
        let mut t = task();
        t.state = 4;
        assert!(eligible(&Snapshot::Task(t), &owned).is_none());
        let mut t = task();
        t.supported = false;
        assert!(eligible(&Snapshot::Task(t), &owned).is_none());
    }

    #[test]
    fn selectors_are_ascii_json_scalars_with_exact_utf16_roundtrips() {
        for text in [
            "Owned’; Write-Output 31337; #",
            "‘’“”'\"`$([Environment]::Exit(99))\r\nline\n\\\t\0\u{1f680}",
            "",
        ] {
            let expression = ps_quote(text);
            assert!(expression.is_ascii());
            let scalar = expression
                .strip_prefix("(ConvertFrom-Json -InputObject '")
                .unwrap()
                .strip_suffix("')")
                .unwrap();
            assert!(!scalar.contains('\''));
            assert_eq!(serde_json::from_str::<String>(scalar).unwrap(), text);
        }
        assert!(ps_quote("’\u{1f680}").contains("\\u2019\\ud83d\\ude80"));
    }

    #[test]
    fn task_archives_normalize_encoding_without_changing_manifest_xml() {
        for declaration in [
            "<?xml version=\"1.0\" encoding=\"UTF-16\"?>",
            "<?xml version='1.0' encoding = 'utf-16' standalone='yes'?>",
            "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-16\"?>",
        ] {
            let original = format!("{declaration}<Task>‘é\u{1f680}’</Task>");
            let expected = original
                .trim_start_matches('\u{feff}')
                .replace("UTF-16", "UTF-8")
                .replace("utf-16", "UTF-8");
            let archive = task_backup_xml(&original);
            assert_eq!(archive.as_bytes(), expected.as_bytes());
            assert_eq!(task_backup_xml(&archive), archive);
            let mut snapshot = task();
            snapshot.xml = original.clone();
            let roundtrip: TaskSnapshot =
                serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
            assert_eq!(roundtrip.xml, original);
        }
        for xml in [
            "<Task/>",
            "<?xml version='1.0'?><Task/>",
            "<?xml-stylesheet href='encoding'?>",
        ] {
            assert_eq!(task_backup_xml(xml), xml);
        }
    }

    #[test]
    fn fixed_scripts_fit_escaped_windows_command_lines_and_use_safe_create_flags() {
        for (text, length) in [
            ("", 2),
            ("a", 3),
            ("\"", 4),
            ("\\", 4),
            ("\\\"", 6),
            ("\u{1f680}", 4),
        ] {
            assert_eq!(quoted_command_len(text), length);
        }
        let selector = ps_quote(&format!("\\{}", "'".repeat(1023)));
        let payload = ps_quote(&format!("C:\\Rescue\\{}\\undo.json", "'".repeat(240)));
        for tail in [
            ENUMERATE_SCRIPT.to_string(),
            format!("Read-Task {selector} | ConvertTo-Json -Depth 32 -Compress"),
            format!("Read-Service {} | ConvertTo-Json -Depth 32 -Compress", ps_quote(&"'".repeat(256))),
            format!("$p=Get-Content -LiteralPath {payload} -Raw -Encoding UTF8 | ConvertFrom-Json;\n{REMOVE_SCRIPT}"),
            format!("$p=Get-Content -LiteralPath {payload} -Raw -Encoding UTF8 | ConvertFrom-Json;\n{RESTORE_SCRIPT}"),
        ] {
            let wrapped = ps_wrapped(&format!("{SCM_SCRIPT}\n{METADATA_SCRIPT}\n{tail}"));
            assert!(quoted_command_len(&wrapped) + 256 < 32_767);
        }
        let flags = 0x02 | 0x10 | 0x20;
        assert_eq!(flags, 0x32);
        assert!(RESTORE_SCRIPT.contains(
            "$parent.RegisterTask($name,$d.xml,0x32,$d.user,$null,$logonType,$d.security)"
        ));
        assert!(RESTORE_SCRIPT.contains("$logonType=3; if($builtin){$logonType=5}"));
        assert!(!RESTORE_SCRIPT.contains("Register-ScheduledTask"));
        assert!(!RESTORE_SCRIPT.contains("SetSecurityDescriptor"));
        assert!(SCM_SCRIPT.contains(".usPreferredNode==ushort.MaxValue"));
        assert!(!SCM_SCRIPT.contains("Marshal.ReadByte(p,2)==0"));
    }

    #[cfg(windows)]
    #[test]
    fn powershell_selectors_roundtrip_smart_quote_injection_without_output() {
        for selector in [
            "Owned’; Write-Output 31337; #",
            "Owned‘; Write-Output 31337; #",
            "‘’“”'`\r\n$([Environment]::Exit(99))\n\u{1f680}",
        ] {
            let output = powershell(&format!(
                "function Read-Selector([string]$selector) {{ ConvertTo-Json -InputObject $selector -Compress }}; Read-Selector {}",
                ps_quote(selector)
            ))
            .unwrap();
            assert_eq!(output.trim().lines().count(), 1);
            assert_eq!(
                serde_json::from_str::<String>(output.trim()).unwrap(),
                selector
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn task_archive_utf8_bytes_parse_in_memory_with_original_utf16_manifest() {
        let original = "<?xml version=\"1.0\" encoding=\"UTF-16\"?><Task>‘é\u{1f680}’</Task>";
        let archive = task_backup_xml(original);
        let probe = format!(
            r#"$bytes=[Text.Encoding]::UTF8.GetBytes({});
$stream=New-Object IO.MemoryStream(,$bytes); $doc=New-Object Xml.XmlDocument;
try {{$doc.Load($stream)}} finally {{$stream.Dispose()}}
if($doc.FirstChild.Encoding -cne 'UTF-8') {{throw 'Archive declaration is not UTF-8'}}
ConvertTo-Json -InputObject $doc.DocumentElement.InnerText -Compress"#,
            ps_quote(&archive)
        );
        let output = powershell(&probe).unwrap();
        assert_eq!(
            serde_json::from_str::<String>(output.trim()).unwrap(),
            "‘é\u{1f680}’"
        );
        assert!(original.contains("UTF-16"));
    }

    #[test]
    fn json_is_strict_and_roundtrips_snapshot_and_undo_payload() {
        let snapshots = vec![Snapshot::Service(service()), Snapshot::Task(task())];
        let json = serde_json::to_string(&snapshots).unwrap();
        assert_eq!(
            parse_snapshots(&format!("\u{feff}{json}")).unwrap(),
            snapshots
        );
        for invalid in [
            "null",
            "{}",
            "[{\"kind\":\"task\",\"definition\":{}}]",
            "[{\"kind\":\"driver\",\"definition\":{}}]",
            "[broken]",
        ] {
            assert!(parse_snapshots(invalid).is_err());
        }
        let (manifest, backup) = expected_paths(123, 2, true);
        let undo = EntryUndo {
            version: FORMAT,
            record_id: 123,
            index: 2,
            snapshot: Snapshot::Task(task()),
            ownership: owned(),
            manifest,
            backup,
        };
        assert_eq!(
            serde_json::from_slice::<EntryUndo>(&serde_json::to_vec(&undo).unwrap()).unwrap(),
            undo
        );
        let json = serde_json::to_string(&task())
            .unwrap()
            .replacen('{', "{\"unknown\":1,", 1);
        assert!(serde_json::from_str::<TaskSnapshot>(&json).is_err());
    }

    #[test]
    fn backup_paths_reject_absolute_traversal_ads_and_aliases() {
        for bad in [
            "",
            "../undo.json",
            "1/../undo.json",
            "1//undo.json",
            "1/./undo.json",
            "/tmp/undo.json",
            "C:/undo.json",
            r"C:\undo.json",
            r"\\server\share\undo.json",
            "1/undo.json:stream",
            "1/undo.json.",
            "1/undo json",
        ] {
            assert!(!relative_backup(Path::new(bad)), "{bad}");
        }
        let (manifest, backup) = expected_paths(123, 0, true);
        assert!(relative_backup(&manifest));
        assert!(relative_backup(&backup));
        let (_, backup) = expected_paths(123, 0, false);
        assert!(relative_backup(&backup));
        assert_eq!(task_path(r"\Folder\Task"), Some((r"\Folder\", "Task")));
        assert!(task_path(r"\Folder\..\Task").is_none());
        assert!(task_path(r"\Folder\Task*").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn fake_task_audit_queries_exclude_custom_and_unknown_sacls() {
        let xml = r#"<Task xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task"><Principals><Principal id="P"><UserId>S-1-5-18</UserId><LogonType>ServiceAccount</LogonType></Principal></Principals><Actions Context="P"><Exec><Command>C:\Program Files\Unique App\task.exe</Command></Exec></Actions></Task>"#;
        let probe = format!(
            r#"{METADATA_SCRIPT}
$xml={}; $found=@()
foreach($mode in @('clean','audited','failed','empty','malformed')) {{
 $fake=[pscustomobject]@{{Path=('\Unique App\'+$mode);Xml=$xml;State=3;Mode=$mode;AuditQueries=0}}
 $fake | Add-Member -MemberType ScriptMethod -Name GetSecurityDescriptor -Value {{
  param($flags)
  if($flags -eq 7) {{return 'O:SYG:SYD:(A;;FA;;;SY)'}}
  if($flags -ne 8) {{throw 'Unexpected security query'}}
  $this.AuditQueries++
  switch($this.Mode) {{
   'clean' {{return 'S:'}}
   'audited' {{return 'S:(AU;SA;FA;;;SY)'}}
   'failed' {{throw 'Audit query unavailable'}}
   'empty' {{return ''}}
   'malformed' {{return 'S:invalid'}}
  }}
 }}
 $found+=,(Snapshot-Task $fake)
 if($fake.AuditQueries -ne 1) {{throw 'Audit query was not attempted'}}
}}
ConvertTo-Json -InputObject @($found) -Depth 32 -Compress"#,
            ps_quote(xml)
        );
        let snapshots = parse_snapshots(&powershell(&probe).unwrap()).unwrap();
        assert_eq!(snapshots.len(), 5);
        for (index, snapshot) in snapshots.iter().enumerate() {
            assert_eq!(eligible(snapshot, &owned()).is_some(), index == 0);
        }
    }

    #[cfg(windows)]
    #[test]
    fn fake_service_security_keys_exclude_audit_aces_and_invalid_descriptors() {
        let probe = format!(
            r#"{METADATA_SCRIPT}
$results=@()
foreach($mode in @('default','clean','audited','malformed')) {{
 $bytes=[byte[]]@(0)
 if($mode -cin @('clean','audited')) {{
  $sddl='O:SYG:SYD:(A;;FA;;;SY)'
  if($mode -ceq 'audited') {{$sddl+='S:(AU;SA;FA;;;SY)'}}
  $raw=New-Object Security.AccessControl.RawSecurityDescriptor($sddl)
  $bytes=New-Object byte[] $raw.BinaryLength; $raw.GetBinaryForm($bytes,0)
 }}
 $fake=[pscustomobject]@{{Mode=$mode;Descriptor=$bytes}}
 $fake | Add-Member -MemberType ScriptMethod -Name GetSubKeyNames -Value {{return ,@()}}
 $fake | Add-Member -MemberType ScriptMethod -Name GetValueNames -Value {{if($this.Mode -ceq 'default') {{return ,@()}}; return ,@('Security')}}
 $fake | Add-Member -MemberType ScriptMethod -Name GetValueKind -Value {{param($name) return [Microsoft.Win32.RegistryValueKind]::Binary}}
 $fake | Add-Member -MemberType ScriptMethod -Name GetValue -Value {{param($name,$fallback,$options) return ,$this.Descriptor}}
 $meta=Service-SecurityMetadata $fake
 $results+=,[ordered]@{{supported=$meta.supported;count=@($meta.values).Count}}
}}
ConvertTo-Json -InputObject @($results) -Depth 8 -Compress"#
        );
        let output = powershell(&probe).unwrap();
        let values: Vec<serde_json::Value> = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(values.len(), 4);
        for (index, value) in values.iter().enumerate() {
            let supported = index < 2;
            assert_eq!(value["supported"], supported);
            assert_eq!(value["count"], if index == 0 { 0 } else { 1 });
            let mut snapshot = service();
            snapshot.registry_supported = supported;
            assert_eq!(
                eligible(&Snapshot::Service(snapshot), &owned()).is_some(),
                supported
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn fake_scheduler_creates_with_original_acl_and_never_overwrites() {
        let xml = r#"<Task xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task"><Principals><Principal id="P"><UserId>S-1-5-18</UserId><LogonType>ServiceAccount</LogonType></Principal></Principals><Actions Context="P"><Exec><Command>C:\Program Files\Unique App\task.exe</Command></Exec></Actions></Task>"#;
        let probe = format!(
            r#"{METADATA_SCRIPT}
function Restore-Fixture {{ {RESTORE_SCRIPT} }}
function Connect-Scheduler {{return $script:scheduler}}
$xml={}; $results=@()
foreach($logon in @('ServiceAccount','InteractiveToken')) {{
 $user='S-1-5-18'; $expectedLogon=5
 if($logon -ceq 'InteractiveToken') {{$user=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value; $expectedLogon=3}}
 $fake=[pscustomobject]@{{Path='\Unique App\Maintenance';Xml=$xml.Replace('S-1-5-18',$user).Replace('ServiceAccount',$logon);State=3;Security='O:SYG:SYD:(A;;FA;;;SY)'}}
 $fake | Add-Member -MemberType ScriptMethod -Name GetSecurityDescriptor -Value {{param($flags) if($flags -eq 8) {{return 'S:'}}; return $this.Security}}
 $p=[pscustomobject]@{{snapshot=(Snapshot-Task $fake)}}
 $parent=[pscustomobject]@{{Calls=0;Existing=$false;Created=$fake;Trace=$null}}
 $parent | Add-Member -MemberType ScriptMethod -Name GetTasks -Value {{param($flags) if($this.Existing) {{return [pscustomobject]@{{Path=$this.Created.Path.ToUpperInvariant()}}}}}}
 $parent | Add-Member -MemberType ScriptMethod -Name RegisterTask -Value {{
  param($name,$xml,$flags,$user,$password,$logonType,$security)
  $this.Calls++
  if($flags -ne 0x32 -or $null -ne $password -or $xml -cne $this.Created.Xml -or $security -cne $this.Created.Security) {{throw 'Unsafe creation arguments'}}
  $this.Trace=[ordered]@{{name=$name;xml=$xml;flags=$flags;user=$user;password_is_null=($null -eq $password);logon=$logonType;security=$security}}
  return $this.Created
 }}
 $script:scheduler=[pscustomobject]@{{Parent=$parent}}
 $script:scheduler | Add-Member -MemberType ScriptMethod -Name GetFolder -Value {{param($folder) if($folder -cne '\Unique App\') {{throw 'Wrong folder'}}; return $this.Parent}}
 Restore-Fixture
 if($parent.Trace.logon -ne $expectedLogon -or $parent.Trace.user -cne $user) {{throw 'Wrong principal creation arguments'}}
 $parent.Existing=$true; $rejected=$false
 try {{Restore-Fixture}} catch {{if($_.Exception.Message -cne 'Task already exists') {{throw}}; $rejected=$true}}
 if(!$rejected -or $parent.Calls -ne 1) {{throw 'Existing task was not protected'}}
 $results+=,$parent.Trace
}}
ConvertTo-Json -InputObject @($results) -Depth 8 -Compress"#,
            ps_quote(xml)
        );
        let output = powershell(&probe).unwrap();
        let traces: Vec<serde_json::Value> = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(traces.len(), 2);
        for (index, trace) in traces.iter().enumerate() {
            assert_eq!(trace["flags"], 0x32);
            assert_eq!(trace["password_is_null"], true);
            assert_eq!(trace["logon"], if index == 0 { 5 } else { 3 });
            assert_eq!(trace["name"], "Maintenance");
            assert_eq!(trace["security"], "O:SYG:SYD:(A;;FA;;;SY)");
        }
    }

    #[cfg(windows)]
    #[test]
    fn json_and_xml_snapshot_fixtures_preserve_exact_evidence_and_principal() {
        let xml = r#"<Task xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task"><Principals><Principal id="P"><UserId>S-1-5-18</UserId><LogonType>ServiceAccount</LogonType></Principal></Principals><Actions Context="P"><Exec><Command>C:\Program Files\Unique App\task.exe</Command><Arguments>--fixture</Arguments></Exec></Actions></Task>"#;
        // Fake COM-shaped object, with no connection to the Task Scheduler.
        let probe=format!("{METADATA_SCRIPT}\n$fake=[pscustomobject]@{{Path='\\Unique App\\Maintenance';Xml={};State=3}}; $fake | Add-Member -MemberType ScriptMethod -Name GetSecurityDescriptor -Value {{param($flags) if($flags -eq 8){{'S:'}}else{{'O:SYG:SYD:(A;;FA;;;SY)'}}}}; Snapshot-Task $fake | ConvertTo-Json -Depth 32 -Compress",ps_quote(xml));
        let output = powershell(&probe).unwrap();
        let snapshot: Snapshot = serde_json::from_str(output.trim()).unwrap();
        assert!(eligible(&snapshot, &owned()).is_some());
        if let Snapshot::Task(t) = snapshot {
            assert_eq!(t.xml, xml);
            assert_eq!(t.sid, "S-1-5-18");
            assert_eq!(t.actions[0].arguments, "--fixture");
        } else {
            panic!("wrong fixture kind");
        }
        let original = serde_json::to_string(&Snapshot::Service(service())).unwrap();
        let changed = original.replace("--service", "--changed");
        let probe=format!("{METADATA_SCRIPT}\n$a=ConvertFrom-Json {}; $b=ConvertFrom-Json {}; if(!(Same $a $a) -or (Same $a $b)){{throw 'Snapshot comparison failure'}}; if(!(Same @() @()) -or (Same @() @('x'))){{throw 'Array comparison failure'}}",ps_quote(&original),ps_quote(&changed));
        powershell(&probe).unwrap();
        let injected = "'; throw 'not data'; # $([Environment]::Exit(99))";
        let result = powershell(&format!(
            "ConvertTo-Json -InputObject {} -Compress",
            ps_quote(injected)
        ))
        .unwrap();
        assert_eq!(
            serde_json::from_str::<String>(result.trim()).unwrap(),
            injected
        );
        for unsupported in [
            xml.replace(
                "<UserId>S-1-5-18</UserId>",
                "<UserId>S-1-5-18</UserId><UserId>S-1-5-19</UserId>",
            ),
            xml.replace("Context=\"P\"", "Context=\"Other\""),
            xml.replace(
                "</Actions>",
                "<Exec><Command>C:\\Other\\app.exe</Command></Exec></Actions>",
            ),
        ] {
            let result = powershell(&format!(
                "{METADATA_SCRIPT}\nTask-Metadata {} | ConvertTo-Json -Depth 16 -Compress",
                ps_quote(&unsupported)
            ))
            .unwrap();
            let parsed: serde_json::Value = serde_json::from_str(result.trim()).unwrap();
            assert!(
                parsed["supported"] == false || parsed["actions"].as_array().unwrap().len() == 2
            );
        }
    }

    // Windows-only XML fixtures use .NET's real DTD-prohibiting parser. These
    // scripts do NOT connect to SCM/Scheduler or create/change OS resources.
    #[cfg(windows)]
    #[test]
    fn xml_fixtures_and_fixed_scripts_compile_without_touching_resources() {
        let xml = r#"<Task xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task"><Principals><Principal id="Author"><UserId>S-1-5-18</UserId><LogonType>ServiceAccount</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals><Actions Context="Author"><Exec><Command>C:\Program Files\Unique App\task.exe</Command><Arguments>--name &amp; value</Arguments></Exec></Actions></Task>"#;
        let output = powershell(&format!(
            "{METADATA_SCRIPT}\nTask-Metadata {} | ConvertTo-Json -Depth 16 -Compress",
            ps_quote(xml)
        ))
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(value["supported"], true);
        assert_eq!(value["actions"][0]["arguments"], "--name & value");
        let mixed = xml.replace(
            "</Actions>",
            "<ComHandler><ClassId>{fixture}</ClassId></ComHandler></Actions>",
        );
        let output = powershell(&format!(
            "{METADATA_SCRIPT}\nTask-Metadata {} | ConvertTo-Json -Depth 16 -Compress",
            ps_quote(&mixed)
        ))
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(output.trim()).unwrap()["supported"],
            false
        );
        for invalid in [
            "<Task>",
            "<!DOCTYPE Task [<!ENTITY x SYSTEM 'file:///C:/Windows/win.ini'>]><Task>&x;</Task>",
        ] {
            assert!(powershell(&format!(
                "{METADATA_SCRIPT}\nTask-Metadata {} | ConvertTo-Json -Depth 16",
                ps_quote(invalid)
            ))
            .is_err());
        }
        // No SCM method is invoked; preferred-node layout is exercised in memory.
        powershell(&format!(
            r#"{SCM_SCRIPT}
$type=[TPUninstall.Scm].GetNestedType('PreferredNode',[Reflection.BindingFlags]::NonPublic)
$marshal=[Runtime.InteropServices.Marshal].GetMethod('PtrToStructure',[type[]]@([IntPtr],[Type]))
$buffer=[Runtime.InteropServices.Marshal]::AllocHGlobal(4)
try {{
 foreach($node in @(-1,0,1)) {{foreach($delete in @(0,1)) {{
  [Runtime.InteropServices.Marshal]::WriteInt16($buffer,0,[int16]$node)
  [Runtime.InteropServices.Marshal]::WriteByte($buffer,2,[byte]$delete)
  $value=$marshal.Invoke($null,[object[]]@($buffer,$type))
  if(($value.usPreferredNode -eq [uint16]::MaxValue) -ne ($node -eq -1)) {{throw 'Incorrect preferred-node interpretation'}}
 }}}}
}} finally {{[Runtime.InteropServices.Marshal]::FreeHGlobal($buffer)}}"#
        ))
        .unwrap();
        for script in [
            SCM_SCRIPT,
            METADATA_SCRIPT,
            ENUMERATE_SCRIPT,
            REMOVE_SCRIPT,
            RESTORE_SCRIPT,
        ] {
            let probe=format!("$tokens=$null;$errors=$null;[void][Management.Automation.Language.Parser]::ParseInput({},[ref]$tokens,[ref]$errors);if($errors.Count -ne 0){{throw ($errors | Out-String)}}",ps_quote(script));
            powershell(&probe).unwrap();
        }
    }
}
