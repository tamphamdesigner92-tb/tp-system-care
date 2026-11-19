//! Thử các công cụ ở chế độ chỉ đọc: bộ nhớ, Turbo, khởi động, ứng dụng đã cài,
//! tàn dư (không xóa gì), tệp lớn trong thư mục người dùng.
//! Chạy: `cargo run -p tp-core --example tools`

use std::path::Path;
use std::sync::atomic::AtomicBool;

use tp_core::modules::startup;
use tp_core::tools::{largefiles, speed, uninstall};

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--benchmark") {
        let started = std::time::Instant::now();
        let apps = uninstall::list().expect("liệt kê ứng dụng");
        println!(
            "apps={} elapsed={:.3}s",
            apps.len(),
            started.elapsed().as_secs_f64()
        );
        let profile = largefiles::locations()
            .into_iter()
            .find(|location| location.kind == "profile")
            .expect("thư mục người dùng");
        let started = std::time::Instant::now();
        let files = largefiles::scan(
            Path::new(&profile.path),
            500 * 1024 * 1024,
            &AtomicBool::new(false),
            &|_, _| {},
        );
        println!(
            "large_files={} threshold_mib=500 elapsed={:.3}s",
            files.len(),
            started.elapsed().as_secs_f64()
        );
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("--review") {
        use tp_core::tools::software_review;
        let scan = software_review::extensions().expect("kiểm kê tiện ích");
        println!(
            "extensions={} skipped={} truncated={}",
            scan.extensions.len(),
            scan.skipped,
            scan.truncated
        );
        let updates =
            software_review::updates(&AtomicBool::new(false)).expect("liệt kê bản cập nhật");
        println!(
            "installed_updates={} uninstallable={}",
            updates.len(),
            updates.iter().filter(|update| update.uninstallable).count()
        );
        return;
    }
    let m = speed::memory();
    println!(
        "RAM: {:.1}/{:.1} GB trống, dùng {}%",
        m.available as f64 / 1e9,
        m.total as f64 / 1e9,
        m.load
    );
    println!("Turbo: {:?}", speed::turbo_status());

    let items = startup::list();
    println!(
        "Khởi động: {} mục ({} đang tắt)",
        items.len(),
        items.iter().filter(|i| !i.enabled).count()
    );

    let apps = uninstall::list().expect("liệt kê ứng dụng");
    println!("Ứng dụng đã cài: {}", apps.len());
    for app in apps.iter().take(5) {
        println!(
            "  {} {} · {:?} · {:?}",
            app.name,
            app.version.as_deref().unwrap_or(""),
            app.install_date,
            app.install_location
        );
    }
    for app in apps.iter().filter(|a| a.install_location.is_some()).take(3) {
        let leftovers = uninstall::scan_leftovers(app, &apps);
        println!(
            "Tàn dư nếu gỡ \"{}\" (ứng dụng còn đang cài, chỉ để xem luật dò):",
            app.name
        );
        for l in leftovers {
            println!(
                "  [{}] {:<14} {:>8.1} MB {}",
                if l.recommended { "x" } else { " " },
                l.kind,
                l.bytes as f64 / 1_048_576.0,
                l.path
            );
        }
    }

    let started = std::time::Instant::now();
    let profile = &largefiles::locations()[0];
    let files = largefiles::scan(
        Path::new(&profile.path),
        500 * 1024 * 1024,
        &AtomicBool::new(false),
        &|_, _| {},
    );
    println!(
        "Tệp từ 500 MB trong {} ({:.1?}):",
        profile.path,
        started.elapsed()
    );
    for f in files.iter().take(8) {
        println!(
            "  {:>8.1} MB {:<9} {:<8} {}",
            f.size as f64 / 1_048_576.0,
            f.category,
            if f.app_data { "appData" } else { "" },
            f.path
        );
    }
}
