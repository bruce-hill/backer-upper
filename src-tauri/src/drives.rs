use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

#[derive(Debug, Clone, Serialize)]
pub struct Drive {
    pub device: String,
    pub label: Option<String>,
    pub uuid: Option<String>,
    pub fstype: Option<String>,
    pub size: Option<String>,
    pub mountpoint: Option<String>,
    pub is_encrypted: bool,
    pub luks_parent: Option<String>,
    pub model: Option<String>,
    pub vendor: Option<String>,
    pub tran: Option<String>,
    pub dev_type: String,
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
        let hw = hardware_name(self.vendor.as_deref(), self.model.as_deref());
        if !hw.is_empty() {
            return hw;
        }
        self.device.clone()
    }
}

fn hardware_name(vendor: Option<&str>, model: Option<&str>) -> String {
    let v = vendor.unwrap_or("").trim();
    let m = model.unwrap_or("").trim();
    match (v.is_empty(), m.is_empty()) {
        (false, false) => format!("{v} {m}"),
        (false, true) => v.to_owned(),
        (true, false) => m.to_owned(),
        (true, true) => String::new(),
    }
}

#[derive(Default, Clone)]
struct DiskMeta {
    model: Option<String>,
    vendor: Option<String>,
    tran: Option<String>,
}

pub fn list_removable_drives() -> Result<Vec<Drive>> {
    let output = Command::new("lsblk")
        .args([
            "-J",
            "-o",
            "NAME,PATH,LABEL,UUID,FSTYPE,SIZE,MOUNTPOINT,HOTPLUG,TYPE,VENDOR,MODEL,TRAN",
        ])
        .output()
        .context("failed to run lsblk")?;

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("failed to parse lsblk JSON")?;

    let mut drives = Vec::new();
    if let Some(devices) = json["blockdevices"].as_array() {
        for dev in devices {
            collect_drives(dev, false, &DiskMeta::default(), None, &mut drives);
        }
    }
    Ok(drives)
}

fn str_field(dev: &serde_json::Value, key: &str) -> Option<String> {
    dev[key].as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

fn is_hotplug(dev: &serde_json::Value) -> bool {
    dev["hotplug"].as_bool().unwrap_or(false) || dev["hotplug"].as_str() == Some("1")
}

fn collect_drives(
    dev: &serde_json::Value,
    parent_hotplug: bool,
    parent_meta: &DiskMeta,
    parent_device: Option<&str>,
    out: &mut Vec<Drive>,
) {
    let hotplug = is_hotplug(dev) || parent_hotplug;
    let dev_type = dev["type"].as_str().unwrap_or("");

    let meta = if dev_type == "disk" {
        DiskMeta {
            model: str_field(dev, "model"),
            vendor: str_field(dev, "vendor"),
            tran: str_field(dev, "tran"),
        }
    } else {
        parent_meta.clone()
    };

    let dev_path = dev["path"].as_str().unwrap_or_default();
    let children = dev["children"].as_array();
    let has_children = children.map(|c| !c.is_empty()).unwrap_or(false);

    if let Some(kids) = children {
        for child in kids {
            collect_drives(child, hotplug, &meta, Some(dev_path), out);
        }
    }

    if !hotplug {
        return;
    }
    if dev_type == "disk" && has_children {
        return;
    }
    if dev_type == "crypt" && !parent_hotplug {
        return;
    }
    if dev_type == "part"
        && children.map_or(false, |kids| {
            kids.iter().any(|k| k["type"].as_str() == Some("crypt"))
        })
    {
        return;
    }

    let fstype = str_field(dev, "fstype");
    let is_encrypted = fstype.as_deref() == Some("crypto_LUKS") || dev_type == "crypt";
    let luks_parent = if dev_type == "crypt" {
        parent_device.map(str::to_owned)
    } else {
        None
    };

    let device = dev_path.to_owned();
    if device.is_empty() {
        return;
    }

    out.push(Drive {
        device,
        label: str_field(dev, "label"),
        uuid: str_field(dev, "uuid"),
        size: str_field(dev, "size"),
        mountpoint: str_field(dev, "mountpoint"),
        fstype,
        is_encrypted,
        luks_parent,
        model: meta.model,
        vendor: meta.vendor,
        tran: meta.tran,
        dev_type: dev_type.to_owned(),
    });
}

pub fn udisks2_obj_path(device: &str) -> String {
    let resolved = std::fs::canonicalize(device)
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .unwrap_or_else(|| device.to_owned());
    let name = resolved.strip_prefix("/dev/").unwrap_or(&resolved);
    let encoded: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    format!("/org/freedesktop/UDisks2/block_devices/{encoded}")
}

fn udisks2_conn() -> Result<Connection> {
    Connection::system().context("failed to connect to D-Bus system bus")
}

fn udisks2_proxy<'a>(
    conn: &'a Connection,
    obj_path: &'a str,
    interface: &'a str,
) -> Result<zbus::blocking::Proxy<'a>> {
    zbus::blocking::Proxy::new(conn, "org.freedesktop.UDisks2", obj_path, interface)
        .with_context(|| format!("failed to create udisks2 proxy for {obj_path}"))
}

pub fn mount_device(device: &str) -> Result<PathBuf> {
    let obj = udisks2_obj_path(device);
    let conn = udisks2_conn()?;
    let proxy = udisks2_proxy(&conn, &obj, "org.freedesktop.UDisks2.Filesystem")?;
    let opts: HashMap<String, OwnedValue> = HashMap::new();
    let mount_path: String = proxy
        .call("Mount", &(opts,))
        .with_context(|| format!("udisks2 Mount failed for {device}"))?;
    Ok(PathBuf::from(mount_path))
}

pub fn doas_mount(device: &str) -> Result<PathBuf> {
    let label = Command::new("lsblk")
        .args(["-n", "-o", "LABEL", device])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "backup".to_owned());
    let user = std::env::var("USER").unwrap_or_else(|_| "user".to_owned());
    let mp = PathBuf::from(format!("/run/media/{user}/{label}"));

    let mkdir = Command::new("doas")
        .args(["mkdir", "-p", &mp.to_string_lossy()])
        .status()
        .context("doas mkdir failed")?;
    if !mkdir.success() {
        anyhow::bail!("could not create mountpoint {}", mp.display());
    }

    let out = Command::new("doas")
        .args(["mount", device, &mp.to_string_lossy()])
        .output()
        .context("doas mount failed")?;
    if !out.status.success() {
        let _ = std::fs::remove_dir(&mp);
        anyhow::bail!("mount: {}", String::from_utf8_lossy(&out.stderr).trim());
    }

    Ok(mp)
}

pub fn doas_unmount(mountpoint: &PathBuf) -> Result<()> {
    let out = Command::new("doas")
        .args(["umount", &mountpoint.to_string_lossy()])
        .output()
        .context("doas umount failed")?;
    if !out.status.success() {
        anyhow::bail!("umount: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let _ = std::fs::remove_dir(mountpoint);
    Ok(())
}

pub fn udisksctl_unmount(device: &str) -> Result<()> {
    let out = Command::new("udisksctl")
        .args(["unmount", "--no-user-interaction", "-b", device])
        .output()
        .context("failed to run udisksctl unmount")?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("udisksctl unmount: {}", msg.trim());
    }
    Ok(())
}

pub fn udisksctl_lock(device: &str) -> Result<()> {
    let out = Command::new("udisksctl")
        .args(["lock", "--no-user-interaction", "-b", device])
        .output()
        .context("failed to run udisksctl lock")?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("udisksctl lock: {}", msg.trim());
    }
    Ok(())
}

pub fn udisksctl_power_off(device: &str) -> Result<()> {
    let out = Command::new("udisksctl")
        .args(["power-off", "--no-user-interaction", "-b", device])
        .output()
        .context("failed to run udisksctl power-off")?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("udisksctl power-off: {}", msg.trim());
    }
    Ok(())
}

const LUKS_MAPPER: &str = "backer-upper";

pub fn unlock_and_mount(device: &str, passphrase: &str) -> Result<(String, PathBuf)> {
    let cleartext_dev = format!("/dev/mapper/{LUKS_MAPPER}");

    if std::path::Path::new(&cleartext_dev).exists() {
        anyhow::bail!(
            "{cleartext_dev} already exists — a previous session may not have closed cleanly.\n\
             Run: doas cryptsetup luksClose {LUKS_MAPPER}"
        );
    }

    let mut child = Command::new("doas")
        .args(["cryptsetup", "luksOpen", "--key-file", "-", device, LUKS_MAPPER])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn: doas cryptsetup luksOpen")?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(passphrase.as_bytes());
    }
    let out = child.wait_with_output().context("wait_with_output")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("No key available") || stderr.contains("Failed to activate") {
            anyhow::bail!("Wrong passphrase");
        }
        anyhow::bail!("luksOpen failed: {}", stderr.trim());
    }

    // Wait for the device-mapper node to appear.
    let _ = Command::new("udevadm").args(["settle"]).status();
    for _ in 0..20 {
        if std::path::Path::new(&cleartext_dev).exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let mp = doas_mount(&cleartext_dev).map_err(|e| {
        let _ = doas_luks_close();
        e
    })?;

    Ok((cleartext_dev, mp))
}

pub fn doas_luks_close() -> Result<()> {
    let out = Command::new("doas")
        .args(["cryptsetup", "luksClose", LUKS_MAPPER])
        .output()
        .context("failed to spawn: doas cryptsetup luksClose")?;
    if !out.status.success() {
        anyhow::bail!(
            "luksClose failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub fn partition_path(disk: &str) -> String {
    if disk.chars().last().map_or(false, |c| c.is_ascii_digit()) {
        format!("{disk}p1")
    } else {
        format!("{disk}1")
    }
}
