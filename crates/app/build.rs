// Bakes the .exe icon from the dot-matrix mark in src/pixel.rs and links it in as resource 1, with
// the version and the manifest (res.rs).

use std::path::PathBuf;

include!("src/pixel.rs");
include!("res.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/pixel.rs");
    println!("cargo:rerun-if-changed=res.rs");
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let icon = out.join("zapretUI.ico");
    std::fs::write(&icon, ico()).expect("icon");
    let manifest_file = out.join("app.manifest");
    std::fs::write(&manifest_file, manifest("asInvoker")).expect("manifest");
    let rc = out.join("app.rc");
    let body = format!("1 ICON \"{}\"\n1 24 \"{}\"\n{}", rc_path(&icon), rc_path(&manifest_file), version_rc("zapretUI", "zapretUI.exe"));
    std::fs::write(&rc, body).expect("rc");
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().expect("resource");
}
