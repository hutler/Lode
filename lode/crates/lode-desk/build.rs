//! Compiles the Slint screens; with the `web` feature, also runs Tauri's build step (config,
//! icons and the permissions of the commands).
//!
//! On Windows (MSVC) the test executables also get a manifest that asks for Common Controls
//! version 6. The updater plugin's dialog code imports `TaskDialogIndirect`, which only
//! version 6 has; the app gets that manifest from `tauri_build`, but test executables do
//! not, so without this they stop at start with `STATUS_ENTRYPOINT_NOT_FOUND`.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-tests=/MANIFESTDEPENDENCY:type='win32' \
             name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
             processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
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
    #[cfg(feature = "web")]
    tauri_build::build();
}
