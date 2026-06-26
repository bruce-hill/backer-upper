use anyhow::{Context, Result};
use serde::Serialize;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default, Serialize)]
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

#[derive(Debug, Clone, Default, Serialize)]
pub struct FormatProgress {
    pub step: usize,
    pub total_steps: usize,
    pub step_name: String,
    pub log: Vec<String>,
    pub finished: bool,
    pub error: Option<String>,
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

fn doas_with_passphrase(
    args: &[&str],
    passphrase: &str,
    progress: &SharedFormatProgress,
) -> Result<()> {
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

fn validate_device(device: &str, is_disk: bool, progress: &SharedFormatProgress) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;

    log(progress, "--- Pre-flight checks ---");

    let meta = std::fs::metadata(device)
        .with_context(|| format!("Cannot access {device}: file not found or permission denied"))?;
    if !meta.file_type().is_block_device() {
        anyhow::bail!("{device} is not a block device");
    }
    log(progress, &format!("✓ {device} exists and is a block device"));

    let lsblk_out = Command::new("lsblk")
        .args(["-J", "-o", "HOTPLUG,RM,TYPE", device])
        .stdin(Stdio::null())
        .output()
        .context("lsblk re-check failed")?;
    let lsblk_json: serde_json::Value =
        serde_json::from_slice(&lsblk_out.stdout).context("lsblk output could not be parsed")?;
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

    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    let is_mounted = |dev: &str| -> bool {
        let canonical = std::fs::canonicalize(dev).ok();
        mounts.lines().any(|line| {
            let mount_dev = line.split_whitespace().next().unwrap_or("");
            if mount_dev == dev {
                return true;
            }
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
            "SAFETY ABORT: {device} is currently mounted. Unmount or eject it before formatting."
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
    fstype: &str,
    encrypt: bool,
    passphrase: &str,
    progress: &SharedFormatProgress,
) -> Result<()> {
    let mapper = "backer-upper-format";
    validate_device(device, is_disk, progress)?;

    let partition: String = if is_disk {
        advance(progress, "Wiping drive");
        doas_run(&["wipefs", "-a", device], progress)?;
        advance(progress, "Creating partition");
        doas_run(
            &["parted", "-s", device, "mklabel", "gpt", "mkpart", "primary", "0%", "100%"],
            progress,
        )?;
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

    let mkfs_cmd = format!("mkfs.{fstype}");
    let format_step = format!("Formatting filesystem ({fstype})");

    if encrypt {
        advance(progress, "Encrypting with LUKS");
        doas_with_passphrase(
            &[
                "cryptsetup",
                "luksFormat",
                "--type",
                "luks2",
                "--batch-mode",
                "--key-file",
                "-",
                &partition,
            ],
            passphrase,
            progress,
        )?;

        advance(progress, &format_step);
        doas_with_passphrase(
            &["cryptsetup", "luksOpen", "--key-file", "-", &partition, mapper],
            passphrase,
            progress,
        )?;

        let mapper_dev = format!("/dev/mapper/{mapper}");
        if !Path::new(&mapper_dev).exists() {
            anyhow::bail!(
                "Mapper device {mapper_dev} did not appear after luksOpen. Cannot continue with {mkfs_cmd}."
            );
        }
        log(progress, &format!("✓ {mapper_dev} is ready"));

        let mkfs_result = Command::new("doas")
            .args([mkfs_cmd.as_str(), "-L", label, &mapper_dev])
            .stdin(Stdio::null())
            .output();

        log(progress, &format!("$ doas cryptsetup luksClose {mapper}"));
        let close_result = Command::new("doas")
            .args(["cryptsetup", "luksClose", mapper])
            .stdin(Stdio::null())
            .output();

        let mkfs_out = mkfs_result.with_context(|| format!("failed to run {mkfs_cmd}"))?;
        append_output(progress, &mkfs_out.stdout, &mkfs_out.stderr);
        if !mkfs_out.status.success() {
            anyhow::bail!(
                "{mkfs_cmd} failed (exit {}): {}",
                mkfs_out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&mkfs_out.stderr).trim()
            );
        }
        if let Ok(out) = close_result {
            append_output(progress, &out.stdout, &out.stderr);
        }
    } else {
        advance(progress, &format_step);
        doas_run(&[mkfs_cmd.as_str(), "-L", label, &partition], progress)?;
    }

    Ok(())
}

pub fn run_format(
    device: String,
    is_disk: bool,
    label: String,
    fstype: String,
    encrypt: bool,
    passphrase: String,
    progress: SharedFormatProgress,
) -> std::thread::JoinHandle<()> {
    let total = match (is_disk, encrypt) {
        (true, true)  => 4,
        (true, false) => 3,
        (false, true) => 3,
        (false, false) => 2,
    };
    {
        let mut p = progress.lock().unwrap();
        *p = FormatProgress {
            total_steps: total,
            ..Default::default()
        };
    }
    std::thread::spawn(move || {
        match do_format(&device, is_disk, &label, &fstype, encrypt, &passphrase, &progress) {
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
