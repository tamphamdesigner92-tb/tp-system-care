//! Kiểm tra luật dò tàn dư trên mọi ứng dụng đã cài (chỉ đọc, không xóa gì).
use tp_core::tools::uninstall;

fn main() {
    let apps = uninstall::list().expect("liệt kê ứng dụng");
    for app in &apps {
        for l in uninstall::scan_leftovers(app, &apps) {
            if l.kind == "folder" {
                println!("{} | {} | {} | {:.0} MB", app.name, if l.recommended { "x" } else { " " }, l.path, l.bytes as f64 / 1_048_576.0);
            }
        }
    }
}
