//! Lõi của TP System Care: engine quét/sửa, các module và Rescue Center.
//!
//! Luồng chung: [`engine::scan`] chạy các module → danh sách [`engine::Finding`]
//! → [`engine::fix`] thực thi hành động của từng mục và ghi vào [`rescue::Rescue`].

pub mod engine;
pub mod modules;
pub mod reg;
pub mod rescue;
pub mod state;
pub mod tools;
mod winutil;

pub use winutil::{is_elevated, open_uri, reveal, unix_now};
