//! T122 acceptance: single sign-on through an OpenID Connect provider, against the test
//! provider of `lode-testkit` (its own keys, discovery and token endpoint).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::time::Duration;

use lode_client::Error;
use lode_proto::admin::{CreatePrincipalRequest, PatchPrincipalRequest};
use lode_proto::auth::LoginRequest;
use lode_proto::PrincipalKind;
use lode_server::{OidcConfig, PasswordLogin};
use lode_testkit::oidc::{browse, Alg, TestProvider};
use lode_testkit::{TestServer, TestServerOptions, TestWorkspace};
use serde_json::{json, Value};

const CLIENT_ID: &str = "lode-test";
const SECRET: &str = "s3cret";

async fn server(provider: &TestProvider, edit: fn(OidcConfig) -> OidcConfig) -> TestServer {
    let cfg = edit(
        OidcConfig::new(provider.issuer(), CLIENT_ID)
            .unwrap()
            .with_client_secret(SECRET),
    );
    TestServer::start_with(TestServerOptions {
        configure: Box::new(move |c| c.oidc = Some(cfg)),
    })
    .await
}

fn with_domains(mut c: OidcConfig) -> OidcConfig {
    c.allowed_domains = vec!["example.com".into()];
    c.label = "Example Corp".into();
    c
}

fn auto_create_admins_only(mut c: OidcConfig) -> OidcConfig {
    c.auto_create = true;
    c.password_login = PasswordLogin::Admins;
    c
}

async fn add_user(s: &TestServer, name: &str, sso: &str, password: Option<&str>) {
    s.admin()
        .create_principal(&CreatePrincipalRequest {
            kind: PrincipalKind::User,
            name: name.into(),
            display_name: None,
            owner: None,
            password: password.map(str::to_owned),
            sso: Some(sso.into()),
        })
        .await
        .unwrap();
}

fn email(provider: &TestProvider, address: &str) {
    provider.set(|b| {
        b.claims.insert("email".into(), json!(address));
        b.claims.insert("email_verified".into(), json!(true));
    });
}

/// A whole sign-in as a desktop program does it, with the test playing the browser.
async fn sign_in(s: &TestServer) -> Result<lode_proto::auth::LoginResponse, Error> {
    let pending = s.anonymous().begin_sso(Some("test")).await.unwrap();
    let url = pending.url().to_owned();
    let wait = tokio::spawn(pending.finish(Duration::from_secs(30)));
    let page = browse(&url).await;
    assert!(
        page.url.contains("/lode-sso?"),
        "the browser ends at the loopback address: {page:?}"
    );
    wait.await.unwrap()
}

fn refusal(r: Result<lode_proto::auth::LoginResponse, Error>) -> String {
    match r {
        Err(Error::SignIn(m)) => m,
        other => panic!("expected a refused sign-in, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sign_in_issues_a_token_and_each_bad_token_is_refused() {
    let provider = TestProvider::start(CLIENT_ID, SECRET).await;
    let s = server(&provider, with_domains).await;
    add_user(&s, "ana", "Ana@Example.com", None).await;

    // The server says it offers single sign-on.
    let info = s.anonymous().info().await.unwrap();
    let sso = info.sso.expect("sso in /info");
    assert_eq!(sso.label, "Example Corp");
    assert!(sso.password_login);

    // A full sign-in, RS256, matching the identity case-insensitively.
    email(&provider, "ana@example.com");
    let resp = sign_in(&s).await.unwrap();
    assert_eq!(resp.principal.name, "ana");
    assert_eq!(resp.principal.sso.as_deref(), Some("ana@example.com"));
    let me = s.client(&resp.token).whoami().await.unwrap();
    assert_eq!(me.principal.name, "ana");
    assert!(me.scopes.iter().any(|x| x == "write"), "default scopes");

    // ES256 works too.
    provider.set(|b| b.alg = Alg::Es256);
    assert_eq!(sign_in(&s).await.unwrap().principal.name, "ana");
    provider.set(|b| b.alg = Alg::Rs256);

    // Wrong audience.
    provider.set(|b| b.audience = Some("someone-else".into()));
    assert!(refusal(sign_in(&s).await).contains("another application"));
    provider.set(|b| b.audience = None);

    // Expired.
    provider.set(|b| b.expires_in = -600);
    assert!(refusal(sign_in(&s).await).contains("expired"));
    provider.set(|b| b.expires_in = 300);

    // Bad signature.
    provider.set(|b| b.tamper = true);
    assert!(refusal(sign_in(&s).await).contains("signature"));
    provider.set(|b| b.tamper = false);

    // A reused nonce: the token carries the nonce of an earlier sign-in.
    let old = provider.nonces().first().cloned().unwrap();
    provider.set(|b| b.nonce = Some(old));
    assert!(refusal(sign_in(&s).await).contains("nonce"));
    provider.set(|b| b.nonce = None);

    // A domain that is not allowed.
    email(&provider, "eve@other.org");
    assert!(refusal(sign_in(&s).await).contains("not in a domain allowed"));

    // auto_create off: an allowed domain, but nobody is linked to it.
    email(&provider, "bob@example.com");
    assert!(refusal(sign_in(&s).await).contains("no Lode user is linked"));

    // An unverified address.
    provider.set(|b| {
        b.claims.insert("email".into(), json!("ana@example.com"));
        b.claims.insert("email_verified".into(), json!(false));
    });
    assert!(refusal(sign_in(&s).await).contains("not verified"));

    // Disabled Lode users stay refused, whatever the provider says.
    email(&provider, "ana@example.com");
    s.admin()
        .patch_principal(
            "ana",
            &PatchPrincipalRequest {
                disabled: Some(true),
                password: None,
                sso: None,
            },
        )
        .await
        .unwrap();
    assert!(refusal(sign_in(&s).await).contains("disabled"));

    // An answer for a sign-in the server does not know gets a page, not a redirect.
    let page = browse(&format!(
        "{}/api/v1/auth/sso/callback?state=unknown&code=x",
        s.url()
    ))
    .await;
    assert_eq!(page.status, 400);
    assert!(page.body.contains("expired or was already used"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_time_codes_need_the_verifier_and_work_once() {
    let provider = TestProvider::start(CLIENT_ID, SECRET).await;
    let s = server(&provider, |c| c).await;
    add_user(&s, "ana", "ana@example.com", None).await;
    email(&provider, "ana@example.com");

    // Play the browser with a loopback address nobody listens on, and stop there.
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let start = format!(
        "{}/api/v1/auth/sso/start?redirect_uri={}&state=state-123&code_challenge={challenge}",
        s.url(),
        "http%3A%2F%2F127.0.0.1%3A9%2Fcb"
    );
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let mut url = start;
    let code = loop {
        let resp = http.get(&url).send().await.unwrap();
        let loc = resp.headers()["location"].to_str().unwrap().to_owned();
        let next = reqwest::Url::parse(&url).unwrap().join(&loc).unwrap();
        if next.port() == Some(9) {
            let q: std::collections::HashMap<_, _> = next.query_pairs().into_owned().collect();
            assert_eq!(q["state"], "state-123");
            break q["code"].clone();
        }
        url = next.to_string();
    };
    let token = |code: &str, verifier: &str| {
        let http = http.clone();
        let url = format!("{}/api/v1/auth/sso/token", s.url());
        let body = json!({ "code": code, "code_verifier": verifier });
        async move {
            let r = http.post(url).json(&body).send().await.unwrap();
            (r.status().as_u16(), r.json::<Value>().await.unwrap())
        }
    };
    // A wrong verifier spends the code.
    let (status, body) = token(&code, &"x".repeat(43)).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (401, Some("unauthenticated"))
    );
    let (status, _) = token(&code, verifier).await;
    assert_eq!(status, 401, "a code works once");
    let _ = provider;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_create_and_password_sign_in_for_admins_only() {
    let provider = TestProvider::start(CLIENT_ID, SECRET).await;
    let s = server(&provider, auto_create_admins_only).await;
    assert!(
        !s.anonymous()
            .info()
            .await
            .unwrap()
            .sso
            .unwrap()
            .password_login
    );

    // auto_create on: the first sign-in creates the user, named after the address.
    email(&provider, "Carla.Diaz@example.com");
    provider.set(|b| {
        b.claims.insert("name".into(), json!("Carla Díaz"));
    });
    let resp = sign_in(&s).await.unwrap();
    assert_eq!(resp.principal.name, "carla.diaz");
    assert_eq!(resp.principal.display_name.as_deref(), Some("Carla Díaz"));
    // The second sign-in finds the same user.
    assert_eq!(sign_in(&s).await.unwrap().principal.name, "carla.diaz");
    // Another address with the same local part gets a name of its own.
    email(&provider, "carla.diaz@example.org");
    assert_eq!(sign_in(&s).await.unwrap().principal.name, "carla.diaz-2");

    // Password sign-in is off for everyone but server admins.
    add_user(&s, "dan", "dan@example.com", Some("correct horse battery")).await;
    let err = s
        .anonymous()
        .login(&LoginRequest {
            name: "dan".into(),
            password: "correct horse battery".into(),
            token_name: None,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("single sign-on"), "{err}");
    // The built-in admin still can.
    s.admin()
        .patch_principal(
            "admin",
            &PatchPrincipalRequest {
                disabled: None,
                password: Some("admin password 1".into()),
                sso: None,
            },
        )
        .await
        .unwrap();
    s.anonymous()
        .login(&LoginRequest {
            name: "admin".into(),
            password: "admin password 1".into(),
            token_name: None,
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_oidc_the_routes_say_so() {
    let s = TestServer::start().await;
    assert!(s.anonymous().info().await.unwrap().sso.is_none());
    let page = browse(&format!(
        "{}/api/v1/auth/sso/start?redirect_uri=x&state=y&code_challenge=z",
        s.url()
    ))
    .await;
    assert_eq!(page.status, 404);
    assert!(page.body.contains("not set up"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_login_with_sso_receives_the_token_on_its_loopback() {
    let provider = TestProvider::start(CLIENT_ID, SECRET).await;
    let s = server(&provider, |c| c).await;
    let ws = TestWorkspace::detached();
    add_user(&s, "ana", "ana@example.com", None).await;
    email(&provider, "ana@example.com");

    let mut child = ws
        .std_cmd()
        .args(["--json", "login", s.url(), "--sso", "--no-browser"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Read stderr to the end on a thread of its own: closing it early would make the
    // child's next message fail.
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if let Some(u) = line.trim().strip_prefix("http") {
                let _ = tx.send(format!("http{u}"));
            }
        }
    });
    let url = tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(30)))
        .await
        .unwrap()
        .expect("lode printed the address");
    assert!(url.contains("/api/v1/auth/sso/start?"), "{url}");
    let page = browse(&url).await;
    assert!(page.body.contains("Signed in"), "{page:?}");
    let out = tokio::task::spawn_blocking(move || child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["name"], "ana");
    assert_eq!(v["sso"], true);
    let me = ws.json(&["whoami"]);
    assert_eq!(me["name"], "ana");

    // A server without single sign-on says so.
    let plain = TestServer::start().await;
    let (code, v) = ws.json_status(&["login", plain.url(), "--sso", "--no-browser"]);
    assert_ne!(code, 0);
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("does not offer single sign-on"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admins_link_users_from_the_cli() {
    let provider = TestProvider::start(CLIENT_ID, SECRET).await;
    let s = server(&provider, |c| c).await;
    let ws = TestWorkspace::new(&s).with_token(s.admin_token());
    let v = ws.json(&[
        "admin",
        "user",
        "create",
        "erin",
        "--sso",
        "Erin@Example.com",
    ]);
    assert_eq!(v["sso"], "erin@example.com");
    // The identity is unique.
    let (code, v) = ws.json_status(&[
        "admin",
        "user",
        "create",
        "erin2",
        "--sso",
        "erin@example.com",
    ]);
    assert_ne!(code, 0);
    assert_eq!(v["error"]["code"], "conflict");
    let v = ws.json(&["admin", "user", "sso", "erin", "--remove"]);
    assert!(v.get("sso").is_none());
    let v = ws.json(&["admin", "user", "sso", "erin", "erin@corp.example"]);
    assert_eq!(v["sso"], "erin@corp.example");
    email(&provider, "erin@corp.example");
    assert_eq!(sign_in(&s).await.unwrap().principal.name, "erin");
}
