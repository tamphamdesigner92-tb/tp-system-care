use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};
use tp_core::tools::{
    component_store, duplicates, install_monitor, largefiles, software_review, tuning,
};

use crate::{AppState, BusyGuard};

pub struct MaintenanceState {
    active: Arc<Mutex<Option<String>>>,
    duplicates: Mutex<Option<Arc<duplicates::Scan>>>,
    tweaks: Mutex<Option<Arc<Vec<tuning::Tweak>>>>,
    caches: Mutex<Option<Arc<tuning::CacheScan>>>,
    app_caches: Mutex<Option<Arc<tuning::CacheScan>>>,
    analysis: Mutex<Option<(Arc<component_store::Analysis>, Instant)>>,
    preparation: Mutex<Option<Preparation>>,
    component_status: Arc<Mutex<Value>>,
    monitor: Arc<install_monitor::Monitor>,
}

impl Default for MaintenanceState {
    fn default() -> Self {
        Self {
            active: Arc::new(Mutex::new(None)),
            duplicates: Mutex::new(None),
            tweaks: Mutex::new(None),
            caches: Mutex::new(None),
            app_caches: Mutex::new(None),
            analysis: Mutex::new(None),
            preparation: Mutex::new(None),
            component_status: Arc::new(Mutex::new(
                json!({"state":"idle", "report":null, "error":null}),
            )),
            monitor: Arc::new(install_monitor::Monitor::default_location()),
        }
    }
}

struct Preparation {
    ready: bool,
    restore_error: Option<String>,
}

struct JobGuard {
    _busy: BusyGuard,
    active: Arc<Mutex<Option<String>>>,
}

impl JobGuard {
    fn acquire(
        state: &AppState,
        tools: &MaintenanceState,
        request_id: &str,
    ) -> Result<Self, String> {
        if request_id.is_empty()
            || request_id.len() > 128
            || !request_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err("invalid_request".into());
        }
        let busy = BusyGuard::acquire(&state.busy)?;
        state.cancel.store(false, Ordering::SeqCst);
        *tools.active.lock().unwrap() = Some(request_id.into());
        Ok(Self {
            _busy: busy,
            active: tools.active.clone(),
        })
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        *self.active.lock().unwrap() = None;
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
}

fn progress(app: &AppHandle, request_id: &str, path: &Path, count: usize, last: &Mutex<Instant>) {
    let mut last = last.lock().unwrap();
    if last.elapsed() >= Duration::from_millis(100) {
        *last = Instant::now();
        let _ = app.emit(
            "maintenance-progress",
            json!({
                "requestId": request_id, "path": path.to_string_lossy(), "count": count,
            }),
        );
    }
}

fn cancel_matching(state: &AppState, tools: &MaintenanceState, request_id: &str) {
    let active = tools.active.lock().unwrap();
    if active.as_deref() == Some(request_id) {
        state.cancel.store(true, Ordering::SeqCst);
    }
}

#[tauri::command]
pub fn maintenance_cancel(
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
    request_id: String,
) {
    cancel_matching(&state, &tools, &request_id);
}

#[tauri::command]
pub async fn duplicate_scan(
    app: AppHandle,
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
    root: String,
    min_mb: u64,
    request_id: String,
) -> Result<Value, String> {
    let _guard = JobGuard::acquire(&state, &tools, &request_id)?;
    *tools.duplicates.lock().unwrap() = None;
    if ![1, 10, 50].contains(&min_mb) {
        return Err("invalid_request".into());
    }
    let cancel = state.cancel.clone();
    let scan = blocking(move || {
        if !largefiles::locations()
            .iter()
            .any(|location| location.path == root)
        {
            return Err("not_found".into());
        }
        let last = Mutex::new(Instant::now() - Duration::from_millis(100));
        duplicates::scan(
            Path::new(&root),
            min_mb * 1024 * 1024,
            &cancel,
            &|path, count| {
                progress(&app, &request_id, path, count, &last);
            },
        )
    })
    .await?;
    let response = serde_json::to_value(&scan).map_err(|e| e.to_string())?;
    *tools.duplicates.lock().unwrap() = Some(Arc::new(scan));
    Ok(response)
}

#[tauri::command]
pub async fn duplicate_recycle(
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
    ids: Vec<String>,
    request_id: String,
) -> Result<duplicates::Removal, String> {
    let _guard = JobGuard::acquire(&state, &tools, &request_id)?;
    let scan = tools
        .duplicates
        .lock()
        .unwrap()
        .clone()
        .ok_or("not_found")?;
    let cancel = state.cancel.clone();
    let rescue = state.rescue.clone();
    blocking(move || duplicates::remove(&scan.groups, &ids, &rescue, &cancel)).await
}

#[tauri::command]
pub fn duplicate_reveal(tools: State<'_, MaintenanceState>, id: String) -> Result<(), String> {
    let path = {
        let cache = tools.duplicates.lock().unwrap();
        let file = cache
            .as_ref()
            .and_then(|scan| {
                scan.groups
                    .iter()
                    .flat_map(|group| &group.files)
                    .find(|file| file.id == id)
            })
            .ok_or("not_found")?;
        file.path.clone()
    };
    tp_core::reveal(Path::new(&path))
}

#[tauri::command]
pub async fn tuning_list(
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
) -> Result<Value, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    *tools.tweaks.lock().unwrap() = None;
    let items = blocking(tuning::list).await?;
    let response = serde_json::to_value(&items).map_err(|e| e.to_string())?;
    *tools.tweaks.lock().unwrap() = Some(Arc::new(items));
    Ok(response)
}

#[tauri::command]
pub async fn tuning_apply(
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
    ids: Vec<String>,
) -> Result<tuning::ApplyReport, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    let items = tools.tweaks.lock().unwrap().clone().ok_or("not_found")?;
    let rescue = state.rescue.clone();
    let report = blocking(move || tuning::apply(&items, &ids, &rescue)).await;
    *tools.tweaks.lock().unwrap() = None;
    report
}

#[tauri::command]
pub async fn browser_cache_scan(
    app: AppHandle,
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
    request_id: String,
) -> Result<Value, String> {
    let _guard = JobGuard::acquire(&state, &tools, &request_id)?;
    *tools.caches.lock().unwrap() = None;
    let cancel = state.cancel.clone();
    let scan = blocking(move || {
        let last = Mutex::new(Instant::now() - Duration::from_millis(100));
        tuning::cache_scan(&cancel, &|path, count| {
            progress(&app, &request_id, path, count, &last)
        })
    })
    .await?;
    let response = serde_json::to_value(&scan).map_err(|e| e.to_string())?;
    *tools.caches.lock().unwrap() = Some(Arc::new(scan));
    Ok(response)
}

#[tauri::command]
pub async fn browser_cache_clear(
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
    ids: Vec<String>,
    request_id: String,
) -> Result<tuning::CacheReport, String> {
    let _guard = JobGuard::acquire(&state, &tools, &request_id)?;
    let scan = tools.caches.lock().unwrap().clone().ok_or("not_found")?;
    let cancel = state.cancel.clone();
    let rescue = state.rescue.clone();
    blocking(move || tuning::cache_clear(&scan.caches, &ids, &rescue, &cancel)).await
}

#[tauri::command]
pub fn browser_cache_reveal(tools: State<'_, MaintenanceState>, id: String) -> Result<(), String> {
    let path = {
        let cache = tools.caches.lock().unwrap();
        let item = cache
            .as_ref()
            .and_then(|scan| scan.caches.iter().find(|item| item.id == id))
            .ok_or("not_found")?;
        item.path.clone()
    };
    tp_core::reveal(Path::new(&path))
}

#[tauri::command]
pub async fn browser_flush_dns(state: State<'_, AppState>) -> Result<(), String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    blocking(tuning::flush_dns).await
}

#[tauri::command]
pub fn browser_network() -> Result<(), String> {
    tp_core::open_uri("ms-settings:network-status")
}

#[tauri::command]
pub async fn app_cache_scan(
    app: AppHandle,
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
    request_id: String,
) -> Result<Value, String> {
    let _guard = JobGuard::acquire(&state, &tools, &request_id)?;
    *tools.app_caches.lock().unwrap() = None;
    let cancel = state.cancel.clone();
    let scan = blocking(move || {
        let last = Mutex::new(Instant::now() - Duration::from_millis(100));
        tuning::app_cache_scan(&cancel, &|path, count| {
            progress(&app, &request_id, path, count, &last)
        })
    })
    .await?;
    let response = serde_json::to_value(&scan).map_err(|e| e.to_string())?;
    *tools.app_caches.lock().unwrap() = Some(Arc::new(scan));
    Ok(response)
}

#[tauri::command]
pub async fn app_cache_clear(
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
    ids: Vec<String>,
    request_id: String,
) -> Result<tuning::CacheReport, String> {
    let _guard = JobGuard::acquire(&state, &tools, &request_id)?;
    let scan = tools
        .app_caches
        .lock()
        .unwrap()
        .clone()
        .ok_or("not_found")?;
    let cancel = state.cancel.clone();
    let rescue = state.rescue.clone();
    blocking(move || tuning::cache_clear(&scan.caches, &ids, &rescue, &cancel)).await
}

#[tauri::command]
pub fn app_cache_reveal(tools: State<'_, MaintenanceState>, id: String) -> Result<(), String> {
    let path = {
        let cache = tools.app_caches.lock().unwrap();
        let item = cache
            .as_ref()
            .and_then(|scan| scan.caches.iter().find(|item| item.id == id))
            .ok_or("not_found")?;
        item.path.clone()
    };
    tp_core::reveal(Path::new(&path))
}

#[tauri::command]
pub async fn extensions_list(
    state: State<'_, AppState>,
) -> Result<software_review::ExtensionScan, String> {
    let guard = BusyGuard::acquire(&state.busy)?;
    blocking(move || {
        let _guard = guard;
        software_review::extensions()
    })
    .await
}

#[tauri::command]
pub fn extension_manager(browser: String) -> Result<String, String> {
    software_review::extension_manager(&browser).map(str::to_owned)
}

#[tauri::command]
pub async fn windows_updates_list(
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
    request_id: String,
) -> Result<Vec<software_review::InstalledUpdate>, String> {
    let guard = JobGuard::acquire(&state, &tools, &request_id)?;
    let cancel = state.cancel.clone();
    blocking(move || {
        let _guard = guard;
        software_review::updates(&cancel)
    })
    .await
}

#[tauri::command]
pub fn windows_updates_open(state: State<'_, AppState>) -> Result<(), String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    software_review::update_settings()
}

fn require_analysis(tools: &MaintenanceState) -> Result<(), String> {
    if tools
        .analysis
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|(analysis, captured)| {
            captured.elapsed() < Duration::from_secs(600) && analysis.cleanup_recommended
        })
    {
        Ok(())
    } else {
        Err("cleanup_not_recommended".into())
    }
}

#[tauri::command]
pub async fn component_analyze(
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
    request_id: String,
) -> Result<Value, String> {
    let _guard = JobGuard::acquire(&state, &tools, &request_id)?;
    *tools.analysis.lock().unwrap() = None;
    *tools.preparation.lock().unwrap() = None;
    let cancel = state.cancel.clone();
    let analysis = blocking(move || component_store::analyze(&cancel)).await?;
    let response = serde_json::to_value(&analysis).map_err(|e| e.to_string())?;
    *tools.analysis.lock().unwrap() = Some((Arc::new(analysis), Instant::now()));
    Ok(response)
}

fn continue_preparation(preparation: &mut Option<Preparation>) -> Result<Value, String> {
    let session = preparation
        .as_mut()
        .filter(|session| !session.ready && session.restore_error.is_some())
        .ok_or("restore_required")?;
    session.ready = true;
    Ok(json!({"ready":true, "restoreError":session.restore_error}))
}

fn consume_preparation(
    preparation: &mut Option<Preparation>,
    confirmed: bool,
) -> Result<(), String> {
    if !confirmed || !preparation.as_ref().is_some_and(|session| session.ready) {
        return Err("restore_required".into());
    }
    *preparation = None;
    Ok(())
}

#[tauri::command]
pub async fn component_prepare(
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
    continue_without_restore: bool,
) -> Result<Value, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    require_analysis(&tools)?;
    if continue_without_restore {
        return continue_preparation(&mut tools.preparation.lock().unwrap());
    }
    *tools.preparation.lock().unwrap() = None;
    let error = tauri::async_runtime::spawn_blocking(|| {
        tp_core::rescue::create_restore_point("TP System Care - Component Store")
    })
    .await
    .map_err(|e| e.to_string())?
    .err();
    let ready = error.is_none();
    *tools.preparation.lock().unwrap() = Some(Preparation {
        ready,
        restore_error: error.clone(),
    });
    Ok(json!({"ready": ready, "restoreError": error}))
}

#[tauri::command]
pub fn component_status(tools: State<'_, MaintenanceState>) -> Value {
    tools.component_status.lock().unwrap().clone()
}

#[tauri::command]
pub fn component_start(
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
    confirmed: bool,
) -> Result<Value, String> {
    let guard = BusyGuard::acquire(&state.busy)?;
    require_analysis(&tools)?;
    consume_preparation(&mut tools.preparation.lock().unwrap(), confirmed)?;
    *tools.analysis.lock().unwrap() = None;
    let status = tools.component_status.clone();
    let running = json!({"state":"running", "report":null, "error":null});
    *status.lock().unwrap() = running.clone();
    let rescue = state.rescue.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let record_id = rescue.new_record_id();
        if let Err(error) = rescue.add_tool_record(record_id, "componentStore", 0, 0, 0, Vec::new())
        {
            *status.lock().unwrap() = json!({"state":"failed", "report":null, "error":error});
            return;
        }
        let result = component_store::cleanup();
        let response = match result {
            Ok(report) => {
                let history_error = rescue
                    .add_tool_record(record_id, "componentStore", 1, 0, 0, Vec::new())
                    .err();
                json!({"state":"completed", "report": report, "error": history_error})
            }
            Err(error) => {
                let history_error = rescue
                    .add_tool_record(record_id, "componentStore", 0, 1, 0, Vec::new())
                    .err();
                let error = history_error
                    .map_or_else(|| error.clone(), |history| format!("{error}; {history}"));
                json!({"state":"failed", "report":null, "error":error})
            }
        };
        *status.lock().unwrap() = response;
    });
    Ok(running)
}

#[tauri::command]
pub async fn monitor_list(
    tools: State<'_, MaintenanceState>,
) -> Result<Vec<install_monitor::Summary>, String> {
    let monitor = tools.monitor.clone();
    blocking(move || monitor.list()).await
}

#[tauri::command]
pub async fn monitor_begin(
    app: AppHandle,
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
    name: String,
    root: String,
    vendor: String,
    request_id: String,
) -> Result<install_monitor::Summary, String> {
    let _guard = JobGuard::acquire(&state, &tools, &request_id)?;
    let monitor = tools.monitor.clone();
    let cancel = state.cancel.clone();
    blocking(move || {
        let last = Mutex::new(Instant::now() - Duration::from_millis(100));
        monitor.begin(&name, Path::new(&root), &vendor, &cancel, &|path, count| {
            progress(&app, &request_id, path, count, &last);
        })
    })
    .await
}

#[tauri::command]
pub async fn monitor_finish(
    app: AppHandle,
    state: State<'_, AppState>,
    tools: State<'_, MaintenanceState>,
    id: u64,
    request_id: String,
) -> Result<install_monitor::Report, String> {
    let _guard = JobGuard::acquire(&state, &tools, &request_id)?;
    let monitor = tools.monitor.clone();
    let cancel = state.cancel.clone();
    blocking(move || {
        let last = Mutex::new(Instant::now() - Duration::from_millis(100));
        monitor.finish(id, &cancel, &|path, count| {
            progress(&app, &request_id, path, count, &last)
        })
    })
    .await
}

#[tauri::command]
pub async fn monitor_detail(
    tools: State<'_, MaintenanceState>,
    id: u64,
) -> Result<install_monitor::Report, String> {
    let monitor = tools.monitor.clone();
    blocking(move || monitor.detail(id)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_state() -> AppState {
        AppState {
            findings: Mutex::new(Default::default()),
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rescue: Arc::new(tp_core::rescue::Rescue::at(
                std::env::temp_dir().join("TPMaintenanceUnused"),
            )),
            store: Arc::new(tp_core::state::Store::default_location()),
        }
    }

    #[test]
    fn jobs_hold_busy_until_cache_update_and_clear_ownership_before_releasing() {
        let state = app_state();
        let tools = MaintenanceState::default();
        assert!(JobGuard::acquire(&state, &tools, "").is_err());
        assert!(JobGuard::acquire(&state, &tools, "invalid request").is_err());
        let guard = JobGuard::acquire(&state, &tools, "fixture-1").unwrap();
        assert!(state.busy.load(Ordering::SeqCst));
        assert_eq!(tools.active.lock().unwrap().as_deref(), Some("fixture-1"));
        assert!(JobGuard::acquire(&state, &tools, "fixture-2").is_err());
        drop(guard);
        assert!(tools.active.lock().unwrap().is_none());
        assert!(!state.busy.load(Ordering::SeqCst));
        assert!(JobGuard::acquire(&state, &tools, "fixture-2").is_ok());
    }

    #[test]
    fn cancellation_cannot_target_a_different_job_or_background_servicing() {
        let state = app_state();
        let tools = MaintenanceState::default();
        let guard = JobGuard::acquire(&state, &tools, "current-job").unwrap();
        cancel_matching(&state, &tools, "previous-job");
        assert!(!state.cancel.load(Ordering::SeqCst));
        cancel_matching(&state, &tools, "current-job");
        assert!(state.cancel.load(Ordering::SeqCst));
        drop(guard);
        state.cancel.store(false, Ordering::SeqCst);
        let _servicing = BusyGuard::acquire(&state.busy).unwrap();
        cancel_matching(&state, &tools, "current-job");
        assert!(!state.cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn cleanup_requires_failed_restore_before_bypass_and_cannot_replay() {
        let mut preparation = None;
        assert!(continue_preparation(&mut preparation).is_err());
        assert!(consume_preparation(&mut preparation, true).is_err());
        preparation = Some(Preparation {
            ready: false,
            restore_error: None,
        });
        assert!(continue_preparation(&mut preparation).is_err());
        preparation = Some(Preparation {
            ready: false,
            restore_error: Some("needs_admin".into()),
        });
        assert!(consume_preparation(&mut preparation, true).is_err());
        let acknowledged = continue_preparation(&mut preparation).unwrap();
        assert_eq!(acknowledged["ready"], true);
        assert_eq!(acknowledged["restoreError"], "needs_admin");
        assert!(continue_preparation(&mut preparation).is_err());
        assert!(consume_preparation(&mut preparation, false).is_err());
        assert!(consume_preparation(&mut preparation, true).is_ok());
        assert!(consume_preparation(&mut preparation, true).is_err());
        preparation = Some(Preparation {
            ready: true,
            restore_error: None,
        });
        assert!(consume_preparation(&mut preparation, true).is_ok());
    }

    #[test]
    fn cleanup_cannot_prepare_without_successful_recommended_analysis() {
        let tools = MaintenanceState::default();
        assert_eq!(
            require_analysis(&tools).unwrap_err(),
            "cleanup_not_recommended"
        );
        assert_eq!(tools.component_status.lock().unwrap()["state"], "idle");
        assert!(tools.preparation.lock().unwrap().is_none());
    }
}
