//! Rescue Center: nơi giữ bản sao lưu, lịch sử thao tác và hoàn tác.
//!
//! Dữ liệu nằm trong `%LOCALAPPDATA%\TP System Care\Rescue`:
//! `history.json` là lịch sử, mỗi lần sửa có thư mục `<mã bản ghi>` chứa tệp sao lưu.

use std::fs;
use std::path::{Path, PathBuf};
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
    RestoreFile { backup: PathBuf, original: PathBuf },
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
    SetToggle { toggle: Toggle, enable: bool },
    /// Nhập lại một khóa registry đã xuất ra tệp `.reg` trước khi xóa cả cây.
    ImportRegFile { file: PathBuf },
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

    fn history_path(&self) -> PathBuf {
        self.dir.join("history.json")
    }

    pub fn record_dir(&self, record_id: u64) -> PathBuf {
        self.dir.join(record_id.to_string())
    }

    /// Lịch sử, mới nhất trước.
    pub fn history(&self) -> Vec<Record> {
        let _guard = self.lock.lock().unwrap();
        let mut records = self.load();
        records.sort_by(|a, b| b.id.cmp(&a.id));
        records
    }

    fn load(&self) -> Vec<Record> {
        fs::read(self.history_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, records: &[Record]) -> Result<(), String> {
        fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_vec_pretty(records).map_err(|e| e.to_string())?;
        // Ghi ra tệp tạm rồi đổi tên để không làm hỏng lịch sử nếu bị ngắt giữa chừng.
        let tmp = self.dir.join("history.json.tmp");
        fs::write(&tmp, json).map_err(|e| e.to_string())?;
        fs::rename(&tmp, self.history_path()).map_err(|e| e.to_string())
    }

    /// Mã bản ghi mới: mili giây hiện tại, tăng dần và không trùng mã đã có.
    pub fn new_record_id(&self) -> u64 {
        let _guard = self.lock.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let newest = self.load().iter().map(|r| r.id).max().unwrap_or(0);
        now.max(newest + 1)
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
        let mut records = self.load();
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
        self.save(&records)
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
        self.add_record(id, Vec::new(), done, failed, bytes, undo)?;
        let _guard = self.lock.lock().unwrap();
        let mut records = self.load();
        if let Some(record) = records.iter_mut().find(|r| r.id == id) {
            record.title = Some(title.to_string());
        }
        self.save(&records)
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
        let mut records = self.load();
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
            match undo_one(&op) {
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
        self.save(&records)?;
        Ok(report)
    }
}

fn undo_one(op: &UndoOp) -> Result<(), String> {
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
pub fn export_registry_key(rescue: &Rescue, record_id: u64, key: &str, file_name: &str) -> Result<PathBuf, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let dir = rescue.record_dir(record_id).join("registry");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(file_name);
    let output = std::process::Command::new("reg.exe")
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
    winutil::run_hidden(
        "reg.exe",
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
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    if !winutil::is_elevated() {
        return Err("needs_admin".into());
    }
    let description: String = description.chars().filter(|c| *c != '\'').collect();
    // Windows chỉ cảnh báo (không báo lỗi) khi đã có điểm khôi phục trong 24 giờ;
    // -WarningAction Stop biến cảnh báo đó thành lỗi để không báo thành công giả.
    let script = format!(
        "$ErrorActionPreference='Stop'; Checkpoint-Computer -Description '{description}' \
         -RestorePointType MODIFY_SETTINGS -WarningAction Stop"
    );
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
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
