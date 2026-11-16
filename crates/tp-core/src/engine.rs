//! Interface chung của engine: module quét ra [`Finding`], mỗi mục mang một
//! [`Action`]; [`fix`] thực thi hành động và ghi lịch sử vào Rescue Center.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::modules::disk::{self, Toggle};
use crate::modules::startup::{self, StartupEntry};
use crate::reg::{self, RawValue};
use crate::rescue::{self, Rescue, UndoOp};
use crate::winutil;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModuleId {
    Junk,
    Privacy,
    Shortcut,
    Registry,
    Startup,
    System,
    Internet,
    Disk,
    Vuln,
    Security,
}

impl ModuleId {
    pub fn as_str(self) -> &'static str {
        match self {
            ModuleId::Junk => "junk",
            ModuleId::Privacy => "privacy",
            ModuleId::Shortcut => "shortcut",
            ModuleId::Registry => "registry",
            ModuleId::Startup => "startup",
            ModuleId::System => "system",
            ModuleId::Internet => "internet",
            ModuleId::Disk => "disk",
            ModuleId::Vuln => "vuln",
            ModuleId::Security => "security",
        }
    }

    pub const ALL: [ModuleId; 10] = [
        ModuleId::Junk,
        ModuleId::Privacy,
        ModuleId::Shortcut,
        ModuleId::Registry,
        ModuleId::Startup,
        ModuleId::System,
        ModuleId::Internet,
        ModuleId::Disk,
        ModuleId::Vuln,
        ModuleId::Security,
    ];
}

/// Việc sẽ làm khi người dùng bấm Sửa cho một mục.
#[derive(Clone, Debug)]
pub enum Action {
    /// Xóa hẳn các tệp. Chỉ xóa tệp nằm trong `roots` (kiểm tra lại lúc sửa).
    DeleteFiles {
        roots: Vec<PathBuf>,
        files: Vec<PathBuf>,
    },
    EmptyRecycleBin,
    /// Xóa mọi giá trị của một khóa dưới HKEY_CURRENT_USER.
    ClearRegistryValues { subkey: String },
    /// Chuyển tệp vào Rescue Center; hoàn tác được.
    QuarantineFile { path: PathBuf },
    /// Tắt mục khởi động theo cách của Task Manager; hoàn tác được.
    DisableStartup(StartupEntry),
    /// Đặt một giá trị registry; giá trị cũ được lưu để hoàn tác.
    SetRegistryValue {
        machine: bool,
        subkey: String,
        name: String,
        value: RawValue,
    },
    /// Xóa các giá trị của một khóa; từng giá trị được lưu để hoàn tác.
    DeleteRegistryValues {
        machine: bool,
        subkey: String,
        names: Vec<String>,
    },
    /// Xóa các khóa không có khóa con; giá trị của chúng được lưu để hoàn tác.
    DeleteRegistryKeys { machine: bool, subkeys: Vec<String> },
    /// Bật/tắt một thiết lập hệ thống đổi bằng lệnh của Windows.
    SetToggle { toggle: Toggle, enable: bool },
    /// Mở một trang cài đặt của Windows để người dùng tự hoàn tất.
    OpenUri(String),
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub id: String,
    pub module: ModuleId,
    /// Loại mục, giao diện dùng để tra nhãn (ví dụ `junk.userTemp`).
    pub kind: &'static str,
    /// Tên riêng của mục (tên lối tắt, tên mục khởi động) nếu có.
    pub name: Option<String>,
    /// Đường dẫn hoặc dòng lệnh liên quan.
    pub detail: Option<String>,
    pub count: u64,
    pub bytes: u64,
    /// Chọn sẵn khi hiện kết quả.
    pub recommended: bool,
    pub undoable: bool,
    pub needs_admin: bool,
    #[serde(skip)]
    pub action: Action,
}

pub struct ScanContext<'a> {
    pub cancel: &'a AtomicBool,
    /// Báo đường dẫn đang quét; nơi nhận tự giới hạn tần suất.
    pub progress: &'a (dyn Fn(&str) + Sync),
}

impl ScanContext<'_> {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn report(&self, path: &Path) {
        (self.progress)(&path.to_string_lossy());
    }
}

/// Một hạng mục quét. Module chỉ đọc; mọi thay đổi đi qua [`fix`].
pub trait Module: Sync {
    fn id(&self) -> ModuleId;
    fn scan(&self, ctx: &ScanContext) -> Vec<Finding>;
}

pub fn module(id: ModuleId) -> &'static dyn Module {
    use crate::modules::*;
    match id {
        ModuleId::Junk => &junk::Junk,
        ModuleId::Privacy => &privacy::Privacy,
        ModuleId::Shortcut => &shortcut::Shortcut,
        ModuleId::Registry => &registry::Registry,
        ModuleId::Startup => &startup::Startup,
        ModuleId::System => &tweaks::System,
        ModuleId::Internet => &tweaks::Internet,
        ModuleId::Disk => &disk::Disk,
        ModuleId::Vuln => &vuln::Vuln,
        ModuleId::Security => &tweaks::Security,
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleResult {
    pub module: ModuleId,
    pub findings: Vec<Finding>,
}

/// Sự kiện trong lúc quét.
pub enum ScanEvent<'a> {
    ModuleStarted(ModuleId),
    Path(&'a str),
    ModuleFinished(&'a ModuleResult),
}

/// Quét lần lượt các module. Dừng sớm nếu `cancel` được bật; các module chưa
/// chạy sẽ không có trong kết quả.
pub fn scan(
    modules: &[ModuleId],
    cancel: &AtomicBool,
    on_event: &(dyn Fn(ScanEvent) + Sync),
) -> Vec<ModuleResult> {
    let mut results = Vec::new();
    for &id in modules {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        on_event(ScanEvent::ModuleStarted(id));
        let progress = |path: &str| on_event(ScanEvent::Path(path));
        let ctx = ScanContext {
            cancel,
            progress: &progress,
        };
        let mut findings = module(id).scan(&ctx);
        for (index, finding) in findings.iter_mut().enumerate() {
            finding.id = format!("{}:{}", id.as_str(), index);
        }
        let result = ModuleResult {
            module: id,
            findings,
        };
        on_event(ScanEvent::ModuleFinished(&result));
        results.push(result);
    }
    results
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixItem {
    pub id: String,
    pub module: Option<ModuleId>,
    pub done: u64,
    pub failed: u64,
    pub bytes: u64,
    /// Số mục đã mở trang cài đặt để người dùng tự hoàn tất.
    pub manual: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixReport {
    pub items: Vec<FixItem>,
    pub done: u64,
    pub failed: u64,
    pub bytes: u64,
    pub manual: u64,
    /// Số thao tác có thể hoàn tác trong Rescue Center.
    pub undoable: u64,
    /// Mã bản ghi lịch sử; `None` nếu không có gì được thực hiện.
    pub record_id: Option<u64>,
}

/// Sửa các mục đã chọn và ghi một bản ghi vào Rescue Center.
pub fn fix(
    findings: &[Finding],
    rescue: &Rescue,
    cancel: &AtomicBool,
    on_progress: &dyn Fn(usize, usize),
) -> FixReport {
    let record_id = rescue.new_record_id();
    let mut report = FixReport::default();
    let mut undo = Vec::new();
    let mut modules = BTreeSet::new();
    // Các khóa registry đã xuất ra tệp .reg và các trang cài đặt đã mở trong lần sửa này.
    let mut exported = BTreeSet::new();
    let mut opened = BTreeSet::new();

    for (index, finding) in findings.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        on_progress(index, findings.len());

        // Sao lưu registry trước khi xóa hoặc đổi trạng thái khởi động.
        let backup_key = match &finding.action {
            Action::DisableStartup(entry) => Some(format!("{}\\{}", reg::hive_name(entry.machine), startup::APPROVED)),
            Action::DeleteRegistryValues { machine, subkey, .. } => {
                Some(format!("{}\\{}", reg::hive_name(*machine), subkey))
            }
            Action::DeleteRegistryKeys { machine, subkeys } => subkeys
                .first()
                .and_then(|key| key.rsplit_once('\\'))
                .map(|(parent, _)| format!("{}\\{}", reg::hive_name(*machine), parent)),
            _ => None,
        };
        if let Some(key) = backup_key {
            if exported.insert(key.clone()) {
                // Khóa chưa tồn tại thì không có gì để sao lưu: bỏ qua lỗi.
                let file = format!("registry-{}.reg", exported.len());
                let _ = rescue::export_registry_key(rescue, record_id, &key, &file);
            }
        }

        let mut item = apply(finding, rescue, record_id, &mut undo, &mut opened);
        item.id = finding.id.clone();
        item.module = Some(finding.module);
        if item.done > 0 {
            modules.insert(finding.module);
        }
        report.done += item.done;
        report.failed += item.failed;
        report.bytes += item.bytes;
        report.manual += item.manual;
        report.items.push(item);
    }
    on_progress(findings.len(), findings.len());

    report.undoable = undo.len() as u64;
    if report.done > 0 {
        let saved = rescue.add_record(
            record_id,
            modules.into_iter().collect(),
            report.done,
            report.failed,
            report.bytes,
            undo,
        );
        if saved.is_ok() {
            report.record_id = Some(record_id);
        }
    }
    report
}

fn apply(
    finding: &Finding,
    rescue: &Rescue,
    record_id: u64,
    undo: &mut Vec<UndoOp>,
    opened: &mut BTreeSet<String>,
) -> FixItem {
    let mut item = FixItem::default();
    match &finding.action {
        Action::DeleteFiles { roots, files } => {
            let mut dirs = BTreeSet::new();
            for file in files {
                if !roots.iter().any(|root| file.starts_with(root)) {
                    item.failed += 1;
                    continue;
                }
                let size = fs::symlink_metadata(file).map(|m| m.len()).unwrap_or(0);
                match fs::remove_file(file) {
                    Ok(()) => {
                        item.done += 1;
                        item.bytes += size;
                        if let Some(parent) = file.parent() {
                            dirs.insert(parent.to_path_buf());
                        }
                    }
                    // Đã biến mất từ lúc quét: không tính là lỗi.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => item.failed += 1,
                }
            }
            remove_empty_dirs(dirs, roots);
            if item.failed > 0 {
                item.error = Some("in_use".into());
            }
        }
        Action::EmptyRecycleBin => match winutil::empty_recycle_bin() {
            Ok(()) => {
                item.done = finding.count;
                item.bytes = finding.bytes;
            }
            Err(e) => {
                item.failed = finding.count;
                item.error = Some(e);
            }
        },
        Action::ClearRegistryValues { subkey } => {
            match crate::modules::privacy::clear_values(subkey) {
                Ok(n) => item.done = n,
                Err(e) => {
                    item.failed = finding.count;
                    item.error = Some(e);
                }
            }
        }
        Action::QuarantineFile { path } => match rescue.quarantine(record_id, path) {
            Ok(op) => {
                undo.push(op);
                item.done = 1;
            }
            Err(e) => {
                item.failed = 1;
                item.error = Some(e);
            }
        },
        Action::DisableStartup(entry) => match startup::disable(entry) {
            Ok(op) => {
                undo.push(op);
                item.done = 1;
            }
            Err(e) => {
                item.failed = 1;
                item.error = Some(e);
            }
        },
        Action::SetRegistryValue {
            machine,
            subkey,
            name,
            value,
        } => {
            let previous = reg::read(*machine, subkey, name);
            match reg::write(*machine, subkey, name, value) {
                Ok(()) => {
                    undo.push(UndoOp::RestoreRegistryValue {
                        machine: *machine,
                        subkey: subkey.clone(),
                        name: name.clone(),
                        previous,
                    });
                    item.done = 1;
                }
                Err(e) => {
                    item.failed = 1;
                    item.error = Some(e);
                }
            }
        }
        Action::DeleteRegistryValues {
            machine,
            subkey,
            names,
        } => {
            for name in names {
                // Đã biến mất từ lúc quét: không tính là lỗi.
                let Some(previous) = reg::read(*machine, subkey, name) else {
                    continue;
                };
                match reg::delete_value(*machine, subkey, name) {
                    Ok(()) => {
                        undo.push(UndoOp::RestoreRegistryValue {
                            machine: *machine,
                            subkey: subkey.clone(),
                            name: name.clone(),
                            previous: Some(previous),
                        });
                        item.done += 1;
                    }
                    Err(e) => {
                        item.failed += 1;
                        item.error = Some(e);
                    }
                }
            }
        }
        Action::DeleteRegistryKeys { machine, subkeys } => {
            for subkey in subkeys {
                // Chỉ xóa khóa phẳng: khóa có khóa con không hoàn tác trọn vẹn được.
                if reg::has_subkeys(*machine, subkey) {
                    item.failed += 1;
                    continue;
                }
                let values = reg::values(*machine, subkey);
                match reg::delete_key(*machine, subkey) {
                    Ok(()) => {
                        undo.push(UndoOp::RestoreRegistryKey {
                            machine: *machine,
                            subkey: subkey.clone(),
                            values,
                        });
                        item.done += 1;
                    }
                    Err(e) => {
                        item.failed += 1;
                        item.error = Some(e);
                    }
                }
            }
        }
        Action::SetToggle { toggle, enable } => match disk::set_toggle(*toggle, *enable) {
            Ok(()) => {
                undo.push(UndoOp::SetToggle {
                    toggle: *toggle,
                    enable: !*enable,
                });
                item.done = 1;
            }
            Err(e) => {
                item.failed = 1;
                item.error = Some(e);
            }
        },
        Action::OpenUri(uri) => {
            if opened.insert(uri.clone()) {
                if let Err(e) = winutil::open_uri(uri) {
                    item.error = Some(e);
                }
            }
            item.manual = 1;
        }
    }
    item
}

/// Xóa các thư mục đã rỗng sau khi dọn, đi ngược lên nhưng không chạm vào `roots`.
fn remove_empty_dirs(dirs: BTreeSet<PathBuf>, roots: &[PathBuf]) {
    // Duyệt ngược để thư mục con được xử lý trước thư mục cha.
    for dir in dirs.into_iter().rev() {
        let mut current = dir.as_path();
        while !roots.iter().any(|root| root == current)
            && roots.iter().any(|root| current.starts_with(root))
            && fs::remove_dir(current).is_ok()
        {
            match current.parent() {
                Some(parent) => current = parent,
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tp-core-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn finding(action: Action) -> Finding {
        Finding {
            id: "junk:0".into(),
            module: ModuleId::Junk,
            kind: "junk.userTemp",
            name: None,
            detail: None,
            count: 0,
            bytes: 0,
            recommended: true,
            undoable: false,
            needs_admin: false,
            action,
        }
    }

    #[test]
    fn delete_files_removes_only_inside_roots() {
        let base = temp_dir("delete");
        let root = base.join("root");
        let outside = base.join("outside.txt");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.tmp"), b"12345").unwrap();
        fs::write(root.join("sub").join("b.tmp"), b"123").unwrap();
        fs::write(&outside, b"keep").unwrap();

        let rescue = Rescue::at(base.join("rescue"));
        let report = fix(
            &[finding(Action::DeleteFiles {
                roots: vec![root.clone()],
                files: vec![root.join("a.tmp"), root.join("sub").join("b.tmp"), outside.clone()],
            })],
            &rescue,
            &AtomicBool::new(false),
            &|_, _| {},
        );

        assert_eq!(report.done, 2);
        assert_eq!(report.failed, 1);
        assert_eq!(report.bytes, 8);
        assert!(outside.exists(), "tệp ngoài root phải còn nguyên");
        assert!(root.exists(), "thư mục root phải còn");
        assert!(!root.join("sub").exists(), "thư mục con rỗng phải được dọn");
        assert_eq!(rescue.history().len(), 1);
        assert_eq!(report.undoable, 0);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn quarantine_then_undo_restores_file() {
        let base = temp_dir("quarantine");
        let file = base.join("Broken.lnk");
        fs::write(&file, b"lnk").unwrap();

        let rescue = Rescue::at(base.join("rescue"));
        let mut f = finding(Action::QuarantineFile { path: file.clone() });
        f.module = ModuleId::Shortcut;
        let report = fix(&[f], &rescue, &AtomicBool::new(false), &|_, _| {});

        assert_eq!(report.done, 1);
        assert_eq!(report.undoable, 1);
        assert!(!file.exists());

        let id = report.record_id.unwrap();
        let undo = rescue.undo(id).unwrap();
        assert_eq!((undo.restored, undo.failed), (1, 0));
        assert_eq!(fs::read(&file).unwrap(), b"lnk");
        assert!(rescue.history()[0].undone);
        assert!(rescue.undo(id).is_err(), "không hoàn tác hai lần");
        fs::remove_dir_all(base).unwrap();
    }

    /// Chạy trên khóa thử riêng dưới HKCU.
    #[cfg(windows)]
    #[test]
    fn registry_actions_are_undone_exactly() {
        let base = temp_dir("registry");
        let rescue = Rescue::at(base.join("rescue"));
        let root = format!("Software\\TP System Care Test\\engine-{}", std::process::id());
        let values = format!("{root}\\Values");
        let flat = format!("{root}\\Flat.exe");
        let nested = format!("{root}\\Nested");

        reg::write(false, &values, "Keep", &RawValue::dword(1)).unwrap();
        reg::write(false, &values, "C:\\gone.dll", &RawValue::dword(2)).unwrap();
        reg::write(false, &values, "Setting", &RawValue::sz("400")).unwrap();
        reg::write(false, &flat, "", &RawValue::sz("C:\\gone\\flat.exe")).unwrap();
        reg::write(false, &flat, "Path", &RawValue::sz("C:\\gone")).unwrap();
        reg::write(false, &format!("{nested}\\Child"), "x", &RawValue::dword(1)).unwrap();

        let findings = [
            finding(Action::SetRegistryValue {
                machine: false,
                subkey: values.clone(),
                name: "Setting".into(),
                value: RawValue::sz("100"),
            }),
            finding(Action::SetRegistryValue {
                machine: false,
                subkey: values.clone(),
                name: "New".into(),
                value: RawValue::dword(0),
            }),
            finding(Action::DeleteRegistryValues {
                machine: false,
                subkey: values.clone(),
                names: vec!["C:\\gone.dll".into(), "AlreadyGone".into()],
            }),
            finding(Action::DeleteRegistryKeys {
                machine: false,
                subkeys: vec![flat.clone(), nested.clone()],
            }),
        ];
        let report = fix(&findings, &rescue, &AtomicBool::new(false), &|_, _| {});

        assert_eq!((report.done, report.failed, report.undoable), (4, 1, 4));
        assert_eq!(reg::read(false, &values, "Setting").unwrap().as_string().unwrap(), "100");
        assert!(reg::read(false, &values, "C:\\gone.dll").is_none());
        assert!(reg::values(false, &flat).is_empty(), "khóa phẳng phải bị xóa");
        assert!(reg::has_subkeys(false, &nested), "khóa có khóa con phải còn nguyên");
        assert!(rescue.record_dir(report.record_id.unwrap()).join("registry").is_dir());

        let undo = rescue.undo(report.record_id.unwrap()).unwrap();
        assert_eq!((undo.restored, undo.failed), (4, 0));
        assert_eq!(reg::read(false, &values, "Setting").unwrap().as_string().unwrap(), "400");
        assert!(reg::read(false, &values, "New").is_none(), "giá trị mới tạo phải bị gỡ");
        assert_eq!(reg::read(false, &values, "C:\\gone.dll").unwrap().as_dword(), Some(2));
        assert_eq!(reg::values(false, &flat).len(), 2);
        assert_eq!(reg::read(false, &values, "Keep").unwrap().as_dword(), Some(1));

        reg::delete_key(false, &root).unwrap();
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn nothing_done_writes_no_record() {
        let base = temp_dir("empty");
        let rescue = Rescue::at(base.join("rescue"));
        let report = fix(
            &[finding(Action::QuarantineFile {
                path: base.join("missing.lnk"),
            })],
            &rescue,
            &AtomicBool::new(false),
            &|_, _| {},
        );
        assert_eq!(report.done, 0);
        assert_eq!(report.failed, 1);
        assert!(report.record_id.is_none());
        assert!(rescue.history().is_empty());
        fs::remove_dir_all(base).unwrap();
    }
}
