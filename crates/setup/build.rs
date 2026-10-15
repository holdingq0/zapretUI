// The installer carries the release files inside itself. `cargo xtask release` points
// ZAPRETUI_PAYLOAD at the staged folder; without it the installer is built empty and says so.
// It also gets the app's icon and a manifest that asks for administrator rights at launch.

use std::path::{Path, PathBuf};

include!("../app/src/pixel.rs");

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security><requestedPrivileges><requestedExecutionLevel level="requireAdministrator" uiAccess="false"/></requestedPrivileges></security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application><supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/></application>
  </compatibility>
</assembly>
"#;

fn files(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, base, out);
        } else if let Ok(rel) = path.strip_prefix(base) {
            println!("cargo:rerun-if-changed={}", path.display());
            out.push((rel.to_string_lossy().replace('\\', "/"), path));
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../app/src/pixel.rs");
    println!("cargo:rerun-if-env-changed=ZAPRETUI_PAYLOAD");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    let mut payload = Vec::new();
    if let Some(dir) = std::env::var_os("ZAPRETUI_PAYLOAD").map(PathBuf::from) {
        files(&dir, &dir, &mut payload);
    }
    let body: String = payload.iter().map(|(name, path)| format!("    ({name:?}, include_bytes!({:?})),\n", path.display().to_string())).collect();
    std::fs::write(out.join("payload.rs"), format!("pub const FILES: &[(&str, &[u8])] = &[\n{body}];\n")).expect("payload");

    let icon = out.join("setup.ico");
    std::fs::write(&icon, ico()).expect("icon");
    let manifest = out.join("setup.manifest");
    std::fs::write(&manifest, MANIFEST).expect("manifest");
    let esc = |p: &Path| p.display().to_string().replace('\\', "\\\\");
    let rc = out.join("setup.rc");
    std::fs::write(&rc, format!("1 ICON \"{}\"\n1 24 \"{}\"\n", esc(&icon), esc(&manifest))).expect("rc");
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().expect("resource");
}
