//! Gỡ bản cập nhật Windows (Gỡ sạch đợt 3).
//!
//! Liệt kê bằng `Get-HotFix` và gỡ bằng `wusa.exe /uninstall` của Windows, luôn
//! hiện hộp xác nhận của chính `wusa` (không chạy im lặng) và không tự khởi động
//! lại. Không ép dừng `wusa`: ngừng chờ chỉ nhả giao diện, việc gỡ vẫn tiếp tục.
//! Bản cập nhật tích lũy gộp với servicing stack thường không gỡ được bằng `wusa`;
//! khi đó người dùng được dẫn sang trang lịch sử Windows Update.

use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    /// Mã `KB` + số, dùng làm mã ở giao diện.
    pub id: String,
    pub description: Option<String>,
    /// `YYYY-MM-DD`.
    pub installed_on: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateResult {
    pub exit_code: Option<u32>,
    pub restart_required: bool,
    /// Vẫn còn trong danh sách sau khi `wusa` thoát. Bản gỡ cần khởi động lại vẫn
    /// được Windows liệt kê cho tới lần khởi động sau.
    pub still_installed: bool,
}

/// `wusa`: 3010 = cần khởi động lại, 1641 = đã yêu cầu khởi động lại.
const RESTART_CODES: [u32; 2] = [3010, 1641];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUpdate {
    id: Option<String>,
    description: Option<String>,
    installed_on: Option<String>,
}

/// Mã KB hợp lệ: `KB` + 6–8 chữ số.
pub fn valid_kb(id: &str) -> bool {
    id.len() >= 8
        && id.len() <= 10
        && id[..2].eq_ignore_ascii_case("KB")
        && id[2..].bytes().all(|c| c.is_ascii_digit())
}

fn valid_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn parse(json: &str) -> Result<Vec<Update>, String> {
    let json = json.trim_start_matches('\u{feff}').trim();
    if json.is_empty() {
        return Ok(Vec::new());
    }
    let raw: Vec<RawUpdate> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut updates: Vec<Update> = Vec::new();
    for item in raw {
        let Some(id) = item
            .id
            .map(|id| id.trim().to_ascii_uppercase())
            .filter(|id| valid_kb(id))
        else {
            continue;
        };
        if updates.iter().any(|known| known.id == id) {
            continue;
        }
        updates.push(Update {
            id,
            description: item
                .description
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty()),
            installed_on: item.installed_on.filter(|d| valid_date(d)),
        });
    }
    updates.sort_by(|a, b| b.installed_on.cmp(&a.installed_on).then(b.id.cmp(&a.id)));
    Ok(updates)
}

#[cfg(windows)]
fn windows_tool(relative: &str) -> Result<std::path::PathBuf, String> {
    use crate::winutil::{self, Known};
    Ok(winutil::known_folder(Known::Windows)
        .ok_or("not_found")?
        .join(relative))
}

#[cfg(windows)]
pub fn list() -> Result<Vec<Update>, String> {
    const SCRIPT: &str = "$ErrorActionPreference='Stop'; \
        [Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false); \
        $items = @(Get-HotFix | ForEach-Object { [pscustomobject]@{ \
            id = [string]$_.HotFixID; description = [string]$_.Description; \
            installedOn = $(if ($_.InstalledOn) { $_.InstalledOn.ToString('yyyy-MM-dd', [Globalization.CultureInfo]::InvariantCulture) } else { $null }) } }); \
        ConvertTo-Json -InputObject $items -Compress";
    let program = windows_tool("System32/WindowsPowerShell/v1.0/powershell.exe")?;
    let output = crate::winutil::run_hidden(
        &program.to_string_lossy(),
        &[
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            SCRIPT,
        ],
        std::time::Duration::from_secs(90),
        None,
    )?;
    parse(&output)
}

#[cfg(not(windows))]
pub fn list() -> Result<Vec<Update>, String> {
    Err("chỉ hỗ trợ Windows".into())
}

/// Chạy `wusa.exe /uninstall /kb:<số> /norestart` và chờ nó thoát. Dùng
/// ShellExecuteEx để Windows tự hỏi UAC khi cần.
#[cfg(windows)]
pub fn uninstall(id: &str, cancel: &AtomicBool) -> Result<UpdateResult, String> {
    use std::sync::atomic::Ordering;
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    if !valid_kb(id) {
        return Err("not_found".into());
    }
    let program = windows_tool("System32/wusa.exe")?;
    let file = HSTRING::from(program.as_os_str());
    let params = HSTRING::from(format!("/uninstall /kb:{} /norestart", &id[2..]));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: core::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    let exit_code = unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        ShellExecuteExW(&mut info).map_err(|e| e.message())?;
        if info.hProcess.is_invalid() {
            None
        } else {
            let mut code = None;
            while !cancel.load(Ordering::SeqCst) {
                match WaitForSingleObject(info.hProcess, 200) {
                    WAIT_OBJECT_0 => {
                        let mut value = 0u32;
                        if GetExitCodeProcess(info.hProcess, &mut value).is_ok() {
                            code = Some(value);
                        }
                        break;
                    }
                    WAIT_TIMEOUT => {}
                    _ => break,
                }
            }
            let _ = CloseHandle(info.hProcess);
            code
        }
    };
    // Không đọc lại được danh sách thì coi như vẫn còn, không báo gỡ thành công giả.
    let still_installed = list()
        .map(|updates| updates.iter().any(|u| u.id == id))
        .unwrap_or(true);
    Ok(UpdateResult {
        exit_code,
        restart_required: exit_code.is_some_and(|code| RESTART_CODES.contains(&code)),
        still_installed,
    })
}

#[cfg(not(windows))]
pub fn uninstall(_id: &str, _cancel: &AtomicBool) -> Result<UpdateResult, String> {
    Err("chỉ hỗ trợ Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_kb_ids() {
        assert!(valid_kb("KB5034441") && valid_kb("kb123456") && valid_kb("KB12345678"));
        assert!(
            !valid_kb("KB12345")
                && !valid_kb("KB123456789")
                && !valid_kb("5034441")
                && !valid_kb("KB50344a1")
        );
        assert!(!valid_kb("KB503444 /quiet"));
    }

    #[test]
    fn parses_hotfix_json_and_drops_invalid_rows() {
        let json = "\u{feff}[{\"id\":\"KB5000001\",\"description\":\"Security Update\",\"installedOn\":\"2026-11-12\"},\
            {\"id\":\"kb5000002\",\"description\":\" Update \",\"installedOn\":\"11/1/2026\"},\
            {\"id\":\"KB5000001\",\"description\":\"dup\",\"installedOn\":null},\
            {\"id\":\"File 1\",\"description\":\"Hotfix\",\"installedOn\":null},\
            {\"id\":null}]";
        let updates = parse(json).unwrap();
        assert_eq!(updates.len(), 2);
        assert_eq!(
            updates[0],
            Update {
                id: "KB5000001".into(),
                description: Some("Security Update".into()),
                installed_on: Some("2026-11-12".into())
            }
        );
        assert_eq!(
            updates[1],
            Update {
                id: "KB5000002".into(),
                description: Some("Update".into()),
                installed_on: None
            }
        );
        assert!(parse("").unwrap().is_empty());
        assert!(parse("{broken").is_err());
    }
}
