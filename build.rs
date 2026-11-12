fn main() {
    println!("cargo:rustc-check-cfg=cfg(diagnostics_plugin)");
    println!("cargo:rerun-if-changed=src/diagnostics_plugin");
    if std::path::Path::new("src/diagnostics_plugin/mod.rs").is_file() {
        println!("cargo:rustc-cfg=diagnostics_plugin");
    }
    // The Administrator pack is optional: without it, or without the `administrator`
    // feature, the app builds as a non-coding platform.
    println!("cargo:rustc-check-cfg=cfg(administrator_pack)");
    println!("cargo:rerun-if-changed=plugins/abilities/Administrator/git_control/remote");
    if std::env::var_os("CARGO_FEATURE_ADMINISTRATOR").is_some()
        && std::path::Path::new("plugins/abilities/Administrator/git_control/remote/mod.rs")
            .is_file()
    {
        println!("cargo:rustc-cfg=administrator_pack");
    }
}
