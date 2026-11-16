//! Các hạng mục quét.

pub mod disk;
pub mod junk;
pub mod privacy;
pub mod registry;
pub mod shortcut;
pub mod startup;
pub mod tweaks;
pub mod vuln;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use walkdir::WalkDir;

use crate::engine::ScanContext;
use crate::winutil::{self, Known};

/// Nơi lấy tệp cho một nhóm rác.
pub(crate) struct Source {
    pub root: PathBuf,
    pub recursive: bool,
    /// Bỏ qua tệp mới sửa trong khoảng này (tệp tạm có thể đang được dùng).
    pub min_age: Duration,
    /// Chỉ lấy tệp có tên bắt đầu bằng một trong các tiền tố (không phân biệt hoa thường).
    pub prefixes: &'static [&'static str],
    /// Chỉ lấy tệp có một trong các phần mở rộng (chữ thường, không có dấu chấm).
    pub extensions: &'static [&'static str],
}

impl Source {
    pub fn dir(root: PathBuf) -> Self {
        Self {
            root,
            recursive: true,
            min_age: Duration::ZERO,
            prefixes: &[],
            extensions: &[],
        }
    }

    pub fn older_than_hours(mut self, hours: u64) -> Self {
        self.min_age = Duration::from_secs(hours * 3600);
        self
    }

    pub fn only_extensions(mut self, extensions: &'static [&'static str]) -> Self {
        self.extensions = extensions;
        self
    }

    pub fn flat(mut self, prefixes: &'static [&'static str]) -> Self {
        self.recursive = false;
        self.prefixes = prefixes;
        self
    }
}

#[derive(Default)]
pub(crate) struct FileSet {
    pub roots: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
    pub bytes: u64,
}

impl FileSet {
    /// Gom tệp từ một nguồn. Không đi theo symlink/junction để không ra khỏi `root`.
    pub fn collect(&mut self, ctx: &ScanContext, source: &Source) {
        if !source.root.is_dir() {
            return;
        }
        let now = SystemTime::now();
        let depth = if source.recursive { usize::MAX } else { 1 };
        let before = self.files.len();
        for entry in WalkDir::new(&source.root)
            .min_depth(1)
            .max_depth(depth)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            if ctx.cancelled() {
                break;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            if !source.prefixes.is_empty() {
                let name = entry.file_name().to_string_lossy().to_lowercase();
                if !source.prefixes.iter().any(|p| name.starts_with(p)) {
                    continue;
                }
            }
            if !source.extensions.is_empty() {
                let matches = entry
                    .path()
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .is_some_and(|e| source.extensions.contains(&e.as_str()));
                if !matches {
                    continue;
                }
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !source.min_age.is_zero() {
                let age = meta
                    .modified()
                    .ok()
                    .and_then(|m| now.duration_since(m).ok())
                    .unwrap_or(Duration::ZERO);
                if age < source.min_age {
                    continue;
                }
            }
            ctx.report(entry.path());
            self.bytes += meta.len();
            self.files.push(entry.into_path());
        }
        if self.files.len() > before {
            self.roots.push(source.root.clone());
        }
    }

    /// Thêm các tệp có tên cho trước nằm ngay trong `dir`.
    pub fn add_named(&mut self, ctx: &ScanContext, dir: &Path, names: &[&str]) {
        let before = self.files.len();
        for name in names {
            let path = dir.join(name);
            if let Ok(meta) = std::fs::symlink_metadata(&path) {
                if meta.is_file() {
                    ctx.report(&path);
                    self.bytes += meta.len();
                    self.files.push(path);
                }
            }
        }
        if self.files.len() > before {
            self.roots.push(dir.to_path_buf());
        }
    }
}

/// Thư mục hồ sơ của các trình duyệt nhân Chromium (Default, Profile 1, ...).
pub(crate) fn chromium_profiles() -> Vec<PathBuf> {
    const USER_DATA: &[&str] = &[
        "Google\\Chrome\\User Data",
        "Microsoft\\Edge\\User Data",
        "BraveSoftware\\Brave-Browser\\User Data",
    ];
    let Some(local) = winutil::known_folder(Known::LocalAppData) else {
        return Vec::new();
    };
    let mut profiles = Vec::new();
    for relative in USER_DATA {
        let Ok(entries) = std::fs::read_dir(local.join(relative)) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().to_string();
            if (name == "Default" || name.starts_with("Profile ")) && entry.path().is_dir() {
                profiles.push(entry.path());
            }
        }
    }
    profiles
}

/// Thư mục cache của các ứng dụng nền Chromium/Electron (Discord, Slack, VS Code...)
/// nằm ngay dưới AppData: nhận ra bằng bộ thư mục `GPUCache` + `Code Cache`.
pub(crate) fn electron_caches() -> Vec<PathBuf> {
    let mut caches = Vec::new();
    for base in [Known::RoamingAppData, Known::LocalAppData] {
        let Some(base) = winutil::known_folder(base) else {
            continue;
        };
        let Ok(apps) = std::fs::read_dir(&base) else {
            continue;
        };
        for app in apps.filter_map(Result::ok).map(|e| e.path()) {
            if app.join("GPUCache").is_dir() && app.join("Code Cache").is_dir() {
                for dir in ["Cache", "Code Cache", "GPUCache"] {
                    caches.push(app.join(dir));
                }
            }
        }
    }
    caches
}

/// Thư mục hồ sơ Firefox dưới thư mục gốc cho trước (Local hoặc Roaming AppData).
pub(crate) fn firefox_profiles(base: Known) -> Vec<PathBuf> {
    let Some(base) = winutil::known_folder(base) else {
        return Vec::new();
    };
    std::fs::read_dir(base.join("Mozilla\\Firefox\\Profiles"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}
