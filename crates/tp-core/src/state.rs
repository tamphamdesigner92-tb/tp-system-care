//! Trạng thái quét gần nhất của từng hạng mục, và hai thứ suy ra từ nó:
//! luật chọn hạng mục của Chế độ AI ([`advise`]) và điểm sức khỏe ([`health`]).
//!
//! "Vấn đề" ở đây chỉ đếm các mục engine khuyến nghị sửa; mục tùy chọn (ví dụ
//! chương trình khởi động còn hoạt động) không làm giảm điểm.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::engine::{Finding, FixReport, ModuleId, ModuleResult};
use crate::winutil::{self, Known};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleState {
    /// Giây tính từ mốc Unix.
    pub last_scan: u64,
    /// Số mục khuyến nghị sửa còn lại.
    pub issues: u64,
    /// Dung lượng của các mục đó.
    pub bytes: u64,
}

pub type States = BTreeMap<ModuleId, ModuleState>;

/// Lưu trạng thái vào `%LOCALAPPDATA%\TP System Care\state.json`.
pub struct Store {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Store {
    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn default_location() -> Self {
        let base = winutil::known_folder(Known::LocalAppData).unwrap_or_else(std::env::temp_dir);
        Self::at(base.join("TP System Care").join("state.json"))
    }

    pub fn load(&self) -> States {
        let _guard = self.lock.lock().unwrap();
        self.read()
    }

    fn read(&self) -> States {
        fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn update(&self, change: impl FnOnce(&mut States)) {
        let _guard = self.lock.lock().unwrap();
        let mut states = self.read();
        change(&mut states);
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec_pretty(&states) {
            let _ = fs::write(&self.path, json);
        }
    }

    /// Ghi kết quả của các hạng mục vừa quét xong.
    pub fn record_scan(&self, results: &[ModuleResult]) {
        let now = winutil::unix_now();
        self.update(|states| {
            for result in results {
                let recommended = result.findings.iter().filter(|f| f.recommended);
                states.insert(
                    result.module,
                    ModuleState {
                        last_scan: now,
                        issues: recommended.clone().count() as u64,
                        bytes: recommended.map(|f| f.bytes).sum(),
                    },
                );
            }
        });
    }

    /// Trừ đi các mục khuyến nghị đã sửa trọn vẹn.
    pub fn record_fix(&self, findings: &[Finding], report: &FixReport) {
        self.update(|states| {
            for item in &report.items {
                let fixed = item.done > 0 && item.failed == 0;
                let Some(finding) = findings.iter().find(|f| f.id == item.id) else {
                    continue;
                };
                if !fixed || !finding.recommended {
                    continue;
                }
                if let Some(state) = states.get_mut(&finding.module) {
                    state.issues = state.issues.saturating_sub(1);
                    state.bytes = state.bytes.saturating_sub(finding.bytes);
                }
            }
        });
    }
}

// ------------------------------------------------------------ Chế độ AI

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Reason {
    /// Chưa quét lần nào.
    Never,
    /// Lần quét trước còn vấn đề chưa sửa.
    Pending,
    /// Đã lâu chưa quét.
    Stale,
    /// Ổ hệ thống sắp đầy.
    LowDisk,
    /// Không hạng mục nào tới hạn; quét nhanh hạng mục thay đổi thường xuyên nhất.
    Routine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Advice {
    pub module: ModuleId,
    pub reason: Reason,
}

const DAY: u64 = 24 * 3600;

/// Bao lâu thì một hạng mục cần quét lại. Rác và dấu vết sinh ra hằng ngày; thiết
/// lập hệ thống hiếm khi đổi.
fn interval(module: ModuleId) -> u64 {
    match module {
        ModuleId::Junk => DAY,
        ModuleId::Privacy | ModuleId::Vuln => 3 * DAY,
        ModuleId::Shortcut
        | ModuleId::Registry
        | ModuleId::Startup
        | ModuleId::System
        | ModuleId::Internet
        | ModuleId::Security => 7 * DAY,
        ModuleId::Disk => 14 * DAY,
    }
}

/// Chọn hạng mục cần quét theo trạng thái máy và lịch sử quét.
pub fn advise(states: &States, now: u64, low_disk: bool) -> Vec<Advice> {
    let mut advice: Vec<Advice> = ModuleId::ALL
        .iter()
        .filter_map(|&module| {
            let reason = match states.get(&module) {
                None => Reason::Never,
                Some(state) if state.issues > 0 => Reason::Pending,
                Some(_) if module == ModuleId::Junk && low_disk => Reason::LowDisk,
                Some(state) if now.saturating_sub(state.last_scan) >= interval(module) => Reason::Stale,
                Some(_) => return None,
            };
            Some(Advice { module, reason })
        })
        .collect();
    if advice.is_empty() {
        advice.push(Advice {
            module: ModuleId::Junk,
            reason: Reason::Routine,
        });
    }
    advice
}

/// Ổ hệ thống còn dưới 10% dung lượng trống.
pub fn system_drive_low() -> bool {
    winutil::known_folder(Known::Windows)
        .and_then(|windows| winutil::disk_space(&windows))
        .is_some_and(|(free, total)| total > 0 && free * 10 < total)
}

// ------------------------------------------------------------ Điểm sức khỏe

/// Trọng số của từng hạng mục, tổng bằng 100.
fn weight(module: ModuleId) -> u32 {
    match module {
        ModuleId::Security => 20,
        ModuleId::Vuln | ModuleId::Junk => 15,
        ModuleId::Registry | ModuleId::System | ModuleId::Disk => 10,
        ModuleId::Privacy | ModuleId::Shortcut | ModuleId::Startup | ModuleId::Internet => 5,
    }
}

/// Mức độ nặng của một hạng mục, từ 0 (sạch) tới 1 (mất hết trọng số).
fn severity(module: ModuleId, state: &ModuleState) -> f64 {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let ratio = match module {
        // Rác tính theo dung lượng: từ 5 GB trở lên là mất hết điểm.
        ModuleId::Junk => state.bytes as f64 / (5.0 * GB),
        // Một lỗ hổng hay thiết lập bảo mật sai đã là nghiêm trọng.
        ModuleId::Security | ModuleId::Vuln | ModuleId::Disk => state.issues as f64 / 2.0,
        ModuleId::System | ModuleId::Internet => state.issues as f64 / 3.0,
        _ => state.issues as f64 / 10.0,
    };
    ratio.clamp(0.0, 1.0)
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    /// 0–100; `None` khi chưa quét hạng mục nào.
    pub score: Option<u32>,
    /// Số hạng mục đã từng quét, trên tổng số `total`.
    pub scanned: u32,
    pub total: u32,
    /// Tổng số mục khuyến nghị sửa còn lại.
    pub issues: u64,
}

/// Điểm tính trên các hạng mục đã quét, quy về thang 100.
pub fn health(states: &States) -> Health {
    let mut weight_sum = 0u32;
    let mut lost = 0.0;
    let mut issues = 0;
    for (&module, state) in states {
        weight_sum += weight(module);
        lost += weight(module) as f64 * severity(module, state);
        issues += state.issues;
    }
    Health {
        score: (weight_sum > 0).then(|| (100.0 * (1.0 - lost / weight_sum as f64)).round() as u32),
        scanned: states.len() as u32,
        total: ModuleId::ALL.len() as u32,
        issues,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(last_scan: u64, issues: u64, bytes: u64) -> ModuleState {
        ModuleState {
            last_scan,
            issues,
            bytes,
        }
    }

    #[test]
    fn weights_sum_to_100() {
        assert_eq!(ModuleId::ALL.iter().map(|&m| weight(m)).sum::<u32>(), 100);
    }

    #[test]
    fn first_run_advises_everything() {
        let advice = advise(&States::new(), 1_000_000, false);
        assert_eq!(advice.len(), ModuleId::ALL.len());
        assert!(advice.iter().all(|a| a.reason == Reason::Never));
    }

    #[test]
    fn advice_follows_issues_age_and_disk() {
        let now = 100 * DAY;
        let mut states: States = ModuleId::ALL.iter().map(|&m| (m, state(now, 0, 0))).collect();
        assert_eq!(
            advise(&states, now, false),
            vec![Advice {
                module: ModuleId::Junk,
                reason: Reason::Routine
            }]
        );

        states.insert(ModuleId::Security, state(now, 2, 0));
        states.insert(ModuleId::Privacy, state(now - 4 * DAY, 0, 0));
        states.insert(ModuleId::Disk, state(now - 4 * DAY, 0, 0));
        let advice = advise(&states, now, true);
        assert_eq!(
            advice,
            vec![
                Advice {
                    module: ModuleId::Junk,
                    reason: Reason::LowDisk
                },
                Advice {
                    module: ModuleId::Privacy,
                    reason: Reason::Stale
                },
                Advice {
                    module: ModuleId::Security,
                    reason: Reason::Pending
                },
            ]
        );
    }

    #[test]
    fn health_score_scales_with_problems() {
        assert_eq!(health(&States::new()).score, None);

        let clean: States = ModuleId::ALL.iter().map(|&m| (m, state(1, 0, 0))).collect();
        assert_eq!(health(&clean).score, Some(100));

        let mut bad = clean.clone();
        bad.insert(ModuleId::Security, state(1, 5, 0)); // mất trọn 20
        bad.insert(ModuleId::Junk, state(1, 3, 5 * 1024 * 1024 * 1024 / 2)); // mất nửa của 15
        let h = health(&bad);
        assert_eq!(h.score, Some(73));
        assert_eq!(h.issues, 8);

        // Chỉ mới quét một hạng mục sạch: điểm quy về thang 100.
        let partial: States = [(ModuleId::Junk, state(1, 0, 0))].into_iter().collect();
        let h = health(&partial);
        assert_eq!((h.score, h.scanned, h.total), (Some(100), 1, 10));
    }

    #[test]
    fn store_records_scan_then_fix() {
        let path = std::env::temp_dir().join(format!("tp-core-state-{}.json", std::process::id()));
        let _ = fs::remove_file(&path);
        let store = Store::at(path.clone());

        let finding = |id: &str, recommended: bool, bytes: u64| Finding {
            id: id.into(),
            module: ModuleId::Junk,
            kind: "junk.userTemp",
            name: None,
            detail: None,
            count: 1,
            bytes,
            recommended,
            undoable: false,
            needs_admin: false,
            action: crate::engine::Action::EmptyRecycleBin,
        };
        let findings = vec![finding("junk:0", true, 100), finding("junk:1", true, 50), finding("junk:2", false, 999)];
        store.record_scan(&[ModuleResult {
            module: ModuleId::Junk,
            findings: findings.clone(),
        }]);
        let saved = store.load()[&ModuleId::Junk];
        assert_eq!((saved.issues, saved.bytes), (2, 150));

        let item = |id: &str, done, failed| crate::engine::FixItem {
            id: id.into(),
            done,
            failed,
            ..Default::default()
        };
        let report = FixReport {
            items: vec![item("junk:0", 1, 0), item("junk:1", 1, 1), item("junk:2", 1, 0)],
            ..Default::default()
        };
        store.record_fix(&findings, &report);
        let saved = store.load()[&ModuleId::Junk];
        assert_eq!((saved.issues, saved.bytes), (1, 50));
        fs::remove_file(path).unwrap();
    }
}
