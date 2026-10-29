use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NameEntry {
    pub epic: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Default)]
pub struct NameCache {
    path: PathBuf,
    map: BTreeMap<String, NameEntry>,
}

impl NameCache {
    pub fn load() -> Self {
        let path = default_path();
        let map = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<BTreeMap<String, NameEntry>>(&raw).ok())
            .unwrap_or_default();
        Self { path, map }
    }

    pub fn resolve(&self, token: &str) -> Result<String> {
        if token.contains('.') {
            return Ok(token.to_string());
        }
        let key = token.to_uppercase();
        match self.map.get(&key) {
            Some(entry) => Ok(entry.epic.clone()),
            None => Err(anyhow::anyhow!(
                "no short name `{token}` pinned - run `search {token}`, then `pin {token} <epic>`"
            )),
        }
    }

    /// Trade-driven pin: only associates when the code is free or already
    /// points at the same epic - never silently re-points to another market.
    pub fn pin_from_trade(&mut self, code: &str, epic: &str, expiry: Option<&str>, name: Option<&str>) {
        let key = code.to_uppercase();
        match self.map.get(&key) {
            None => {
                self.map.insert(key, entry(epic, expiry, name));
            }
            Some(existing) if existing.epic == epic => {
                self.map.insert(key, entry(epic, expiry, name));
            }
            Some(_) => {}
        }
    }

    pub fn pin_explicit(&mut self, code: &str, epic: &str, expiry: Option<&str>, name: Option<&str>) {
        self.map.insert(code.to_uppercase(), entry(epic, expiry, name));
    }

    pub fn unpin(&mut self, code: &str) -> bool {
        self.map.remove(&code.to_uppercase()).is_some()
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&self.map).context("cannot serialise names")?;
        std::fs::write(&self.path, format!("{json}\n"))
            .with_context(|| format!("cannot write {}", self.path.display()))
    }
}

fn entry(epic: &str, expiry: Option<&str>, name: Option<&str>) -> NameEntry {
    NameEntry {
        epic: epic.to_string(),
        expiry: expiry.map(str::to_owned),
        name: name.map(str::to_owned),
    }
}

pub fn default_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".config/ig/names.json")
}
