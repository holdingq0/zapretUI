use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub check_interval_min: u32,
    pub auto_update: bool,
    /// Where the names of the neural networks are asked (see `aidns::upstream`); empty: nowhere.
    pub ai_dns: String,
    /// The user's own resolver, kept while a preset is chosen.
    pub ai_dns_custom: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            check_interval_min: 10,
            auto_update: true,
            ai_dns: String::new(),
            ai_dns_custom: String::new(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        std::fs::read_to_string(paths::config_file())
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        paths::write_atomic(&paths::config_file(), &toml::to_string_pretty(self)?)?;
        Ok(())
    }
}
