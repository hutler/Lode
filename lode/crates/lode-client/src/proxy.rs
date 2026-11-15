//! Content downloads through a caching proxy (T154).
//!
//! A client with a proxy sends blob and chunk-list downloads to it and everything else to the
//! server. When the proxy cannot be reached, answers with a 5xx, or sends content that does
//! not match its id, the client warns once and from then on downloads from the server.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use reqwest::{RequestBuilder, Response, Url};

use crate::error::{Error, Result};
use crate::{Client, Retry};

/// Called with a warning when the proxy fails and the client turns to the server.
pub type ProxyWarn = Arc<dyn Fn(String) + Send + Sync>;

/// The proxy of one client and its clones.
pub(crate) struct ProxyRoute {
    pub(crate) base: Url,
    failed: AtomicBool,
    warn: Option<ProxyWarn>,
}

impl ProxyRoute {
    pub(crate) fn new(base: Url, warn: Option<ProxyWarn>) -> Self {
        Self {
            base,
            failed: AtomicBool::new(false),
            warn,
        }
    }
}

/// Whether a failure through the proxy means the proxy, not the request, is at fault.
pub(crate) fn is_proxy_failure(e: &Error) -> bool {
    match e {
        Error::Api(a) => a.status >= 500,
        Error::Transport(_) | Error::Io(_) | Error::HashMismatch { .. } | Error::Protocol(_) => {
            true
        }
        _ => false,
    }
}

impl Client {
    /// The configured caching proxy, whether or not it is still in use.
    pub fn proxy_url(&self) -> Option<&Url> {
        self.proxy.as_ref().map(|p| &p.base)
    }

    /// Whether content downloads still go through the proxy (it has not failed).
    pub fn proxy_active(&self) -> bool {
        self.proxy
            .as_ref()
            .is_some_and(|p| !p.failed.load(Ordering::SeqCst))
    }

    /// Stops using the proxy, warning once.
    pub(crate) fn proxy_failed(&self, e: &Error) {
        let Some(p) = &self.proxy else { return };
        if p.failed.swap(true, Ordering::SeqCst) {
            return;
        }
        let msg = format!(
            "the caching proxy {} failed ({e}); downloading from the server instead",
            p.base
        );
        tracing::warn!("{msg}");
        if let Some(w) = &p.warn {
            w(msg);
        }
    }

    /// Sends a content download to the proxy while it works, else to the server. `make`
    /// builds the request for a URL. The flag tells whether the proxy answered.
    pub(crate) async fn send_content(
        &self,
        retry: Retry,
        segments: &[&str],
        make: impl Fn(RequestBuilder) -> RequestBuilder,
    ) -> Result<(Response, bool)> {
        if let Some(p) = self.proxy.as_ref().filter(|_| self.proxy_active()) {
            let url = Self::url_on(&p.base, segments)?;
            match self
                .send(Retry::No, || {
                    Ok(make(self.request(reqwest::Method::GET, url.clone())))
                })
                .await
            {
                Ok(r) => return Ok((r, true)),
                Err(e) if is_proxy_failure(&e) => self.proxy_failed(&e),
                Err(e) => return Err(e),
            }
        }
        let url = self.url(segments)?;
        let resp = self
            .send(retry, || {
                Ok(make(self.request(reqwest::Method::GET, url.clone())))
            })
            .await?;
        Ok((resp, false))
    }
}
