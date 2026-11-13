//! T080 acceptance: header timeouts and the per-address connection limit, so the server can
//! face the internet without a reverse proxy.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lode_server::{init, open_state, BoundServer, ServerConfig, SystemClock};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

struct Running {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Running {
    async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .expect("server stops")
            .unwrap()
            .unwrap();
    }
}

async fn start(dir: &Path, cfg: impl FnOnce(&mut ServerConfig)) -> Running {
    let data = dir.join("data");
    init(&data).await.unwrap();
    let mut config = ServerConfig::with_data_dir(&data);
    config.listen = "127.0.0.1:0".parse().unwrap();
    cfg(&mut config);
    let state = open_state(config.clone(), Arc::new(SystemClock), Vec::new())
        .await
        .unwrap();
    let server = BoundServer::bind(&config, lode_server::router(state))
        .await
        .unwrap();
    let addr = server.local_addr();
    let (tx, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(server.run(async {
        let _ = rx.await;
    }));
    Running {
        addr,
        stop: Some(tx),
        task,
    }
}

/// Reads until the peer closes the connection; `None` if it is still open after `limit`.
async fn closed_within<R: AsyncRead + Unpin>(r: &mut R, limit: Duration) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    let read = async {
        loop {
            match r.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => out.extend_from_slice(&buf[..n]),
            }
        }
    };
    tokio::time::timeout(limit, read).await.ok().map(|()| out)
}

/// Sends one keep-alive request and reads its response (the info body is small JSON).
async fn request(stream: &mut TcpStream) -> String {
    stream
        .write_all(b"GET /api/v1/info HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(5);
    while !String::from_utf8_lossy(&out).contains("\"api_version\"") {
        let left = deadline.saturating_duration_since(Instant::now());
        let n = tokio::time::timeout(left, stream.read(&mut buf))
            .await
            .expect("response in time")
            .unwrap();
        assert!(n > 0, "closed early: {}", String::from_utf8_lossy(&out));
        out.extend_from_slice(&buf[..n]);
    }
    String::from_utf8(out).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_headers_and_idle_connections_are_closed() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), |c| c.connections.header_timeout_seconds = 1).await;

    // Headers trickling in one byte at a time never finish in time.
    let mut slow = TcpStream::connect(server.addr).await.unwrap();
    let started = Instant::now();
    let trickle = b"GET /api/v1/info HTTP/1.1\r\nHost: x\r\nX-Padding: aaaaaaaaaaaaaaaa\r\n\r\n";
    let mut cut_off = false;
    for b in trickle {
        if slow.write_all(&[*b]).await.is_err() {
            cut_off = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        if started.elapsed() > Duration::from_secs(5) {
            break;
        }
    }
    let rest = closed_within(&mut slow, Duration::from_secs(3)).await;
    assert!(cut_off || rest.is_some(), "slow client still connected");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(900),
        "closed too early: {elapsed:?}"
    );
    if let Some(rest) = rest {
        let text = String::from_utf8_lossy(&rest);
        assert!(!text.contains("api_version"), "{text}");
    }

    // A connection that never sends anything is closed too.
    let mut silent = TcpStream::connect(server.addr).await.unwrap();
    assert!(closed_within(&mut silent, Duration::from_secs(3))
        .await
        .is_some());

    // A kept-alive connection serves several requests, then is closed once idle.
    let mut kept = TcpStream::connect(server.addr).await.unwrap();
    assert!(request(&mut kept).await.starts_with("HTTP/1.1 200"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(request(&mut kept).await.starts_with("HTTP/1.1 200"));
    let idle = Instant::now();
    assert!(closed_within(&mut kept, Duration::from_secs(3))
        .await
        .is_some());
    assert!(idle.elapsed() >= Duration::from_millis(900));

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connections_past_the_per_address_limit_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), |c| c.connections.max_per_address = 2).await;

    let mut first = TcpStream::connect(server.addr).await.unwrap();
    let mut second = TcpStream::connect(server.addr).await.unwrap();
    assert!(request(&mut first).await.starts_with("HTTP/1.1 200"));
    assert!(request(&mut second).await.starts_with("HTTP/1.1 200"));

    // The third from the same address is closed without an answer.
    let mut third = TcpStream::connect(server.addr).await.unwrap();
    let _ = third
        .write_all(b"GET /api/v1/info HTTP/1.1\r\nHost: x\r\n\r\n")
        .await;
    let answer = closed_within(&mut third, Duration::from_secs(3))
        .await
        .expect("refused connection is closed");
    assert!(answer.is_empty(), "{}", String::from_utf8_lossy(&answer));

    // Another address still connects (Linux routes all of 127.0.0.0/8 to loopback).
    #[cfg(target_os = "linux")]
    {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.2:0".parse().unwrap()).unwrap();
        let mut other = socket.connect(server.addr).await.unwrap();
        assert!(request(&mut other).await.starts_with("HTTP/1.1 200"));
    }

    // Closing one frees its slot.
    drop(first);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut again = TcpStream::connect(server.addr).await.unwrap();
        let _ = again
            .write_all(b"GET /api/v1/info HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await;
        let answer = closed_within(&mut again, Duration::from_secs(3))
            .await
            .unwrap_or_default();
        if String::from_utf8_lossy(&answer).starts_with("HTTP/1.1 200") {
            break;
        }
        assert!(Instant::now() < deadline, "slot was not freed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    drop(second);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http2_over_tls_is_served_and_silent_clients_are_closed() {
    let dir = tempfile::tempdir().unwrap();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    std::fs::write(dir.path().join("cert.pem"), cert.cert.pem()).unwrap();
    std::fs::write(dir.path().join("key.pem"), cert.signing_key.serialize_pem()).unwrap();
    let server = start(dir.path(), |c| {
        c.tls = Some(lode_server::TlsConfig::Files {
            cert: dir.path().join("cert.pem"),
            key: dir.path().join("key.pem"),
        });
        c.connections.header_timeout_seconds = 1;
    })
    .await;

    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.cert.der().clone()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client.alpn_protocols = vec![b"h2".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();

    // A request over HTTP/2.
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let tls = connector.connect(name.clone(), tcp).await.unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let (mut send, conn) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        hyper_util::rt::TokioIo::new(tls),
    )
    .await
    .unwrap();
    tokio::spawn(conn);
    let req = hyper::Request::get(format!(
        "https://localhost:{}/api/v1/info",
        server.addr.port()
    ))
    .body(http_body_util::Empty::<bytes::Bytes>::new())
    .unwrap();
    let resp = send.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.version(), hyper::Version::HTTP_2);
    drop(send);

    // A client that finishes the handshake but never sends a request is closed.
    let tcp = TcpStream::connect(server.addr).await.unwrap();
    let mut tls = connector.connect(name, tcp).await.unwrap();
    let started = Instant::now();
    assert!(closed_within(&mut tls, Duration::from_secs(3))
        .await
        .is_some());
    assert!(started.elapsed() >= Duration::from_millis(900));

    server.stop().await;
}
