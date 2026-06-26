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

fn get_mountpoint(device: &str) -> Option<std::path::PathBuf> {
    let out = Command::new("lsblk")
        .args(["-n", "-o", "MOUNTPOINT", device])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(std::path::PathBuf::from)
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

    let existing_mp = get_mountpoint(device);
    let (mp, we_mounted) = if let Some(mp) = existing_mp {
        (mp, false)
    } else {
        match crate::drives::mount_device(device) {
            Err(e) => {
                info.note = Some(format!("Could not mount for preview: {e}"));
                info.finished = true;
                return info;
            }
            Ok(mp) => (mp, true),
        }
    };

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
    if we_mounted {
        let _ = crate::drives::udisksctl_unmount(device);
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

fn device_is_mounted(dev: &str, mounts: &str) -> bool {
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
}

fn find_mounted_device(device: &str, is_disk: bool) -> Option<String> {
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    if device_is_mounted(device, &mounts) {
        return Some(device.to_owned());
    }
    if is_disk {
        let part = crate::drives::partition_path(device);
        if Path::new(&part).exists() && device_is_mounted(&part, &mounts) {
            return Some(part);
        }
    }
    None
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

    if device_is_mounted(device, &mounts) {
        anyhow::bail!(
            "SAFETY ABORT: {device} is currently mounted. Unmount or eject it before formatting."
        );
    }
    if is_disk {
        let partition = crate::drives::partition_path(device);
        if Path::new(&partition).exists() && device_is_mounted(&partition, &mounts) {
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

// A single step in the format sequence. Both the executor and the preview are
// derived from the same Vec<FormatStep>, so they cannot diverge.
// `preview` is the line shown to the user; None means an internal-only step.
struct FormatStep {
    preview: Option<String>,
    action: FormatAction,
}

enum FormatAction {
    Advance(String),
    Doas { args: Vec<String>, needs_passphrase: bool, is_cleanup: bool },
    WaitForDevice(String),
    CheckDevice(String),
}

fn doas_step(args: &[&str]) -> FormatStep {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    FormatStep { preview: Some(format!("doas {}", args.join(" "))), action: FormatAction::Doas { args, needs_passphrase: false, is_cleanup: false } }
}

fn doas_passphrase_step(args: &[&str]) -> FormatStep {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    FormatStep { preview: Some(format!("doas {}", args.join(" "))), action: FormatAction::Doas { args, needs_passphrase: true, is_cleanup: false } }
}

fn doas_cleanup_step(args: &[&str]) -> FormatStep {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    FormatStep { preview: Some(format!("doas {}", args.join(" "))), action: FormatAction::Doas { args, needs_passphrase: false, is_cleanup: true } }
}

fn build_format_steps(device: &str, is_disk: bool, label: &str, fstype: &str, encrypt: bool) -> Vec<FormatStep> {
    let no_preview = |action| FormatStep { preview: None, action };
    let mapper = "backer-upper-format";
    let mapper_dev = format!("/dev/mapper/{mapper}");
    let mkfs_cmd = format!("mkfs.{fstype}");
    let format_step_name = format!("Formatting filesystem ({fstype})");
    let mut steps: Vec<FormatStep> = Vec::new();

    let partition = if is_disk {
        let part = crate::drives::partition_path(device);
        steps.push(no_preview(FormatAction::Advance("Zeroing drive".into())));
        steps.push(doas_step(&["dd", "if=/dev/zero", &format!("of={device}"), "bs=4M"]));
        steps.push(no_preview(FormatAction::Advance("Creating partition".into())));
        steps.push(doas_step(&["parted", "-s", device, "mklabel", "gpt", "mkpart", "primary", "0%", "100%"]));
        steps.push(no_preview(FormatAction::WaitForDevice(part.clone())));
        part
    } else {
        steps.push(no_preview(FormatAction::Advance("Zeroing partition".into())));
        steps.push(doas_step(&["dd", "if=/dev/zero", &format!("of={device}"), "bs=4M"]));
        device.to_owned()
    };

    if encrypt {
        steps.push(no_preview(FormatAction::Advance("Encrypting with LUKS".into())));
        steps.push(doas_passphrase_step(&["cryptsetup", "luksFormat", "--type", "luks2", "--batch-mode", "--key-file", "-", &partition]));
        steps.push(no_preview(FormatAction::Advance(format_step_name)));
        steps.push(doas_passphrase_step(&["cryptsetup", "luksOpen", "--key-file", "-", &partition, mapper]));
        steps.push(no_preview(FormatAction::CheckDevice(mapper_dev.clone())));
        steps.push(doas_step(&[&mkfs_cmd, "-L", label, &mapper_dev]));
        steps.push(doas_cleanup_step(&["cryptsetup", "luksClose", mapper]));
    } else {
        steps.push(no_preview(FormatAction::Advance(format_step_name)));
        steps.push(doas_step(&[&mkfs_cmd, "-L", label, &partition]));
    }

    steps
}

pub fn format_command_preview(device: &str, is_disk: bool, label: &str, fstype: &str, encrypt: bool) -> Vec<String> {
    build_format_steps(device, is_disk, label, fstype, encrypt)
        .into_iter()
        .filter_map(|step| step.preview)
        .collect()
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
    if let Some(mounted_dev) = find_mounted_device(device, is_disk) {
        progress.lock().unwrap().total_steps += 1;
        advance(progress, "Unmounting drive");
        crate::drives::udisksctl_unmount(&mounted_dev)
            .with_context(|| format!("Failed to unmount {mounted_dev} before formatting"))?;
    }

    validate_device(device, is_disk, progress)?;

    let steps = build_format_steps(device, is_disk, label, fstype, encrypt);
    let mut pending_error: Option<anyhow::Error> = None;

    for step in steps {
        match step.action {
            FormatAction::Advance(name) => {
                if pending_error.is_none() {
                    advance(progress, &name);
                }
            }
            FormatAction::Doas { args, needs_passphrase, is_cleanup } => {
                if pending_error.is_some() && !is_cleanup {
                    continue;
                }
                let refs: Vec<&str> = args.iter().map(|a| a.as_str()).collect();
                let result = if needs_passphrase {
                    doas_with_passphrase(&refs, passphrase, progress)
                } else {
                    doas_run(&refs, progress)
                };
                if let Err(e) = result {
                    if pending_error.is_none() && !is_cleanup {
                        pending_error = Some(e);
                    }
                }
            }
            FormatAction::WaitForDevice(path) => {
                if pending_error.is_some() {
                    continue;
                }
                log(progress, &format!("Waiting for {path} to appear…"));
                let mut appeared = false;
                for _ in 0..20 {
                    let _ = Command::new("doas")
                        .args(["udevadm", "settle"])
                        .stdin(Stdio::null())
                        .status();
                    if Path::new(&path).exists() {
                        appeared = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                if appeared {
                    log(progress, &format!("✓ {path} is ready"));
                } else {
                    pending_error = Some(anyhow::anyhow!(
                        "Partition device {path} did not appear after partitioning. \
                         Try replugging the drive and running again."
                    ));
                }
            }
            FormatAction::CheckDevice(path) => {
                if pending_error.is_some() {
                    continue;
                }
                if Path::new(&path).exists() {
                    log(progress, &format!("✓ {path} is ready"));
                } else {
                    pending_error = Some(anyhow::anyhow!(
                        "Mapper device {path} did not appear after luksOpen. Cannot continue."
                    ));
                }
            }
        }
    }

    pending_error.map_or(Ok(()), Err)
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
    let total = build_format_steps(&device, is_disk, &label, &fstype, encrypt)
        .iter()
        .filter(|s| matches!(s.action, FormatAction::Advance(_)))
        .count();
    {
        let mut p = progress.lock().unwrap();
        *p = FormatProgress { total_steps: total, ..Default::default() };
    }
    std::thread::spawn(move || {
        match do_format(&device, is_disk, &label, &fstype, encrypt, &passphrase, &progress) {
            Ok(()) => {
                let mut p = progress.lock().unwrap();
                p.finished = true;
                p.step = p.total_steps;
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
