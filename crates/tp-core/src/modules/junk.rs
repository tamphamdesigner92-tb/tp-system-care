//! Dọn tệp rác: tệp tạm, bộ nhớ đệm, báo cáo lỗi, bản dump, Thùng rác; và phần
//! dọn sâu: cache của ứng dụng, cache shader, tệp log cũ, cache tải bản cập nhật.

use crate::engine::{Action, Finding, Module, ModuleId, ScanContext};
use crate::modules::{chromium_profiles, electron_caches, firefox_profiles, FileSet, Source};
use crate::winutil::{self, Known};

pub struct Junk;

struct Group {
    kind: &'static str,
    needs_admin: bool,
    /// Chọn sẵn khi hiện kết quả.
    recommended: bool,
    sources: Vec<Source>,
}

fn groups() -> Vec<Group> {
    let local = winutil::known_folder(Known::LocalAppData);
    let program_data = winutil::known_folder(Known::ProgramData);
    let windows = winutil::known_folder(Known::Windows);
    let mut groups = Vec::new();

    // Tệp tạm chỉ lấy tệp cũ hơn 24 giờ: tệp mới có thể đang được chương trình dùng.
    groups.push(Group {
        kind: "junk.userTemp",
        needs_admin: false,
        recommended: true,
        sources: vec![Source::dir(std::env::temp_dir()).older_than_hours(24)],
    });
    if let Some(windows) = &windows {
        groups.push(Group {
            kind: "junk.windowsTemp",
            needs_admin: true,
            recommended: true,
            sources: vec![Source::dir(windows.join("Temp")).older_than_hours(24)],
        });
    }

    let mut cache = Vec::new();
    for profile in chromium_profiles() {
        for dir in ["Cache", "Code Cache", "GPUCache"] {
            cache.push(Source::dir(profile.join(dir)));
        }
    }
    for profile in firefox_profiles(Known::LocalAppData) {
        cache.push(Source::dir(profile.join("cache2")));
    }
    groups.push(Group {
        kind: "junk.browserCache",
        needs_admin: false,
        recommended: true,
        sources: cache,
    });

    if let Some(local) = &local {
        groups.push(Group {
            kind: "junk.thumbnails",
            needs_admin: false,
            recommended: true,
            sources: vec![Source::dir(local.join("Microsoft\\Windows\\Explorer"))
                .flat(&["thumbcache_", "iconcache_"])],
        });
    }

    let mut reports = Vec::new();
    for base in [&local, &program_data].into_iter().flatten() {
        for dir in ["ReportArchive", "ReportQueue"] {
            reports.push(Source::dir(base.join("Microsoft\\Windows\\WER").join(dir)));
        }
    }
    groups.push(Group {
        kind: "junk.errorReports",
        needs_admin: false,
        recommended: true,
        sources: reports,
    });

    let mut dumps = Vec::new();
    if let Some(local) = &local {
        dumps.push(Source::dir(local.join("CrashDumps")));
    }
    if let Some(windows) = &windows {
        dumps.push(Source::dir(windows.join("Minidump")));
        dumps.push(Source::dir(windows.join("LiveKernelReports")).only_extensions(&["dmp"]));
        dumps.push(Source::dir(windows.clone()).flat(&["memory.dmp"]));
    }
    groups.push(Group {
        kind: "junk.crashDumps",
        needs_admin: windows.is_some(),
        recommended: true,
        sources: dumps,
    });

    // ---- Dọn sâu ----
    groups.push(Group {
        kind: "junk.appCache",
        needs_admin: false,
        recommended: true,
        sources: electron_caches().into_iter().map(Source::dir).collect(),
    });
    if let Some(local) = &local {
        groups.push(Group {
            kind: "junk.systemCache",
            needs_admin: false,
            recommended: true,
            sources: vec![
                Source::dir(local.join("D3DSCache")),
                Source::dir(local.join("Microsoft\\Windows\\INetCache")),
            ],
        });
    }
    if let Some(windows) = &windows {
        // Log đang ghi có thể bị khóa; chỉ lấy tệp cũ hơn 7 ngày.
        groups.push(Group {
            kind: "junk.logs",
            needs_admin: true,
            recommended: true,
            sources: vec![Source::dir(windows.join("Logs"))
                .older_than_hours(7 * 24)
                .only_extensions(&["log", "etl", "cab"])],
        });
        // Bản cập nhật Windows đã tải về. Xóa khi Windows Update đang cài sẽ làm
        // lần cài đó phải tải lại, nên không chọn sẵn.
        groups.push(Group {
            kind: "junk.updateDownloads",
            needs_admin: true,
            recommended: false,
            sources: vec![Source::dir(windows.join("SoftwareDistribution\\Download")).older_than_hours(24)],
        });
        groups.push(Group {
            kind: "junk.deliveryCache",
            needs_admin: true,
            recommended: true,
            sources: vec![Source::dir(windows.join(
                "ServiceProfiles\\NetworkService\\AppData\\Local\\Microsoft\\Windows\\DeliveryOptimization\\Cache",
            ))],
        });
    }

    groups
}

impl Module for Junk {
    fn id(&self) -> ModuleId {
        ModuleId::Junk
    }

    fn scan(&self, ctx: &ScanContext) -> Vec<Finding> {
        let mut findings = Vec::new();
        for group in groups() {
            if ctx.cancelled() {
                return findings;
            }
            let mut set = FileSet::default();
            for source in &group.sources {
                set.collect(ctx, source);
            }
            if set.files.is_empty() {
                continue;
            }
            findings.push(Finding {
                id: String::new(),
                module: ModuleId::Junk,
                kind: group.kind,
                name: None,
                detail: set.roots.first().map(|p| p.to_string_lossy().to_string()),
                count: set.files.len() as u64,
                bytes: set.bytes,
                recommended: group.recommended,
                undoable: false,
                needs_admin: group.needs_admin,
                action: Action::DeleteFiles {
                    roots: set.roots,
                    files: set.files,
                },
            });
        }

        // Thùng rác có thể chứa tệp người dùng còn muốn lấy lại: không chọn sẵn.
        if let Some((count, bytes)) = winutil::recycle_bin_info() {
            if count > 0 {
                findings.push(Finding {
                    id: String::new(),
                    module: ModuleId::Junk,
                    kind: "junk.recycleBin",
                    name: None,
                    detail: None,
                    count,
                    bytes,
                    recommended: false,
                    undoable: false,
                    needs_admin: false,
                    action: Action::EmptyRecycleBin,
                });
            }
        }
        findings
    }
}
