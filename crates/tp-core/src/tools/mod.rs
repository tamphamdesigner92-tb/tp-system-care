//! Công cụ chạy theo yêu cầu, ngoài luồng quét/sửa của engine: Tăng tốc, Gỡ ứng
//! dụng sạch, Tìm tệp lớn.

pub mod largefiles;
pub mod speed;
pub mod uninstall;

pub use crate::winutil::recycle;
