//! T178 acceptance: `loded doctor` names what is wrong with a server's setup.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use lode_proto::doctor::{DoctorReport, DoctorStatus};
use lode_server::init;

fn status(report: &DoctorReport, name: &str) -> DoctorStatus {
    report
        .checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no {name} check in {report:?}"))
        .status
}

fn detail<'a>(report: &'a DoctorReport, name: &str) -> &'a str {
    &report
        .checks
        .iter()
        .find(|c| c.name == name)
        .unwrap()
        .detail
}

/// A certificate valid from `from` to `to` (years), written as cert.pem and key.pem.
fn write_cert(dir: &Path, from: i32, to: i32) {
    let mut params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
    params.not_before = rcgen::date_time_ymd(from, 1, 1);
    params.not_after = rcgen::date_time_ymd(to, 1, 1);
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    std::fs::write(dir.join("cert.pem"), cert.pem()).unwrap();
    std::fs::write(dir.join("key.pem"), key.serialize_pem()).unwrap();
}

#[tokio::test]
async fn an_expired_certificate_and_missing_backups_are_named() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    init(&data).await.unwrap();
    write_cert(dir.path(), 2020, 2021);
    let cfg = dir.path().join("loded.toml");
    std::fs::write(
        &cfg,
        "listen = '127.0.0.1:0'\ndata_dir = 'data'\n[tls]\ncert = 'cert.pem'\nkey = 'key.pem'\n\
         [backup]\nevery = '24h'\ndir = 'backups'\n",
    )
    .unwrap();
    let now = 1_800_000_000;
    let report = lode_server::doctor::run(&cfg, now).await;
    assert_eq!(status(&report, "config"), DoctorStatus::Ok);
    assert_eq!(status(&report, "data_dir"), DoctorStatus::Ok);
    assert_eq!(status(&report, "certificate"), DoctorStatus::Fail);
    assert!(
        detail(&report, "certificate").contains("expired"),
        "{report:?}"
    );
    assert_eq!(status(&report, "domain"), DoctorStatus::Skip);
    assert_eq!(status(&report, "backups"), DoctorStatus::Warn);
    assert_eq!(status(&report, "database"), DoctorStatus::Ok);
    assert_eq!(status(&report, "store"), DoctorStatus::Skip);
    assert_eq!(report.status, DoctorStatus::Fail);
    assert_eq!(report.exit_code(), 1);

    // A good certificate leaves only the missing backup: a warning.
    write_cert(dir.path(), 2020, 2040);
    let report = lode_server::doctor::run(&cfg, now).await;
    assert_eq!(
        status(&report, "certificate"),
        DoctorStatus::Ok,
        "{report:?}"
    );
    assert_eq!(report.status, DoctorStatus::Warn);
    assert_eq!(report.exit_code(), 2);
}

#[tokio::test]
async fn a_broken_config_or_missing_data_dir_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("loded.toml");
    std::fs::write(&cfg, "listen = [").unwrap();
    let report = lode_server::doctor::run(&cfg, 0).await;
    assert_eq!(report.checks.len(), 1);
    assert_eq!(status(&report, "config"), DoctorStatus::Fail);

    std::fs::write(&cfg, "listen = '127.0.0.1:0'\ndata_dir = 'missing'\n").unwrap();
    let report = lode_server::doctor::run(&cfg, 0).await;
    assert_eq!(status(&report, "data_dir"), DoctorStatus::Fail);
    assert!(detail(&report, "data_dir").contains("loded init"));
    assert_eq!(status(&report, "database"), DoctorStatus::Fail);
    assert_eq!(report.exit_code(), 1);
}
