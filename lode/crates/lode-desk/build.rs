//! Compiles the Slint screens, and on Windows gives the program its icon and a manifest.
//!
//! The manifest asks for Common Controls version 6 (the folder picker's dialogs need it) and
//! tells Windows the app handles display scaling and long paths. Tests and examples get it
//! too, so they start the way the app does.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    #[cfg(windows)]
    if target_os == "windows" {
        println!("cargo:rerun-if-changed=packaging/windows/lode-desk.rc");
        println!("cargo:rerun-if-changed=packaging/windows/lode-desk.manifest");
        println!("cargo:rerun-if-changed=icons/icon.ico");
        if let Err(e) = embed_resource::compile_for_everything(
            "packaging/windows/lode-desk.rc",
            embed_resource::NONE,
        )
        .manifest_required()
        {
            println!("cargo:warning=cannot embed the icon and manifest: {e}");
            std::process::exit(1);
        }
    }
    // Windows 11 style everywhere (Slint's `native` style would need Qt); macOS gets its own.
    let style = if target_os == "macos" {
        "cupertino"
    } else {
        "fluent"
    };
    // Debug builds keep the element names the UI tests find controls by.
    let debug = std::env::var("PROFILE").map_or(true, |p| p != "release");
    let config = slint_build::CompilerConfiguration::new()
        .with_style(style.into())
        .with_debug_info(debug);
    if let Err(e) = slint_build::compile_with_config("ui/slint/app.slint", config) {
        println!("cargo:warning=cannot compile the Slint screens: {e}");
        std::process::exit(1);
    }
}
