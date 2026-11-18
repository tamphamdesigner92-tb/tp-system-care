//! Thử các công cụ ở chế độ chỉ đọc: bộ nhớ, Turbo, khởi động, ứng dụng đã cài,
//! tàn dư (không xóa gì), tệp lớn trong thư mục người dùng.
//! Chạy: `cargo run -p tp-core --example tools`

use std::path::Path;
use std::sync::atomic::AtomicBool;

use tp_core::modules::startup;
use tp_core::tools::{browser_extensions, largefiles, speed, uninstall, windows_updates};

fn main() {
    let m = speed::memory();
    println!("RAM: {:.1}/{:.1} GB trống, dùng {}%", m.available as f64 / 1e9, m.total as f64 / 1e9, m.load);
    println!("Turbo: {:?}", speed::turbo_status());

    let items = startup::list();
    println!("Khởi động: {} mục ({} đang tắt)", items.len(), items.iter().filter(|i| !i.enabled).count());

    let apps = uninstall::list().expect("liệt kê ứng dụng");
    println!("Ứng dụng đã cài: {}", apps.len());
    for app in apps.iter().take(5) {
        println!("  {} {} · {:?} · {:?}", app.name, app.version.as_deref().unwrap_or(""), app.install_date, app.install_location);
    }
    for app in apps.iter().filter(|a| a.install_location.is_some()).take(3) {
        let leftovers = uninstall::scan_leftovers(app, &apps);
        println!("Tàn dư nếu gỡ \"{}\" (ứng dụng còn đang cài, chỉ để xem luật dò):", app.name);
        for l in leftovers {
            println!("  [{}] {:<14} {:>8.1} MB {}", if l.recommended { "x" } else { " " }, l.kind, l.bytes as f64 / 1_048_576.0, l.path);
        }
    }

    let now = tp_core::unix_now();
    let mut used: Vec<_> = apps.iter().filter_map(|a| Some((a.last_used?, a))).collect();
    used.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    println!("Có lần mở (UserAssist): {}/{} ứng dụng", used.len(), apps.len());
    for (t, app) in used.iter().take(5).chain(used.iter().rev().take(3)) {
        println!("  {:>5} ngày trước  {}", (now - t) / 86_400, app.name);
    }
    for app in apps.iter().filter(|a| !a.companions.is_empty()) {
        println!("Cài cùng lúc: {} ↔ {}", app.name, app.companions.join(", "));
    }

    let started = std::time::Instant::now();
    match browser_extensions::list() {
        Ok(scan) => {
            println!("Tiện ích trình duyệt: {} ({} hồ sơ không đọc được, {:.1?})", scan.extensions.len(), scan.skipped, started.elapsed());
            for e in &scan.extensions {
                println!("  {:<8} {:<22} {:?}/{:?} {:?} {}", e.browser, e.profile.as_deref().unwrap_or("-"), e.source, e.removal, e.enabled, e.name);
            }
        }
        Err(e) => println!("Tiện ích trình duyệt: lỗi {e}"),
    }

    let started = std::time::Instant::now();
    match windows_updates::list() {
        Ok(updates) => {
            println!("Bản cập nhật Windows: {} ({:.1?})", updates.len(), started.elapsed());
            for u in updates.iter().take(5) {
                println!("  {} {:?} {:?}", u.id, u.description, u.installed_on);
            }
        }
        Err(e) => println!("Bản cập nhật Windows: lỗi {e}"),
    }

    let started = std::time::Instant::now();
    let profile = &largefiles::locations()[0];
    let files = largefiles::scan(Path::new(&profile.path), 500 * 1024 * 1024, &AtomicBool::new(false), &|_, _| {});
    println!("Tệp từ 500 MB trong {} ({:.1?}):", profile.path, started.elapsed());
    for f in files.iter().take(8) {
        println!("  {:>8.1} MB {:<9} {:<8} {}", f.size as f64 / 1_048_576.0, f.category, if f.app_data { "appData" } else { "" }, f.path);
    }
}
