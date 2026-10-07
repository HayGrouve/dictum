use std::path::PathBuf;

use anyhow::{Context, Result};

/// Where Dictum keeps its files.
/// - Windows: settings in `%APPDATA%\Dictum`, models and log in `%LOCALAPPDATA%\Dictum`.
/// - macOS: everything in `~/Library/Application Support/Dictum`.
pub struct Paths {
    pub config_file: PathBuf,
    pub models_dir: PathBuf,
    pub log_file: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let config_dir = dirs::config_dir().context("no config directory")?.join("Dictum");
        let data_dir = dirs::data_local_dir().context("no data directory")?.join("Dictum");
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("failed to create {}", data_dir.display()))?;
        Ok(Self {
            config_file: config_dir.join("config.toml"),
            models_dir: data_dir.join("models"),
            log_file: data_dir.join("dictum.log"),
        })
    }
}
