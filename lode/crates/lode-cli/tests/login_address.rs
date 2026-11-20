//! `lode login` with an address that is not the server's: the error says so and, when the
//! server answers at the same host without the path, names the address to use.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lode_testkit::{TestServer, TestWorkspace};
use serde_json::Value;

fn login(ws: &TestWorkspace, url: &str) -> (i32, Value) {
    let out = ws
        .cmd()
        .args([
            "--json",
            "login",
            url,
            "--name",
            "admin",
            "--password-stdin",
        ])
        .write_stdin("wrong\n")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{e}\n{text}\n{}", String::from_utf8_lossy(&out.stderr)));
    (out.status.code().unwrap_or(-1), v)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_address_with_a_path_names_the_server_address() {
    let server = TestServer::start().await;
    let ws = TestWorkspace::detached();
    let root = server.url().trim_end_matches('/').to_owned();

    // The web admin pages, and a path nothing answers at.
    for path in ["/admin/", "/depots/game"] {
        let (code, v) = login(&ws, &format!("{root}{path}"));
        assert_ne!(code, 0, "{v}");
        assert_eq!(v["error"]["code"], "not_a_lode_server", "{path}: {v}");
        assert_eq!(
            v["error"]["details"]["suggested"],
            root.as_str(),
            "{path}: {v}"
        );
        let message = v["error"]["message"].as_str().unwrap();
        assert!(message.contains(&root), "{message}");
    }

    // The right address reaches the sign-in itself, which refuses the wrong password.
    let (code, v) = login(&ws, &root);
    assert_ne!(code, 0, "{v}");
    assert_eq!(v["error"]["code"], "unauthenticated", "{v}");
}
