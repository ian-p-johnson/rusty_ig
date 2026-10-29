use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct Config {
    #[serde(default)]
    pub api_name: String,
    pub api_key: String,
    pub identifier: String,
    pub password: String,
    #[serde(default)]
    pub base_url: Option<String>,
}

pub fn default_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".config/ig/ig.toml")
}

pub fn load(path: &Path) -> Result<Config> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read config file {}", path.display()))?;
    let cfg: Config = toml::from_str(&raw)
        .with_context(|| format!("cannot parse config file {}", path.display()))?;

    if cfg.api_key.trim().is_empty() {
        bail!("api_key is empty in {}", path.display());
    }
    if cfg.identifier.trim().is_empty()
        || cfg.identifier.contains("REPLACE_ME")
        || cfg.password.contains("REPLACE_ME")
    {
        bail!(
            "identifier/password not set in {} - fill them in before running",
            path.display()
        );
    }
    Ok(cfg)
}
