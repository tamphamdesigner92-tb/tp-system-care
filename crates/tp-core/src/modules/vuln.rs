//! Vá lỗ hổng: hỏi Windows Update Agent các bản cập nhật chưa cài.
//!
//! Engine không tự tải và cài bản vá: việc đó do Windows Update làm, nên "sửa" ở
//! đây là mở trang Windows Update để người dùng cài. Các mục này được tính là
//! "cần bạn hoàn tất", không tính là đã sửa.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::engine::{Action, Finding, Module, ModuleId, ScanContext};
use crate::winutil;

pub struct Vuln;

const WINDOWS_UPDATE: &str = "ms-settings:windowsupdate";

/// Tra cứu trực tuyến nên có thể chậm; quá thời gian này thì bỏ qua hạng mục.
const TIMEOUT: Duration = Duration::from_secs(120);

const SCRIPT: &str = "$ErrorActionPreference='Stop';\
[Console]::OutputEncoding=[Text.Encoding]::UTF8;\
$s=(New-Object -ComObject Microsoft.Update.Session).CreateUpdateSearcher();\
$r=$s.Search(\"IsInstalled=0 and IsHidden=0 and Type='Software'\");\
$o=@();foreach($u in $r.Updates){$o+=[pscustomobject]@{title=[string]$u.Title;severity=[string]$u.MsrcSeverity}};\
ConvertTo-Json -InputObject $o -Compress";

#[derive(Deserialize)]
struct Update {
    title: String,
    #[serde(default)]
    severity: String,
}

fn parse(output: &str) -> Vec<Update> {
    serde_json::from_str(output.trim().trim_start_matches('\u{feff}')).unwrap_or_default()
}

impl Module for Vuln {
    fn id(&self) -> ModuleId {
        ModuleId::Vuln
    }

    fn scan(&self, ctx: &ScanContext) -> Vec<Finding> {
        ctx.report(Path::new("Windows Update"));
        let output = winutil::run_hidden(
            "powershell.exe",
            &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", SCRIPT],
            TIMEOUT,
            Some(ctx.cancel),
        );
        let Ok(output) = output else {
            return Vec::new();
        };
        parse(&output)
            .into_iter()
            .map(|update| {
                // Microsoft chỉ gán mức nghiêm trọng cho bản vá bảo mật.
                let security = !update.severity.trim().is_empty();
                Finding {
                    id: String::new(),
                    module: ModuleId::Vuln,
                    kind: if security { "vuln.security" } else { "vuln.update" },
                    name: Some(update.title),
                    detail: security.then_some(update.severity),
                    count: 1,
                    bytes: 0,
                    recommended: security,
                    undoable: false,
                    needs_admin: false,
                    action: Action::OpenUri(WINDOWS_UPDATE.to_string()),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_update_list() {
        let json = "\u{feff}[{\"title\":\"2026-11 Cumulative Update (KB5000001)\",\"severity\":\"Critical\"},{\"title\":\"Driver\",\"severity\":\"\"}]\r\n";
        let updates = parse(json);
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].severity, "Critical");
        assert!(parse("[]").is_empty());
        assert!(parse("garbage").is_empty());
    }
}
