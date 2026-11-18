// The service's version resource: its name in Task Manager and in the file's properties.

use std::path::PathBuf;

include!("../app/res.rs");

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../app/res.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let rc = out.join("svc.rc");
    std::fs::write(&rc, version_rc("zapretUI service", "zapretUI-svc.exe")).expect("rc");
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().expect("resource");
}
