// The installer's icon and a manifest that asks for administrator rights at launch.
// The program itself is appended to the built exe by `cargo xtask release`.

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

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../app/src/pixel.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let icon = out.join("setup.ico");
    std::fs::write(&icon, ico()).expect("icon");
    let manifest = out.join("setup.manifest");
    std::fs::write(&manifest, MANIFEST).expect("manifest");
    let esc = |p: &Path| p.display().to_string().replace('\\', "\\\\");
    let rc = out.join("setup.rc");
    std::fs::write(&rc, format!("1 ICON \"{}\"\n1 24 \"{}\"\n", esc(&icon), esc(&manifest))).expect("rc");
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().expect("resource");
}
