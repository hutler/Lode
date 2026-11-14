//! Lode Desk.

// No console window behind the app in Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    #[cfg(feature = "web")]
    let result = lode_desk::web::run().map_err(|e| e.to_string());
    #[cfg(not(feature = "web"))]
    let result = lode_desk::app::run();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
