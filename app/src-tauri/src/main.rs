// Không mở cửa sổ console ở bản release trên Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tp_system_care_lib::run()
}
