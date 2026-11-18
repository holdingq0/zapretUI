// A release has two files: the installer and the portable program, one exe each. Both carry the
// program as the same signed pack appended to their end: the installer puts it in Program Files,
// the portable exe next to itself, and the updater takes it from the installer, only the parts of
// it that changed. The pack: the manifest (version, every file with its size and sha256), an Ed25519
// signature of the manifest made with the key whose public half is built into the program, then
// the files in manifest order.
//
//   pack:    "ZUIUPD01" | manifest length (u32 LE) | manifest JSON | signature (64 bytes) | files
//   trailer: exe | pack | pack offset (u64 LE) | "ZUITAIL1"

use serde::{Deserialize, Serialize};

const MAGIC: &[u8; 8] = b"ZUIUPD01";
const TAIL: &[u8; 8] = b"ZUITAIL1";
pub const SIGNATURE_LEN: usize = 64;
/// The end of an exe with a pack: where the pack starts, and the mark.
pub const TRAILER_LEN: usize = 16;

pub fn setup_asset(version: &str) -> String {
    format!("zapretUI-setup-{version}.exe")
}

/// The portable program. No version in the name: it updates the folder it unpacks itself into, and
/// "releases/latest/download/zapretUI-portable.exe" always is the newest.
pub const PORTABLE_ASSET: &str = "zapretUI-portable.exe";

/// The installer with the pack appended; Windows ignores data past the end of an exe image.
pub fn attach(exe: &[u8], pack: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(exe.len() + pack.len() + 16);
    out.extend_from_slice(exe);
    out.extend_from_slice(pack);
    out.extend_from_slice(&(exe.len() as u64).to_le_bytes());
    out.extend_from_slice(TAIL);
    out
}

/// Where the pack starts, read from the last TRAILER_LEN bytes of an exe; None without a pack.
pub fn pack_offset(trailer: &[u8]) -> Option<u64> {
    let offset = trailer.strip_suffix(TAIL.as_slice())?;
    Some(u64::from_le_bytes(offset.try_into().ok()?))
}

/// The pack appended to an exe, if there is one.
pub fn attached(bytes: &[u8]) -> Option<&[u8]> {
    let at = bytes.len().checked_sub(TRAILER_LEN)?;
    let offset = usize::try_from(pack_offset(&bytes[at..])?).ok()?;
    bytes.get(offset..at)
}

/// Whether the file ends with a pack. Reads only its last bytes.
pub fn carries_pack(path: &std::path::Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let mut trailer = [0u8; TRAILER_LEN];
    file.seek(SeekFrom::End(-(TRAILER_LEN as i64))).is_ok() && file.read_exact(&mut trailer).is_ok() && pack_offset(&trailer).is_some()
}

/// The length of the pack's head (magic, manifest length, manifest, signature), once `bytes`, the
/// start of a pack, hold the manifest length. The files follow right after it.
pub fn head_len(bytes: &[u8]) -> Option<usize> {
    let rest = bytes.strip_prefix(MAGIC.as_slice())?;
    let len = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize;
    Some(MAGIC.len() + 4 + len + SIGNATURE_LEN)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        let file = |place, path: &str, size| FileEntry { place, path: path.into(), size, sha256: String::new() };
        Manifest { version: "1.2.3".into(), files: vec![file(Place::Bin, "a.exe", 3), file(Place::Engine, "lua/b.lua", 2)] }
    }

    #[test]
    fn a_pack_comes_back_out_of_the_installer() {
        let manifest = manifest();
        let body = serde_json::to_vec(&manifest).unwrap();
        let signature = [7u8; SIGNATURE_LEN];
        let packed = pack(&body, &signature, &[b"abc".to_vec(), b"de".to_vec()]);
        let exe = b"MZ installer".to_vec();
        let whole = attach(&exe, &packed);

        let found = attached(&whole).unwrap();
        assert_eq!(found, &packed[..]);
        assert_eq!(pack_offset(&whole[whole.len() - TRAILER_LEN..]), Some(exe.len() as u64));
        let head = head_len(found).unwrap();
        assert_eq!(&found[head..], b"abcde");
        let (got_manifest, got_signature, files) = unpack(found).unwrap();
        assert_eq!(got_manifest, &body[..]);
        assert_eq!(got_signature, &signature[..]);
        assert_eq!(split(&manifest, files).unwrap(), vec![&b"abc"[..], &b"de"[..]]);
    }

    #[test]
    fn broken_input_is_refused() {
        let manifest = manifest();
        assert!(attached(b"MZ installer without a pack").is_none());
        assert!(pack_offset(b"0123456789abcdef").is_none());
        assert!(head_len(b"ZUIUPD01\x01\x00").is_none());
        assert!(unpack(b"ZUIUPD01\x01\x00").is_none());
        assert!(unpack(b"not a pack at all").is_none());
        assert!(split(&manifest, b"abcd").is_none(), "too short");
        assert!(split(&manifest, b"abcdef").is_none(), "bytes left over");
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(hex(&[10, 255, 0]), "0aff00");
        assert_eq!(unhex(" 0aFF00\n"), Some(vec![10, 255, 0]));
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("zz"), None);
    }
}
