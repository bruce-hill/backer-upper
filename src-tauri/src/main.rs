mod backup;
mod commands;
mod config;
mod drives;
mod format;
mod state;

use std::sync::Mutex;
use state::AppState;

fn main() {
    tauri::Builder::default()
        .manage(Mutex::new(AppState::new()))
        .invoke_handler(tauri::generate_handler![
            commands::list_drives,
            commands::open_drive,
            commands::unlock_drive,
            commands::get_config,
            commands::save_config,
            commands::update_config,
            commands::add_job,
            commands::delete_job,
            commands::eject,
            commands::unmount_device,
            commands::preview_commands,
            commands::start_backup,
            commands::get_backup_progress,
            commands::pause_backup,
            commands::resume_backup,
            commands::cancel_backup,
            commands::start_probe_drive,
            commands::get_drive_probe,
            commands::start_format,
            commands::get_format_progress,
            commands::get_status,
            commands::list_snapshots,
            commands::preview_restore,
            commands::start_restore,
            commands::quit,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
