//! The build version every Lode program reports.
//!
//! [`VERSION`] is the release version on a release (`0.2.0`) and the release version plus the
//! commit it was built from on any other commit (`0.2.0+5.g4a33c89`: five commits after the
//! newest `lode-v*` tag, at commit `4a33c89`). `build.rs` explains the rules.
//!
//! Version checks (updates, the minimum version a client needs) keep comparing
//! `CARGO_PKG_VERSION`; this string is for people and logs.

#![forbid(unsafe_code)]

/// The build version, such as `0.2.0` or `0.2.0+5.g4a33c89`.
pub const VERSION: &str = env!("LODE_BUILD_VERSION");

/// The release version this build is based on, such as `0.2.0`.
pub const RELEASE: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_starts_with_the_release() {
        assert!(VERSION == RELEASE || VERSION.starts_with(&format!("{RELEASE}+")));
    }
}
