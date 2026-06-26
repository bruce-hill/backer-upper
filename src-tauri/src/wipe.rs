use anyhow::{Context, Result};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default, Serialize)]
pub struct WipeProgress {
    pub bytes_written: u64,
    pub total_bytes: u64,
    pub finished: bool,
    pub error: Option<String>,
    pub cancelled: bool,
}

pub type SharedWipeProgress = Arc<Mutex<WipeProgress>>;

pub const FILL_FILENAME: &str = ".backer-upper-wipe";

pub fn cleanup_fill_file(drive_root: &Path) {
    let fill_path = drive_root.join(FILL_FILENAME);
    if fill_path.exists() {
        let _ = std::fs::remove_file(&fill_path);
    }
}

fn available_bytes(path: &Path) -> u64 {
    let out = Command::new("df")
        .args(["--block-size=1", "--output=avail"])
        .arg(path)
        .output()
        .ok();
    out.and_then(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .nth(1)
            .and_then(|l| l.trim().parse::<u64>().ok())
    })
    .unwrap_or(0)
}

fn do_wipe(drive_root: &Path, progress: &SharedWipeProgress) -> Result<()> {
    let fill_path = drive_root.join(FILL_FILENAME);

    let total = available_bytes(drive_root);
    progress.lock().unwrap().total_bytes = total;

    let buf = vec![0u8; 4 * 1024 * 1024]; // 4 MB chunks
    let mut bytes_written = 0u64;

    {
        let mut file =
            std::fs::File::create(&fill_path).context("failed to create fill file")?;

        loop {
            if progress.lock().unwrap().cancelled {
                break;
            }
            match file.write_all(&buf) {
                Ok(()) => {
                    bytes_written += buf.len() as u64;
                    progress.lock().unwrap().bytes_written = bytes_written;
                }
                Err(e) if e.raw_os_error() == Some(28) => break, // ENOSPC — disk full, expected
                Err(e) => {
                    let _ = std::fs::remove_file(&fill_path);
                    return Err(e.into());
                }
            }
        }
        // Flush dirty page cache to disk before unlinking so the zeros actually
        // reach the block device (close() alone does not guarantee writeback).
        let _ = file.sync_all();
    }

    std::fs::remove_file(&fill_path).context("failed to remove fill file")?;
    Ok(())
}

pub fn run_wipe(drive_root: PathBuf, progress: SharedWipeProgress) {
    {
        let mut p = progress.lock().unwrap();
        *p = WipeProgress::default();
    }
    std::thread::spawn(move || {
        match do_wipe(&drive_root, &progress) {
            Ok(()) => {
                let mut p = progress.lock().unwrap();
                p.finished = true;
                if !p.cancelled {
                    p.bytes_written = p.total_bytes;
                }
            }
            Err(e) => {
                let mut p = progress.lock().unwrap();
                p.error = Some(e.to_string());
                p.finished = true;
            }
        }
    });
}
