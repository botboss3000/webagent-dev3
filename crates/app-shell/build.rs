fn main() {
    // package.py writes the bundle zip before building; a plain `cargo check` gets an
    // empty placeholder so the crate still compiles.
    let bundle = std::path::Path::new("bundle/app-bundle.zip");
    if !bundle.exists() {
        std::fs::create_dir_all("bundle").expect("bundle folder");
        std::fs::write(bundle, []).expect("placeholder bundle");
    }
    println!("cargo:rerun-if-changed=bundle/app-bundle.zip");
    tauri_build::build()
}
