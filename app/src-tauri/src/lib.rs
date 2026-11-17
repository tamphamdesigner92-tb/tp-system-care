use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tp_core::engine::{self, Finding, FixReport, ModuleId, ModuleResult, ScanEvent};
use tp_core::rescue::{self, Record, Rescue, UndoReport};
use tp_core::state::{self, Advice, Health, Store};

mod tools;

/// Khoảng cách tối thiểu giữa hai sự kiện "đang quét tệp X" gửi lên giao diện.
const PATH_EVENT_INTERVAL: Duration = Duration::from_millis(80);

pub(crate) struct AppState {
    /// Kết quả của lần quét gần nhất, tra theo mã mục.
    findings: Mutex<HashMap<String, Finding>>,
    pub(crate) cancel: Arc<AtomicBool>,
    /// Chỉ cho một lần quét hoặc sửa chạy tại một thời điểm.
    pub(crate) busy: Arc<AtomicBool>,
    pub(crate) rescue: Arc<Rescue>,
    /// Trạng thái quét gần nhất, dùng cho Chế độ AI và điểm sức khỏe.
    store: Arc<Store>,
}

/// Giữ cờ `busy` và tự nhả khi xong, kể cả khi tác vụ bị lỗi.
pub(crate) struct BusyGuard(Arc<AtomicBool>);

impl BusyGuard {
    pub(crate) fn acquire(flag: &Arc<AtomicBool>) -> Result<Self, String> {
        if flag.swap(true, Ordering::SeqCst) {
            Err("busy".into())
        } else {
            Ok(Self(flag.clone()))
        }
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ScanProgress<'a> {
    ModuleStarted { module: ModuleId },
    Path { path: &'a str },
    ModuleFinished { module: ModuleId, count: usize, bytes: u64 },
}

#[derive(Clone, Serialize)]
struct FixProgress {
    current: usize,
    total: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanReport {
    modules: Vec<ModuleResult>,
    cancelled: bool,
}

#[tauri::command]
fn is_elevated() -> bool {
    tp_core::is_elevated()
}

#[tauri::command]
async fn scan(app: AppHandle, state: State<'_, AppState>, modules: Vec<ModuleId>) -> Result<ScanReport, String> {
    let guard = BusyGuard::acquire(&state.busy)?;
    let cancel = state.cancel.clone();
    cancel.store(false, Ordering::SeqCst);

    let worker_cancel = cancel.clone();
    let results = tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let last_path = Mutex::new(Instant::now() - PATH_EVENT_INTERVAL);
        engine::scan(&modules, &worker_cancel, &|event| {
            let payload = match event {
                ScanEvent::ModuleStarted(module) => ScanProgress::ModuleStarted { module },
                ScanEvent::Path(path) => {
                    let mut last = last_path.lock().unwrap();
                    if last.elapsed() < PATH_EVENT_INTERVAL {
                        return;
                    }
                    *last = Instant::now();
                    ScanProgress::Path { path }
                }
                ScanEvent::ModuleFinished(result) => ScanProgress::ModuleFinished {
                    module: result.module,
                    count: result.findings.len(),
                    bytes: result.findings.iter().map(|f| f.bytes).sum(),
                },
            };
            let _ = app.emit("scan-progress", payload);
        })
    })
    .await
    .map_err(|e| e.to_string())?;

    state.store.record_scan(&results);
    let mut map = state.findings.lock().unwrap();
    map.clear();
    for finding in results.iter().flat_map(|r| &r.findings) {
        map.insert(finding.id.clone(), finding.clone());
    }
    Ok(ScanReport {
        modules: results,
        cancelled: cancel.load(Ordering::SeqCst),
    })
}

/// Dừng lần quét hoặc sửa đang chạy.
#[tauri::command]
fn cancel(state: State<'_, AppState>) {
    state.cancel.store(true, Ordering::SeqCst);
}

#[tauri::command]
async fn fix(app: AppHandle, state: State<'_, AppState>, ids: Vec<String>) -> Result<FixReport, String> {
    let guard = BusyGuard::acquire(&state.busy)?;
    // Lấy các mục ra khỏi kết quả quét: mỗi mục chỉ sửa được một lần.
    let selected: Vec<Finding> = {
        let mut map = state.findings.lock().unwrap();
        ids.iter().filter_map(|id| map.remove(id)).collect()
    };
    let cancel = state.cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    let rescue = state.rescue.clone();
    let store = state.store.clone();

    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let report = engine::fix(&selected, &rescue, &cancel, &|current, total| {
            let _ = app.emit("fix-progress", FixProgress { current, total });
        });
        store.record_fix(&selected, &report);
        report
    })
    .await
    .map_err(|e| e.to_string())
}

/// Các hạng mục Chế độ AI sẽ quét, kèm lý do.
#[tauri::command]
fn advise(state: State<'_, AppState>) -> Vec<Advice> {
    state::advise(&state.store.load(), tp_core::unix_now(), state::system_drive_low())
}

#[tauri::command]
fn health(state: State<'_, AppState>) -> Health {
    state::health(&state.store.load())
}

#[tauri::command]
fn rescue_history(state: State<'_, AppState>) -> Vec<Record> {
    state.rescue.history()
}

#[tauri::command]
async fn rescue_undo(state: State<'_, AppState>, id: u64) -> Result<UndoReport, String> {
    let guard = BusyGuard::acquire(&state.busy)?;
    let rescue = state.rescue.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        rescue.undo(id)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn create_restore_point() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| rescue::create_restore_point("TP System Care"))
        .await
        .map_err(|e| e.to_string())?
}

pub fn run() {
    tauri::Builder::default()
        .manage(AppState {
            findings: Mutex::new(HashMap::new()),
            cancel: Arc::new(AtomicBool::new(false)),
            busy: Arc::new(AtomicBool::new(false)),
            rescue: Arc::new(Rescue::default_location()),
            store: Arc::new(Store::default_location()),
        })
        .manage(tools::ToolState::default())
        .invoke_handler(tauri::generate_handler![
            is_elevated,
            scan,
            cancel,
            fix,
            advise,
            health,
            rescue_history,
            rescue_undo,
            create_restore_point,
            tools::memory_status,
            tools::release_ram,
            tools::turbo_status,
            tools::turbo_set,
            tools::startup_list,
            tools::startup_set,
            tools::apps_list,
            tools::app_uninstall,
            tools::leftovers_scan,
            tools::leftovers_remove,
            tools::large_locations,
            tools::large_scan,
            tools::large_recycle,
            tools::reveal
        ])
        .run(tauri::generate_context!())
        .expect("error while running TP System Care");
}
