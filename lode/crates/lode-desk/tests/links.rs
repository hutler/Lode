//! T113: `lode://` links are read and written by the same rules as the window
//! (`ui/src/links.ts`), checked against the shared cases in `ui/src/linkCases.json`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_desk::links::{self, Link, MAX_LINK};
use serde_json::Value;

const CASES: &str = include_str!("../ui/src/linkCases.json");

fn cases() -> Vec<Value> {
    let doc: Value = serde_json::from_str(CASES).unwrap();
    doc["cases"].as_array().unwrap().clone()
}

#[test]
fn every_shared_case_is_read_as_listed() {
    let all = cases();
    assert!(all.len() > 40);
    for case in all {
        let text = case["text"].as_str().unwrap();
        let got = links::parse(text);
        if case["link"].is_null() {
            assert!(got.is_err(), "{text} should be refused, read as {got:?}");
        } else {
            let want: Link = serde_json::from_value(case["link"].clone()).unwrap();
            assert_eq!(got.as_ref().ok(), Some(&want), "{text}");
        }
    }
}

#[test]
fn links_built_from_a_target_read_back_to_it() {
    for case in cases() {
        if case["link"].is_null() {
            continue;
        }
        let link: Link = serde_json::from_value(case["link"].clone()).unwrap();
        let built = links::build(&link).unwrap();
        if case["build"].as_bool() == Some(true) {
            assert_eq!(built, case["text"].as_str().unwrap());
        }
        assert_eq!(links::parse(&built).unwrap(), link, "{built}");
    }
}

#[test]
fn long_and_unbuildable_links_are_refused() {
    let long = format!("lode://h/game/{}@1", "a".repeat(MAX_LINK));
    assert!(links::parse(&long).is_err());
    let file = |server: &str, path: &str, rev: Option<u64>, branch: Option<&str>| Link::File {
        server: server.into(),
        depot: "game".into(),
        path: path.into(),
        rev,
        branch: branch.map(Into::into),
    };
    assert!(links::build(&file("https://h", "a.txt", None, None)).is_err());
    assert!(links::build(&file("https://h/lode", "a.txt", Some(1), None)).is_err());
    assert!(links::build(&file("ftp://h", "a.txt", Some(1), None)).is_err());
    assert!(links::build(&file("https://h", "a/../b", Some(1), None)).is_err());
    assert!(links::build(&file("https://h", "a.txt", None, Some("x/../y"))).is_err());
    assert!(links::build(&file("https://h", "r1", None, Some("main"))).is_err());
    assert!(links::build(&file("https://h", "shelf/a.txt", None, Some("main"))).is_err());
    assert!(links::build(&file("https://h", "r1", Some(3), Some("main"))).is_ok());
    assert_eq!(
        links::build(&file("https://H:443/", "a b/c.txt", Some(2), None)).unwrap(),
        "lode://h:443/game/a%20b/c.txt@2"
    );
}

#[test]
fn the_link_is_found_among_launch_arguments() {
    let args = ["lode-desk.exe", "--flag", "LODE://h/game/r1"];
    assert_eq!(links::from_args(args).as_deref(), Some("LODE://h/game/r1"));
    assert_eq!(links::from_args(["lode-desk"]), None);
}
