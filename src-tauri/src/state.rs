use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::backup::{BackupProgress, SharedProgress};
use crate::config::Config;
use crate::format::{DriveInfo, FormatProgress, SharedDriveInfo, SharedFormatProgress};

pub struct AppState {
    pub mount_point: Option<PathBuf>,
    pub mounted_device: Option<String>,
    pub mapper_name: Option<String>,
    pub config: Option<Config>,
    pub config_dirty: bool,
    pub progress: SharedProgress,
    pub backup_running: bool,
    pub backup_finished_msg: Option<String>,
    pub format_progress: SharedFormatProgress,
    pub format_running: bool,
    pub format_drive_info: SharedDriveInfo,
    pub status_msg: Option<String>,
    pub is_restore: bool,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            mount_point: None,
            mounted_device: None,
            mapper_name: None,
            config: None,
            config_dirty: false,
            progress: Arc::new(Mutex::new(BackupProgress::default())),
            backup_running: false,
            backup_finished_msg: None,
            format_progress: Arc::new(Mutex::new(FormatProgress::default())),
            format_running: false,
            format_drive_info: Arc::new(Mutex::new(DriveInfo::default())),
            status_msg: None,
            is_restore: false,
        }
    }
}
