//! Tauri's build step: embeds the config, icons and the permissions of the commands.
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
    tauri_build::build();
}
