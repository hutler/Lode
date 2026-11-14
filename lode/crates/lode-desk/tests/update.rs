//! T114: update checks, the settings that turn them off, and signature checking. A package
//! signed with any key but the one built into the app is refused.
// The updater is part of the Tauri window until T135.
#![cfg(feature = "web-test")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;

use base64::Engine;
use lode_desk::update::{self, Settings, UpdateCheck};
use serde_json::json;
use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime};
use tauri::App;

const PACKAGE: &[u8] = b"the new Lode Desk, as an installer would hold it";

fn b64(text: String) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

/// A minisign key pair, as `tauri signer generate` makes one: the public key and a signer.
fn keypair() -> (String, minisign::SecretKey) {
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    (b64(kp.pk.to_box().unwrap().into_string()), kp.sk)
}

fn sign(sk: &minisign::SecretKey, data: &[u8]) -> String {
    let sig = minisign::sign(None, sk, data, Some("lode desk test"), None).unwrap();
    b64(sig.into_string())
}

/// Serves the feed at `/latest.json` and the package at `/package` until the test ends.
fn serve_feed(signature: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let feed = json!({
        "version": "99.0.0",
        "notes": "Branches, merges and lode:// links.",
        "pub_date": "2026-11-20T10:00:00Z",
        "platforms": { "test-target": { "signature": signature, "url": format!("{base}/package") } }
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
                ("application/octet-stream", PACKAGE.to_vec())
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

/// The app with the updater set up as `tauri.conf.json` does, with `pubkey` built in.
fn app(pubkey: &str, base: &str) -> App<MockRuntime> {
    let mut ctx = mock_context(noop_assets());
    ctx.config_mut().plugins.0.insert(
        "updater".into(),
        json!({
            "pubkey": pubkey,
            "endpoints": [format!("{base}/latest.json")],
            "dangerousInsecureTransportProtocol": true
        }),
    );
    mock_builder()
        .plugin(
            tauri_plugin_updater::Builder::new()
                .target("test-target")
                .build(),
        )
        .build(ctx)
        .unwrap()
}

fn settings_file(dir: &Path, check: bool) -> std::path::PathBuf {
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

#[tokio::test(flavor = "multi_thread")]
async fn an_update_signed_with_the_built_in_key_is_offered_and_downloaded() {
    let (pk, sk) = keypair();
    let base = serve_feed(sign(&sk, PACKAGE));
    let app = app(&pk, &base);
    let dir = tempfile::tempdir().unwrap();
    let (check, found) = update::find(app.handle(), &settings_file(dir.path(), true), false)
        .await
        .unwrap();
    match check {
        UpdateCheck::Available {
            version,
            notes,
            date,
            ..
        } => {
            assert_eq!(version, "99.0.0");
            assert_eq!(notes, "Branches, merges and lode:// links.");
            assert_eq!(date.as_deref(), Some("2026-11-20T10:00:00Z"));
        }
        other => panic!("expected an update, got {other:?}"),
    }
    let bytes = update::download(&found.unwrap()).await.unwrap();
    assert_eq!(bytes, PACKAGE);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_signed_with_another_key_is_refused() {
    let (pk, _ours) = keypair();
    let (_, theirs) = keypair();
    let base = serve_feed(sign(&theirs, PACKAGE));
    let app = app(&pk, &base);
    let dir = tempfile::tempdir().unwrap();
    let (_, found) = update::find(app.handle(), &settings_file(dir.path(), true), true)
        .await
        .unwrap();
    let found = found.unwrap();
    let err = update::download(&found).await.unwrap_err();
    assert_eq!(err.code, "bad_signature", "{err:?}");
    assert!(found.install(PACKAGE).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn checks_stop_for_the_setting_and_without_a_key() {
    let (pk, sk) = keypair();
    let base = serve_feed(sign(&sk, PACKAGE));
    let dir = tempfile::tempdir().unwrap();
    let off = settings_file(dir.path(), false);

    // The setting stops the daily check but not one the user asks for.
    let (check, _) = update::find(app(&pk, &base).handle(), &off, false)
        .await
        .unwrap();
    assert!(matches!(check, UpdateCheck::Off { .. }), "{check:?}");
    let (check, _) = update::find(app(&pk, &base).handle(), &off, true)
        .await
        .unwrap();
    assert!(matches!(check, UpdateCheck::Available { .. }), "{check:?}");

    // A build without a key never offers updates.
    let (check, found) = update::find(app("", &base).handle(), &off, true)
        .await
        .unwrap();
    assert_eq!(check, UpdateCheck::Unconfigured);
    assert!(found.is_none());
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
