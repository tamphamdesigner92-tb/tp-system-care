//! Các lời gọi API Windows dùng chung.

use std::path::{Path, PathBuf};

/// Tiến trình hiện tại có đang chạy với quyền quản trị (đã nâng quyền UAC) hay không.
#[cfg(windows)]
pub fn is_elevated() -> bool {
    use core::ffi::c_void;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut TOKEN_ELEVATION as *mut c_void),
            core::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}

/// Thư mục đặc biệt của Windows. Dùng API thay vì biến môi trường vì Desktop,
/// Documents... có thể đã bị chuyển hướng (ví dụ sang OneDrive).
#[derive(Clone, Copy)]
pub enum Known {
    Desktop,
    PublicDesktop,
    StartMenu,
    CommonStartMenu,
    Startup,
    CommonStartup,
    Recent,
    LocalAppData,
    RoamingAppData,
    ProgramData,
    Windows,
}

#[cfg(windows)]
pub fn known_folder(which: Known) -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::*;

    let id = match which {
        Known::Desktop => &FOLDERID_Desktop,
        Known::PublicDesktop => &FOLDERID_PublicDesktop,
        Known::StartMenu => &FOLDERID_StartMenu,
        Known::CommonStartMenu => &FOLDERID_CommonStartMenu,
        Known::Startup => &FOLDERID_Startup,
        Known::CommonStartup => &FOLDERID_CommonStartup,
        Known::Recent => &FOLDERID_Recent,
        Known::LocalAppData => &FOLDERID_LocalAppData,
        Known::RoamingAppData => &FOLDERID_RoamingAppData,
        Known::ProgramData => &FOLDERID_ProgramData,
        Known::Windows => &FOLDERID_Windows,
    };
    unsafe {
        let raw = SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let path = raw.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(raw.as_ptr() as *const _));
        path
    }
}

#[cfg(not(windows))]
pub fn known_folder(_which: Known) -> Option<PathBuf> {
    None
}

/// Thay `%TÊN%` bằng giá trị biến môi trường; biến không tồn tại thì giữ nguyên.
pub fn expand_env(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(value) if !name.is_empty() => out.push_str(&value),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Đường dẫn nằm trên ổ cứng cố định đang gắn vào máy. Chỉ khi đó việc "tệp không
/// tồn tại" mới đáng tin: ổ mạng hay ổ rời có thể chỉ đang tạm ngắt.
#[cfg(windows)]
pub fn on_fixed_drive(path: &Path) -> bool {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    const DRIVE_FIXED: u32 = 3;

    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return false; // UNC hoặc đường dẫn tương đối
    }
    let root = format!("{}:\\", bytes[0] as char);
    unsafe { GetDriveTypeW(&HSTRING::from(root)) == DRIVE_FIXED }
}

#[cfg(not(windows))]
pub fn on_fixed_drive(_path: &Path) -> bool {
    false
}

/// Đích mà một tệp `.lnk` trỏ tới. `None` nếu lối tắt không trỏ tới tệp thường
/// (ứng dụng Store, mục shell, lối tắt MSI dạng quảng bá).
#[cfg(windows)]
pub fn shortcut_target(lnk: &Path) -> Option<PathBuf> {
    use windows::core::{Interface, HSTRING};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED, STGM_READ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink, SLGP_RAWPATH};

    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = (|| {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
            let file: IPersistFile = link.cast().ok()?;
            file.Load(&HSTRING::from(lnk.as_os_str()), STGM_READ).ok()?;
            let mut buf = [0u16; 1024];
            link.GetPath(&mut buf, core::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
                .ok()?;
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            if len == 0 {
                return None;
            }
            let raw = String::from_utf16_lossy(&buf[..len]);
            Some(PathBuf::from(expand_env(&raw)))
        })();
        if init.is_ok() {
            CoUninitialize();
        }
        result
    }
}

#[cfg(not(windows))]
pub fn shortcut_target(_lnk: &Path) -> Option<PathBuf> {
    None
}

/// Số mục và tổng dung lượng trong Thùng rác của mọi ổ.
#[cfg(windows)]
pub fn recycle_bin_info() -> Option<(u64, u64)> {
    use windows::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    unsafe {
        let mut info = SHQUERYRBINFO {
            cbSize: core::mem::size_of::<SHQUERYRBINFO>() as u32,
            ..Default::default()
        };
        SHQueryRecycleBinW(None, &mut info).ok()?;
        Some((info.i64NumItems.max(0) as u64, info.i64Size.max(0) as u64))
    }
}

#[cfg(not(windows))]
pub fn recycle_bin_info() -> Option<(u64, u64)> {
    None
}

#[cfg(windows)]
pub fn empty_recycle_bin() -> Result<(), String> {
    use windows::Win32::UI::Shell::{
        SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND,
    };
    unsafe {
        SHEmptyRecycleBinW(
            None,
            None,
            SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND,
        )
        .map_err(|e| e.message())
    }
}

#[cfg(not(windows))]
pub fn empty_recycle_bin() -> Result<(), String> {
    Err("chỉ hỗ trợ Windows".into())
}

/// Dung lượng trống và tổng dung lượng (byte) của ổ chứa `path`.
#[cfg(windows)]
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let (mut free, mut total) = (0u64, 0u64);
    unsafe {
        GetDiskFreeSpaceExW(&HSTRING::from(path.as_os_str()), Some(&mut free), Some(&mut total), None).ok()?;
    }
    Some((free, total))
}

#[cfg(not(windows))]
pub fn disk_space(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// Mở một URI (ví dụ `ms-settings:windowsupdate`) bằng shell của Windows.
pub fn open_uri(uri: &str) -> Result<(), String> {
    std::process::Command::new("explorer.exe")
        .arg(uri)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Chạy một chương trình không hiện cửa sổ và trả về stdout nếu nó kết thúc thành
/// công. Tiến trình bị dừng khi quá `timeout` hoặc khi `cancel` được bật.
pub fn run_hidden(
    program: &str,
    args: &[&str],
    timeout: std::time::Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;

    // Đọc stdout/stderr ở luồng riêng để tiến trình không bị nghẽn khi in nhiều.
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });

    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None => {
                let cancelled = cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed));
                if cancelled || started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(if cancelled { "cancelled" } else { "timeout" }.into());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    };
    let stdout = String::from_utf8_lossy(&out.join().unwrap_or_default()).to_string();
    if status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default()).trim().to_string();
        Err(if stderr.is_empty() { stdout.trim().to_string() } else { stderr })
    }
}

/// Giây tính từ mốc Unix.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_env_replaces_known_and_keeps_unknown() {
        std::env::set_var("TP_TEST_VAR", "C:\\X");
        assert_eq!(expand_env("%TP_TEST_VAR%\\a.exe"), "C:\\X\\a.exe");
        assert_eq!(expand_env("%TP_NO_SUCH_VAR%\\a"), "%TP_NO_SUCH_VAR%\\a");
        assert_eq!(expand_env("100% sure"), "100% sure");
        assert_eq!(expand_env("plain"), "plain");
    }
}
