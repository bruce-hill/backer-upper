use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use tauri::State;

use crate::backup::{format_duration, rsync_command_string, run_backup, BackupProgress};
use crate::config::{Config, SyncJob};
use crate::drives::{self, Drive};
use crate::format::{probe_drive, run_format, DriveInfo, FormatProgress};
use crate::state::AppState;

// ── Serializable response types ──────────────────────────────────────────────

#[derive(Serialize)]
pub struct DriveJson {
    pub device: String,
    pub label: Option<String>,
    pub fstype: Option<String>,
    pub size: Option<String>,
    pub mountpoint: Option<String>,
    pub is_encrypted: bool,
    pub luks_parent: Option<String>,
    pub model: Option<String>,
    pub vendor: Option<String>,
    pub tran: Option<String>,
    pub dev_type: String,
    pub display_name: String,
    pub is_mounted: bool,
}

impl From<Drive> for DriveJson {
    fn from(d: Drive) -> Self {
        DriveJson {
            display_name: d.display_name(),
            is_mounted: d.is_mounted(),
            device: d.device,
            label: d.label,
            fstype: d.fstype,
            size: d.size,
            mountpoint: d.mountpoint,
            is_encrypted: d.is_encrypted,
            luks_parent: d.luks_parent,
            model: d.model,
            vendor: d.vendor,
            tran: d.tran,
            dev_type: d.dev_type,
        }
    }
}

#[derive(Serialize)]
pub struct MountResult {
    pub mount_point: String,
    pub config: Config,
}

#[derive(Serialize)]
pub struct OpenResult {
    pub needs_password: bool,
    pub mounted: Option<MountResult>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct PreviewCommand {
    pub name: String,
    pub cmd: String,
}

#[derive(Serialize)]
pub struct BackupProgressJson {
    pub current_job: usize,
    pub total_jobs: usize,
    pub job_name: String,
    pub current_file: String,
    pub elapsed: String,
    pub eta: String,
    pub overall_fraction: f32,
    pub files_transferred: u64,
    pub files_total: u64,
    pub finished: bool,
    pub cancelled: bool,
    pub error: Option<String>,
    pub paused: bool,
    pub log_lines: Vec<String>,
    pub finished_msg: Option<String>,
    pub running: bool,
}

#[derive(Serialize)]
pub struct AppStatus {
    pub mount_point: Option<String>,
    pub config: Option<Config>,
    pub config_dirty: bool,
    pub status_msg: Option<String>,
    pub backup_running: bool,
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_drives() -> Result<Vec<DriveJson>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        drives::list_removable_drives()
            .map(|ds| ds.into_iter().map(DriveJson::from).collect())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn open_drive(
    state: State<'_, Mutex<AppState>>,
    device: String,
) -> Result<OpenResult, String> {
    let drive = match tauri::async_runtime::spawn_blocking({
        let device = device.clone();
        move || {
            drives::list_removable_drives()
                .map_err(|e| e.to_string())
                .and_then(|ds| {
                    ds.into_iter()
                        .find(|d| d.device == device)
                        .ok_or_else(|| format!("Drive {device} not found"))
                })
        }
    })
    .await
    {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => return Ok(OpenResult { needs_password: false, mounted: None, error: Some(e) }),
        Err(e) => return Ok(OpenResult { needs_password: false, mounted: None, error: Some(e.to_string()) }),
    };

    if drive.is_mounted() {
        let mp = PathBuf::from(drive.mountpoint.as_deref().unwrap_or("/"));
        let config = load_config(&mp);
        let mut s = state.lock().unwrap();
        s.mount_point = Some(mp.clone());
        if let Some(luks_parent) = &drive.luks_parent {
            s.mapper_name = Some(drive.device.clone());
            s.mounted_device = Some(luks_parent.clone());
        } else {
            s.mounted_device = Some(drive.device.clone());
        }
        s.config = Some(config.clone());
        s.config_dirty = false;
        return Ok(OpenResult {
            needs_password: false,
            mounted: Some(MountResult { mount_point: mp.display().to_string(), config }),
            error: None,
        });
    }

    if drive.is_encrypted {
        return Ok(OpenResult { needs_password: true, mounted: None, error: None });
    }

    let dev = drive.device.clone();
    match tauri::async_runtime::spawn_blocking(move || drives::mount_device(&dev)).await {
        Ok(Ok(mp)) => {
            let config = load_config(&mp);
            let mut s = state.lock().unwrap();
            s.mounted_device = Some(drive.device);
            s.mount_point = Some(mp.clone());
            s.config = Some(config.clone());
            s.config_dirty = false;
            Ok(OpenResult {
                needs_password: false,
                mounted: Some(MountResult { mount_point: mp.display().to_string(), config }),
                error: None,
            })
        }
        Ok(Err(e)) => Ok(OpenResult {
            needs_password: false,
            mounted: None,
            error: Some(format!("Mount failed: {e}")),
        }),
        Err(e) => Ok(OpenResult { needs_password: false, mounted: None, error: Some(e.to_string()) }),
    }
}

#[tauri::command]
pub async fn unlock_drive(
    state: State<'_, Mutex<AppState>>,
    device: String,
    password: String,
) -> Result<MountResult, String> {
    let luks_dev = device.clone();
    let (dm_device, mp) = tauri::async_runtime::spawn_blocking(move || {
        drives::unlock_and_mount(&device, &password).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    let config = load_config(&mp);
    let mut s = state.lock().unwrap();
    s.mounted_device = Some(luks_dev);
    s.mapper_name = Some(dm_device);
    s.mount_point = Some(mp.clone());
    s.config = Some(config.clone());
    s.config_dirty = false;
    Ok(MountResult { mount_point: mp.display().to_string(), config })
}

#[tauri::command]
pub fn get_config(state: State<'_, Mutex<AppState>>) -> Option<Config> {
    state.lock().unwrap().config.clone()
}

#[tauri::command]
pub fn save_config(state: State<'_, Mutex<AppState>>) -> Result<(), String> {
    let s = state.lock().unwrap();
    if let (Some(cfg), Some(mp)) = (&s.config, &s.mount_point) {
        cfg.save(mp).map_err(|e| e.to_string())?;
        drop(s);
        state.lock().unwrap().config_dirty = false;
        Ok(())
    } else {
        Err("No config or mount point".to_owned())
    }
}

#[tauri::command]
pub fn update_config(state: State<'_, Mutex<AppState>>, config: Config) -> Result<(), String> {
    let mut s = state.lock().unwrap();
    s.config = Some(config);
    s.config_dirty = true;
    Ok(())
}

#[tauri::command]
pub fn add_job(state: State<'_, Mutex<AppState>>) -> Result<Config, String> {
    let mut s = state.lock().unwrap();
    if let Some(cfg) = &mut s.config {
        let idx = cfg.jobs.len();
        cfg.jobs.push(SyncJob::new(
            format!("Job {}", idx + 1),
            std::env::var("HOME").unwrap_or_default(),
        ));
        s.config_dirty = true;
        Ok(s.config.clone().unwrap())
    } else {
        Err("No config loaded".to_owned())
    }
}

#[tauri::command]
pub fn delete_job(state: State<'_, Mutex<AppState>>, idx: usize) -> Result<Config, String> {
    let mut s = state.lock().unwrap();
    if let Some(cfg) = &mut s.config {
        if idx < cfg.jobs.len() {
            cfg.jobs.remove(idx);
            s.config_dirty = true;
            Ok(s.config.clone().unwrap())
        } else {
            Err("Job index out of range".to_owned())
        }
    } else {
        Err("No config loaded".to_owned())
    }
}

#[tauri::command]
pub async fn eject(state: State<'_, Mutex<AppState>>) -> Result<(), String> {
    let (mapper_name, mounted_device) = {
        let s = state.lock().unwrap();
        (s.mapper_name.clone(), s.mounted_device.clone())
    };

    tauri::async_runtime::spawn_blocking(move || {
        let result = match (&mapper_name, &mounted_device) {
            (Some(cleartext_dev), Some(luks_dev)) => drives::udisksctl_unmount(cleartext_dev)
                .and_then(|()| drives::udisksctl_lock(luks_dev)),
            (None, Some(dev)) => drives::udisksctl_unmount(dev),
            _ => Ok(()),
        };
        if result.is_ok() {
            if let Some(dev) = &mounted_device {
                let _ = drives::udisksctl_power_off(dev);
            }
        }
        result.map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    let mut s = state.lock().unwrap();
    s.mount_point = None;
    s.mounted_device = None;
    s.mapper_name = None;
    s.config = None;
    s.config_dirty = false;
    s.backup_finished_msg = None;
    Ok(())
}

#[tauri::command]
pub fn preview_commands(state: State<'_, Mutex<AppState>>) -> Vec<PreviewCommand> {
    let s = state.lock().unwrap();
    match (&s.config, &s.mount_point) {
        (Some(cfg), Some(mp)) => cfg
            .jobs
            .iter()
            .filter(|j| j.enabled)
            .map(|j| PreviewCommand {
                name: j.name.clone(),
                cmd: rsync_command_string(j, mp),
            })
            .collect(),
        _ => vec![],
    }
}

#[tauri::command]
pub fn start_backup(state: State<'_, Mutex<AppState>>) -> Result<(), String> {
    let (cfg, mp, progress) = {
        let s = state.lock().unwrap();
        match (s.config.clone(), s.mount_point.clone()) {
            (Some(c), Some(m)) => (c, m, std::sync::Arc::clone(&s.progress)),
            _ => return Err("No config or mount point".to_owned()),
        }
    };

    {
        let mut p = progress.lock().unwrap();
        *p = BackupProgress::default();
    }

    state.lock().unwrap().backup_running = true;
    state.lock().unwrap().backup_finished_msg = None;

    run_backup(&cfg, &mp, progress);
    Ok(())
}

#[tauri::command]
pub fn get_backup_progress(state: State<'_, Mutex<AppState>>) -> BackupProgressJson {
    let mut s = state.lock().unwrap();
    let p = s.progress.lock().unwrap().clone();

    let running = s.backup_running && !p.finished && p.error.is_none();

    if s.backup_running && (p.finished || p.error.is_some()) {
        s.backup_running = false;
        if p.cancelled {
            s.backup_finished_msg = Some("Backup cancelled.".to_owned());
        } else if let Some(ref err) = p.error {
            s.backup_finished_msg = Some(format!("Backup failed: {err}"));
        } else {
            let elapsed = format_duration(p.elapsed_secs as u64);
            s.backup_finished_msg = Some(format!("Backup complete in {elapsed}!"));
            if let Some(cfg) = &mut s.config {
                cfg.last_backup = Some(chrono::Local::now());
            }
            if !s.config_dirty {
                if let (Some(cfg), Some(mp)) = (&s.config, &s.mount_point) {
                    let _ = cfg.save(mp);
                }
            }
        }
    }

    let finished_msg = s.backup_finished_msg.clone();

    let eta = p.eta_string();
    let elapsed = format_duration(p.elapsed_secs as u64);
    let overall_fraction = p.overall_fraction();
    BackupProgressJson {
        current_job: p.current_job,
        total_jobs: p.total_jobs,
        job_name: p.job_name,
        current_file: p.current_file,
        elapsed,
        eta,
        overall_fraction,
        files_transferred: p.files_transferred,
        files_total: p.files_total,
        finished: p.finished,
        cancelled: p.cancelled,
        error: p.error,
        paused: p.paused,
        log_lines: p.log_lines,
        finished_msg,
        running,
    }
}

#[tauri::command]
pub fn pause_backup(state: State<'_, Mutex<AppState>>) {
    let s = state.lock().unwrap();
    let p = s.progress.lock().unwrap();
    if let Some(pid) = p.child_pid {
        drop(p);
        drop(s);
        let _ = std::process::Command::new("kill")
            .args(["-STOP", &pid.to_string()])
            .status();
        state.lock().unwrap().progress.lock().unwrap().paused = true;
    }
}

#[tauri::command]
pub fn resume_backup(state: State<'_, Mutex<AppState>>) {
    let s = state.lock().unwrap();
    let p = s.progress.lock().unwrap();
    if let Some(pid) = p.child_pid {
        drop(p);
        drop(s);
        let _ = std::process::Command::new("kill")
            .args(["-CONT", &pid.to_string()])
            .status();
        state.lock().unwrap().progress.lock().unwrap().paused = false;
    }
}

#[tauri::command]
pub fn cancel_backup(state: State<'_, Mutex<AppState>>) {
    let s = state.lock().unwrap();
    let mut p = s.progress.lock().unwrap();
    p.cancelled = true;
    p.finished = true;
    if let Some(pid) = p.child_pid {
        drop(p);
        drop(s);
        let _ = std::process::Command::new("kill")
            .args([&pid.to_string()])
            .status();
    }
}

#[tauri::command]
pub fn start_probe_drive(
    state: State<'_, Mutex<AppState>>,
    device: String,
    fstype: Option<String>,
) {
    let shared = probe_drive(device, fstype);
    state.lock().unwrap().format_drive_info = shared;
}

#[tauri::command]
pub fn get_drive_probe(state: State<'_, Mutex<AppState>>) -> DriveInfo {
    state
        .lock()
        .unwrap()
        .format_drive_info
        .lock()
        .unwrap()
        .clone()
}

#[tauri::command]
pub fn start_format(
    state: State<'_, Mutex<AppState>>,
    device: String,
    is_disk: bool,
    label: String,
    passphrase: String,
) -> Result<(), String> {
    let progress = std::sync::Arc::clone(&state.lock().unwrap().format_progress);
    state.lock().unwrap().format_running = true;
    run_format(device, is_disk, label, passphrase, progress);
    Ok(())
}

#[tauri::command]
pub fn get_format_progress(state: State<'_, Mutex<AppState>>) -> FormatProgress {
    let s = state.lock().unwrap();
    let p = s.format_progress.lock().unwrap().clone();
    if s.format_running && p.finished {
        drop(s);
        state.lock().unwrap().format_running = false;
    }
    p
}

#[tauri::command]
pub fn get_status(state: State<'_, Mutex<AppState>>) -> AppStatus {
    let s = state.lock().unwrap();
    AppStatus {
        mount_point: s.mount_point.as_ref().map(|p| p.display().to_string()),
        config: s.config.clone(),
        config_dirty: s.config_dirty,
        status_msg: s.status_msg.clone(),
        backup_running: s.backup_running,
    }
}

fn load_config(mp: &PathBuf) -> Config {
    Config::load(mp).unwrap_or_default()
}

#[tauri::command]
pub fn quit(app: tauri::AppHandle) {
    app.exit(0);
}
