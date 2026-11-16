//! Tối ưu khởi động: liệt kê chương trình tự chạy cùng Windows và tắt chúng theo
//! đúng cách của Task Manager (khóa `StartupApproved`), nên không xóa gì và bật
//! lại được từ Task Manager hoặc Rescue Center.
//!
//! Engine không có cơ sở dữ liệu để biết chương trình nào "không cần", nên chỉ
//! chọn sẵn các mục trỏ tới tệp không còn tồn tại; phần còn lại để người dùng chọn.

use std::path::{Path, PathBuf};

use crate::engine::{Action, Finding, Module, ModuleId, ScanContext};
use crate::modules::shortcut::is_missing;
use crate::reg::{self, RawValue};
use crate::rescue::UndoOp;
use crate::winutil::{self, Known};

pub struct Startup;

const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_WOW64: &str = "Software\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Run";
pub const APPROVED: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved";

/// Vị trí trạng thái bật/tắt của một mục khởi động.
#[derive(Clone, Debug)]
pub struct StartupEntry {
    /// `true`: HKEY_LOCAL_MACHINE (cần quyền quản trị); `false`: HKEY_CURRENT_USER.
    pub machine: bool,
    /// Khóa con dưới `StartupApproved`: `Run`, `Run32` hoặc `StartupFolder`.
    pub approved: &'static str,
    pub name: String,
}

/// Lấy đường dẫn tệp thực thi từ dòng lệnh trong khóa Run.
pub fn parse_command(command: &str) -> Option<PathBuf> {
    let command = winutil::expand_env(command.trim());
    if let Some(rest) = command.strip_prefix('"') {
        let end = rest.find('"')?;
        return Some(PathBuf::from(&rest[..end]));
    }
    // Không có ngoặc kép thì đường dẫn có thể chứa dấu cách: thử từng đoạn đầu
    // kết thúc ở dấu cách cho tới khi gặp tệp có thật.
    let ends = command
        .match_indices(' ')
        .map(|(i, _)| i)
        .chain(std::iter::once(command.len()));
    for end in ends {
        let candidate = &command[..end];
        if Path::new(candidate).is_file() {
            return Some(PathBuf::from(candidate));
        }
        let with_exe = format!("{candidate}.exe");
        if Path::new(&with_exe).is_file() {
            return Some(PathBuf::from(with_exe));
        }
    }
    // Không đoạn nào tồn tại: cắt sau phần mở rộng .exe nếu có, không thì lấy từ đầu tiên.
    let lower = command.to_lowercase();
    let end = lower
        .find(".exe")
        .map(|i| i + 4)
        .or_else(|| command.find(' '))
        .unwrap_or(command.len());
    let path = &command[..end];
    (!path.is_empty()).then(|| PathBuf::from(path))
}

fn finding(entry: StartupEntry, command: String, target: Option<PathBuf>) -> Option<Finding> {
    if let (Some(target), Some(windows)) = (&target, winutil::known_folder(Known::Windows)) {
        // Thành phần của Windows (ví dụ SecurityHealthSystray): không liệt kê.
        if target.starts_with(&windows) {
            return None;
        }
    }
    let broken = target.as_deref().is_some_and(is_missing);
    Some(Finding {
        id: String::new(),
        module: ModuleId::Startup,
        kind: if broken { "startup.broken" } else { "startup.entry" },
        name: Some(entry.name.clone()),
        detail: Some(command),
        count: 1,
        bytes: 0,
        recommended: broken,
        undoable: true,
        needs_admin: entry.machine,
        action: Action::DisableStartup(entry),
    })
}

impl Module for Startup {
    fn id(&self) -> ModuleId {
        ModuleId::Startup
    }

    fn scan(&self, ctx: &ScanContext) -> Vec<Finding> {
        let mut findings = Vec::new();

        let registry = [(false, RUN, "Run"), (true, RUN, "Run"), (true, RUN_WOW64, "Run32")];
        for (machine, run_key, approved) in registry {
            for (name, command) in run_values(machine, run_key) {
                if ctx.cancelled() {
                    return findings;
                }
                ctx.report(Path::new(&command));
                let entry = StartupEntry {
                    machine,
                    approved,
                    name,
                };
                if is_disabled(&entry) {
                    continue;
                }
                let target = parse_command(&command);
                findings.extend(finding(entry, command, target));
            }
        }

        let folders = [(false, Known::Startup), (true, Known::CommonStartup)];
        for (machine, folder) in folders {
            let Some(dir) = winutil::known_folder(folder) else {
                continue;
            };
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for file in entries.filter_map(Result::ok) {
                if ctx.cancelled() {
                    return findings;
                }
                let path = file.path();
                let name = file.file_name().to_string_lossy().to_string();
                if !path.is_file() || name.eq_ignore_ascii_case("desktop.ini") {
                    continue;
                }
                ctx.report(&path);
                let entry = StartupEntry {
                    machine,
                    approved: "StartupFolder",
                    name,
                };
                if is_disabled(&entry) {
                    continue;
                }
                let is_lnk = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
                let target = if is_lnk {
                    winutil::shortcut_target(&path)
                } else {
                    Some(path.clone())
                };
                findings.extend(finding(entry, path.to_string_lossy().to_string(), target));
            }
        }
        findings
    }
}

fn approved_key(entry_approved: &str) -> String {
    format!("{APPROVED}\\{entry_approved}")
}

/// Giá trị StartupApproved: byte đầu lẻ nghĩa là đã tắt, 8 byte cuối là thời điểm tắt.
fn disabled_value() -> Vec<u8> {
    // FILETIME: số khoảng 100 nano giây từ 01/01/1601.
    let filetime = (winutil::unix_now() + 11_644_473_600) * 10_000_000;
    let mut bytes = vec![3, 0, 0, 0];
    bytes.extend_from_slice(&filetime.to_le_bytes());
    bytes
}

fn run_values(machine: bool, run_key: &str) -> Vec<(String, String)> {
    reg::values(machine, run_key)
        .into_iter()
        .filter_map(|(name, value)| {
            let command = value.as_string()?;
            (!name.is_empty() && !command.trim().is_empty()).then_some((name, command))
        })
        .collect()
}

fn read_state(machine: bool, subkey: &str, name: &str) -> Option<Vec<u8>> {
    reg::read(machine, subkey, name).map(|value| value.bytes)
}

fn write_state(machine: bool, subkey: &str, name: &str, bytes: &[u8]) -> Result<(), String> {
    let value = RawValue {
        vtype: reg::REG_BINARY,
        bytes: bytes.to_vec(),
    };
    reg::write(machine, subkey, name, &value)
}


fn is_disabled(entry: &StartupEntry) -> bool {
    read_state(entry.machine, &approved_key(entry.approved), &entry.name)
        .and_then(|bytes| bytes.first().copied())
        .is_some_and(|first| first & 1 == 1)
}

/// Tắt một mục khởi động; trả về bước hoàn tác mang trạng thái cũ.
pub fn disable(entry: &StartupEntry) -> Result<UndoOp, String> {
    let subkey = approved_key(entry.approved);
    let previous = read_state(entry.machine, &subkey, &entry.name);
    write_state(entry.machine, &subkey, &entry.name, &disabled_value())?;
    Ok(UndoOp::RestoreStartup {
        machine: entry.machine,
        subkey,
        name: entry.name.clone(),
        previous,
    })
}

/// Trả trạng thái StartupApproved về như trước khi tắt.
pub fn restore(machine: bool, subkey: &str, name: &str, previous: Option<&[u8]>) -> Result<(), String> {
    match previous {
        Some(bytes) => write_state(machine, subkey, name, bytes),
        None => reg::delete_value(machine, subkey, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_quoted_command() {
        assert_eq!(
            parse_command("\"C:\\Program Files\\App\\app.exe\" --minimized"),
            Some(PathBuf::from("C:\\Program Files\\App\\app.exe"))
        );
    }

    #[test]
    fn parse_unquoted_missing_path_with_spaces() {
        assert_eq!(
            parse_command("C:\\No Such Dir\\tool.exe /background"),
            Some(PathBuf::from("C:\\No Such Dir\\tool.exe"))
        );
    }

    #[test]
    fn parse_unquoted_existing_file_with_args() {
        let exe = std::env::current_exe().unwrap();
        let command = format!("{} -arg value", exe.display());
        assert_eq!(parse_command(&command), Some(exe));
    }

    #[test]
    fn disabled_value_has_odd_first_byte_and_timestamp() {
        let value = disabled_value();
        assert_eq!(value.len(), 12);
        assert_eq!(value[0] & 1, 1);
        assert!(value[4..].iter().any(|b| *b != 0));
    }

    /// Chạy trên khóa thử riêng dưới HKCU, không đụng tới StartupApproved thật.
    #[cfg(windows)]
    #[test]
    fn write_then_restore_registry_state() {
        let subkey = format!("Software\\TP System Care Test\\{}", std::process::id());
        assert_eq!(read_state(false, &subkey, "App"), None);

        write_state(false, &subkey, "App", &[2, 0, 0, 0]).unwrap();
        write_state(false, &subkey, "App", &disabled_value()).unwrap();
        assert_eq!(read_state(false, &subkey, "App").unwrap()[0], 3);

        restore(false, &subkey, "App", Some(&[2, 0, 0, 0])).unwrap();
        assert_eq!(read_state(false, &subkey, "App"), Some(vec![2, 0, 0, 0]));

        restore(false, &subkey, "App", None).unwrap();
        assert_eq!(read_state(false, &subkey, "App"), None);

        reg::delete_key(false, &subkey).unwrap();
    }
}
