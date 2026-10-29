use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::backup::list_snapshot_names;

/// One place on the drive where the path to purge exists.
#[derive(Debug, Clone, Serialize)]
pub struct PurgeTarget {
    /// `None` for the live (most recent) copy at the drive root.
    pub snapshot: Option<String>,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PurgeResult {
    pub log_lines: Vec<String>,
    pub purged: usize,
    pub failed: usize,
}

/// Check that `rel` is a non-empty relative path made only of plain names.
pub fn validate_rel(rel: &Path) -> Result<(), String> {
    if rel.as_os_str().is_empty() {
        return Err("Path to purge must not be empty".to_owned());
    }
    for comp in rel.components() {
        if !matches!(comp, Component::Normal(_)) {
            return Err(format!(
                "Invalid path \"{}\": must use only plain names (no '..', '.', or leading '/')",
                rel.display()
            ));
        }
    }
    Ok(())
}

/// Whether `rel` exists under `base`. Errors if any parent component is a symlink: rsync
/// preserves symlinks, so a backed-up link could otherwise send `rm` outside the drive.
fn locate(base: &Path, rel: &Path) -> Result<Option<bool>, String> {
    let comps: Vec<Component> = rel.components().collect();
    let mut cur = base.to_path_buf();
    for (i, comp) in comps.iter().enumerate() {
        cur.push(comp);
        let meta = match std::fs::symlink_metadata(&cur) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", cur.display())),
        };
        if i + 1 == comps.len() {
            return Ok(Some(meta.is_dir()));
        }
        if meta.file_type().is_symlink() {
            return Err(format!(
                "Refusing to purge through symlink {}",
                cur.display()
            ));
        }
        if !meta.is_dir() {
            return Ok(None);
        }
    }
    Ok(None)
}

fn snapshot_dir(drive_root: &Path, snap: &str) -> PathBuf {
    drive_root.join("snapshots").join(snap)
}

/// Every location on the drive (live copy and each snapshot) that contains `rel`.
pub fn find_targets(drive_root: &Path, rel: &Path) -> Result<Vec<PurgeTarget>, String> {
    validate_rel(rel)?;
    let mut targets = Vec::new();
    if let Some(is_dir) = locate(drive_root, rel)? {
        targets.push(PurgeTarget {
            snapshot: None,
            is_dir,
        });
    }
    for snap in list_snapshot_names(drive_root) {
        if let Some(is_dir) = locate(&snapshot_dir(drive_root, &snap), rel)? {
            targets.push(PurgeTarget {
                snapshot: Some(snap),
                is_dir,
            });
        }
    }
    Ok(targets)
}

fn run_doas(args: &[&str]) -> Result<String, String> {
    let out = Command::new("doas")
        .args(args)
        .output()
        .map_err(|e| format!("failed to run doas: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        Err(if stderr.is_empty() {
            format!("exited {}", out.status.code().unwrap_or(-1))
        } else {
            stderr.to_owned()
        })
    }
}

fn remove(path: &Path) -> Result<(), String> {
    let p = path.to_str().ok_or("path is not valid UTF-8")?;
    run_doas(&["rm", "-rf", "--", p]).map(|_| ())
}

fn is_readonly_subvolume(snap: &str) -> Result<bool, String> {
    let out = run_doas(&["btrfs", "property", "get", "-ts", snap, "ro"])?;
    Ok(out.trim() == "ro=true")
}

fn set_readonly(snap: &str, ro: bool) -> Result<(), String> {
    let val = if ro { "true" } else { "false" };
    run_doas(&["btrfs", "property", "set", "-ts", snap, "ro", val]).map(|_| ())
}

/// Delete `rel` from a snapshot, temporarily lifting its read-only flag if it has one.
fn purge_from_snapshot(drive_root: &Path, snap: &str, rel: &Path) -> Result<(), String> {
    let dir = snapshot_dir(drive_root, snap);
    let dir_str = dir.to_str().ok_or("snapshot path is not valid UTF-8")?;
    let was_ro = is_readonly_subvolume(dir_str)?;
    if was_ro {
        set_readonly(dir_str, false).map_err(|e| format!("could not make writable: {e}"))?;
    }
    let removed = remove(&dir.join(rel));
    if was_ro {
        if let Err(e) = set_readonly(dir_str, true) {
            return Err(format!(
                "could not restore read-only flag ({e}){}",
                match removed {
                    Ok(()) => " — file was removed".to_owned(),
                    Err(re) => format!("; removal also failed: {re}"),
                }
            ));
        }
    }
    removed
}

/// Delete `rel` from the live copy and every snapshot. Locations are re-discovered here rather
/// than trusted from the caller.
pub fn purge(drive_root: &Path, rel: &Path) -> Result<PurgeResult, String> {
    let targets = find_targets(drive_root, rel)?;
    let mut result = PurgeResult {
        log_lines: Vec::new(),
        purged: 0,
        failed: 0,
    };
    if targets.is_empty() {
        result
            .log_lines
            .push(format!("{} was not found on the drive.", rel.display()));
        return Ok(result);
    }
    for t in &targets {
        let (label, outcome) = match &t.snapshot {
            None => ("current backup".to_owned(), remove(&drive_root.join(rel))),
            Some(snap) => (
                format!("snapshot {snap}"),
                purge_from_snapshot(drive_root, snap, rel),
            ),
        };
        match outcome {
            Ok(()) => {
                result.purged += 1;
                result.log_lines.push(format!("Removed from {label}"));
            }
            Err(e) => {
                result.failed += 1;
                result.log_lines.push(format!("FAILED for {label}: {e}"));
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rel_rejects_escapes() {
        assert!(validate_rel(Path::new("home/a.txt")).is_ok());
        assert!(validate_rel(Path::new("")).is_err());
        assert!(validate_rel(Path::new("/etc/passwd")).is_err());
        assert!(validate_rel(Path::new("home/../x")).is_err());
        assert!(validate_rel(Path::new("./x")).is_err());
    }

    #[test]
    fn locate_refuses_symlinked_parents() {
        let tmp = std::env::temp_dir().join(format!("purge-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("real/sub")).unwrap();
        std::fs::write(tmp.join("real/sub/f"), b"x").unwrap();
        std::os::unix::fs::symlink(tmp.join("real"), tmp.join("link")).unwrap();

        assert_eq!(locate(&tmp, Path::new("real/sub/f")).unwrap(), Some(false));
        assert_eq!(locate(&tmp, Path::new("real/sub")).unwrap(), Some(true));
        assert_eq!(locate(&tmp, Path::new("real/missing")).unwrap(), None);
        assert_eq!(locate(&tmp, Path::new("real/sub/f/below")).unwrap(), None);
        // A symlink as the final component is fine: rm removes the link itself.
        assert_eq!(locate(&tmp, Path::new("link")).unwrap(), Some(false));
        assert!(locate(&tmp, Path::new("link/sub/f")).is_err());

        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
