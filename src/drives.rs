use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

#[derive(Debug, Clone)]
pub struct Drive {
    pub device: String,
    pub label: Option<String>,
    #[allow(dead_code)]
    pub uuid: Option<String>,
    pub fstype: Option<String>,
    pub size: Option<String>,
    pub mountpoint: Option<String>,
    pub is_encrypted: bool,
    pub model: Option<String>,
    pub vendor: Option<String>,
    pub tran: Option<String>,
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
            "-J", "-o",
            "NAME,PATH,LABEL,UUID,FSTYPE,SIZE,MOUNTPOINT,HOTPLUG,TYPE,VENDOR,MODEL,TRAN",
        ])
        .output()
        .context("failed to run lsblk")?;

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("failed to parse lsblk JSON")?;

    let mut drives = Vec::new();
    if let Some(devices) = json["blockdevices"].as_array() {
        for dev in devices {
            collect_drives(dev, false, &DiskMeta::default(), &mut drives);
        }
    }
    Ok(drives)
}

fn str_field(dev: &serde_json::Value, key: &str) -> Option<String> {
    dev[key].as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

fn is_hotplug(dev: &serde_json::Value) -> bool {
    dev["hotplug"].as_bool().unwrap_or(false)
        || dev["hotplug"].as_str() == Some("1")
}

fn collect_drives(
    dev: &serde_json::Value,
    parent_hotplug: bool,
    parent_meta: &DiskMeta,
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

    let children = dev["children"].as_array();
    let has_children = children.map(|c| !c.is_empty()).unwrap_or(false);

    if let Some(kids) = children {
        for child in kids {
            collect_drives(child, hotplug, &meta, out);
        }
    }

    if !hotplug { return; }
    if dev_type == "disk" && has_children { return; }
    if dev_type == "crypt" && !parent_hotplug { return; }
    // A LUKS partition that has already been unlocked has a crypt child.
    // Skip the partition — the crypt child is already in `out` and is what the user wants.
    if dev_type == "part" && children.map_or(false, |kids| {
        kids.iter().any(|k| k["type"].as_str() == Some("crypt"))
    }) {
        return;
    }

    let fstype = str_field(dev, "fstype");
    let is_encrypted = fstype.as_deref() == Some("crypto_LUKS") || dev_type == "crypt";

    let device = dev["path"].as_str().unwrap_or_default().to_owned();
    if device.is_empty() { return; }

    out.push(Drive {
        device,
        label: str_field(dev, "label"),
        uuid: str_field(dev, "uuid"),
        size: str_field(dev, "size"),
        mountpoint: str_field(dev, "mountpoint"),
        fstype,
        is_encrypted,
        model: meta.model,
        vendor: meta.vendor,
        tran: meta.tran,
    });
}

// ── udisks2 D-Bus mount/unlock (no root, no console prompts) ────────────────

/// Convert a /dev/XYZ path to the udisks2 D-Bus object path.
/// udisks2 encodes the name by replacing non-[A-Za-z0-9_] chars with '_'.
fn udisks2_obj_path(device: &str) -> String {
    let name = device.strip_prefix("/dev/").unwrap_or(device);
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

/// Mount a plain (non-encrypted) removable device via udisks2.
/// Returns the mount point udisks2 chose.
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

/// Unmount a filesystem using its udisks2 D-Bus object path.
pub fn unmount_filesystem(obj_path: &str) -> Result<()> {
    let conn = udisks2_conn()?;
    let proxy = udisks2_proxy(&conn, obj_path, "org.freedesktop.UDisks2.Filesystem")?;
    let opts: HashMap<String, OwnedValue> = HashMap::new();
    proxy.call("Unmount", &(opts,)).context("Unmount failed")
}

/// Unmount a plain device by its /dev/… path.
pub fn unmount_device(device: &str) -> Result<()> {
    unmount_filesystem(&udisks2_obj_path(device))
}

/// Lock a LUKS device by its /dev/… path.
pub fn lock_luks(device: &str) -> Result<()> {
    let obj = udisks2_obj_path(device);
    let conn = udisks2_conn()?;
    let proxy = udisks2_proxy(&conn, &obj, "org.freedesktop.UDisks2.Encrypted")?;
    let opts: HashMap<String, OwnedValue> = HashMap::new();
    proxy.call("Lock", &(opts,)).context("Lock failed")
}

/// Unlock a LUKS device with the supplied passphrase (passed directly via D-Bus,
/// no terminal interaction) and mount the cleartext device.
/// Returns (cleartext_dbus_object_path, mount_point).
pub fn unlock_and_mount(device: &str, passphrase: &str) -> Result<(String, PathBuf)> {
    let obj = udisks2_obj_path(device);
    let conn = udisks2_conn()?;

    // Call Encrypted.Unlock(passphrase, options) → cleartext object path
    let enc = udisks2_proxy(&conn, &obj, "org.freedesktop.UDisks2.Encrypted")?;
    let opts: HashMap<String, OwnedValue> = HashMap::new();
    let cleartext: OwnedObjectPath = enc
        .call("Unlock", &(passphrase, opts))
        .map_err(|e| {
            let s = e.to_string();
            if s.contains("No key available")
                || s.contains("Failed to activate")
                || s.contains("Operation not permitted")
            {
                anyhow::anyhow!("Wrong passphrase")
            } else {
                anyhow::anyhow!("Unlock failed: {s}")
            }
        })?;

    // Call Filesystem.Mount(options) on the cleartext device → mount path
    let fs = udisks2_proxy(&conn, cleartext.as_str(), "org.freedesktop.UDisks2.Filesystem")?;
    let mount_opts: HashMap<String, OwnedValue> = HashMap::new();
    let mount_path: String = fs
        .call("Mount", &(mount_opts,))
        .context("udisks2 Mount of cleartext device failed")?;

    Ok((cleartext.to_string(), PathBuf::from(mount_path)))
}

/// Unmount a filesystem and (if LUKS) lock the encrypted device.
/// `cleartext_obj` is the D-Bus object path returned by `unlock_and_mount`,
/// or empty if the device was not encrypted.
#[allow(dead_code)]
pub fn unmount_and_close(mount_point: &Path, cleartext_obj: &str, luks_device: &str) -> Result<()> {
    if let Ok(conn) = udisks2_conn() {
        if !cleartext_obj.is_empty() {
            // Unmount the cleartext device
            if let Ok(fs) = udisks2_proxy(&conn, cleartext_obj, "org.freedesktop.UDisks2.Filesystem") {
                let opts: HashMap<String, OwnedValue> = HashMap::new();
                let _: zbus::Result<()> = fs.call("Unmount", &(opts,));
            }
            // Lock the LUKS container
            let luks_obj = udisks2_obj_path(luks_device);
            if let Ok(enc) = udisks2_proxy(&conn, &luks_obj, "org.freedesktop.UDisks2.Encrypted") {
                let opts: HashMap<String, OwnedValue> = HashMap::new();
                let _: zbus::Result<()> = enc.call("Lock", &(opts,));
            }
        } else {
            let obj = udisks2_obj_path(mount_point.to_str().unwrap_or(""));
            if let Ok(fs) = udisks2_proxy(&conn, &obj, "org.freedesktop.UDisks2.Filesystem") {
                let opts: HashMap<String, OwnedValue> = HashMap::new();
                let _: zbus::Result<()> = fs.call("Unmount", &(opts,));
            }
        }
    }
    Ok(())
}
