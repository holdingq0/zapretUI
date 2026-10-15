// A release has two files: the installer and the portable zip. The installer carries the program
// as a signed pack appended to its end, and the updater downloads the same installer to take the
// pack from it. The pack: the manifest (version, every file with its size and sha256), an Ed25519
// signature of the manifest made with the key whose public half is built into the program, then
// the files in manifest order.
//
//   pack:    "ZUIUPD01" | manifest length (u32 LE) | manifest JSON | signature (64 bytes) | files
//   trailer: installer exe | pack | pack offset (u64 LE) | "ZUITAIL1"

use serde::{Deserialize, Serialize};

const MAGIC: &[u8; 8] = b"ZUIUPD01";
const TAIL: &[u8; 8] = b"ZUITAIL1";
pub const SIGNATURE_LEN: usize = 64;

pub fn setup_asset(version: &str) -> String {
    format!("zapretUI-setup-{version}.exe")
}

/// The installer with the pack appended; Windows ignores data past the end of an exe image.
pub fn attach(exe: &[u8], pack: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(exe.len() + pack.len() + 16);
    out.extend_from_slice(exe);
    out.extend_from_slice(pack);
    out.extend_from_slice(&(exe.len() as u64).to_le_bytes());
    out.extend_from_slice(TAIL);
    out
}

/// The pack appended to an installer, if there is one.
pub fn attached(bytes: &[u8]) -> Option<&[u8]> {
    let body = bytes.strip_suffix(TAIL.as_slice())?;
    let at = body.len().checked_sub(8)?;
    let offset = usize::try_from(u64::from_le_bytes(body[at..].try_into().ok()?)).ok()?;
    body.get(offset..at)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Place {
    /// Program Files\zapretUI (or the portable folder): the app and the service.
    Bin,
    /// The engine folder: winws2 and its files.
    Engine,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub place: Place,
    /// Path inside its place, with forward slashes ("lua/zapret-lib.lua").
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub files: Vec<FileEntry>,
}

pub fn pack(manifest: &[u8], signature: &[u8], files: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(manifest.len() + files.iter().map(Vec::len).sum::<usize>() + 80);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(manifest.len() as u32).to_le_bytes());
    out.extend_from_slice(manifest);
    out.extend_from_slice(signature);
    for file in files {
        out.extend_from_slice(file);
    }
    out
}

/// Splits a downloaded update into (manifest, signature, files). Nothing in it is trusted yet.
pub fn unpack(bytes: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let rest = bytes.strip_prefix(MAGIC.as_slice())?;
    let len = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize;
    let manifest = rest.get(4..4 + len)?;
    let signature = rest.get(4 + len..4 + len + SIGNATURE_LEN)?;
    let files = rest.get(4 + len + SIGNATURE_LEN..)?;
    Some((manifest, signature, files))
}

/// Cuts the files part into one slice per manifest entry, in order; None if the sizes do not add up.
pub fn split<'a>(manifest: &Manifest, mut files: &'a [u8]) -> Option<Vec<&'a [u8]>> {
    let mut out = Vec::with_capacity(manifest.files.len());
    for entry in &manifest.files {
        let size = usize::try_from(entry.size).ok()?;
        if files.len() < size {
            return None;
        }
        let (data, rest) = files.split_at(size);
        out.push(data);
        files = rest;
    }
    files.is_empty().then_some(out)
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
