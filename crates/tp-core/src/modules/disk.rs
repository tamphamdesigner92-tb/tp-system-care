//! Tối ưu ổ đĩa: kiểm tra TRIM cho SSD và lịch tối ưu ổ đĩa định kỳ của Windows.
//! Hai thiết lập này đổi bằng lệnh của Windows (`fsutil`, `schtasks`) và cần quyền
//! quản trị để sửa.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::engine::{Action, Finding, Module, ModuleId, ScanContext};
use crate::winutil;

pub struct Disk;

/// Thiết lập bật/tắt được. Là enum cố định (không phải dòng lệnh tùy ý) vì bước
/// hoàn tác được đọc lại từ tệp lịch sử.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Toggle {
    /// Lệnh TRIM báo cho SSD biết khối nào đã trống.
    Trim,
    /// Tác vụ định kỳ "Optimize Drives" (chống phân mảnh HDD, TRIM lại SSD).
    DefragTask,
}

const DEFRAG_TASK: &str = "\\Microsoft\\Windows\\Defrag\\ScheduledDefrag";
const TIMEOUT: Duration = Duration::from_secs(20);

/// `fsutil` in ra dạng `NTFS DisableDeleteNotify = 1  (Enabled)`.
fn trim_disabled(output: &str) -> Option<bool> {
    let line = output.lines().find(|line| line.contains("NTFS DisableDeleteNotify"))?;
    let value = line.split('=').nth(1)?.trim();
    Some(value.starts_with('1'))
}

/// Trong XML của tác vụ, `<Enabled>false</Enabled>` nằm trong `<Settings>`.
fn task_disabled(xml: &str) -> Option<bool> {
    let settings = xml.split("<Settings>").nth(1)?.split("</Settings>").next()?;
    Some(settings.contains("<Enabled>false</Enabled>"))
}

pub fn set_toggle(toggle: Toggle, enable: bool) -> Result<(), String> {
    if !winutil::is_elevated() {
        return Err("needs_admin".into());
    }
    let result = match toggle {
        Toggle::Trim => winutil::run_hidden(
            "fsutil.exe",
            &["behavior", "set", "DisableDeleteNotify", if enable { "0" } else { "1" }],
            TIMEOUT,
            None,
        ),
        Toggle::DefragTask => winutil::run_hidden(
            "schtasks.exe",
            &["/Change", "/TN", DEFRAG_TASK, if enable { "/ENABLE" } else { "/DISABLE" }],
            TIMEOUT,
            None,
        ),
    };
    result.map(|_| ())
}

fn finding(kind: &'static str, toggle: Toggle) -> Finding {
    Finding {
        id: String::new(),
        module: ModuleId::Disk,
        kind,
        name: None,
        detail: None,
        count: 1,
        bytes: 0,
        recommended: true,
        undoable: true,
        needs_admin: true,
        action: Action::SetToggle { toggle, enable: true },
    }
}

impl Module for Disk {
    fn id(&self) -> ModuleId {
        ModuleId::Disk
    }

    fn scan(&self, ctx: &ScanContext) -> Vec<Finding> {
        let mut findings = Vec::new();

        ctx.report(Path::new("fsutil behavior query DisableDeleteNotify"));
        let trim = winutil::run_hidden(
            "fsutil.exe",
            &["behavior", "query", "DisableDeleteNotify"],
            TIMEOUT,
            Some(ctx.cancel),
        );
        if trim.ok().and_then(|out| trim_disabled(&out)) == Some(true) {
            findings.push(finding("disk.trimOff", Toggle::Trim));
        }
        if ctx.cancelled() {
            return findings;
        }

        ctx.report(Path::new(DEFRAG_TASK));
        let task = winutil::run_hidden(
            "schtasks.exe",
            &["/Query", "/TN", DEFRAG_TASK, "/XML"],
            TIMEOUT,
            Some(ctx.cancel),
        );
        if task.ok().and_then(|xml| task_disabled(&xml)) == Some(true) {
            findings.push(finding("disk.optimizeOff", Toggle::DefragTask));
        }
        findings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fsutil_output() {
        let on = "NTFS DisableDeleteNotify = 0  (Disabled)\r\nReFS DisableDeleteNotify = 1  (Enabled)\r\n";
        let off = "NTFS DisableDeleteNotify = 1  (Enabled)\r\nReFS DisableDeleteNotify = 0  (Disabled)\r\n";
        assert_eq!(trim_disabled(on), Some(false));
        assert_eq!(trim_disabled(off), Some(true));
        assert_eq!(trim_disabled("Error: access denied"), None);
    }

    #[test]
    fn parses_task_xml() {
        let enabled = "<Task><Triggers><Enabled>false</Enabled></Triggers><Settings><Enabled>true</Enabled></Settings></Task>";
        let disabled = "<Task><Settings><Enabled>false</Enabled></Settings></Task>";
        assert_eq!(task_disabled(enabled), Some(false));
        assert_eq!(task_disabled(disabled), Some(true));
        assert_eq!(task_disabled("ERROR"), None);
    }
}
