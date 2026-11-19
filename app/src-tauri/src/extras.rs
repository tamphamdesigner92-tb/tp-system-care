//! Lệnh Gỡ sạch đợt 3: tiện ích trình duyệt và bản cập nhật Windows. Giao diện
//! chỉ gửi mã của mục backend vừa liệt kê.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::State;
use tp_core::tools::browser_extensions::{self, Extension, ExtensionRemoval, ExtensionScan};
use tp_core::tools::windows_updates::{self, Update, UpdateResult};

use crate::{AppState, BusyGuard};

#[derive(Default)]
pub struct ExtrasState {
    extensions: Mutex<Option<Arc<Vec<Extension>>>>,
    updates: Mutex<Vec<Update>>,
    preparation: Mutex<Option<UpdatePreparation>>,
}

struct UpdatePreparation {
    id: String,
    ready: bool,
    restore_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preparation {
    ready: bool,
    restore_error: Option<String>,
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
}

// ------------------------------------------------------------ Tiện ích trình duyệt

#[tauri::command]
pub async fn extensions_list(
    state: State<'_, AppState>,
    extras: State<'_, ExtrasState>,
) -> Result<ExtensionScan, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    *extras.extensions.lock().unwrap() = None;
    let scan = blocking(browser_extensions::list).await?;
    *extras.extensions.lock().unwrap() = Some(Arc::new(scan.extensions.clone()));
    Ok(scan)
}

#[tauri::command]
pub async fn extensions_remove(
    state: State<'_, AppState>,
    extras: State<'_, ExtrasState>,
    ids: Vec<String>,
) -> Result<ExtensionRemoval, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    let items = extras
        .extensions
        .lock()
        .unwrap()
        .clone()
        .ok_or("not_found")?;
    let rescue = state.rescue.clone();
    let report = blocking(move || browser_extensions::remove(&items, &ids, &rescue)).await;
    // Danh sách cũ không còn đúng sau khi xóa: buộc giao diện tải lại.
    if report.is_ok() {
        *extras.extensions.lock().unwrap() = None;
    }
    report
}

#[tauri::command]
pub fn extension_manage(extras: State<'_, ExtrasState>, id: String) -> Result<(), String> {
    let item = extras
        .extensions
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|items| items.iter().find(|item| item.id == id).cloned())
        .ok_or("not_found")?;
    if item.removal != browser_extensions::Removal::Browser {
        return Err("protected_app".into());
    }
    browser_extensions::open_manage(&item)
}

// ------------------------------------------------------------ Bản cập nhật Windows

#[tauri::command]
pub async fn updates_list(
    state: State<'_, AppState>,
    extras: State<'_, ExtrasState>,
) -> Result<Vec<Update>, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    extras.updates.lock().unwrap().clear();
    *extras.preparation.lock().unwrap() = None;
    let updates = blocking(windows_updates::list).await?;
    *extras.updates.lock().unwrap() = updates.clone();
    Ok(updates)
}

fn known_update(extras: &ExtrasState, id: &str) -> Result<(), String> {
    if extras
        .updates
        .lock()
        .unwrap()
        .iter()
        .any(|update| update.id == id)
    {
        Ok(())
    } else {
        Err("not_found".into())
    }
}

/// Bỏ qua điểm khôi phục chỉ được sau một lần tạo thất bại cho đúng bản cập nhật này.
fn acknowledge(
    preparation: &mut Option<UpdatePreparation>,
    id: &str,
) -> Result<Preparation, String> {
    let session = preparation
        .as_mut()
        .filter(|session| session.id == id && !session.ready && session.restore_error.is_some())
        .ok_or("restore_required")?;
    session.ready = true;
    Ok(Preparation {
        ready: true,
        restore_error: session.restore_error.clone(),
    })
}

/// Mỗi lần chuẩn bị chỉ dùng cho một lần gỡ.
fn consume(preparation: &mut Option<UpdatePreparation>, id: &str) -> Result<(), String> {
    if !preparation
        .as_ref()
        .is_some_and(|session| session.ready && session.id == id)
    {
        return Err("restore_required".into());
    }
    *preparation = None;
    Ok(())
}

#[tauri::command]
pub async fn update_prepare(
    state: State<'_, AppState>,
    extras: State<'_, ExtrasState>,
    id: String,
    continue_without_restore: bool,
) -> Result<Preparation, String> {
    let _guard = BusyGuard::acquire(&state.busy)?;
    known_update(&extras, &id)?;
    if continue_without_restore {
        return acknowledge(&mut extras.preparation.lock().unwrap(), &id);
    }
    *extras.preparation.lock().unwrap() = None;
    let restore_error = blocking(|| {
        Ok(tp_core::rescue::create_restore_point("TP System Care - Windows Update").err())
    })
    .await?;
    let ready = restore_error.is_none();
    *extras.preparation.lock().unwrap() = Some(UpdatePreparation {
        id,
        ready,
        restore_error: restore_error.clone(),
    });
    Ok(Preparation {
        ready,
        restore_error,
    })
}

#[tauri::command]
pub async fn update_uninstall(
    state: State<'_, AppState>,
    extras: State<'_, ExtrasState>,
    id: String,
) -> Result<UpdateResult, String> {
    let guard = BusyGuard::acquire(&state.busy)?;
    known_update(&extras, &id)?;
    consume(&mut extras.preparation.lock().unwrap(), &id)?;
    let cancel = state.cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    let rescue = state.rescue.clone();
    blocking(move || {
        let _guard = guard;
        let result = windows_updates::uninstall(&id, &cancel)?;
        // Ngừng chờ thì chưa biết kết quả: không ghi thành công hay thất bại.
        if result.exit_code.is_some() {
            let done = u64::from(!result.still_installed || result.restart_required);
            let _ = rescue.add_tool_record(
                rescue.new_record_id(),
                &format!("windowsUpdate:{id}"),
                done,
                1 - done,
                0,
                Vec::new(),
            );
        }
        Ok(result)
    })
    .await
}

#[tauri::command]
pub fn updates_history() -> Result<(), String> {
    tp_core::open_uri("ms-settings:windowsupdate-history")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_bypass_is_bound_to_one_failed_update_and_cannot_replay() {
        let mut preparation = None;
        assert!(acknowledge(&mut preparation, "KB5000001").is_err());
        assert!(consume(&mut preparation, "KB5000001").is_err());
        preparation = Some(UpdatePreparation {
            id: "KB5000001".into(),
            ready: false,
            restore_error: Some("needs_admin".into()),
        });
        assert!(consume(&mut preparation, "KB5000001").is_err());
        assert!(acknowledge(&mut preparation, "KB5000002").is_err());
        assert_eq!(
            acknowledge(&mut preparation, "KB5000001")
                .unwrap()
                .restore_error
                .as_deref(),
            Some("needs_admin")
        );
        assert!(acknowledge(&mut preparation, "KB5000001").is_err());
        assert!(consume(&mut preparation, "KB5000002").is_err());
        assert!(consume(&mut preparation, "KB5000001").is_ok());
        assert!(consume(&mut preparation, "KB5000001").is_err());
    }

    #[test]
    fn uninstall_requires_listed_update() {
        let extras = ExtrasState::default();
        assert!(known_update(&extras, "KB5000001").is_err());
        extras.updates.lock().unwrap().push(Update {
            id: "KB5000001".into(),
            description: None,
            installed_on: None,
        });
        assert!(known_update(&extras, "KB5000001").is_ok());
        assert!(known_update(&extras, "KB5000002").is_err());
    }
}
