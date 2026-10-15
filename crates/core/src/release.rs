// What a GitHub release of zapretUI carries besides the zip for first installs:
// every file on its own, manifest.json with their hashes and manifest.sig, an Ed25519 signature
// of manifest.json made with the key whose public half is built into zapretUI-svc.exe.

use serde::{Deserialize, Serialize};

pub const MANIFEST: &str = "manifest.json";
pub const SIGNATURE: &str = "manifest.sig";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Place {
    /// Program Files\zapretUI: the app and the service.
    Bin,
    /// ProgramData\zapretUI\engine: winws2 and its files.
    Engine,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub place: Place,
    /// Path inside its place, with forward slashes ("lua/zapret-lib.lua").
    pub path: String,
    pub sha256: String,
}

impl FileEntry {
    /// Release assets are flat: "engine.lua.zapret-lib.lua" for engine files, the name itself for the rest.
    pub fn asset(&self) -> String {
        match self.place {
            Place::Bin => self.path.clone(),
            Place::Engine => format!("engine.{}", self.path.replace('/', ".")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub files: Vec<FileEntry>,
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}
