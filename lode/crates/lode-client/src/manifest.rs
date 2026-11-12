//! NDJSON manifest responses as a `Stream` of typed lines.

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::BytesMut;
use futures::{Stream, StreamExt};
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// Longest manifest line accepted; a real line is a few hundred bytes.
const MAX_LINE: usize = 1024 * 1024;

/// A manifest being received: entries (full manifest) or deltas (with a base), one per line,
/// plus the access epoch the server sent with it (ARCHITECTURE §6).
pub struct ManifestStream<T> {
    inner: Pin<Box<dyn Stream<Item = Result<T>> + Send>>,
    access_epoch: Option<u64>,
}

impl<T> std::fmt::Debug for ManifestStream<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManifestStream")
            .field("access_epoch", &self.access_epoch)
            .finish_non_exhaustive()
    }
}

impl<T> ManifestStream<T> {
    /// `X-Lode-Access-Epoch` of the response; a client stores it and does a full sync when
    /// it changes.
    pub fn access_epoch(&self) -> Option<u64> {
        self.access_epoch
    }
}

impl<T> Stream for ManifestStream<T> {
    type Item = Result<T>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.poll_next_unpin(cx)
    }
}

impl<T: DeserializeOwned + Send + 'static> ManifestStream<T> {
    pub(crate) fn new(resp: reqwest::Response, access_epoch: Option<u64>) -> Self {
        Self {
            inner: Box::pin(lines(resp.bytes_stream())),
            access_epoch,
        }
    }
}

struct LineState<S> {
    body: S,
    buf: BytesMut,
    /// Bytes of `buf` already searched for a newline.
    scanned: usize,
    eof: bool,
}

fn lines<T, S>(body: S) -> impl Stream<Item = Result<T>> + Send
where
    T: DeserializeOwned + Send + 'static,
    S: Stream<Item = reqwest::Result<bytes::Bytes>> + Send + Unpin + 'static,
{
    let state = LineState {
        body,
        buf: BytesMut::new(),
        scanned: 0,
        eof: false,
    };
    futures::stream::try_unfold(state, |mut st| async move {
        loop {
            if let Some(i) = st.buf[st.scanned..].iter().position(|&b| b == b'\n') {
                let line = st.buf.split_to(st.scanned + i + 1);
                st.scanned = 0;
                if let Some(item) = parse(&line[..line.len() - 1])? {
                    return Ok(Some((item, st)));
                }
                continue;
            }
            st.scanned = st.buf.len();
            if st.eof {
                let rest = st.buf.split();
                st.scanned = 0;
                return match parse(&rest)? {
                    Some(item) => Ok(Some((item, st))),
                    None => Ok(None),
                };
            }
            if st.buf.len() > MAX_LINE {
                return Err(Error::Protocol("manifest line is too long".into()));
            }
            match st.body.next().await {
                Some(Ok(chunk)) => st.buf.extend_from_slice(&chunk),
                Some(Err(e)) => return Err(Error::Transport(e)),
                None => st.eof = true,
            }
        }
    })
}

fn parse<T: DeserializeOwned>(line: &[u8]) -> Result<Option<T>> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice(line)
        .map(Some)
        .map_err(|e| Error::Protocol(format!("bad manifest line: {e}")))
}
