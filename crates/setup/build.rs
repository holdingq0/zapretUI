// The installer's icon, its version and a manifest that asks for administrator rights at launch.
// The program itself is appended to the built exe by `cargo xtask release`.

use std::path::PathBuf;

include!("../app/src/pixel.rs");
include!("../app/res.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../app/src/pixel.rs");
    println!("cargo:rerun-if-changed=../app/res.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let icon = out.join("setup.ico");
    std::fs::write(&icon, ico()).expect("icon");
    let manifest_file = out.join("setup.manifest");
    std::fs::write(&manifest_file, manifest("requireAdministrator")).expect("manifest");
    let rc = out.join("setup.rc");
    let body = format!(
        "1 ICON \"{}\"\n1 24 \"{}\"\n{}",
        rc_path(&icon),
        rc_path(&manifest_file),
        version_rc("zapretUI installer", "zapretUI-setup.exe")
    );
    std::fs::write(&rc, body).expect("rc");
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().expect("resource");
}
