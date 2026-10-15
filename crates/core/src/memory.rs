use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::paths;
use crate::reserve::Reserve;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NetMemory {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub learned_at: u64,
    #[serde(default)]
    pub blocked: Vec<String>,
    #[serde(default)]
    pub unfixed: Vec<String>,
    #[serde(default)]
    pub reserve: Reserve,
    #[serde(default)]
    pub direct: BTreeMap<String, u64>,
}

impl NetMemory {
    pub fn load(id: &str) -> anyhow::Result<Option<Self>> {
        match std::fs::read_to_string(paths::network_file(id)) {
            Ok(s) => {
                let mut memory: Self = toml::from_str(&s)?;
                memory.reserve.retain_safe();
                Ok(Some(memory))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, id: &str) -> anyhow::Result<()> {
        paths::write_atomic(&paths::network_file(id), &toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// Forgets networks not seen for a year (a café, a friend's Wi-Fi): one file per network otherwise
/// piles up forever. The current network is always kept.
pub fn prune(keep: &str, max_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(paths::network_file(keep).parent().unwrap_or(std::path::Path::new("."))) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let current = path.file_stem().is_some_and(|s| s == keep);
        let stale = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > max_age);
        if !current && stale && path.extension().is_some_and(|e| e == "toml") {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
