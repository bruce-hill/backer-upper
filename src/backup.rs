use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::config::{Config, SyncJob, SyncMode};

#[derive(Debug, Clone)]
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
            (self.bytes_transferred as f32 / self.bytes_total as f32)
                .clamp(0.0, 1.0)
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

fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

pub type SharedProgress = Arc<Mutex<BackupProgress>>;

/// Build the rsync argument list for one job (without actually running it).
pub fn rsync_args(job: &SyncJob, drive_root: &Path) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-avz".to_owned(),
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

/// Format the rsync command for a job as a shell-displayable string.
pub fn rsync_command_string(job: &SyncJob, drive_root: &Path) -> String {
    let args = rsync_args(job, drive_root);
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    format!("rsync {}", quoted.join(" "))
}

fn shell_quote(s: &str) -> String {
    if s.contains(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '$' | '`' | '(' | ')' | '&' | '|' | ';' | '<' | '>' | '!' | '#' | '~' | '*' | '?')) {
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
    let jobs: Vec<SyncJob> = config
        .jobs
        .iter()
        .filter(|j| j.enabled)
        .cloned()
        .collect();
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
            // Check for cancellation between jobs
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

            // Publish PID so the UI can send SIGSTOP/SIGCONT/SIGTERM
            progress.lock().unwrap().child_pid = Some(child.id());

            if let Some(stdout) = child.stdout.take() {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    let Ok(line) = line else { continue };
                    let elapsed = start.elapsed().as_secs_f64();
                    parse_rsync_line(&line, &progress, elapsed);
                    let mut p = progress.lock().unwrap();
                    p.elapsed_secs = elapsed;
                    if p.log_lines.len() > 500 {
                        p.log_lines.remove(0);
                    }
                    p.log_lines.push(line);
                }
            }

            progress.lock().unwrap().child_pid = None;

            let cancelled = progress.lock().unwrap().cancelled;

            let status = match child.wait() {
                Ok(s) => s,
                Err(e) => {
                    if cancelled { break; }
                    let mut p = progress.lock().unwrap();
                    p.error = Some(format!("rsync wait error: {e}"));
                    return;
                }
            };

            if cancelled { break; }

            if !status.success() {
                let code = status.code().unwrap_or(-1);
                // rsync exit code 24 = some files vanished — treat as warning
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

        // Create btrfs snapshot
        let snapshot_name = format!(
            "snapshot-{}",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
        let snapshots_dir = drive_root.join(".snapshots");
        let _ = std::fs::create_dir_all(&snapshots_dir);

        let snap_status = Command::new("btrfs")
            .args([
                "subvolume",
                "snapshot",
                "-r",
                drive_root.to_str().unwrap_or("."),
                snapshots_dir.join(&snapshot_name).to_str().unwrap_or("."),
            ])
            .status();

        let elapsed = start.elapsed().as_secs_f64();
        let mut p = progress.lock().unwrap();
        p.finished = true;
        p.elapsed_secs = elapsed;
        p.current_job = total_jobs;

        match snap_status {
            Err(e) => {
                p.log_lines.push(format!("Warning: btrfs snapshot failed: {e}"));
            }
            Ok(snap) if !snap.success() => {
                p.log_lines.push(format!(
                    "Warning: btrfs snapshot exited {}",
                    snap.code().unwrap_or(-1)
                ));
            }
            Ok(_) => {
                p.log_lines.push(format!("Snapshot created: {snapshot_name}"));
            }
        }
    })
}

fn parse_rsync_line(line: &str, progress: &SharedProgress, elapsed: f64) {
    // progress2 format:  "    123,456,789  45%   12.34MB/s    0:01:23 (xfr#42, to-chk=100/200)"
    let trimmed = line.trim();

    // Detect xfr lines
    if trimmed.contains("to-chk=") || trimmed.contains("xfr#") {
        if let Some(pct_pos) = trimmed.find('%') {
            let before_pct: &str = &trimmed[..pct_pos];
            let parts: Vec<&str> = before_pct.split_whitespace().collect();
            if let Some(pct_str) = parts.last() {
                if let Ok(pct) = pct_str.parse::<f64>() {
                    let mut p = progress.lock().unwrap();
                    // Synthesize bytes from percentage
                    if p.bytes_total > 0 {
                        p.bytes_transferred = (p.bytes_total as f64 * pct / 100.0) as u64;
                    }
                    // Estimate total time
                    if pct > 1.0 {
                        let estimated = elapsed / (pct / 100.0);
                        p.estimated_total_secs = Some(estimated);
                    }
                }
            }
        }

        // Parse to-chk=done/total
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

    // Detect byte-count totals from --info=progress2 summary lines
    // Format: "      2,048,576,000 100%  123.45MB/s    0:00:16 (xfr#1234, ir-chk=0/5678)"
    if trimmed.contains("ir-chk=") || trimmed.starts_with("Total") {
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if let Some(bytes_str) = parts.first() {
            let bytes: u64 = bytes_str
                .replace(',', "")
                .parse()
                .unwrap_or(0);
            if bytes > 0 {
                let mut p = progress.lock().unwrap();
                p.bytes_total = bytes;
            }
        }
        return;
    }

    // Detect "current file" lines (plain paths)
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
