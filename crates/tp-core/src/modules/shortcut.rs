//! Sửa lối tắt hỏng: tệp `.lnk` trên Desktop và Start Menu trỏ tới tệp không còn.

use std::path::Path;

use walkdir::WalkDir;

use crate::engine::{Action, Finding, Module, ModuleId, ScanContext};
use crate::winutil::{self, Known};

pub struct Shortcut;

/// Lối tắt chỉ bị coi là hỏng khi đích là đường dẫn tuyệt đối trên ổ cứng cố định
/// và chắc chắn không tồn tại. Ổ mạng, ổ rời và lỗi truy cập đều không tính.
pub(crate) fn is_missing(target: &Path) -> bool {
    target.is_absolute() && winutil::on_fixed_drive(target) && matches!(target.try_exists(), Ok(false))
}

impl Module for Shortcut {
    fn id(&self) -> ModuleId {
        ModuleId::Shortcut
    }

    fn scan(&self, ctx: &ScanContext) -> Vec<Finding> {
        let places = [
            (Known::Desktop, false),
            (Known::StartMenu, false),
            (Known::PublicDesktop, true),
            (Known::CommonStartMenu, true),
        ];
        let mut findings = Vec::new();
        for (place, shared) in places {
            let Some(root) = winutil::known_folder(place) else {
                continue;
            };
            for entry in WalkDir::new(&root)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
            {
                if ctx.cancelled() {
                    return findings;
                }
                let path = entry.path();
                let is_lnk = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
                if !entry.file_type().is_file() || !is_lnk {
                    continue;
                }
                ctx.report(path);
                let Some(target) = winutil::shortcut_target(path) else {
                    continue;
                };
                if !is_missing(&target) {
                    continue;
                }
                findings.push(Finding {
                    id: String::new(),
                    module: ModuleId::Shortcut,
                    kind: "shortcut.broken",
                    name: path.file_stem().map(|s| s.to_string_lossy().to_string()),
                    detail: Some(target.to_string_lossy().to_string()),
                    count: 1,
                    bytes: 0,
                    recommended: true,
                    undoable: true,
                    needs_admin: shared,
                    action: Action::QuarantineFile {
                        path: path.to_path_buf(),
                    },
                });
            }
        }
        findings
    }
}
