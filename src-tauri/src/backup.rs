use serde::Serialize;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::config::{Config, SyncJob, SyncMode};

#[derive(Debug, Clone, Default, Serialize)]
pub struct BackupProgress {
    pub current_job: usize,
    pub total_jobs: usize,
    pub job_name: String,
    pub files_transferred: u64,
    pub files_total: u64,
    pub bytes_transferred: u64,
    pub bytes_total: u64,
    pub current_file: String,
    pub elapsed_secs: f64,
    pub estimated_total_secs: Option<f64>,
    pub finished: bool,
    pub cancelled: bool,
    pub error: Option<String>,
    pub log_lines: Vec<String>,
    pub child_pid: Option<u32>,
    pub paused: bool,
}

impl BackupProgress {
    pub fn fraction(&self) -> f32 {
        if self.bytes_total == 0 {
            0.0
        } else {
            (self.bytes_transferred as f32 / self.bytes_total as f32).clamp(0.0, 1.0)
        }
    }

    pub fn overall_fraction(&self) -> f32 {
        if self.total_jobs == 0 {
            return 0.0;
        }
        let per_job = 1.0 / self.total_jobs as f32;
        let completed = self.current_job as f32 * per_job;
        completed + self.fraction() * per_job
    }

    pub fn eta_string(&self) -> String {
        if let Some(total) = self.estimated_total_secs {
            let remaining = (total - self.elapsed_secs).max(0.0);
            format_duration(remaining as u64)
        } else {
            "calculating…".to_owned()
        }
    }
}

pub fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

pub type SharedProgress = Arc<Mutex<BackupProgress>>;

pub fn rsync_args(job: &SyncJob, drive_root: &Path) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-av".to_owned(),
        "--info=progress2".to_owned(),
        "--no-inc-recursive".to_owned(),
        "--partial-dir=.rsync-partial".to_owned(),
    ];
    if job.mode == SyncMode::Backup {
        args.push("--delete".to_owned());
    }
    for excl in &job.excludes {
        args.push(format!("--exclude={excl}"));
    }
    let dest = drive_root.join(&job.destination);
    args.push(format!("{}/", job.source.display()));
    args.push(format!("{}/", dest.display()));
    args
}

pub fn rsync_command_string(job: &SyncJob, drive_root: &Path) -> String {
    let args = rsync_args(job, drive_root);
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    format!("rsync {}", quoted.join(" "))
}

fn shell_quote(s: &str) -> String {
    if s.contains(|c: char| {
        c.is_whitespace()
            || matches!(c, '"' | '\'' | '\\' | '$' | '`' | '(' | ')' | '&' | '|' | ';' | '<' | '>' | '!' | '#' | '~' | '*' | '?')
    }) {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s.to_owned()
    }
}

pub fn run_backup(
    config: &Config,
    drive_root: &Path,
    progress: SharedProgress,
) -> std::thread::JoinHandle<()> {
    let jobs: Vec<SyncJob> = config.jobs.iter().filter(|j| j.enabled).cloned().collect();
    let drive_root = drive_root.to_path_buf();
    let total_jobs = jobs.len();

    std::thread::spawn(move || {
        {
            let mut p = progress.lock().unwrap();
            p.total_jobs = total_jobs;
            p.current_job = 0;
            p.finished = false;
            p.cancelled = false;
            p.error = None;
            p.child_pid = None;
            p.paused = false;
        }

        let start = std::time::Instant::now();

        for (idx, job) in jobs.iter().enumerate() {
            if progress.lock().unwrap().cancelled {
                break;
            }
            {
                let mut p = progress.lock().unwrap();
                p.current_job = idx;
                p.job_name = job.name.clone();
                p.files_transferred = 0;
                p.files_total = 0;
                p.bytes_transferred = 0;
                p.bytes_total = 0;
                p.current_file.clear();
            }

            let dest = drive_root.join(&job.destination);
            if let Err(e) = std::fs::create_dir_all(&dest) {
                let mut p = progress.lock().unwrap();
                p.error = Some(format!("mkdir {}: {e}", dest.display()));
                return;
            }

            let args = rsync_args(job, &drive_root);
            let mut child = match Command::new("rsync")
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    let mut p = progress.lock().unwrap();
                    p.error = Some(format!("failed to spawn rsync: {e}"));
                    return;
                }
            };

            progress.lock().unwrap().child_pid = Some(child.id());

            if let Some(stdout) = child.stdout.take() {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    let Ok(line) = line else { continue };
                    let elapsed = start.elapsed().as_secs_f64();
                    for seg in line.split('\r') {
                        let seg = seg.trim();
                        if !seg.is_empty() {
                            parse_rsync_line(seg, &progress, elapsed);
                        }
                    }
                    let display = line
                        .split('\r')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .last()
                        .unwrap_or("")
                        .to_owned();
                    if !display.is_empty() {
                        let mut p = progress.lock().unwrap();
                        p.elapsed_secs = elapsed;
                        if p.log_lines.len() > 500 {
                            p.log_lines.remove(0);
                        }
                        p.log_lines.push(display);
                    }
                }
            }

            progress.lock().unwrap().child_pid = None;
            let cancelled = progress.lock().unwrap().cancelled;

            let status = match child.wait() {
                Ok(s) => s,
                Err(e) => {
                    if cancelled {
                        break;
                    }
                    let mut p = progress.lock().unwrap();
                    p.error = Some(format!("rsync wait error: {e}"));
                    return;
                }
            };

            if cancelled {
                break;
            }
            if !status.success() {
                let code = status.code().unwrap_or(-1);
                // exit code 24 = partial transfer due to vanished source files; treat as success
                if code != 24 {
                    let mut p = progress.lock().unwrap();
                    p.error = Some(format!(
                        "rsync exited with code {code} for job \"{}\"",
                        job.name
                    ));
                    return;
                }
            }
        }

        if progress.lock().unwrap().cancelled {
            let elapsed = start.elapsed().as_secs_f64();
            let mut p = progress.lock().unwrap();
            p.elapsed_secs = elapsed;
            p.current_job = total_jobs;
            return;
        }

        let snapshot_name = chrono::Local::now().format("%Y-%m-%d").to_string();
        let snapshots_dir = drive_root.join("snapshots");
        let _ = std::fs::create_dir_all(&snapshots_dir);

        let snap_output = Command::new("doas")
            .args([
                "btrfs",
                "subvolume",
                "snapshot",
                "-r",
                ".",
                snapshots_dir.join(&snapshot_name).to_str().unwrap_or("."),
            ])
            .current_dir(&drive_root)
            .output();

        let elapsed = start.elapsed().as_secs_f64();
        let mut p = progress.lock().unwrap();
        p.finished = true;
        p.elapsed_secs = elapsed;
        p.current_job = total_jobs;

        match snap_output {
            Err(e) => {
                p.log_lines.push(format!("Warning: btrfs snapshot failed: {e}"));
            }
            Ok(out) if !out.status.success() => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let stderr = stderr.trim();
                if stderr.is_empty() {
                    p.log_lines.push(format!(
                        "Warning: btrfs snapshot exited {}",
                        out.status.code().unwrap_or(-1)
                    ));
                } else {
                    p.log_lines.push(format!("Warning: btrfs snapshot failed: {stderr}"));
                }
                if let Ok(mnt) = Command::new("findmnt")
                    .args(["--output=TARGET,OPTIONS", "--target", drive_root.to_str().unwrap_or(".")])
                    .output()
                {
                    let out = String::from_utf8_lossy(&mnt.stdout);
                    let out = out.trim();
                    if !out.is_empty() {
                        p.log_lines.push(format!("  mount: {out}"));
                    }
                }
                if let Ok(sv) = Command::new("doas")
                    .args(["btrfs", "subvolume", "show", drive_root.to_str().unwrap_or(".")])
                    .output()
                {
                    let out = String::from_utf8_lossy(&sv.stdout);
                    for line in out.lines().take(6) {
                        p.log_lines.push(format!("  {line}"));
                    }
                }
            }
            Ok(_) => {
                p.log_lines.push(format!("Snapshot created: {snapshot_name}"));
            }
        }
    })
}

fn parse_rsync_line(line: &str, progress: &SharedProgress, elapsed: f64) {
    let trimmed = line.trim();

    if trimmed.contains("to-chk=") || trimmed.contains("xfr#") {
        if let Some(pct_pos) = trimmed.find('%') {
            let before_pct: &str = &trimmed[..pct_pos];
            let parts: Vec<&str> = before_pct.split_whitespace().collect();
            if parts.len() >= 2 {
                let bytes_str = parts[parts.len() - 2].replace(',', "");
                let pct_str = parts[parts.len() - 1];
                if let (Ok(bytes), Ok(pct)) =
                    (bytes_str.parse::<u64>(), pct_str.parse::<f64>())
                {
                    let mut p = progress.lock().unwrap();
                    p.bytes_transferred = bytes;
                    if pct > 0.0 {
                        p.bytes_total = (bytes as f64 * 100.0 / pct) as u64;
                    }
                    if pct > 1.0 {
                        let estimated = elapsed / (pct / 100.0);
                        p.estimated_total_secs = Some(estimated);
                    }
                }
            }
        }
        if let Some(pos) = trimmed.find("to-chk=") {
            let rest = &trimmed[pos + 7..];
            let nums: Vec<&str> = rest.split('/').collect();
            if nums.len() >= 2 {
                let remaining: u64 = nums[0].trim_end_matches(')').parse().unwrap_or(0);
                let total: u64 = nums[1]
                    .trim_end_matches(')')
                    .split_whitespace()
                    .next()
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0);
                let mut p = progress.lock().unwrap();
                p.files_total = total;
                p.files_transferred = total.saturating_sub(remaining);
            }
        }
        return;
    }

    if trimmed.contains("ir-chk=") {
        return;
    }

    if !trimmed.is_empty()
        && !trimmed.starts_with("sending")
        && !trimmed.starts_with("receiving")
        && !trimmed.starts_with("sent")
        && !trimmed.starts_with("total")
        && !trimmed.contains('%')
    {
        let mut p = progress.lock().unwrap();
        p.current_file = trimmed.to_owned();
    }
}
