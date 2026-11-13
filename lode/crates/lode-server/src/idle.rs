//! Request bodies that stop arriving: each body fails once no bytes have come for the
//! configured idle time (`[limits] body_idle_seconds`), so a client that trickles or stalls
//! cannot hold a request, an upload's temporary file or a connection forever.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, BodyDataStream, Bytes};
use axum::extract::{Request, State};
use futures::{Stream, StreamExt};
use tokio::time::{Instant, Sleep};

use crate::state::AppState;

/// A body stream that fails after `idle` without data.
struct IdleBody {
    inner: BodyDataStream,
    idle: Duration,
    timer: Pin<Box<Sleep>>,
}

impl Stream for IdleBody {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        match this.inner.poll_next_unpin(cx) {
            Poll::Ready(item) => {
                let next = Instant::now() + this.idle;
                this.timer.as_mut().reset(next);
                Poll::Ready(item)
            }
            Poll::Pending => match this.timer.as_mut().poll(cx) {
                Poll::Ready(()) => Poll::Ready(Some(Err(axum::Error::new(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the request body stalled",
                ))))),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

/// Middleware: wraps every request body in the idle timeout.
pub(crate) async fn limit_idle(State(state): State<AppState>, req: Request) -> Request {
    let idle = Duration::from_secs(state.config().limits.body_idle_seconds.max(1));
    let (parts, body) = req.into_parts();
    let stream = IdleBody {
        inner: body.into_data_stream(),
        idle,
        timer: Box::pin(tokio::time::sleep(idle)),
    };
    Request::from_parts(parts, Body::from_stream(stream))
}
