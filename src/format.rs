use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

// ── Drive probe (temporary mount to read contents before formatting) ──────────

#[derive(Debug, Clone, Default)]
pub struct DriveInfo {
    pub lsblk_text: String,
    pub df_text: Option<String>,
    pub ls_text: Option<String>,
    pub note: Option<String>,
    pub finished: bool,
}

pub type SharedDriveInfo = Arc<Mutex<DriveInfo>>;

pub fn probe_drive(device: String, fstype: Option<String>) -> SharedDriveInfo {
    let shared = Arc::new(Mutex::new(DriveInfo::default()));
    let ret = Arc::clone(&shared);

    std::thread::spawn(move || {
        *shared.lock().unwrap() = do_probe(&device, fstype.as_deref());
    });

    ret
}

fn do_probe(device: &str, fstype: Option<&str>) -> DriveInfo {
    let mut info = DriveInfo::default();

    // Always collect lsblk output — useful regardless of filesystem
    if let Ok(out) = Command::new("lsblk")
        .args(["-o", "NAME,SIZE,FSTYPE,LABEL,HOTPLUG,TYPE", device])
        .stdin(Stdio::null())
        .output()
    {
        info.lsblk_text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    }

    match fstype {
        Some("crypto_LUKS") => {
            info.note = Some(
                "Encrypted (LUKS) — contents cannot be read without the passphrase.".to_owned(),
            );
            info.finished = true;
            return info;
        }
        None | Some("") => {
            info.note = Some("No filesystem detected on this device.".to_owned());
            info.finished = true;
            return info;
        }
        _ => {}
    }

    // Temporarily mount to gather df and ls output, then unmount.
    match crate::drives::mount_device(device) {
        Err(e) => {
            info.note = Some(format!("Could not mount for preview: {e}"));
        }
        Ok(mp) => {
            let mp_str = mp.to_string_lossy();

            if let Ok(out) = Command::new("df")
                .args(["-h", &*mp_str])
                .stdin(Stdio::null())
                .output()
            {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                if !text.trim().is_empty() {
                    info.df_text = Some(text);
                }
            }

            if let Ok(out) = Command::new("ls")
                .args(["-lAh", &*mp_str])
                .stdin(Stdio::null())
                .output()
            {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                if !text.trim().is_empty() {
                    info.ls_text = Some(text);
                }
            }

            let _ = crate::drives::udisksctl_unmount(device);
        }
    }

    info.finished = true;
    info
}

#[derive(Debug, Clone)]
pub struct FormatProgress {
    pub step: usize,
    pub total_steps: usize,
    pub step_name: String,
    pub log: Vec<String>,
    pub finished: bool,
    pub error: Option<String>,
}

impl Default for FormatProgress {
    fn default() -> Self {
        FormatProgress {
            step: 0,
            total_steps: 0,
            step_name: String::new(),
            log: Vec::new(),
            finished: false,
            error: None,
        }
    }
}

pub type SharedFormatProgress = Arc<Mutex<FormatProgress>>;

fn log(progress: &SharedFormatProgress, msg: &str) {
    progress.lock().unwrap().log.push(msg.to_owned());
}

fn advance(progress: &SharedFormatProgress, name: &str) {
    let mut p = progress.lock().unwrap();
    p.step += 1;
    p.step_name = name.to_owned();
    p.log.push(format!(">>> {name}"));
}

fn append_output(progress: &SharedFormatProgress, stdout: &[u8], stderr: &[u8]) {
    let mut p = progress.lock().unwrap();
    for line in String::from_utf8_lossy(stdout).lines() {
        let line = line.trim();
        if !line.is_empty() {
            p.log.push(line.to_owned());
        }
    }
    for line in String::from_utf8_lossy(stderr).lines() {
        let line = line.trim();
        if !line.is_empty() {
            p.log.push(line.to_owned());
        }
    }
}

fn doas_run(args: &[&str], progress: &SharedFormatProgress) -> Result<()> {
    log(progress, &format!("$ doas {}", args.join(" ")));
    let out = Command::new("doas")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to spawn: doas {}", args.join(" ")))?;
    append_output(progress, &out.stdout, &out.stderr);
    if !out.status.success() {
        anyhow::bail!(
            "doas {} failed (exit {}): {}",
            args.join(" "),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn doas_with_passphrase(args: &[&str], passphrase: &str, progress: &SharedFormatProgress) -> Result<()> {
    // Don't log --key-file arg values; just show the command shape
    log(progress, &format!("$ doas {}", args.join(" ")));
    let mut child = Command::new("doas")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn: doas {}", args.join(" ")))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(passphrase.as_bytes());
    }
    let out = child.wait_with_output().context("wait_with_output")?;
    append_output(progress, &out.stdout, &out.stderr);
    if !out.status.success() {
        anyhow::bail!(
            "doas {} failed (exit {}): {}",
            args.join(" "),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Pre-flight safety checks run before any destructive command.
fn validate_device(device: &str, is_disk: bool, progress: &SharedFormatProgress) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;

    log(progress, "--- Pre-flight checks ---");

    // 1. Device file must exist and be a block device
    let meta = std::fs::metadata(device)
        .with_context(|| format!("Cannot access {device}: file not found or permission denied"))?;
    if !meta.file_type().is_block_device() {
        anyhow::bail!("{device} is not a block device");
    }
    log(progress, &format!("✓ {device} exists and is a block device"));

    // 2. Re-verify it's hotplug/removable via a fresh lsblk call
    let lsblk_out = Command::new("lsblk")
        .args(["-J", "-o", "HOTPLUG,RM,TYPE", device])
        .stdin(Stdio::null())
        .output()
        .context("lsblk re-check failed")?;
    let lsblk_json: serde_json::Value = serde_json::from_slice(&lsblk_out.stdout)
        .context("lsblk output could not be parsed")?;
    let dev_info = &lsblk_json["blockdevices"][0];
    let hotplug = dev_info["hotplug"].as_bool().unwrap_or(false)
        || dev_info["hotplug"].as_str() == Some("1")
        || dev_info["rm"].as_bool().unwrap_or(false)
        || dev_info["rm"].as_str() == Some("1");
    if !hotplug {
        anyhow::bail!(
            "SAFETY ABORT: {device} is not a hotplug/removable device. \
             Refusing to format a drive that may be an internal system disk."
        );
    }
    log(progress, &format!("✓ {device} is hotplug/removable"));

    // 3. Check /proc/mounts — neither the device itself nor its partitions may be mounted
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    let is_mounted = |dev: &str| -> bool {
        let canonical = std::fs::canonicalize(dev).ok();
        mounts.lines().any(|line| {
            let mount_dev = line.split_whitespace().next().unwrap_or("");
            if mount_dev == dev { return true; }
            if let Some(ref c) = canonical {
                if let Ok(mc) = std::fs::canonicalize(mount_dev) {
                    return &mc == c;
                }
            }
            false
        })
    };

    if is_mounted(device) {
        anyhow::bail!(
            "SAFETY ABORT: {device} is currently mounted. \
             Unmount or eject it before formatting."
        );
    }
    if is_disk {
        let partition = crate::drives::partition_path(device);
        if Path::new(&partition).exists() && is_mounted(&partition) {
            anyhow::bail!(
                "SAFETY ABORT: partition {partition} is currently mounted. \
                 Unmount or eject it before formatting."
            );
        }
    }
    log(progress, &format!("✓ {device} is not mounted"));

    // 4. Mapper name must not already exist (leftover from a failed previous run)
    let mapper_path = "/dev/mapper/backer-upper-format";
    if Path::new(mapper_path).exists() {
        anyhow::bail!(
            "SAFETY ABORT: {mapper_path} already exists, likely left open by a previous failed \
             format attempt. Close it first:\n  doas cryptsetup luksClose backer-upper-format"
        );
    }
    log(progress, "✓ Mapper slot is free");
    log(progress, "--- All checks passed, starting format ---");

    Ok(())
}

fn do_format(
    device: &str,
    is_disk: bool,
    label: &str,
    passphrase: &str,
    progress: &SharedFormatProgress,
) -> Result<()> {
    let mapper = "backer-upper-format";

    // Run safety checks before touching anything
    validate_device(device, is_disk, progress)?;

    let partition: String = if is_disk {
        advance(progress, "Wiping drive");
        doas_run(&["wipefs", "-a", device], progress)?;

        advance(progress, "Creating partition");
        doas_run(
            &["parted", "-s", device, "mklabel", "gpt", "mkpart", "primary", "0%", "100%"],
            progress,
        )?;

        // Wait for the partition device node to appear — poll with udevadm settle
        let part = crate::drives::partition_path(device);
        log(progress, &format!("Waiting for {part} to appear…"));
        let mut appeared = false;
        for _ in 0..20 {
            let _ = Command::new("doas")
                .args(["udevadm", "settle"])
                .stdin(Stdio::null())
                .status();
            if Path::new(&part).exists() {
                appeared = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        if !appeared {
            anyhow::bail!(
                "Partition device {part} did not appear after partitioning. \
                 Try replugging the drive and running again."
            );
        }
        log(progress, &format!("✓ {part} is ready"));
        part
    } else {
        advance(progress, "Wiping partition");
        doas_run(&["wipefs", "-a", device], progress)?;
        device.to_owned()
    };

    advance(progress, "Encrypting with LUKS");
    doas_with_passphrase(
        &[
            "cryptsetup", "luksFormat",
            "--type", "luks2",
            "--batch-mode",
            "--key-file", "-",
            &partition,
        ],
        passphrase,
        progress,
    )?;

    advance(progress, "Formatting filesystem (btrfs)");
    doas_with_passphrase(
        &["cryptsetup", "luksOpen", "--key-file", "-", &partition, mapper],
        passphrase,
        progress,
    )?;

    // Verify mapper appeared before running mkfs
    let mapper_dev = format!("/dev/mapper/{mapper}");
    if !Path::new(&mapper_dev).exists() {
        anyhow::bail!(
            "Mapper device {mapper_dev} did not appear after luksOpen. \
             Cannot continue with mkfs.btrfs."
        );
    }
    log(progress, &format!("✓ {mapper_dev} is ready"));

    let mkfs_result = Command::new("doas")
        .args(["mkfs.btrfs", "-L", label, &mapper_dev])
        .stdin(Stdio::null())
        .output();

    // Always close the mapper, even if mkfs failed
    log(progress, &format!("$ doas cryptsetup luksClose {mapper}"));
    let close_result = Command::new("doas")
        .args(["cryptsetup", "luksClose", mapper])
        .stdin(Stdio::null())
        .output();

    // Now check mkfs result
    let mkfs_out = mkfs_result.context("failed to run mkfs.btrfs")?;
    append_output(progress, &mkfs_out.stdout, &mkfs_out.stderr);
    if !mkfs_out.status.success() {
        anyhow::bail!(
            "mkfs.btrfs failed (exit {}): {}",
            mkfs_out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&mkfs_out.stderr).trim()
        );
    }

    if let Ok(out) = close_result {
        append_output(progress, &out.stdout, &out.stderr);
    }

    Ok(())
}

pub fn run_format(
    device: String,
    is_disk: bool,
    label: String,
    passphrase: String,
    progress: SharedFormatProgress,
) -> std::thread::JoinHandle<()> {
    // +1 for the pre-flight validation step shown in the UI
    let total = if is_disk { 4 } else { 3 };
    {
        let mut p = progress.lock().unwrap();
        *p = FormatProgress {
            total_steps: total,
            ..Default::default()
        };
    }

    std::thread::spawn(move || {
        match do_format(&device, is_disk, &label, &passphrase, &progress) {
            Ok(()) => {
                let mut p = progress.lock().unwrap();
                p.finished = true;
                p.step = total;
                p.step_name = "Done".to_owned();
                p.log.push(String::new());
                p.log.push(format!(
                    "✓ Drive '{label}' is ready. Unplug and replug it, then select it to start backing up."
                ));
            }
            Err(e) => {
                let mut p = progress.lock().unwrap();
                p.error = Some(e.to_string());
                p.finished = true;
                p.log.push(String::new());
                p.log.push(format!("✗ Format failed: {e}"));
            }
        }
    })
}
