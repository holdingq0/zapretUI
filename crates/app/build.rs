// Bakes the .exe icon from the dot-matrix mark in src/pixel.rs and links it in as resource 1.

use std::path::PathBuf;

include!("src/pixel.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/pixel.rs");
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let icon = out.join("zapretUI.ico");
    std::fs::write(&icon, ico()).expect("icon");
    let rc = out.join("app.rc");
    let path = icon.display().to_string().replace('\\', "\\\\");
    std::fs::write(&rc, format!("1 ICON \"{path}\"\n")).expect("rc");
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().expect("resource");
}
