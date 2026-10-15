use serde::{Deserialize, Serialize};

use crate::recipe::Recipe;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Reserve {
    #[serde(default)]
    pub tls: Vec<Recipe>,
    #[serde(default)]
    pub http: Vec<Recipe>,
    #[serde(default)]
    pub quic: Vec<Recipe>,
    #[serde(default)]
    pub voice: Vec<Recipe>,
}

impl Reserve {
    pub fn len(&self) -> usize {
        self.tls.len() + self.http.len() + self.quic.len() + self.voice.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn retain_safe(&mut self) {
        for list in [&mut self.tls, &mut self.http, &mut self.quic, &mut self.voice] {
            list.retain(Recipe::is_safe);
        }
    }
}
