//! Quét thử trên máy này và in kết quả. Chỉ đọc, không sửa gì.
//! Chạy: `cargo run -p tp-core --example scan`

use std::sync::atomic::AtomicBool;

use tp_core::engine::{self, ModuleId};

fn main() {
    let modules = ModuleId::ALL;
    let started = std::time::Instant::now();
    let last = std::sync::Mutex::new(started);
    let results = engine::scan(&modules, &AtomicBool::new(false), &|event| {
        if let engine::ScanEvent::ModuleFinished(result) = event {
            let mut last = last.lock().unwrap();
            println!("-- {} xong sau {:.1?}", result.module.as_str(), last.elapsed());
            *last = std::time::Instant::now();
        }
    });
    for result in &results {
        println!("== {} ({} mục)", result.module.as_str(), result.findings.len());
        for f in &result.findings {
            println!(
                "  [{}] {:<24} {:>7} tệp {:>9.1} MB  {}{}",
                if f.recommended { "x" } else { " " },
                f.kind,
                f.count,
                f.bytes as f64 / 1_048_576.0,
                f.name.as_deref().unwrap_or(""),
                f.detail.as_deref().map(|d| format!("  ← {d}")).unwrap_or_default(),
            );
        }
    }
    println!("Thời gian quét: {:.1?}", started.elapsed());
}
