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
use tp_core::tools::uninstall::{self, AppInfo, Leftover, RemoveReport, UninstallResult};

use crate::{AppState, BusyGuard};

/// Kết quả gần nhất của từng công cụ. Giao diện chỉ gửi lên mã, nên backend chỉ
/// thao tác trên những mục chính nó vừa liệt kê.
#[derive(Default)]
pub struct ToolState {
    apps: Mutex<HashMap<String, AppInfo>>,
    leftovers: Mutex<(String, Vec<Leftover>)>,
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
pub async fn release_ram() -> Result<RamReport, String> {
    blocking(speed::release_ram).await
}

#[tauri::command]
pub async fn turbo_status() -> Result<Turbo, String> {
    blocking(speed::turbo_status).await
}

/// Bật hoặc tắt Turbo Boost; trả về kết quả giải phóng RAM khi bật.
#[tauri::command]
pub async fn turbo_set(on: bool) -> Result<Option<RamReport>, String> {
    blocking(move || if on { speed::turbo_on().map(Some) } else { speed::turbo_off().map(|_| None) }).await?
}

#[tauri::command]
pub async fn startup_list() -> Result<Vec<StartupItem>, String> {
    blocking(startup::list).await
}

#[tauri::command]
pub async fn startup_set(id: String, enabled: bool) -> Result<StartupItem, String> {
    blocking(move || {
        let item = startup::list().into_iter().find(|i| i.id == id).ok_or("not_found")?;
        startup::set_enabled(&item.entry, enabled)?;
        startup::list().into_iter().find(|i| i.id == id).ok_or_else(|| "not_found".to_string())
    })
    .await?
}

// ------------------------------------------------------------ Gỡ ứng dụng

#[tauri::command]
pub async fn apps_list(tools: State<'_, ToolState>) -> Result<Vec<AppInfo>, String> {
    let apps = blocking(uninstall::list).await?;
    *tools.apps.lock().unwrap() = apps.iter().map(|a| (a.id.clone(), a.clone())).collect();
    Ok(apps)
}

fn cached_app(tools: &ToolState, id: &str) -> Result<AppInfo, String> {
    tools.apps.lock().unwrap().get(id).cloned().ok_or_else(|| "not_found".into())
}

#[tauri::command]
pub async fn app_uninstall(
    state: State<'_, AppState>,
    tools: State<'_, ToolState>,
    id: String,
) -> Result<UninstallResult, String> {
    let guard = BusyGuard::acquire(&state.busy)?;
    let app = cached_app(&tools, &id)?;
    let cancel = state.cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    blocking(move || {
        let _guard = guard;
        uninstall::run_uninstaller(&app, &cancel)
    })
    .await?
}

#[tauri::command]
pub async fn leftovers_scan(tools: State<'_, ToolState>, id: String) -> Result<Vec<Leftover>, String> {
    let app = cached_app(&tools, &id)?;
    *tools.leftovers.lock().unwrap() = (id.clone(), Vec::new());
    let leftovers = blocking(move || {
        if uninstall::still_installed(&app) {
            return Err("still_installed".to_string());
        }
        let installed = uninstall::list();
        Ok(uninstall::scan_leftovers(&app, &installed))
    })
    .await??;
    *tools.leftovers.lock().unwrap() = (id, leftovers.clone());
    Ok(leftovers)
}

#[tauri::command]
pub async fn leftovers_remove(
    state: State<'_, AppState>,
    tools: State<'_, ToolState>,
    id: String,
    ids: Vec<String>,
) -> Result<RemoveReport, String> {
    let guard = BusyGuard::acquire(&state.busy)?;
    let app = cached_app(&tools, &id)?;
    let selected: Vec<Leftover> = {
        let cache = tools.leftovers.lock().unwrap();
        if cache.0 != id {
            return Err("not_found".into());
        }
        cache.1.iter().filter(|l| ids.contains(&l.id)).cloned().collect()
    };
    let rescue = state.rescue.clone();
    blocking(move || {
        let _guard = guard;
        if uninstall::still_installed(&app) {
            return Err("still_installed".to_string());
        }
        Ok(uninstall::remove_leftovers(&selected, &rescue, &format!("uninstall:{}", app.name)))
    })
    .await?
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
        || tools.leftovers.lock().unwrap().1.iter().any(|l| l.path == path && l.kind == "folder");
    if !known {
        return Err("not_found".into());
    }
    tp_core::reveal(Path::new(&path))
}
