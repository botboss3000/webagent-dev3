fn main() {
    println!("cargo:rustc-check-cfg=cfg(diagnostics_plugin)");
    println!("cargo:rerun-if-changed=src/diagnostics_plugin");
    if std::path::Path::new("src/diagnostics_plugin/mod.rs").is_file() {
        println!("cargo:rustc-cfg=diagnostics_plugin");
    }
}
