fn main() {
    // Bản release yêu cầu quyền quản trị (UAC) ngay khi mở.
    // Bản dev giữ quyền thường để `tauri dev` chạy được từ terminal không nâng quyền.
    let release = std::env::var("PROFILE").map(|p| p == "release").unwrap_or(false);

    let mut attributes = tauri_build::Attributes::new();
    if release {
        let windows = tauri_build::WindowsAttributes::new()
            .app_manifest(include_str!("app.admin.manifest"));
        attributes = attributes.windows_attributes(windows);
    }
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}
