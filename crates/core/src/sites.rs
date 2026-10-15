use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SiteMode {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sites {
    #[serde(default)]
    pub modes: BTreeMap<String, SiteMode>,
}

impl Sites {
    pub fn load() -> Self {
        std::fs::read_to_string(paths::sites_file())
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, expand: &dyn Fn(&str) -> Vec<String>) -> anyhow::Result<()> {
        paths::write_atomic(&paths::sites_file(), &toml::to_string_pretty(self)?)?;
        let list = |mode: SiteMode| {
            self.modes
                .iter()
                .filter(|(_, m)| **m == mode)
                .flat_map(|(h, _)| expand(h))
                .map(|h| format!("{h}\n"))
                .collect::<String>()
        };
        paths::write_atomic(&paths::always_file(), &list(SiteMode::Always))?;
        paths::write_atomic(&paths::exclude_file(), &list(SiteMode::Never))?;
        Ok(())
    }

    pub fn mode(&self, host: &str) -> SiteMode {
        self.modes.get(host).copied().unwrap_or_default()
    }

    pub fn set(&mut self, host: String, mode: SiteMode) {
        if mode == SiteMode::Auto {
            self.modes.remove(&host);
        } else {
            self.modes.insert(host, mode);
        }
    }
}

pub fn normalize_host(input: &str) -> Option<String> {
    let s = input.trim().to_ascii_lowercase();
    let s = s.split("://").last()?;
    let s = s.split(['/', '?', '#']).next()?;
    let s = s.rsplit_once('@').map_or(s, |(_, h)| h);
    let s = s.split(':').next()?.trim_end_matches('.');
    let s = s.strip_prefix("www.").unwrap_or(s);
    let ok = !s.is_empty()
        && s.len() <= 253
        && s.contains('.')
        && s.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    ok.then(|| s.to_string())
}
