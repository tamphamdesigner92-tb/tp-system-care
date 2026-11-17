//! Tìm tệp lớn để người dùng tự chọn xóa (vào Thùng rác).
//!
//! Bỏ qua thư mục hệ thống và thư mục chương trình: tệp lớn ở đó thuộc về Windows
//! hoặc ứng dụng, xóa đi sẽ làm hỏng chúng.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use walkdir::WalkDir;

use crate::winutil::{self, Known};

/// Số tệp tối đa trả về, lớn nhất trước.
const LIMIT: usize = 300;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LargeFile {
    pub path: String,
    pub size: u64,
    /// Giây tính từ mốc Unix.
    pub modified: u64,
    /// `video`, `archive`, `image` (ảnh đĩa), `installer` hoặc `other`.
    pub category: &'static str,
    /// Nằm trong AppData: thường là dữ liệu hoặc cache của một ứng dụng, xóa có
    /// thể làm ứng dụng đó hỏng hoặc mất dữ liệu.
    pub app_data: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub path: String,
    /// `profile` cho thư mục người dùng, `drive` cho gốc ổ đĩa.
    pub kind: &'static str,
    pub free: u64,
    pub total: u64,
}

/// Những nơi có thể quét: thư mục người dùng, rồi từng ổ cứng cố định.
pub fn locations() -> Vec<Location> {
    let mut list = Vec::new();
    let mut add = |path: PathBuf, kind| {
        let (free, total) = winutil::disk_space(&path).unwrap_or((0, 0));
        list.push(Location {
            path: path.to_string_lossy().to_string(),
            kind,
            free,
            total,
        });
    };
    if let Some(profile) = winutil::known_folder(Known::Profile) {
        add(profile, "profile");
    }
    for drive in winutil::fixed_drives() {
        add(drive, "drive");
    }
    list
}

fn category(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "m4v" | "webm" | "flv" | "ts" => "video",
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" => "archive",
        "iso" | "img" | "vhd" | "vhdx" | "vmdk" | "wim" | "esd" => "image",
        "exe" | "msi" | "msix" | "appx" | "msixbundle" => "installer",
        _ => "other",
    }
}

/// Thư mục bị bỏ qua khi quét.
fn excluded_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [Known::Windows, Known::ProgramFiles, Known::ProgramFilesX86, Known::ProgramData]
        .into_iter()
        .filter_map(winutil::known_folder)
        .collect();
    if let Some(local) = winutil::known_folder(Known::LocalAppData) {
        // Dữ liệu của chính ứng dụng (bản sao lưu Rescue Center).
        dirs.push(local.join("TP System Care"));
    }
    dirs
}

/// Tên thư mục và tệp hệ thống ở gốc ổ đĩa.
const SYSTEM_NAMES: &[&str] = &[
    "$recycle.bin",
    "system volume information",
    "$winreagent",
    "$windows.~bt",
    "$windows.~ws",
    "recovery",
    "config.msi",
    "pagefile.sys",
    "hiberfil.sys",
    "swapfile.sys",
    "dumpstack.log",
];

/// Tìm tệp từ `min_size` byte trở lên dưới `root`.
pub fn scan(root: &Path, min_size: u64, cancel: &AtomicBool, progress: &dyn Fn(&Path, usize)) -> Vec<LargeFile> {
    let excluded = excluded_dirs();
    let app_data: Vec<PathBuf> = [Known::LocalAppData, Known::RoamingAppData]
        .into_iter()
        .filter_map(winutil::known_folder)
        .collect();
    let mut files = Vec::new();
    let mut seen = 0usize;
    let walker = WalkDir::new(root).follow_links(false).into_iter().filter_entry(|entry| {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        !(entry.depth() == 1 && SYSTEM_NAMES.contains(&name.as_str()))
            && !excluded.iter().any(|dir| entry.path() == dir)
    });
    for entry in walker.filter_map(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        seen += 1;
        if seen % 500 == 0 {
            progress(entry.path(), seen);
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() < min_size {
            continue;
        }
        let modified = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        files.push(LargeFile {
            path: entry.path().to_string_lossy().to_string(),
            size: meta.len(),
            modified,
            category: category(entry.path()),
            app_data: app_data.iter().any(|dir| entry.path().starts_with(dir)),
        });
    }
    files.sort_by(|a, b| b.size.cmp(&a.size));
    files.truncate(LIMIT);
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_only_big_files_largest_first() {
        let dir = std::env::temp_dir().join(format!("tp-core-large-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("small.txt"), vec![0u8; 10]).unwrap();
        std::fs::write(dir.join("big.mp4"), vec![0u8; 3000]).unwrap();
        std::fs::write(dir.join("sub").join("bigger.zip"), vec![0u8; 5000]).unwrap();

        let files = scan(&dir, 1000, &AtomicBool::new(false), &|_, _| {});
        let names: Vec<_> = files.iter().map(|f| (Path::new(&f.path).file_name().unwrap().to_string_lossy().to_string(), f.category)).collect();
        assert_eq!(names, vec![("bigger.zip".to_string(), "archive"), ("big.mp4".to_string(), "video")]);

        assert!(scan(&dir, 1000, &AtomicBool::new(true), &|_, _| {}).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
