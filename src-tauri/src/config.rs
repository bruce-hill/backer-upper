use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const CONFIG_FILENAME: &str = "backer-upper.toml";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SyncMode {
    Backup,
    Media,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncJob {
    pub name: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub excludes: Vec<String>,
    pub mode: SyncMode,
    pub enabled: bool,
    /// Skip directories tagged with a CACHEDIR.TAG file (Cache Directory Tagging Specification).
    #[serde(default = "default_true")]
    pub exclude_caches: bool,
    /// Command run on the remote host in place of `rsync` (passed as `--rsync-path`), e.g.
    /// `doas -n -u vmail /usr/bin/rsync` to read files the login user can't. Remote sources only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_rsync_path: Option<String>,
}

fn default_true() -> bool {
    true
}

impl SyncJob {
    pub fn new(name: impl Into<String>, source: impl Into<PathBuf>) -> Self {
        let source: PathBuf = source.into();
        let dest_name = source
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("backup"));
        SyncJob {
            name: name.into(),
            source,
            destination: dest_name,
            excludes: Vec::new(),
            mode: SyncMode::Backup,
            enabled: true,
            exclude_caches: true,
            remote_rsync_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub jobs: Vec<SyncJob>,
    pub last_backup: Option<chrono::DateTime<chrono::Local>>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            jobs: vec![SyncJob::new("Home", dirs_home())],
            last_backup: None,
        }
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/home"))
}

impl Config {
    pub fn load(drive_root: &Path) -> Result<Self> {
        let path = drive_root.join(CONFIG_FILENAME);
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let cfg: Config =
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
            Ok(cfg)
        } else {
            let cfg = Config::default();
            cfg.save(drive_root)?;
            Ok(cfg)
        }
    }

    pub fn save(&self, drive_root: &Path) -> Result<()> {
        let path = drive_root.join(CONFIG_FILENAME);
        let text = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}
