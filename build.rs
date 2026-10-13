const COMMANDS: &[&str] = &["request_sync", "sync_status"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
