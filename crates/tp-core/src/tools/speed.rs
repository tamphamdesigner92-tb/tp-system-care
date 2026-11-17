//! Tăng tốc: tình trạng bộ nhớ, giải phóng RAM và Turbo Boost.
//!
//! Giải phóng RAM cắt bộ nhớ làm việc (working set) của các tiến trình mở được:
//! Windows đẩy trang ít dùng ra bộ nhớ chờ, nên RAM trống tăng ngay nhưng tiến
//! trình sẽ nạp lại khi cần. Đây là tác dụng tạm thời, giao diện phải nói rõ.
//!
//! Turbo Boost chuyển sang gói nguồn Hiệu năng cao rồi giải phóng RAM; tắt Turbo
//! trả về gói nguồn trước đó.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::winutil::{self, Known};

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub total: u64,
    pub available: u64,
    /// Phần trăm RAM đang dùng.
    pub load: u32,
}

#[cfg(windows)]
pub fn memory() -> Memory {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: core::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    if unsafe { GlobalMemoryStatusEx(&mut status) }.is_err() {
        return Memory::default();
    }
    Memory {
        total: status.ullTotalPhys,
        available: status.ullAvailPhys,
        load: status.dwMemoryLoad,
    }
}

#[cfg(not(windows))]
pub fn memory() -> Memory {
    Memory::default()
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RamReport {
    pub before: Memory,
    pub after: Memory,
    /// Số tiến trình đã cắt được bộ nhớ làm việc.
    pub processes: u32,
}

#[cfg(windows)]
pub fn release_ram() -> RamReport {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::ProcessStatus::{K32EmptyWorkingSet, K32EnumProcesses};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA};

    let before = memory();
    let mut pids = vec![0u32; 8192];
    let mut needed = 0u32;
    let mut processes = 0;
    unsafe {
        if K32EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut needed).as_bool() {
            let count = (needed as usize / 4).min(pids.len());
            for &pid in &pids[..count] {
                if pid == 0 {
                    continue;
                }
                // Tiến trình của hệ thống hoặc người dùng khác không mở được khi
                // chạy quyền thường: bỏ qua.
                let Ok(handle) = OpenProcess(PROCESS_SET_QUOTA | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                    continue;
                };
                if K32EmptyWorkingSet(handle).as_bool() {
                    processes += 1;
                }
                let _ = CloseHandle(handle);
            }
        }
    }
    // Chờ Windows cập nhật số liệu bộ nhớ.
    std::thread::sleep(Duration::from_millis(600));
    RamReport {
        before,
        after: memory(),
        processes,
    }
}

#[cfg(not(windows))]
pub fn release_ram() -> RamReport {
    RamReport {
        before: Memory::default(),
        after: Memory::default(),
        processes: 0,
    }
}

// ------------------------------------------------------------ Turbo Boost

/// GUID cố định của Windows cho gói nguồn Hiệu năng cao và Hiệu năng tối đa.
const HIGH_PERFORMANCE: &str = "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c";
const ULTIMATE: &str = "e9a42b02-d5df-448d-aa00-03f14749eb61";
const POWERCFG_TIMEOUT: Duration = Duration::from_secs(15);

fn is_guid(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.len() == 5
        && parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Mọi GUID trong đầu ra của powercfg (đầu ra được dịch theo ngôn ngữ Windows,
/// nên chỉ dựa vào dạng GUID).
fn guids(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .filter(|word| is_guid(word))
        .map(|word| word.to_lowercase())
        .collect()
}

fn powercfg(args: &[&str]) -> Result<String, String> {
    winutil::run_hidden("powercfg.exe", args, POWERCFG_TIMEOUT, None)
}

fn active_scheme() -> Option<String> {
    guids(&powercfg(&["/getactivescheme"]).ok()?).into_iter().next()
}

/// Gói nguồn hiệu năng cao có sẵn trên máy. Máy dùng Modern Standby thường chỉ
/// có gói Cân bằng, khi đó Turbo Boost không khả dụng.
fn target_scheme() -> Option<&'static str> {
    let listed = guids(&powercfg(&["/list"]).ok()?);
    [HIGH_PERFORMANCE, ULTIMATE]
        .into_iter()
        .find(|guid| listed.iter().any(|g| g == guid))
}

#[derive(Serialize, Deserialize)]
struct Saved {
    /// Gói nguồn đang dùng trước khi bật Turbo.
    previous: String,
}

fn turbo_file() -> PathBuf {
    let base = winutil::known_folder(Known::LocalAppData).unwrap_or_else(std::env::temp_dir);
    base.join("TP System Care").join("turbo.json")
}

fn saved() -> Option<Saved> {
    serde_json::from_slice(&fs::read(turbo_file()).ok()?).ok()
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Turbo {
    pub on: bool,
    pub available: bool,
}

pub fn turbo_status() -> Turbo {
    let target = target_scheme();
    let on = saved().is_some() && target.is_some() && active_scheme().as_deref() == target;
    Turbo {
        on,
        available: target.is_some(),
    }
}

/// Bật Turbo: lưu gói nguồn hiện tại, chuyển sang Hiệu năng cao, giải phóng RAM.
pub fn turbo_on() -> Result<RamReport, String> {
    let target = target_scheme().ok_or("no_plan")?;
    let previous = active_scheme().ok_or("powercfg")?;
    // Đã bật từ trước thì giữ gói nguồn gốc đã lưu.
    if saved().is_none() {
        let file = turbo_file();
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_vec(&Saved { previous }).map_err(|e| e.to_string())?;
        fs::write(&file, json).map_err(|e| e.to_string())?;
    }
    powercfg(&["/setactive", target])?;
    Ok(release_ram())
}

/// Tắt Turbo: trả về gói nguồn trước khi bật.
pub fn turbo_off() -> Result<(), String> {
    let saved = saved().ok_or("not_on")?;
    powercfg(&["/setactive", &saved.previous])?;
    fs::remove_file(turbo_file()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_guids_in_localized_output() {
        let en = "Power Scheme GUID: 381b4222-f694-41f0-9685-ff5bb260df2e  (Balanced)";
        let vi = "GUID Lược đồ Nguồn: 8C5E7FDA-E8BF-4A96-9A85-A6E23A8C635C  (Hiệu năng cao) *";
        assert_eq!(guids(en), vec!["381b4222-f694-41f0-9685-ff5bb260df2e"]);
        assert_eq!(guids(vi), vec![HIGH_PERFORMANCE]);
        assert!(guids("no guid here 1234-5678").is_empty());
        assert!(!is_guid("381b4222-f694-41f0-9685-ff5bb260df2"));
    }

    #[cfg(windows)]
    #[test]
    fn memory_numbers_are_sane() {
        let m = memory();
        assert!(m.total > 0 && m.available <= m.total && m.load <= 100);
    }
}
