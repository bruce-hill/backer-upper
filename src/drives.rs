use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct Drive {
    pub device: String,       // e.g. /dev/sdb1
    pub label: Option<String>,
    pub uuid: Option<String>,
    pub fstype: Option<String>,
    pub size: Option<String>,
    pub mountpoint: Option<String>,
    pub is_encrypted: bool,   // LUKS
    pub is_removable: bool,
}

impl Drive {
    pub fn is_mounted(&self) -> bool {
        self.mountpoint.is_some()
    }

    pub fn display_name(&self) -> String {
        if let Some(label) = &self.label {
            if !label.is_empty() {
                return label.clone();
            }
        }
        self.device.clone()
    }
}

pub fn list_removable_drives() -> Result<Vec<Drive>> {
    let output = Command::new("lsblk")
        .args([
            "-J", "-o",
            "NAME,PATH,LABEL,UUID,FSTYPE,SIZE,MOUNTPOINT,HOTPLUG,TYPE",
            "--bytes",
        ])
        .output()
        .context("failed to run lsblk")?;

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("failed to parse lsblk JSON")?;

    let mut drives = Vec::new();
    if let Some(devices) = json["blockdevices"].as_array() {
        collect_drives(devices, &mut drives);
    }
    Ok(drives)
}

fn collect_drives(devices: &[serde_json::Value], out: &mut Vec<Drive>) {
    for dev in devices {
        let hotplug = dev["hotplug"].as_bool().unwrap_or(false);
        let dev_type = dev["type"].as_str().unwrap_or("");

        // recurse into partitions/children
        if let Some(children) = dev["children"].as_array() {
            collect_drives(children, out);
        }

        // We want removable partitions or LUKS containers
        if !hotplug && dev_type != "crypt" {
            continue;
        }

        let fstype = dev["fstype"].as_str().map(str::to_owned);
        let mountpoint = dev["mountpoint"].as_str().map(str::to_owned);

        let is_encrypted = fstype.as_deref() == Some("crypto_LUKS")
            || dev_type == "crypt";

        let drive = Drive {
            device: dev["path"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            label: dev["label"].as_str().map(str::to_owned),
            uuid: dev["uuid"].as_str().map(str::to_owned),
            fstype,
            size: dev["size"].as_str().map(str::to_owned),
            mountpoint,
            is_encrypted,
            is_removable: hotplug,
        };

        if !drive.device.is_empty() {
            out.push(drive);
        }
    }
}

/// Unlock a LUKS device. Returns the mapper path (e.g. /dev/mapper/backer-uuid).
pub fn unlock_luks(device: &str, password: &str, mapper_name: &str) -> Result<String> {
    let mut child = Command::new("pkexec")
        .args(["cryptsetup", "open", device, mapper_name, "--key-file", "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn cryptsetup")?;

    use std::io::Write;
    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        stdin.write_all(password.as_bytes())?;
    }

    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("cryptsetup failed — wrong password or device error");
    }
    Ok(format!("/dev/mapper/{mapper_name}"))
}

/// Mount a filesystem at mount_point (creates it if needed).
pub fn mount_drive(device: &str, mount_point: &Path) -> Result<()> {
    std::fs::create_dir_all(mount_point)?;
    let status = Command::new("pkexec")
        .args([
            "mount",
            device,
            mount_point.to_str().unwrap(),
        ])
        .status()
        .context("failed to spawn mount")?;

    if !status.success() {
        anyhow::bail!("mount failed");
    }
    Ok(())
}

/// Unmount and close LUKS.
pub fn unmount_and_close(mount_point: &Path, mapper_name: &str) -> Result<()> {
    let _ = Command::new("pkexec")
        .args(["umount", mount_point.to_str().unwrap()])
        .status();

    let _ = Command::new("pkexec")
        .args(["cryptsetup", "close", mapper_name])
        .status();

    Ok(())
}

pub fn default_mount_point(uuid: &str) -> PathBuf {
    PathBuf::from(format!("/run/media/backer-upper/{uuid}"))
}
