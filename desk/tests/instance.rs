//! T135: one copy of the app per user. A second start hands its `lode://` link to the first
//! over the local socket and exits; a start without a link only brings the window forward.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::mpsc;
use std::time::Duration;

use lode_desk::instance::{self, Start};

#[test]
fn a_second_start_hands_its_link_to_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let Start::First(first) = instance::claim(dir.path(), None).unwrap() else {
        panic!("the first start should become the first copy");
    };
    let (tx, rx) = mpsc::channel();
    first.serve(move |link| {
        let _ = tx.send(link);
    });

    let link = "lode://lode.example.com/game/r42";
    assert!(matches!(
        instance::claim(dir.path(), Some(link)).unwrap(),
        Start::HandedOver
    ));
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap().as_deref(),
        Some(link)
    );

    // Without a link, the first copy is only asked to come forward.
    assert!(matches!(
        instance::claim(dir.path(), None).unwrap(),
        Start::HandedOver
    ));
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), None);
}

#[test]
fn a_message_without_the_token_is_ignored() {
    use std::io::{BufRead, BufReader, Write};
    let dir = tempfile::tempdir().unwrap();
    let Start::First(first) = instance::claim(dir.path(), None).unwrap() else {
        panic!("the first start should become the first copy");
    };
    let (tx, rx) = mpsc::channel();
    first.serve(move |link| {
        let _ = tx.send(link);
    });
    let address: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("instance.json")).unwrap()).unwrap();
    let port = address["port"].as_u64().unwrap();
    let mut s = std::net::TcpStream::connect(("127.0.0.1", u16::try_from(port).unwrap())).unwrap();
    s.write_all(b"{\"token\":\"guess\",\"link\":\"lode://h/game/r1\"}\n")
        .unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut answer = String::new();
    let _ = BufReader::new(&s).read_line(&mut answer);
    assert_eq!(answer, "", "no answer without the token");
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
}

#[test]
fn the_lock_is_free_again_once_the_first_copy_ends() {
    let dir = tempfile::tempdir().unwrap();
    let first = instance::claim(dir.path(), None).unwrap();
    assert!(matches!(first, Start::First(_)));
    drop(first);
    assert!(matches!(
        instance::claim(dir.path(), None).unwrap(),
        Start::First(_)
    ));
}

#[test]
fn a_held_lock_with_no_answer_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    // The first copy holds the lock but never serves.
    let _first = instance::claim(dir.path(), None).unwrap();
    std::fs::remove_file(dir.path().join("instance.json")).unwrap();
    assert!(!instance::hand_over(
        dir.path(),
        Some("lode://h/game/r1"),
        Duration::from_millis(300)
    ));
}
