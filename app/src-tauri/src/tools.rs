//! Lệnh cho các công cụ ngoài luồng quét: Tăng tốc, Gỡ ứng dụng sạch, Tìm tệp lớn.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tp_core::modules::startup::{self, StartupItem};
use tp_core::tools::largefiles::{self, LargeFile, Location};
use tp_core::tools::speed::{self, Memory, RamReport, Turbo};
use tp_core::tools::uninstall::{self, AppInfo, AppKind, BatchUninstallReport, Leftover, RemoveReport, UninstallMode, UninstallResult};

use crate::{AppState, BusyGuard};

/// Kết quả gần nhất của từng công cụ. Giao diện chỉ gửi lên mã, nên backend chỉ
/// thao tác trên những mục chính nó vừa liệt kê.
#[derive(Default)]
pub struct ToolState {
    apps: Mutex<HashMap<String, AppInfo>>,
    leftovers: Mutex<HashMap<(String, UninstallMode), Vec<Leftover>>>,
    preparation: Mutex<Option<PreparationSession>>,
    large_files: Mutex<Vec<LargeFile>>,
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|e| e.to_string())
}

// ------------------------------------------------------------ Tăng tốc

#[tauri::command]
pub fn memory_status() -> Memory {
    speed::memory()
}

#[tauri::command]
pub async fn release_ram(state: State<'_, AppState>) -> Result<RamReport, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    blocking(speed::release_ram).await
}

#[tauri::command]
pub async fn turbo_status() -> Result<Turbo, String> {
    blocking(speed::turbo_status).await
}

/// Bật hoặc tắt Turbo Boost; trả về kết quả giải phóng RAM khi bật.
#[tauri::command]
pub async fn turbo_set(state: State<'_, AppState>, on: bool) -> Result<Option<RamReport>, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    blocking(move || if on { speed::turbo_on().map(Some) } else { speed::turbo_off().map(|_| None) }).await?
}

#[tauri::command]
pub async fn startup_list() -> Result<Vec<StartupItem>, String> {
    blocking(startup::list).await
}

#[tauri::command]
pub async fn startup_set(state: State<'_, AppState>, id: String, enabled: bool) -> Result<StartupItem, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    blocking(move || {
        let item = startup::list().into_iter().find(|i| i.id == id).ok_or("not_found")?;
        startup::set_enabled(&item.entry, enabled)?;
        startup::list().into_iter().find(|i| i.id == id).ok_or_else(|| "not_found".to_string())
    })
    .await?
}

// ------------------------------------------------------------ Gỡ ứng dụng

struct PreparationSession {
    ids: Vec<String>,
    mode: UninstallMode,
    ready: bool,
    started: bool,
    restore_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UninstallPreparation {
    ready: bool,
    restore_error: Option<String>,
}

#[derive(Clone, Serialize)]
struct UninstallProgress {
    current: usize,
    total: usize,
    name: String,
}

#[tauri::command]
pub async fn apps_list(state: State<'_, AppState>, tools: State<'_, ToolState>) -> Result<Vec<AppInfo>, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    let apps = blocking(uninstall::list).await??;
    *tools.apps.lock().unwrap() = apps.iter().map(|a| (a.id.clone(), a.clone())).collect();
    tools.leftovers.lock().unwrap().clear();
    *tools.preparation.lock().unwrap() = None;
    Ok(apps)
}

fn cached_app(tools: &ToolState, id: &str) -> Result<AppInfo, String> {
    tools.apps.lock().unwrap().get(id).cloned().ok_or_else(|| "not_found".into())
}

fn cached_apps(tools: &ToolState, ids: &[String], mode: UninstallMode) -> Result<Vec<AppInfo>, String> {
    if ids.is_empty() || ids.iter().collect::<std::collections::HashSet<_>>().len() != ids.len() {
        return Err("not_found".into());
    }
    if mode == UninstallMode::Forced && ids.len() != 1 {
        return Err("protected_app".into());
    }
    ids.iter().map(|id| {
        let app = cached_app(tools, id)?;
        if !app.removable || mode == UninstallMode::Forced && app.kind != AppKind::Desktop
            || mode == UninstallMode::Standard && app.synthetic {
            return Err("protected_app".into());
        }
        Ok(app)
    }).collect()
}

fn require_prepared(tools: &ToolState, ids: &[String], mode: UninstallMode, exact: bool) -> Result<(), String> {
    let preparation = tools.preparation.lock().unwrap();
    if preparation.as_ref().is_some_and(|session| session.ready && session.mode == mode
        && if exact { session.ids == ids } else { ids.iter().all(|id| session.ids.contains(id)) }) {
        Ok(())
    } else {
        Err("restore_required".into())
    }
}

fn start_prepared(tools: &ToolState, ids: &[String]) -> Result<(), String> {
    require_prepared(tools, ids, UninstallMode::Standard, true)?;
    let mut preparation = tools.preparation.lock().unwrap();
    let session = preparation.as_mut().unwrap();
    if session.started { return Err("restore_required".into()); }
    session.started = true;
    Ok(())
}

#[tauri::command]
pub async fn app_force_target(
    state: State<'_, AppState>, tools: State<'_, ToolState>, name: String, path: String,
) -> Result<AppInfo, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    let app = blocking(move || uninstall::force_target(&name, &path, &uninstall::list()?)).await??;
    tools.apps.lock().unwrap().insert(app.id.clone(), app.clone());
    Ok(app)
}

#[tauri::command]
pub async fn uninstall_prepare(
    state: State<'_, AppState>, tools: State<'_, ToolState>, ids: Vec<String>, mode: UninstallMode,
    continue_without_restore: bool,
) -> Result<UninstallPreparation, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    cached_apps(&tools, &ids, mode)?;
    if continue_without_restore {
        let mut preparation = tools.preparation.lock().unwrap();
        let session = preparation.as_mut().filter(|session| session.ids == ids && session.mode == mode
            && !session.ready && session.restore_error.is_some()).ok_or("restore_required")?;
        session.ready = true;
        return Ok(UninstallPreparation { ready: true, restore_error: session.restore_error.clone() });
    }
    *tools.preparation.lock().unwrap() = None;
    tools.leftovers.lock().unwrap().retain(|(id, _), _| !ids.contains(id));
    let restore_error = blocking(|| tp_core::rescue::create_restore_point("TP System Care - Uninstall")).await?.err();
    let ready = restore_error.is_none();
    *tools.preparation.lock().unwrap() = Some(PreparationSession { ids, mode, ready, started: false, restore_error: restore_error.clone() });
    Ok(UninstallPreparation { ready, restore_error })
}

#[tauri::command]
pub async fn app_uninstall(
    state: State<'_, AppState>, tools: State<'_, ToolState>, id: String,
) -> Result<UninstallResult, String> {
    let guard = BusyGuard::acquire(&state.busy)?;
    require_prepared(&tools, std::slice::from_ref(&id), UninstallMode::Standard, true)?;
    let app = cached_apps(&tools, std::slice::from_ref(&id), UninstallMode::Standard)?.remove(0);
    start_prepared(&tools, &[id])?;
    let cancel = state.cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    blocking(move || { let _guard = guard; uninstall::run_uninstaller(&app, &cancel) }).await?
}

#[tauri::command]
pub async fn apps_uninstall(
    app: AppHandle, state: State<'_, AppState>, tools: State<'_, ToolState>, ids: Vec<String>,
) -> Result<BatchUninstallReport, String> {
    let guard = BusyGuard::acquire(&state.busy)?;
    require_prepared(&tools, &ids, UninstallMode::Standard, true)?;
    let apps = cached_apps(&tools, &ids, UninstallMode::Standard)?;
    start_prepared(&tools, &ids)?;
    let cancel = state.cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    let rescue = state.rescue.clone();
    blocking(move || {
        let _guard = guard;
        let report = uninstall::run_batch(&apps, &cancel, &|current, total, name| {
            let _ = app.emit("uninstall-progress", UninstallProgress { current, total, name: name.into() });
        });
        let done = report.items.iter().filter(|item| item.result.as_ref().is_some_and(|result| !result.still_installed)).count() as u64;
        if done > 0 {
            let names = report.items.iter().filter(|item| item.result.as_ref().is_some_and(|result| !result.still_installed)).map(|item| item.name.as_str()).collect::<Vec<_>>().join(", ");
            let _ = rescue.add_tool_record(rescue.new_record_id(), &format!("uninstall:{names}"), done, report.items.len() as u64 - done, 0, Vec::new());
        }
        report
    }).await
}

#[tauri::command]
pub async fn leftovers_scan(
    state: State<'_, AppState>, tools: State<'_, ToolState>, id: String, mode: UninstallMode,
) -> Result<Vec<Leftover>, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    require_prepared(&tools, std::slice::from_ref(&id), mode, false)?;
    let app = cached_apps(&tools, std::slice::from_ref(&id), mode)?.remove(0);
    tools.leftovers.lock().unwrap().remove(&(id.clone(), mode));
    let leftovers = blocking(move || uninstall::scan_with_mode(&app, &uninstall::list()?, mode)).await??;
    tools.leftovers.lock().unwrap().insert((id, mode), leftovers.clone());
    Ok(leftovers)
}

#[tauri::command]
pub async fn leftovers_remove(
    state: State<'_, AppState>, tools: State<'_, ToolState>, id: String, ids: Vec<String>, mode: UninstallMode,
) -> Result<RemoveReport, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    require_prepared(&tools, std::slice::from_ref(&id), mode, false)?;
    let app = cached_apps(&tools, std::slice::from_ref(&id), mode)?.remove(0);
    let selected: Vec<Leftover> = {
        let cache = tools.leftovers.lock().unwrap();
        let leftovers = cache.get(&(id.clone(), mode)).ok_or("not_found")?;
        if ids.is_empty() || ids.iter().any(|id| !leftovers.iter().any(|leftover| &leftover.id == id)) {
            return Err("not_found".into());
        }
        leftovers.iter().filter(|leftover| ids.contains(&leftover.id)).cloned().collect()
    };
    let rescue = state.rescue.clone();
    let report = blocking(move || {
        let current = uninstall::scan_with_mode(&app, &uninstall::list()?, mode)?;
        if selected.iter().any(|selected| !current.iter().any(|leftover| leftover.kind == selected.kind && leftover.path == selected.path)) {
            return Err("changed_target".to_string());
        }
        Ok(uninstall::remove_leftovers(&selected, &rescue, &format!("uninstall:{}", app.name)))
    }).await??;
    tools.leftovers.lock().unwrap().remove(&(id, mode));
    Ok(report)
}

// ------------------------------------------------------------ Tệp lớn

#[derive(Clone, Serialize)]
struct LargeProgress {
    path: String,
    count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LargeScan {
    files: Vec<LargeFile>,
    cancelled: bool,
}

#[tauri::command]
pub async fn large_locations() -> Result<Vec<Location>, String> {
    blocking(largefiles::locations).await
}

#[tauri::command]
pub async fn large_scan(
    app: AppHandle,
    state: State<'_, AppState>,
    tools: State<'_, ToolState>,
    root: String,
    min_mb: u64,
) -> Result<LargeScan, String> {
    // Chỉ quét những nơi backend tự liệt kê.
    if !largefiles::locations().iter().any(|l| l.path == root) {
        return Err("not_found".into());
    }
    let guard = BusyGuard::acquire(&state.busy)?;
    let cancel = state.cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    let worker_cancel = cancel.clone();
    let files = blocking(move || {
        let _guard = guard;
        let last = Mutex::new(Instant::now());
        largefiles::scan(Path::new(&root), min_mb.max(1) * 1024 * 1024, &worker_cancel, &|path, count| {
            let mut last = last.lock().unwrap();
            if last.elapsed() >= Duration::from_millis(100) {
                *last = Instant::now();
                let _ = app.emit("large-progress", LargeProgress {
                    path: path.to_string_lossy().to_string(),
                    count,
                });
            }
        })
    })
    .await?;
    *tools.large_files.lock().unwrap() = files.clone();
    Ok(LargeScan {
        files,
        cancelled: cancel.load(Ordering::SeqCst),
    })
}

/// Chuyển các tệp lớn đã chọn vào Thùng rác và ghi lịch sử.
#[tauri::command]
pub async fn large_recycle(
    state: State<'_, AppState>,
    tools: State<'_, ToolState>,
    paths: Vec<String>,
) -> Result<RemoveReport, String> {
    let guard = BusyGuard::acquire(&state.busy)?;
    let selected: Vec<LargeFile> = {
        let mut cache = tools.large_files.lock().unwrap();
        let (picked, rest) = cache.drain(..).partition(|f| paths.contains(&f.path));
        *cache = rest;
        picked
    };
    let rescue = state.rescue.clone();
    blocking(move || {
        let _guard = guard;
        let mut report = RemoveReport::default();
        for file in &selected {
            match tp_core::tools::recycle(&PathBuf::from(&file.path)) {
                Ok(()) => {
                    report.done += 1;
                    report.bytes += file.size;
                }
                Err(_) => report.failed += 1,
            }
        }
        if report.done > 0 {
            let id = rescue.new_record_id();
            if rescue
                .add_tool_record(id, "largeFiles", report.done, report.failed, report.bytes, Vec::new())
                .is_ok()
            {
                report.record_id = Some(id);
            }
        }
        report
    })
    .await
}

/// Mở Explorer tại một tệp hoặc thư mục mà công cụ vừa liệt kê.
#[tauri::command]
pub fn reveal(tools: State<'_, ToolState>, path: String) -> Result<(), String> {
    let known = tools.large_files.lock().unwrap().iter().any(|f| f.path == path)
        || tools.leftovers.lock().unwrap().values().flatten().any(|l| l.path == path && matches!(l.kind, "folder" | "shortcut"));
    if !known {
        return Err("not_found".into());
    }
    tp_core::reveal(Path::new(&path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_is_bound_to_ids_mode_and_restore_readiness() {
        let tools = ToolState::default();
        let ids = vec!["one".into(), "two".into()];
        assert!(require_prepared(&tools, &ids, UninstallMode::Standard, true).is_err());
        *tools.preparation.lock().unwrap() = Some(PreparationSession {
            ids: ids.clone(), mode: UninstallMode::Standard, ready: false, started: false, restore_error: Some("needs_admin".into()),
        });
        assert!(require_prepared(&tools, &ids, UninstallMode::Standard, true).is_err());
        tools.preparation.lock().unwrap().as_mut().unwrap().ready = true;
        assert!(require_prepared(&tools, &ids[..1], UninstallMode::Standard, false).is_ok());
        assert!(require_prepared(&tools, &ids[..1], UninstallMode::Standard, true).is_err());
        assert!(require_prepared(&tools, &ids, UninstallMode::Forced, false).is_err());
        assert!(require_prepared(&tools, &["other".into()], UninstallMode::Standard, false).is_err());
    }

    #[test]
    fn native_uninstall_session_cannot_be_replayed() {
        let tools = ToolState::default();
        let ids = vec!["one".into()];
        *tools.preparation.lock().unwrap() = Some(PreparationSession {
            ids: ids.clone(), mode: UninstallMode::Standard, ready: true, started: false, restore_error: None,
        });
        assert!(start_prepared(&tools, &ids).is_ok());
        assert!(start_prepared(&tools, &ids).is_err());
        assert!(require_prepared(&tools, &ids, UninstallMode::Standard, false).is_ok());
    }
}
