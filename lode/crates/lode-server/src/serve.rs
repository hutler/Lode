//! The connection loop: accepting TCP connections, the per-address connection limit, TLS
//! handshakes, HTTP/1.1 and HTTP/2 with header timeouts, and graceful shutdown.
//!
//! This replaces `axum::serve` so the server can face the internet without a reverse proxy:
//! a client must send its request headers within the header timeout (also while a kept-alive
//! connection waits for its next request), and one address cannot hold more than a set number
//! of connections.

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::Router;
use hyper::body::Incoming;
use hyper::server::conn::{http1, http2};
use hyper::Request;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::graceful::{GracefulShutdown, Watcher};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tower::Service;

use crate::server::ClientInfo;

/// How long a client may take to finish the TLS handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Connection settings from `[connections]`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Settings {
    /// Time allowed for a request's headers, and for the next request on an idle connection.
    pub header_timeout: Duration,
    /// Most open connections from one address (one /64 network for IPv6).
    pub max_per_address: usize,
}

/// Counts open connections per client address.
#[derive(Clone)]
pub(crate) struct Limiter {
    open: Arc<Mutex<HashMap<IpAddr, usize>>>,
    max: usize,
}

/// One counted connection; dropping it frees the slot.
pub(crate) struct Permit {
    limiter: Limiter,
    key: IpAddr,
}

impl Limiter {
    pub(crate) fn new(max: usize) -> Self {
        Self {
            open: Arc::default(),
            max,
        }
    }

    /// A permit for one more connection from `ip`, or `None` when it already has the most.
    pub(crate) fn try_acquire(&self, ip: IpAddr) -> Option<Permit> {
        let key = limit_key(ip);
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        let n = open.entry(key).or_insert(0);
        if *n >= self.max {
            return None;
        }
        *n += 1;
        Some(Permit {
            limiter: self.clone(),
            key,
        })
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut open = self.limiter.open.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = open.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                open.remove(&self.key);
            }
        }
    }
}

/// The address a connection is counted under: IPv4 as is (also when mapped into IPv6), IPv6
/// by its /64 network, since one host can easily use many addresses of its network.
fn limit_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => {
            let net = u128::from(v6) & !((1u128 << 64) - 1);
            IpAddr::V6(Ipv6Addr::from(net))
        }
    }
}

/// Accepts connections on `tcp` until `shutdown` completes, then stops accepting and waits
/// for open connections to finish their in-flight requests.
pub(crate) async fn serve<F>(
    tcp: TcpListener,
    tls: Option<TlsAcceptor>,
    router: Router,
    settings: Settings,
    shutdown: F,
) where
    F: Future<Output = ()> + Send + 'static,
{
    let graceful = GracefulShutdown::new();
    let limiter = Limiter::new(settings.max_per_address);
    tokio::pin!(shutdown);
    loop {
        let (stream, addr) = tokio::select! {
            accepted = tcp.accept() => match accepted {
                Ok(c) => c,
                Err(e) => {
                    // Usually out of file descriptors; give the system a moment.
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let Some(permit) = limiter.try_acquire(addr.ip()) else {
            tracing::debug!(%addr, "connection refused: too many from this address");
            continue;
        };
        if let Err(e) = stream.set_nodelay(true) {
            tracing::debug!(%addr, error = %e, "cannot set TCP_NODELAY");
        }
        let router = router.clone();
        let tls = tls.clone();
        let watcher = graceful.watcher();
        tokio::spawn(async move {
            let _permit = permit;
            match tls {
                None => connection(stream, false, addr, router, settings, watcher).await,
                Some(acceptor) => {
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(stream)) => {
                            let alpn = stream.get_ref().1.alpn_protocol();
                            if alpn == Some(crate::acme::ACME_TLS_ALPN) {
                                // An ACME validation: the handshake was all it needed.
                                return;
                            }
                            let h2 = alpn == Some(b"h2");
                            connection(stream, h2, addr, router, settings, watcher).await;
                        }
                        Ok(Err(e)) => tracing::debug!(%addr, error = %e, "TLS handshake failed"),
                        Err(_) => tracing::debug!(%addr, "TLS handshake timed out"),
                    }
                }
            }
        });
    }
    drop(tcp);
    graceful.shutdown().await;
}

/// Serves one connection: HTTP/2 when TLS negotiated it, HTTP/1.1 otherwise.
async fn connection<I>(
    io: I,
    h2: bool,
    addr: SocketAddr,
    router: Router,
    settings: Settings,
    watcher: Watcher,
) where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let seen = Arc::new(AtomicBool::new(false));
    let service = {
        let seen = Arc::clone(&seen);
        hyper::service::service_fn(move |mut req: Request<Incoming>| {
            seen.store(true, Ordering::Relaxed);
            // Handlers read the client address (login rate limiting, logs).
            req.extensions_mut().insert(ConnectInfo(ClientInfo(addr)));
            // A router is always ready, so `call` needs no `poll_ready` first.
            let mut router = router.clone();
            async move { router.call(req).await }
        })
    };
    // The first request must arrive in time whatever the protocol (HTTP/1.1 also enforces it
    // per request; for HTTP/2 this covers a slow connection preface).
    let first_request = async {
        tokio::time::sleep(settings.header_timeout).await;
        if seen.load(Ordering::Relaxed) {
            std::future::pending::<()>().await;
        }
    };
    let io = TokioIo::new(io);
    let t = settings.header_timeout;
    let result = if h2 {
        let mut builder = http2::Builder::new(TokioExecutor::new());
        builder
            .timer(TokioTimer::new())
            // Dead peers are noticed and their connections closed.
            .keep_alive_interval(t)
            .keep_alive_timeout(t);
        let conn = watcher.watch(builder.serve_connection(io, service));
        tokio::select! {
            r = conn => r,
            () = first_request => {
                tracing::debug!(%addr, "no request within the header timeout");
                return;
            }
        }
    } else {
        let mut builder = http1::Builder::new();
        builder.timer(TokioTimer::new()).header_read_timeout(t);
        let conn = watcher.watch(builder.serve_connection(io, service));
        tokio::select! {
            r = conn => r,
            () = first_request => {
                tracing::debug!(%addr, "no request within the header timeout");
                return;
            }
        }
    };
    if let Err(e) = result {
        tracing::debug!(%addr, error = %e, "connection ended with an error");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn limits_per_address_and_frees_slots() {
        let l = Limiter::new(2);
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        let p1 = l.try_acquire(a).unwrap();
        let _p2 = l.try_acquire(a).unwrap();
        assert!(l.try_acquire(a).is_none());
        assert!(l.try_acquire(b).is_some());
        drop(p1);
        assert!(l.try_acquire(a).is_some());
    }

    #[test]
    fn ipv6_counts_by_network_and_mapped_ipv4_as_ipv4() {
        let l = Limiter::new(1);
        let _p = l.try_acquire("2001:db8::1".parse().unwrap()).unwrap();
        assert!(l.try_acquire("2001:db8::ffff:2".parse().unwrap()).is_none());
        assert!(l.try_acquire("2001:db8:0:1::1".parse().unwrap()).is_some());
        let _q = l.try_acquire("192.0.2.7".parse().unwrap()).unwrap();
        assert!(l.try_acquire("::ffff:192.0.2.7".parse().unwrap()).is_none());
    }
}
