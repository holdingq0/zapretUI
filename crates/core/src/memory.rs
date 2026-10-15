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

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
