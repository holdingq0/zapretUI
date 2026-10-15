use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Priors(HashMap<String, HashMap<String, f64>>);

impl Priors {
    pub fn builtin() -> Self {
        match toml::from_str(include_str!("../../../resources/priors.toml")) {
            Ok(map) => Self(map),
            Err(e) => {
                tracing::error!("priors.toml: {e}");
                Self::default()
            }
        }
    }

    pub fn weight(&self, kind: &str, value: &str) -> f64 {
        self.0.get(kind).and_then(|m| m.get(value)).copied().unwrap_or(0.5)
    }
}
