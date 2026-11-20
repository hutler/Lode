//! T114 and T135: update checks, the settings that turn them off, the feed, and signature
//! checking. A package signed with any key but the one built into the app is refused, and so
//! is a damaged one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use base64::Engine;
use lode_desk::update::{self, Feed, InstallKind, Settings, UpdateCheck, Updater};
use serde_json::json;

const PACKAGE: &[u8] = b"the new Lode Desk, as an installer would hold it";

fn b64(text: String) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

/// A minisign key pair as `cargo packager signer generate` (and `tauri signer generate`
/// before it) writes one: the public key, base64-wrapped, and a signer.
fn keypair() -> (String, minisign::SecretKey) {
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    (b64(kp.pk.to_box().unwrap().into_string()), kp.sk)
}

/// A `.sig` file's text, base64-wrapped, as the packager writes it next to each bundle.
fn sign(sk: &minisign::SecretKey, data: &[u8]) -> String {
    let sig = minisign::sign(None, sk, data, Some("lode desk test"), None).unwrap();
    b64(sig.into_string())
}

/// Serves the feed at `/latest.json` and `package` at `/package` until the test ends.
fn serve_feed(version: &str, platform: &str, signature: String, package: &'static [u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let feed = json!({
        "version": version,
        "notes": "Branches, merges and lode:// links.",
        "pub_date": "2026-11-20T10:00:00Z",
        "platforms": { platform: { "signature": signature, "url": format!("{base}/package") } }
    })
    .to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let (kind, body): (&str, Vec<u8>) = if req.starts_with("GET /latest.json") {
                ("application/json", feed.clone().into_bytes())
            } else if req.starts_with("GET /package") {
                ("application/octet-stream", package.to_vec())
            } else {
                let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n");
                continue;
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(&body);
        }
    });
    base
}

fn settings_file(dir: &Path, check: bool) -> PathBuf {
    let file = dir.join("settings.json");
    update::save_settings(
        &file,
        &Settings {
            check_updates: check,
        },
    )
    .unwrap();
    file
}

/// This copy at version 0.1.0, installed by the MSI, with `pubkey` built in.
fn updater(pubkey: &str, base: &str, settings_file: PathBuf) -> Updater {
    Updater {
        pubkey: pubkey.into(),
        feed: format!("{base}/latest.json"),
        current: "0.1.0".into(),
        platforms: update::platforms(&InstallKind::Msi, "windows", "x86_64"),
        settings_file,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_signed_with_the_built_in_key_is_offered_and_downloaded() {
    let (pk, sk) = keypair();
    let base = serve_feed("99.0.0", "windows-x86_64-msi", sign(&sk, PACKAGE), PACKAGE);
    let dir = tempfile::tempdir().unwrap();
    let u = updater(&pk, &base, settings_file(dir.path(), true));
    let (check, found) = u.check(false, None).await.unwrap();
    match check {
        UpdateCheck::Available {
            version,
            notes,
            date,
            current,
        } => {
            assert_eq!(version, "99.0.0");
            assert_eq!(current, "0.1.0");
            assert_eq!(notes, "Branches, merges and lode:// links.");
            assert_eq!(date.as_deref(), Some("2026-11-20T10:00:00Z"));
        }
        other => panic!("expected an update, got {other:?}"),
    }
    let bytes = u.download(&found.unwrap()).await.unwrap();
    assert_eq!(bytes, PACKAGE);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_signed_with_another_key_is_refused() {
    let (pk, _ours) = keypair();
    let (_, theirs) = keypair();
    let base = serve_feed("99.0.0", "windows-x86_64", sign(&theirs, PACKAGE), PACKAGE);
    let dir = tempfile::tempdir().unwrap();
    let u = updater(&pk, &base, settings_file(dir.path(), true));
    let (_, found) = u.check(true, None).await.unwrap();
    // The plain platform name serves installs of any kind.
    let err = u.download(&found.unwrap()).await.unwrap_err();
    assert_eq!(err.code, "bad_signature", "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_damaged_package_is_refused() {
    let (pk, sk) = keypair();
    let mut damaged = PACKAGE.to_vec();
    damaged[3] ^= 1;
    let damaged: &'static [u8] = damaged.leak();
    let base = serve_feed("99.0.0", "windows-x86_64-msi", sign(&sk, PACKAGE), damaged);
    let dir = tempfile::tempdir().unwrap();
    let u = updater(&pk, &base, settings_file(dir.path(), true));
    let (_, found) = u.check(true, None).await.unwrap();
    let err = u.download(&found.unwrap()).await.unwrap_err();
    assert_eq!(err.code, "bad_signature", "{err:?}");
    // A signature that is not one at all is refused the same way.
    assert_eq!(
        update::verify(&pk, &b64("not a signature".into()), PACKAGE)
            .unwrap_err()
            .code,
        "bad_signature"
    );
    assert_eq!(
        update::verify(&pk, "%%%", PACKAGE).unwrap_err().code,
        "bad_signature"
    );
    assert!(update::verify(&pk, &sign(&sk, PACKAGE), PACKAGE).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_stop_for_the_setting_and_without_a_key() {
    let (pk, sk) = keypair();
    let base = serve_feed("99.0.0", "windows-x86_64-msi", sign(&sk, PACKAGE), PACKAGE);
    let dir = tempfile::tempdir().unwrap();
    let off = settings_file(dir.path(), false);

    // The setting stops the daily check but not one the user asks for.
    let (check, _) = updater(&pk, &base, off.clone())
        .check(false, None)
        .await
        .unwrap();
    assert!(matches!(check, UpdateCheck::Off { .. }), "{check:?}");
    let (check, _) = updater(&pk, &base, off.clone())
        .check(true, None)
        .await
        .unwrap();
    assert!(matches!(check, UpdateCheck::Available { .. }), "{check:?}");

    // The environment variable stops even that.
    let set = std::ffi::OsString::from("1");
    let (check, _) = updater(&pk, &base, off.clone())
        .check(true, Some(&set))
        .await
        .unwrap();
    assert!(matches!(check, UpdateCheck::Off { .. }), "{check:?}");

    // A build without a key never offers updates.
    let (check, found) = updater("", &base, off).check(true, None).await.unwrap();
    assert_eq!(check, UpdateCheck::Unconfigured);
    assert!(found.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_version_or_one_for_another_system_is_not_offered() {
    let (pk, sk) = keypair();
    let dir = tempfile::tempdir().unwrap();
    let file = settings_file(dir.path(), true);
    let base = serve_feed("0.1.0", "windows-x86_64-msi", sign(&sk, PACKAGE), PACKAGE);
    let (check, _) = updater(&pk, &base, file.clone())
        .check(true, None)
        .await
        .unwrap();
    assert_eq!(
        check,
        UpdateCheck::Current {
            current: "0.1.0".into()
        }
    );
    let base = serve_feed("99.0.0", "linux-x86_64-deb", sign(&sk, PACKAGE), PACKAGE);
    let (check, found) = updater(&pk, &base, file).check(true, None).await.unwrap();
    assert!(matches!(check, UpdateCheck::Current { .. }), "{check:?}");
    assert!(found.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn feeds_are_taken_only_over_https_or_from_this_computer() {
    let dir = tempfile::tempdir().unwrap();
    let (pk, _) = keypair();
    let u = updater(
        &pk,
        "http://updates.example.com",
        settings_file(dir.path(), true),
    );
    let err = u.check(true, None).await.unwrap_err();
    assert!(err.message.contains("not an https address"), "{err:?}");
}

#[test]
fn the_environment_variable_turns_every_check_off() {
    let on = Settings::default();
    assert!(on.check_updates);
    assert_eq!(update::off_reason(&on, None, false), None);
    let set = std::ffi::OsString::from("1");
    let why = update::off_reason(&on, Some(&set), true).unwrap();
    assert!(why.contains(update::NO_UPDATE_ENV), "{why}");
    assert_eq!(
        update::off_reason(&on, Some(std::ffi::OsStr::new("")), false),
        None
    );

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sub/settings.json");
    assert_eq!(update::load_settings(&file), on);
    update::save_settings(
        &file,
        &Settings {
            check_updates: false,
        },
    )
    .unwrap();
    assert!(!update::load_settings(&file).check_updates);
    std::fs::write(&file, "not json").unwrap();
    assert_eq!(update::load_settings(&file), on);
}

#[test]
fn versions_compare_by_number() {
    assert!(update::is_newer("0.2.0", "0.1.9"));
    assert!(update::is_newer("0.10.0", "0.9.0"));
    assert!(update::is_newer("1.0.0", "1.0.0-dev.3"));
    assert!(!update::is_newer("1.0.0", "1.0.0"));
    assert!(!update::is_newer("0.9.0", "0.10.0"));
    assert!(!update::is_newer("1.0", "0.1.0"));
    assert!(!update::is_newer("x.1.0", "0.1.0"));
}

#[test]
fn the_feed_lists_signed_bundles_for_their_platforms() {
    let files: BTreeMap<String, Option<String>> = [
        ("lode-desk-0.3.0-windows-x86_64.msi", Some("SIGMSI\n")),
        ("lode-desk-0.3.0-windows-x86_64-setup.exe", Some("SIGNSIS")),
        ("lode-desk-0.3.0-linux-x86_64.AppImage", Some("SIGAPPIMAGE")),
        ("lode-desk-0.3.0-linux-x86_64.deb", None),
        ("lode-desk-0.3.0-macos-universal.app.tar.gz", Some("SIGMAC")),
        ("lode-desk-0.3.0-macos-universal.dmg", None),
        ("lode-desk-0.2.0-windows-x86_64.msi", Some("OLD")),
    ]
    .into_iter()
    .map(|(n, s)| (n.to_owned(), s.map(str::to_owned)))
    .collect();
    let feed = update::write_feed(
        "0.3.0",
        "https://github.com/o/Lode/releases/download/lode-v0.3.0/",
        "Notes",
        "2026-11-20T10:00:00Z",
        &files,
    )
    .unwrap();
    let get = |p: &str| feed.platforms.get(p).cloned();
    let msi = get("windows-x86_64-msi").unwrap();
    assert_eq!(msi.signature, "SIGMSI");
    assert_eq!(
        msi.url,
        "https://github.com/o/Lode/releases/download/lode-v0.3.0/lode-desk-0.3.0-windows-x86_64.msi"
    );
    assert_eq!(get("windows-x86_64"), Some(msi));
    assert_eq!(get("windows-x86_64-nsis").unwrap().signature, "SIGNSIS");
    assert_eq!(get("linux-x86_64").unwrap().signature, "SIGAPPIMAGE");
    assert!(
        get("linux-x86_64-deb").is_none(),
        "unsigned bundles stay out"
    );
    assert_eq!(get("darwin-aarch64-app").unwrap().signature, "SIGMAC");
    assert_eq!(feed.platforms.len(), 9);
    // It reads back as the app reads it, and matches the schema's shape.
    let text = serde_json::to_string(&feed).unwrap();
    let back: Feed = serde_json::from_str(&text).unwrap();
    assert_eq!(back, feed);
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["pub_date"], "2026-11-20T10:00:00Z");

    assert!(update::write_feed("0.3", "https://x", "", "", &files).is_err());
    assert!(update::write_feed("0.3.0-rc1", "https://x", "", "", &files).is_err());
    assert!(update::write_feed("0.3.0", "http://x", "", "", &files).is_err());
    assert!(update::write_feed("0.4.0", "https://x", "", "", &files).is_err());
}

#[test]
fn the_install_kind_decides_the_package() {
    use update::install_kind;
    let appimage = std::ffi::OsString::from("/home/a/Apps/Lode Desk.AppImage");
    assert_eq!(
        install_kind(
            Path::new("/tmp/.mount_x/usr/bin/lode-desk"),
            "linux",
            Some(&appimage)
        ),
        InstallKind::AppImage(PathBuf::from("/home/a/Apps/Lode Desk.AppImage"))
    );
    assert_eq!(
        install_kind(Path::new("/usr/bin/lode-desk"), "linux", None),
        InstallKind::Deb
    );
    assert_eq!(
        install_kind(
            Path::new("/home/a/src/target/debug/lode-desk"),
            "linux",
            None
        ),
        InstallKind::Unknown
    );
    assert_eq!(
        install_kind(
            Path::new("C:\\Program Files\\Lode Desk\\lode-desk.exe"),
            "windows",
            None
        ),
        InstallKind::Msi
    );
    assert_eq!(
        install_kind(
            Path::new("/Applications/Lode Desk.app/Contents/MacOS/lode-desk"),
            "macos",
            None
        ),
        InstallKind::MacApp(PathBuf::from("/Applications/Lode Desk.app"))
    );
    // The setup program leaves its uninstaller next to the app.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("uninstall.exe"), b"").unwrap();
    assert_eq!(
        install_kind(&dir.path().join("lode-desk.exe"), "windows", None),
        InstallKind::Nsis
    );

    assert_eq!(
        update::platforms(&InstallKind::Nsis, "windows", "x86_64"),
        ["windows-x86_64-nsis", "windows-x86_64"]
    );
    assert_eq!(
        update::platforms(&InstallKind::MacApp(PathBuf::new()), "macos", "aarch64"),
        ["darwin-aarch64-app", "darwin-aarch64"]
    );
    assert_eq!(
        update::platforms(&InstallKind::Unknown, "linux", "x86_64"),
        ["linux-x86_64"]
    );
}

#[test]
fn an_appimage_is_replaced_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Lode Desk.AppImage");
    std::fs::write(&app, b"old").unwrap();
    update::replace_appimage(&app, PACKAGE).unwrap();
    assert_eq!(std::fs::read(&app).unwrap(), PACKAGE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&app).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "still runnable");
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn a_signature_made_by_cargo_packager_is_accepted() {
    // `cargo packager signer generate` and `cargo packager signer sign` (0.11.8) made these;
    // the release workflow signs the installers the same way.
    let pubkey = include_str!("fixtures/packager-test.key.pub");
    let signature = include_str!("fixtures/packager-test.bin.sig");
    let data = include_bytes!("fixtures/packager-test.bin");
    assert!(update::verify(pubkey, signature, data).is_ok());
    let mut damaged = data.to_vec();
    damaged[0] ^= 1;
    assert_eq!(
        update::verify(pubkey, signature, &damaged)
            .unwrap_err()
            .code,
        "bad_signature"
    );
}
