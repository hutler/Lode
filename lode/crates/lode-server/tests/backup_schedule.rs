//! T082 acceptance: scheduled backups run when due by the clock, and old ones beyond `keep`
//! are removed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use lode_server::maintenance::{backup_tick, run_backups, BackupSchedule, BackupTick};
use lode_server::module::Stop;
use lode_server::{init, open_state, AppState, ManualClock, ServerConfig};

const HOUR: i64 = 60 * 60 * 1000;
/// 2026-11-01 03:00:00 UTC.
const START: i64 = 1_793_502_000_000;

async fn state(dir: &Path, clock: &ManualClock) -> AppState {
    let data = dir.join("data");
    init(&data).await.unwrap();
    let config = ServerConfig::with_data_dir(&data);
    open_state(config, Arc::new(clock.clone()), Vec::new())
        .await
        .unwrap()
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn backups_follow_the_clock_and_keep_the_newest() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(START);
    let state = state(dir.path(), &clock).await;
    let backups = dir.path().join("backups");
    let schedule = BackupSchedule {
        every: Duration::from_secs(24 * 60 * 60),
        keep: 3,
        dir: backups.clone(),
        copy_to: None,
    };
    std::fs::create_dir_all(backups.join("keep-me")).unwrap();
    std::fs::create_dir_all(backups.join("lode-backup-20261031T000000Z.partial")).unwrap();

    // The first check makes a backup at once and clears a leftover partial one.
    let BackupTick::Done { path, removed, .. } = backup_tick(&state, &schedule).await.unwrap()
    else {
        panic!("first backup expected");
    };
    let database = if state.meta().dialect() == lode_meta::Dialect::Postgres {
        lode_server::maintenance::DATABASE_NOTE_FILE
    } else {
        "meta.db"
    };
    assert!(path.join(database).is_file());
    assert!(removed.is_empty());
    assert_eq!(names(&backups), ["keep-me", "lode-backup-20261101T030000Z"]);

    // Not due an hour later, with the time left.
    clock.advance_ms(HOUR);
    match backup_tick(&state, &schedule).await.unwrap() {
        BackupTick::NotDue(left) => assert_eq!(left, Duration::from_secs(23 * 60 * 60)),
        other => panic!("not due expected: {other:?}"),
    }

    // Four more days: four more backups; only the newest three are kept.
    for _ in 0..4 {
        clock.advance_ms(24 * HOUR);
        assert!(matches!(
            backup_tick(&state, &schedule).await.unwrap(),
            BackupTick::Done { .. }
        ));
    }
    assert_eq!(
        names(&backups),
        [
            "keep-me",
            "lode-backup-20261103T040000Z",
            "lode-backup-20261104T040000Z",
            "lode-backup-20261105T040000Z",
        ]
    );
}

#[tokio::test]
async fn the_scheduler_backs_up_and_stops() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(START);
    let state = state(dir.path(), &clock).await;
    let backups = dir.path().join("backups");
    let schedule = BackupSchedule {
        every: Duration::from_secs(60 * 60),
        keep: 7,
        dir: backups.clone(),
        copy_to: None,
    };
    let (stop_tx, stop) = Stop::new();
    let task = tokio::spawn(run_backups(state, schedule, stop));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !backups.join("lode-backup-20261101T030000Z").is_dir() {
        assert!(std::time::Instant::now() < deadline, "no backup");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    stop_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("stops")
        .unwrap();
}
