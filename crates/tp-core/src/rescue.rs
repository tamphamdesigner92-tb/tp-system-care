//! Rescue Center: nơi giữ bản sao lưu, lịch sử thao tác và hoàn tác.
//!
//! Dữ liệu nằm trong `%LOCALAPPDATA%\TP System Care\Rescue`:
//! `history.json` là lịch sử, mỗi lần sửa có thư mục `<mã bản ghi>` chứa tệp sao lưu.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::engine::ModuleId;
use crate::modules::disk::{self, Toggle};
use crate::modules::startup;
use crate::reg::{self, RawValue};
use crate::winutil::{self, Known};

/// Một bước hoàn tác được lưu cùng bản ghi.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum UndoOp {
    /// Đưa tệp đã cách ly về chỗ cũ.
    RestoreFile {
        backup: PathBuf,
        original: PathBuf,
    },
    /// Trả giá trị StartupApproved về trạng thái trước khi tắt.
    /// `previous = None` nghĩa là trước đó không có giá trị này.
    RestoreStartup {
        machine: bool,
        subkey: String,
        name: String,
        previous: Option<Vec<u8>>,
    },
    /// Ghi lại giá trị registry cũ; `previous = None` nghĩa là trước đó không có.
    RestoreRegistryValue {
        machine: bool,
        subkey: String,
        name: String,
        previous: Option<RawValue>,
    },
    /// Tạo lại một khóa registry đã xóa cùng các giá trị của nó.
    RestoreRegistryKey {
        machine: bool,
        subkey: String,
        values: Vec<(String, RawValue)>,
    },
    SetToggle {
        toggle: Toggle,
        enable: bool,
    },
    /// Nhập lại một khóa registry đã xuất ra tệp `.reg` trước khi xóa cả cây.
    ImportRegFile {
        file: PathBuf,
    },
    RestoreUninstallEntry {
        entry: crate::tools::uninstall_entries::EntryUndo,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub id: u64,
    /// Giây tính từ mốc Unix.
    pub time: u64,
    pub modules: Vec<ModuleId>,
    /// Tên thao tác của công cụ ngoài luồng quét (ví dụ "Gỡ: 7-Zip").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub done: u64,
    pub failed: u64,
    pub bytes: u64,
    /// Các bước hoàn tác còn lại; rỗng nếu thao tác không hoàn tác được.
    pub undo: Vec<UndoOp>,
    /// Số bước hoàn tác lúc tạo bản ghi.
    pub undoable: u64,
    pub undone: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoReport {
    pub restored: u64,
    pub failed: u64,
}

pub struct Rescue {
    dir: PathBuf,
    /// Tuần tự hóa việc đọc-sửa-ghi `history.json`.
    lock: Mutex<()>,
}

impl Rescue {
    pub fn at(dir: PathBuf) -> Self {
        Self {
            dir,
            lock: Mutex::new(()),
        }
    }

    pub fn default_location() -> Self {
        let base = winutil::known_folder(Known::LocalAppData).unwrap_or_else(std::env::temp_dir);
        Self::at(base.join("TP System Care").join("Rescue"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn record_dir(&self, record_id: u64) -> PathBuf {
        self.dir.join(record_id.to_string())
    }

    /// Lịch sử, mới nhất trước.
    pub fn history(&self) -> Result<Vec<Record>, String> {
        let _guard = self.lock.lock().unwrap();
        let (mut records, _journal) = self.load(false)?;
        records.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(records)
    }

    fn load(&self, create: bool) -> Result<(Vec<Record>, Journal), String> {
        let mut journal = Journal::open(&self.dir, create).map_err(|e| e.to_string())?;
        let records = match journal.file.as_mut() {
            Some(file) => {
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
                serde_json::from_slice(&bytes)
                    .map_err(|e| format!("rescue_history_invalid: {e}"))?
            }
            None => Vec::new(),
        };
        journal.validate_parents().map_err(|e| e.to_string())?;
        Ok((records, journal))
    }

    fn save(&self, mut journal: Journal, records: &[Record]) -> Result<(), String> {
        journal.validate_parents().map_err(|e| e.to_string())?;
        let json = serde_json::to_vec_pretty(records).map_err(|e| e.to_string())?;
        let (tmp, mut file) = journal.scratch().map_err(|e| e.to_string())?;
        let result = (|| {
            file.write_all(&json).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            let source = open_journal_file(&tmp, true).map_err(|e| e.to_string())?;
            if journal_info(&source, false).map_err(|e| e.to_string())?
                != journal_info(&file, false).map_err(|e| e.to_string())?
            {
                return Err("rescue_scratch_changed".into());
            }
            let target = match open_journal_file(&journal.dir.join("history.json"), true) {
                Ok(file) => Some(file),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.to_string()),
            };
            let identity = |file: &File| journal_info(file, false).map_err(|e| e.to_string());
            if target.as_ref().map(identity).transpose()?
                != journal.file.as_ref().map(identity).transpose()?
            {
                return Err("rescue_history_changed".into());
            }
            journal.validate_parents().map_err(|e| e.to_string())?;
            drop(target);
            drop(journal.file.take());
            publish_journal(&tmp, &journal.dir.join("history.json"))
        })();
        if result.is_err() {
            discard_scratch(&file, &tmp);
        }
        result
    }

    /// Mã bản ghi mới: mili giây hiện tại, tăng dần và không trùng mã đã có.
    pub fn new_record_id(&self) -> u64 {
        let _guard = self.lock.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let newest = self
            .load(false)
            .ok()
            .and_then(|(records, _journal)| records.iter().map(|r| r.id).max());
        newest.map_or(now, |id| now.max(id.saturating_add(1)))
    }

    pub fn add_record(
        &self,
        id: u64,
        modules: Vec<ModuleId>,
        done: u64,
        failed: u64,
        bytes: u64,
        undo: Vec<UndoOp>,
    ) -> Result<(), String> {
        let _guard = self.lock.lock().unwrap();
        let (mut records, journal) = self.load(true)?;
        records.push(Record {
            id,
            time: winutil::unix_now(),
            modules,
            title: None,
            done,
            failed,
            bytes,
            undoable: undo.len() as u64,
            undo,
            undone: false,
        });
        self.save(journal, &records)
    }

    /// Ghi bản ghi cho thao tác của một công cụ (không thuộc hạng mục quét nào).
    pub fn add_tool_record(
        &self,
        id: u64,
        title: &str,
        done: u64,
        failed: u64,
        bytes: u64,
        undo: Vec<UndoOp>,
    ) -> Result<(), String> {
        let _guard = self.lock.lock().unwrap();
        let (mut records, journal) = self.load(true)?;
        let record = Record {
            id,
            time: winutil::unix_now(),
            modules: Vec::new(),
            title: Some(title.to_string()),
            done,
            failed,
            bytes,
            undoable: undo.len() as u64,
            undo,
            undone: false,
        };
        if let Some(existing) = records.iter_mut().find(|record| record.id == id) {
            let time = existing.time;
            *existing = record;
            existing.time = time;
        } else {
            records.push(record);
        }
        self.save(journal, &records)
    }

    /// Chuyển tệp vào thư mục sao lưu của bản ghi.
    pub fn quarantine(&self, record_id: u64, path: &Path) -> Result<UndoOp, String> {
        let dir = self.record_dir(record_id).join("files");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let name = path
            .file_name()
            .ok_or_else(|| "đường dẫn không hợp lệ".to_string())?;
        let mut backup = dir.join(name);
        let mut n = 1;
        while backup.exists() {
            backup = dir.join(format!("{}_{}", n, name.to_string_lossy()));
            n += 1;
        }
        move_file(path, &backup)?;
        Ok(UndoOp::RestoreFile {
            backup,
            original: path.to_path_buf(),
        })
    }

    /// Hoàn tác một bản ghi. Bước nào thất bại được giữ lại để thử lại sau.
    pub fn undo(&self, record_id: u64) -> Result<UndoReport, String> {
        let _guard = self.lock.lock().unwrap();
        let (mut records, journal) = self.load(false)?;
        let record = records
            .iter_mut()
            .find(|r| r.id == record_id)
            .ok_or_else(|| "not_found".to_string())?;
        if record.undo.is_empty() {
            return Err("nothing_to_undo".into());
        }

        let mut report = UndoReport {
            restored: 0,
            failed: 0,
        };
        let mut remaining = Vec::new();
        // Hoàn tác theo thứ tự ngược với lúc sửa.
        for op in std::mem::take(&mut record.undo).into_iter().rev() {
            match undo_one(&op, &self.dir) {
                Ok(()) => report.restored += 1,
                Err(_) => {
                    report.failed += 1;
                    remaining.push(op);
                }
            }
        }
        remaining.reverse();
        record.undone = remaining.is_empty();
        record.undo = remaining;
        self.save(journal, &records)?;
        Ok(report)
    }
}

struct Journal {
    dir: PathBuf,
    parents: Vec<File>,
    file: Option<File>,
}

impl Journal {
    fn open(dir: &Path, create: bool) -> io::Result<Self> {
        let dir = std::path::absolute(dir)?;
        if dir
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(io::Error::other("rescue_directory_alias"));
        }
        let mut journal = Self {
            dir,
            parents: Vec::new(),
            file: None,
        };
        let parents: Vec<_> = journal.dir.ancestors().map(Path::to_path_buf).collect();
        for parent in parents.into_iter().rev() {
            journal.validate_parents()?;
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                use windows::Win32::Storage::FileSystem::{
                    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
                    FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
                };
                options
                    .access_mode(FILE_READ_ATTRIBUTES.0 | FILE_LIST_DIRECTORY.0)
                    .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0);
            }
            #[cfg(not(windows))]
            if fs::symlink_metadata(&parent).is_ok_and(|meta| meta.file_type().is_symlink()) {
                return Err(io::Error::other("rescue_directory_alias"));
            }
            let file = match options.open(&parent) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound && create => {
                    fs::create_dir(&parent)?;
                    options.open(&parent)?
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(journal),
                Err(e) => return Err(e),
            };
            journal_info(&file, true)?;
            journal.parents.push(file);
        }
        journal.validate_parents()?;
        journal.file = match open_journal_file(&journal.dir.join("history.json"), false) {
            Ok(file) => Some(file),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        Ok(journal)
    }

    fn validate_parents(&self) -> io::Result<()> {
        for parent in &self.parents {
            journal_info(parent, true)?;
        }
        Ok(())
    }

    fn scratch(&self) -> io::Result<(PathBuf, File)> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..32 {
            let path = self.dir.join(format!(
                ".history-{}-{nonce}-{}.tmp",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                use windows::Win32::Foundation::GENERIC_WRITE;
                use windows::Win32::Storage::FileSystem::{
                    DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
                };
                options
                    .access_mode(GENERIC_WRITE.0 | DELETE.0)
                    .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_DELETE.0)
                    .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
            }
            match options.open(&path) {
                Ok(file) => return Ok((path, file)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::other("rescue_scratch_exists"))
    }
}

fn open_journal_file(path: &Path, probe: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        let share = if probe {
            FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0
        } else {
            FILE_SHARE_READ.0
        };
        options
            .share_mode(share)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    }
    #[cfg(not(windows))]
    {
        let _ = probe;
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(io::Error::other("rescue_history_alias"));
        }
    }
    let file = options.open(path)?;
    journal_info(&file, false)?;
    Ok(file)
}

fn journal_info(file: &File, directory: bool) -> io::Result<(u64, u64)> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_REPARSE_POINT,
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
            .map_err(|e| io::Error::other(e.to_string()))?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
            || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0) != directory
            || (!directory && info.nNumberOfLinks != 1)
        {
            return Err(io::Error::other("rescue_journal_alias"));
        }
        Ok((
            info.dwVolumeSerialNumber as u64,
            ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        ))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = file.metadata()?;
        if (directory && !meta.is_dir()) || (!directory && (!meta.is_file() || meta.nlink() != 1)) {
            return Err(io::Error::other("rescue_journal_alias"));
        }
        Ok((meta.dev(), meta.ino()))
    }
}

#[cfg(windows)]
fn publish_journal(from: &Path, to: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let from: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        MoveFileExW(
            PCWSTR(from.as_ptr()),
            PCWSTR(to.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn publish_journal(from: &Path, to: &Path) -> Result<(), String> {
    fs::rename(from, to).map_err(|e| e.to_string())?;
    File::open(to.parent().ok_or("rescue_directory_missing")?)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| e.to_string())
}

fn discard_scratch(file: &File, path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
        };
        let info = FILE_DISPOSITION_INFO {
            DeleteFile: true.into(),
        };
        let _ = unsafe {
            SetFileInformationByHandle(
                HANDLE(file.as_raw_handle()),
                FileDispositionInfo,
                (&info as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of_val(&info) as u32,
            )
        };
        let _ = path;
    }
    #[cfg(not(windows))]
    if let Ok(current) = open_journal_file(path, true) {
        if journal_info(file, false).ok() == journal_info(&current, false).ok() {
            let _ = fs::remove_file(path);
        }
    }
}

fn undo_one(op: &UndoOp, rescue_root: &Path) -> Result<(), String> {
    match op {
        UndoOp::RestoreFile { backup, original } => {
            if original.exists() {
                return Err("tệp gốc đã tồn tại".into());
            }
            if let Some(parent) = original.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            move_file(backup, original)
        }
        UndoOp::RestoreStartup {
            machine,
            subkey,
            name,
            previous,
        } => startup::restore(*machine, subkey, name, previous.as_deref()),
        UndoOp::RestoreRegistryValue {
            machine,
            subkey,
            name,
            previous,
        } => reg::restore(*machine, subkey, name, previous.as_ref()),
        UndoOp::RestoreRegistryKey {
            machine,
            subkey,
            values,
        } => {
            // Khóa không có giá trị nào vẫn phải được tạo lại.
            if values.is_empty() {
                reg::write(*machine, subkey, "", &RawValue::sz(""))?;
                return reg::delete_value(*machine, subkey, "");
            }
            for (name, value) in values {
                reg::write(*machine, subkey, name, value)?;
            }
            Ok(())
        }
        UndoOp::SetToggle { toggle, enable } => disk::set_toggle(*toggle, *enable),
        UndoOp::ImportRegFile { file } => import_registry_file(file),
        UndoOp::RestoreUninstallEntry { entry } => {
            crate::tools::uninstall_entries::restore(entry, rescue_root)
        }
    }
}

/// Di chuyển tệp; nếu khác ổ đĩa thì chép rồi xóa.
fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to).map_err(|e| e.to_string())?;
    fs::remove_file(from).map_err(|e| {
        let _ = fs::remove_file(to);
        e.to_string()
    })
}

/// Xuất một khóa registry ra tệp `.reg` trong thư mục của bản ghi.
#[cfg(windows)]
pub fn export_registry_key(
    rescue: &Rescue,
    record_id: u64,
    key: &str,
    file_name: &str,
) -> Result<PathBuf, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let dir = rescue.record_dir(record_id).join("registry");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(file_name);
    let program = winutil::known_folder(Known::Windows)
        .ok_or("not_found")?
        .join("System32/reg.exe");
    let output = std::process::Command::new(program)
        .arg("export")
        .arg(key)
        .arg(&target)
        .arg("/y")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(target)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(not(windows))]
pub fn export_registry_key(_: &Rescue, _: u64, _: &str, _: &str) -> Result<PathBuf, String> {
    Err("chỉ hỗ trợ Windows".into())
}

/// Nhập một tệp `.reg` đã xuất bởi [`export_registry_key`].
#[cfg(windows)]
fn import_registry_file(file: &Path) -> Result<(), String> {
    let program = winutil::known_folder(Known::Windows)
        .ok_or("not_found")?
        .join("System32/reg.exe");
    winutil::run_hidden(
        &program.to_string_lossy(),
        &["import", &file.to_string_lossy()],
        std::time::Duration::from_secs(30),
        None,
    )
    .map(|_| ())
}

#[cfg(not(windows))]
fn import_registry_file(_file: &Path) -> Result<(), String> {
    Err("chỉ hỗ trợ Windows".into())
}

/// Tạo điểm khôi phục hệ thống. Cần quyền quản trị và System Protection đang bật.
/// Lỗi trả về là mã ngắn (`needs_admin`, `too_soon`, `disabled`) hoặc thông báo gốc.
#[cfg(windows)]
pub fn create_restore_point(description: &str) -> Result<(), String> {
    if !winutil::is_elevated() {
        return Err("needs_admin".into());
    }
    let description = description.replace('\'', "''");
    // Windows chỉ cảnh báo (không báo lỗi) khi đã có điểm khôi phục trong 24 giờ;
    // -WarningAction Stop biến cảnh báo đó thành lỗi để không báo thành công giả.
    let script = format!(
        "$ErrorActionPreference='Stop'; Checkpoint-Computer -Description '{description}' \
         -RestorePointType MODIFY_SETTINGS -WarningAction Stop"
    );
    let program = winutil::known_folder(Known::Windows)
        .ok_or("not_found")?
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let message = match winutil::run_hidden(
        &program.to_string_lossy(),
        &["-NoProfile", "-NonInteractive", "-Command", &script],
        std::time::Duration::from_secs(180),
        None,
    ) {
        Ok(_) => return Ok(()),
        Err(message) => message,
    };
    if message.contains("1440") {
        Err("too_soon".into())
    } else if message.to_lowercase().contains("disabled") {
        Err("disabled".into())
    } else {
        Err(message)
    }
}

#[cfg(not(windows))]
pub fn create_restore_point(_: &str) -> Result<(), String> {
    Err("chỉ hỗ trợ Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "TPRescueFixture-{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        fn rescue(&self) -> Rescue {
            Rescue::at(self.0.join("Rescue"))
        }

        fn pending(&self, rescue: &Rescue) -> (PathBuf, PathBuf) {
            let backup = self.0.join("backup.txt");
            let original = self.0.join("original.txt");
            fs::write(&backup, b"recovery bytes").unwrap();
            rescue
                .add_tool_record(
                    1,
                    "fixture",
                    0,
                    0,
                    0,
                    vec![UndoOp::RestoreFile {
                        backup: backup.clone(),
                        original: original.clone(),
                    }],
                )
                .unwrap();
            (backup, original)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn assert_mutations_denied(rescue: &Rescue) {
        assert!(rescue.history().is_err());
        let _ = rescue.new_record_id();
        assert!(rescue
            .add_record(2, Vec::new(), 1, 0, 0, Vec::new())
            .is_err());
        assert!(rescue
            .add_tool_record(1, "overwrite", 1, 0, 0, Vec::new())
            .is_err());
        assert!(rescue.undo(1).is_err());
    }

    #[test]
    fn tool_recovery_is_updated_without_duplicate_records() {
        let fixture = Fixture::new();
        let rescue = fixture.rescue();
        let (backup, original) = fixture.pending(&rescue);
        let before = rescue.history().unwrap();
        rescue
            .add_tool_record(1, "fixture", 1, 0, 12, before[0].undo.clone())
            .unwrap();
        let after = Rescue::at(rescue.dir().to_path_buf()).history().unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].undoable, 1);
        assert_eq!(before[0].done, 0);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].time, before[0].time);
        assert_eq!(after[0].done, 1);
        assert_eq!(after[0].bytes, 12);
        assert_eq!(after[0].undo.len(), 1);
        assert!(backup.exists() && !original.exists());
    }

    #[test]
    fn missing_history_and_normal_record_history_undo() {
        let fixture = Fixture::new();
        let rescue = fixture.rescue();
        assert!(rescue.history().unwrap().is_empty());
        let id = rescue.new_record_id();
        assert!(!rescue.dir().exists());
        let original = fixture.0.join("shortcut.lnk");
        fs::write(&original, b"fixture shortcut").unwrap();
        let op = rescue.quarantine(id, &original).unwrap();
        rescue
            .add_record(id, vec![ModuleId::Shortcut], 1, 0, 16, vec![op])
            .unwrap();
        rescue
            .add_tool_record(id + 1, "newer", 1, 0, 0, Vec::new())
            .unwrap();
        let records = rescue.history().unwrap();
        assert_eq!(
            records.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![id + 1, id]
        );
        assert!(rescue.new_record_id() > id + 1);
        let report = rescue.undo(id).unwrap();
        assert_eq!((report.restored, report.failed), (1, 0));
        assert_eq!(fs::read(original).unwrap(), b"fixture shortcut");
        assert!(rescue.history().unwrap()[1].undone);
        assert!(rescue.history().unwrap()[1].undo.is_empty());
        assert!(rescue.undo(id).is_err());
    }

    #[test]
    fn corrupt_history_preserves_bytes_and_denies_all_mutations() {
        let fixture = Fixture::new();
        let rescue = fixture.rescue();
        let (backup, original) = fixture.pending(&rescue);
        let path = rescue.dir().join("history.json");
        let valid = fs::read(&path).unwrap();
        for corrupt in [
            Vec::new(),
            b"not JSON".to_vec(),
            b"{}".to_vec(),
            valid[..valid.len() - 1].to_vec(),
        ] {
            fs::write(&path, &corrupt).unwrap();
            assert_mutations_denied(&rescue);
            assert_eq!(fs::read(&path).unwrap(), corrupt);
            assert_eq!(fs::read(&backup).unwrap(), b"recovery bytes");
            assert!(!original.exists());
            assert_eq!(fs::read_dir(rescue.dir()).unwrap().count(), 1);
        }
        fs::write(&path, &valid).unwrap();
        let records = rescue.history().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, 1);
        assert_eq!(records[0].undo.len(), 1);
        assert!(!records[0].undone);
        assert_eq!(fs::read(path).unwrap(), valid);
    }

    #[test]
    fn planted_predictable_tmp_hardlink_is_untouched() {
        let fixture = Fixture::new();
        let rescue = fixture.rescue();
        fixture.pending(&rescue);
        let target = fixture.0.join("target.txt");
        let planted = rescue.dir().join("history.json.tmp");
        fs::write(&target, b"must not truncate").unwrap();
        fs::hard_link(&target, &planted).unwrap();
        rescue
            .add_record(2, Vec::new(), 1, 0, 0, Vec::new())
            .unwrap();
        rescue
            .add_tool_record(2, "upsert", 2, 0, 0, Vec::new())
            .unwrap();
        assert_eq!(rescue.undo(1).unwrap().restored, 1);
        assert_eq!(rescue.history().unwrap().len(), 2);
        assert_eq!(fs::read(&target).unwrap(), b"must not truncate");
        assert_eq!(fs::read(&planted).unwrap(), b"must not truncate");
        assert_eq!(fs::read_dir(rescue.dir()).unwrap().count(), 2);
    }

    #[test]
    fn hardlinked_journal_is_rejected_without_touching_recovery() {
        let fixture = Fixture::new();
        let rescue = fixture.rescue();
        let (backup, original) = fixture.pending(&rescue);
        let path = rescue.dir().join("history.json");
        let valid = fs::read(&path).unwrap();
        let alias = fixture.0.join("journal-alias.json");
        fs::hard_link(&path, &alias).unwrap();
        assert_mutations_denied(&rescue);
        assert_eq!(fs::read(&path).unwrap(), valid);
        assert_eq!(fs::read(&alias).unwrap(), valid);
        assert!(backup.exists() && !original.exists());
        fs::remove_file(alias).unwrap();
        assert_eq!(rescue.history().unwrap().len(), 1);
    }

    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("cmd.exe")
            .args(["/D", "/C"])
            .raw_arg(format!(
                "mklink /J \"{}\" \"{}\"",
                link.display(),
                target.display()
            ))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(any(windows, unix))]
    #[test]
    fn reparse_ancestor_is_rejected_even_when_history_is_missing() {
        let fixture = Fixture::new();
        let rescue = fixture.rescue();
        fixture.pending(&rescue);
        let valid = fs::read(rescue.dir().join("history.json")).unwrap();
        let alias = fixture.0.join("alias");
        #[cfg(windows)]
        junction(&alias, rescue.dir());
        #[cfg(unix)]
        std::os::unix::fs::symlink(rescue.dir(), &alias).unwrap();
        assert_mutations_denied(&Rescue::at(alias.clone()));
        assert_mutations_denied(&Rescue::at(alias.join("missing")));
        assert!(!rescue.dir().join("missing").exists());
        assert_eq!(fs::read(rescue.dir().join("history.json")).unwrap(), valid);
        #[cfg(windows)]
        fs::remove_dir(alias).unwrap();
        #[cfg(unix)]
        fs::remove_file(alias).unwrap();
    }

    #[cfg(any(windows, unix))]
    #[test]
    fn reparse_journal_is_rejected_without_modifying_target() {
        let fixture = Fixture::new();
        let rescue = fixture.rescue();
        fs::create_dir(rescue.dir()).unwrap();
        let target = fixture.0.join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep.txt"), b"keep").unwrap();
        let alias = rescue.dir().join("history.json");
        #[cfg(windows)]
        junction(&alias, &target);
        #[cfg(unix)]
        std::os::unix::fs::symlink(target.join("keep.txt"), &alias).unwrap();
        assert_mutations_denied(&rescue);
        assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"keep");
        assert_eq!(fs::read_dir(&target).unwrap().count(), 1);
        #[cfg(windows)]
        fs::remove_dir(alias).unwrap();
        #[cfg(unix)]
        fs::remove_file(alias).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn unreadable_journal_denies_mutation_and_keeps_old_history() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        let rescue = fixture.rescue();
        let (backup, original) = fixture.pending(&rescue);
        let path = rescue.dir().join("history.json");
        let valid = fs::read(&path).unwrap();
        let blocker = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert_mutations_denied(&rescue);
        drop(blocker);
        assert_eq!(fs::read(path).unwrap(), valid);
        assert!(backup.exists() && !original.exists());
    }

    #[cfg(windows)]
    #[test]
    fn transaction_holds_ancestors_and_journal_against_rename_and_write() {
        let fixture = Fixture::new();
        let rescue = fixture.rescue();
        fixture.pending(&rescue);
        let (_, mut journal) = rescue.load(false).unwrap();
        assert!(fs::write(rescue.dir().join("history.json"), b"overwrite").is_err());
        drop(journal.file.take());
        for parent in [rescue.dir(), fixture.0.as_path()] {
            let renamed = parent.with_extension("renamed");
            assert!(!renamed.exists());
            let result = fs::rename(parent, &renamed);
            if result.is_ok() {
                fs::rename(&renamed, parent).unwrap();
            }
            assert!(result.is_err());
        }
        drop(journal);
        assert_eq!(rescue.history().unwrap().len(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn publication_failure_keeps_history_and_cleans_only_owned_scratch() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
        let fixture = Fixture::new();
        let rescue = fixture.rescue();
        fixture.pending(&rescue);
        let path = rescue.dir().join("history.json");
        let valid = fs::read(&path).unwrap();
        let planted = rescue.dir().join("history.json.tmp");
        fs::write(&planted, b"preexisting tmp").unwrap();
        let blocker = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(&path)
            .unwrap();
        assert!(rescue
            .add_tool_record(2, "blocked", 1, 0, 0, Vec::new())
            .is_err());
        drop(blocker);
        assert_eq!(fs::read(path).unwrap(), valid);
        assert_eq!(fs::read(planted).unwrap(), b"preexisting tmp");
        assert_eq!(fs::read_dir(rescue.dir()).unwrap().count(), 2);
        assert_eq!(rescue.history().unwrap().len(), 1);
    }
}
