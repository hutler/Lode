//! Adapters between HTTP bodies and `tokio::io`, and a streaming zstd decoder for uploads.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, BodyDataStream};
use bytes::{Buf, Bytes};
use futures::{Stream, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use zstd::stream::raw::{Decoder, InBuffer, Operation, OutBuffer};

/// Reads a request body as a byte stream. A transport error (such as the client going away)
/// surfaces as `ConnectionAborted`.
pub(crate) struct BodyReader {
    stream: BodyDataStream,
    chunk: Bytes,
    done: bool,
}

impl BodyReader {
    pub(crate) fn new(body: Body) -> Self {
        Self {
            stream: body.into_data_stream(),
            chunk: Bytes::new(),
            done: false,
        }
    }
}

impl AsyncRead for BodyReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        while this.chunk.is_empty() {
            if this.done {
                return Poll::Ready(Ok(()));
            }
            match this.stream.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => this.done = true,
                Poll::Ready(Some(Ok(b))) => this.chunk = b,
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        e.to_string(),
                    )))
                }
            }
        }
        let n = this.chunk.len().min(out.remaining());
        out.put_slice(&this.chunk[..n]);
        this.chunk.advance(n);
        Poll::Ready(Ok(()))
    }
}

const IN_BUF: usize = 64 * 1024;

/// Decodes a zstd stream (one or more frames) from `inner`. Malformed or truncated input is
/// `InvalidData`. The output length is bounded by the blob store's size cap downstream.
pub(crate) struct ZstdDecodeReader<R> {
    inner: R,
    decoder: Decoder<'static>,
    in_buf: Box<[u8]>,
    in_pos: usize,
    in_len: usize,
    inner_eof: bool,
    /// True while a frame has been started and not finished.
    in_frame: bool,
}

impl<R> ZstdDecodeReader<R> {
    pub(crate) fn new(inner: R) -> io::Result<Self> {
        Ok(Self {
            inner,
            decoder: Decoder::new()?,
            in_buf: vec![0u8; IN_BUF].into_boxed_slice(),
            in_pos: 0,
            in_len: 0,
            inner_eof: false,
            in_frame: false,
        })
    }
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl<R: AsyncRead + Unpin> AsyncRead for ZstdDecodeReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if out.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if this.in_pos == this.in_len && !this.inner_eof {
                let mut rb = ReadBuf::new(&mut this.in_buf);
                match Pin::new(&mut this.inner).poll_read(cx, &mut rb) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Ready(Ok(())) => {
                        this.in_pos = 0;
                        this.in_len = rb.filled().len();
                        this.inner_eof = this.in_len == 0;
                    }
                }
            }
            if this.in_pos == this.in_len && this.inner_eof {
                if this.in_frame {
                    return Poll::Ready(Err(invalid("zstd stream is truncated".into())));
                }
                return Poll::Ready(Ok(()));
            }
            let dst = out.initialize_unfilled();
            let mut inb = InBuffer::around(&this.in_buf[this.in_pos..this.in_len]);
            let mut outb = OutBuffer::around(dst);
            let hint = this
                .decoder
                .run(&mut inb, &mut outb)
                .map_err(|e| invalid(format!("invalid zstd stream: {e}")))?;
            let consumed = inb.pos();
            let produced = outb.pos();
            this.in_pos += consumed;
            if consumed > 0 || produced > 0 {
                this.in_frame = hint != 0;
            }
            if produced > 0 {
                out.advance(produced);
                return Poll::Ready(Ok(()));
            }
        }
    }
}

const OUT_CHUNK: usize = 64 * 1024;

/// Turns a reader into a response body stream of chunks.
pub(crate) fn reader_stream<R>(reader: R) -> impl Stream<Item = io::Result<Bytes>> + Send
where
    R: AsyncRead + Send + Unpin + 'static,
{
    futures::stream::try_unfold(reader, |mut r| async move {
        let mut buf = vec![0u8; OUT_CHUNK];
        let n = r.read(&mut buf).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.truncate(n);
        Ok(Some((Bytes::from(buf), r)))
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    async fn decode(input: Vec<u8>) -> io::Result<Vec<u8>> {
        let mut r = ZstdDecodeReader::new(&input[..])?;
        let mut out = Vec::new();
        r.read_to_end(&mut out).await?;
        Ok(out)
    }

    #[tokio::test]
    async fn decodes_one_and_several_frames() {
        let a: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut enc = zstd::encode_all(&a[..], 3).unwrap();
        assert_eq!(decode(enc.clone()).await.unwrap(), a);
        enc.extend(zstd::encode_all(&b"tail"[..], 3).unwrap());
        let mut want = a.clone();
        want.extend_from_slice(b"tail");
        assert_eq!(decode(enc).await.unwrap(), want);
        assert_eq!(decode(Vec::new()).await.unwrap(), Vec::<u8>::new());
    }

    #[tokio::test]
    async fn rejects_garbage_and_truncation() {
        let err = decode(b"definitely not zstd".to_vec()).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let enc = zstd::encode_all(&vec![7u8; 100_000][..], 3).unwrap();
        let err = decode(enc[..enc.len() - 3].to_vec()).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
