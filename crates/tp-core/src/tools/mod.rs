//! Công cụ chạy theo yêu cầu, ngoài luồng quét/sửa của engine: Tăng tốc, Gỡ ứng
//! dụng sạch, Tìm tệp lớn.

pub mod browser_extensions;
pub mod component_store;
pub mod duplicates;
pub(crate) mod verified_recycle;
pub mod install_monitor;
pub mod tuning;
pub mod largefiles;
pub mod speed;
pub mod uninstall;
pub mod store_apps;
pub mod uninstall_entries;
pub mod windows_updates;

pub use crate::winutil::recycle;
