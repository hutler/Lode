//! T081 acceptance: built-in TLS from an ACME server, against a local Pebble test server.
//!
//! Needs the `pebble` and `pebble-challtestsrv` binaries, named by the `PEBBLE` and
//! `PEBBLE_CHALLTESTSRV` environment variables (CI installs them with
//! `go install github.com/letsencrypt/pebble/v2/cmd/...`). Without them the test says so and
//! passes, so a plain `cargo test` works anywhere.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lode_server::{
    init, open_state, AcmeConfig, BoundServer, ServerConfig, SystemClock, TlsConfig,
};
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const DOMAIN: &str = "lode.example.com";

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn client_config(roots: rustls::RootCertStore) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// One HTTPS GET with HTTP/1.1; the response text and the server's leaf certificate.
async fn https_get(
    addr: SocketAddr,
    name: &str,
    config: Arc<rustls::ClientConfig>,
    path: &str,
) -> std::io::Result<(String, CertificateDer<'static>)> {
    let tcp = TcpStream::connect(addr).await?;
    let connector = tokio_rustls::TlsConnector::from(config);
    let server_name = ServerName::try_from(name.to_owned()).map_err(std::io::Error::other)?;
    let mut tls = connector.connect(server_name, tcp).await?;
    let leaf = tls.get_ref().1.peer_certificates().unwrap()[0]
        .clone()
        .into_owned();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {name}\r\nConnection: close\r\n\r\n");
    tls.write_all(req.as_bytes()).await?;
    let mut out = Vec::new();
    // Pebble closes without close_notify; what arrived before that is enough.
    let _ = tls.read_to_end(&mut out).await;
    Ok((String::from_utf8_lossy(&out).into_owned(), leaf))
}

async fn wait_for_port(addr: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while TcpStream::connect(addr).await.is_err() {
        assert!(Instant::now() < deadline, "{addr} did not start");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

struct Pebble {
    _pebble: Killed,
    _dns: Killed,
    directory: String,
    management: SocketAddr,
    ca_file: PathBuf,
    ca: rustls::RootCertStore,
}

/// Starts Pebble, validating TLS-ALPN-01 on `tls_port`, with every name resolving to
/// 127.0.0.1. Its own HTTPS certificate is made here.
async fn start_pebble(dir: &Path, pebble: &str, challtestsrv: &str, tls_port: u16) -> Pebble {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let params =
        rcgen::CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .unwrap();
    let cert = params.signed_by(&key, &ca).unwrap();
    let ca_file = dir.join("pebble-ca.pem");
    std::fs::write(&ca_file, ca.as_ref().pem()).unwrap();
    std::fs::write(dir.join("pebble.pem"), cert.pem()).unwrap();
    std::fs::write(dir.join("pebble.key"), key.serialize_pem()).unwrap();

    let (listen, management, dns) = (free_port(), free_port(), free_port());
    let config = serde_json::json!({ "pebble": {
        "listenAddress": format!("127.0.0.1:{listen}"),
        "managementListenAddress": format!("127.0.0.1:{management}"),
        "certificate": dir.join("pebble.pem"),
        "privateKey": dir.join("pebble.key"),
        "httpPort": 5002,
        "tlsPort": tls_port,
        "ocspResponderURL": "",
        "externalAccountBindingRequired": false,
    }});
    let config_file = dir.join("pebble.json");
    std::fs::write(&config_file, config.to_string()).unwrap();

    let dns_server = Killed(
        Command::new(challtestsrv)
            .args(["-management", &format!("127.0.0.1:{}", free_port())])
            .args(["-dnsserver", &format!("127.0.0.1:{dns}")])
            .args(["-http01", "", "-https01", "", "-tlsalpn01", "", "-doh", ""])
            .args(["-defaultIPv4", "127.0.0.1", "-defaultIPv6", ""])
            .stdout(Stdio::null())
            .spawn()
            .expect("start pebble-challtestsrv"),
    );
    let server = Killed(
        Command::new(pebble)
            .arg("-config")
            .arg(&config_file)
            .args(["-dnsserver", &format!("127.0.0.1:{dns}")])
            .env("PEBBLE_VA_NOSLEEP", "1")
            .env("PEBBLE_WFE_NONCEREJECT", "0")
            .stdout(Stdio::null())
            .spawn()
            .expect("start pebble"),
    );
    wait_for_port(([127, 0, 0, 1], listen).into()).await;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.as_ref().der().clone()).unwrap();
    Pebble {
        _pebble: server,
        _dns: dns_server,
        directory: format!("https://127.0.0.1:{listen}/dir"),
        management: ([127, 0, 0, 1], management).into(),
        ca_file,
        ca: roots,
    }
}

async fn run(
    config: &ServerConfig,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let state = open_state(config.clone(), Arc::new(SystemClock), Vec::new())
        .await
        .unwrap();
    let server = BoundServer::bind(config, lode_server::router(state))
        .await
        .unwrap();
    assert_eq!(server.scheme(), "https");
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        server
            .run(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });
    (tx, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn obtains_a_certificate_and_reuses_it_after_a_restart() {
    let (Ok(pebble_bin), Ok(challtestsrv)) = (
        std::env::var("PEBBLE"),
        std::env::var("PEBBLE_CHALLTESTSRV"),
    ) else {
        eprintln!("skipped: set PEBBLE and PEBBLE_CHALLTESTSRV to run the ACME test");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let pebble = start_pebble(dir.path(), &pebble_bin, &challtestsrv, port).await;

    let data = dir.path().join("data");
    init(&data).await.unwrap();
    let mut config = ServerConfig::with_data_dir(&data);
    config.listen = addr;
    config.tls = Some(TlsConfig::Acme(AcmeConfig {
        domain: DOMAIN.to_owned(),
        email: Some("ops@example.com".to_owned()),
        directory: pebble.directory.clone(),
        ca: Some(pebble.ca_file.clone()),
    }));
    let (stop, task) = run(&config).await;

    // Pebble's issuing root, from its management interface.
    let (roots, _) = https_get(
        pebble.management,
        "localhost",
        client_config(pebble.ca.clone()),
        "/roots/0",
    )
    .await
    .unwrap();
    let pem = &roots[roots.find("-----BEGIN").expect("root PEM")..];
    let mut issued_by = rustls::RootCertStore::empty();
    for cert in rustls::pki_types::pem::PemObject::pem_slice_iter(pem.as_bytes()) {
        issued_by.add(cert.unwrap()).unwrap();
    }
    let client = client_config(issued_by);

    // The server answers over HTTPS once the certificate is issued.
    let deadline = Instant::now() + Duration::from_secs(60);
    let first = loop {
        match https_get(addr, DOMAIN, Arc::clone(&client), "/api/v1/info").await {
            Ok((text, leaf)) if text.starts_with("HTTP/1.1 200") => break leaf,
            Ok(_) | Err(_) => {
                assert!(Instant::now() < deadline, "no certificate within 60 s");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };
    let cached: Vec<_> = std::fs::read_dir(data.join("acme"))
        .unwrap()
        .map(|e| e.unwrap().path().join(format!("{DOMAIN}.crt.pem")))
        .filter(|p| p.is_file())
        .collect();
    assert_eq!(cached.len(), 1, "one cached certificate");

    // After a restart, with the ACME server gone, the cached certificate is served at once.
    stop.send(()).unwrap();
    task.await.unwrap();
    drop(pebble);
    let (stop, task) = run(&config).await;
    let (text, leaf) = https_get(addr, DOMAIN, client, "/api/v1/info")
        .await
        .unwrap();
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert_eq!(leaf, first, "the same certificate");
    stop.send(()).unwrap();
    task.await.unwrap();
}
