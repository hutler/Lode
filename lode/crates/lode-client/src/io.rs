//! Streaming adapters: response bodies as `AsyncRead`, and zstd in both directions, so no
//! blob is ever held in memory whole.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf, Bytes};
use futures::{Stream, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use zstd::stream::raw::{Decoder, Encoder, InBuffer, Operation, OutBuffer};

const BUF: usize = 64 * 1024;

/// Reads a stream of byte chunks. Stream errors surface as `ConnectionAborted`, so callers
/// can tell a broken transfer from a local write failure.
pub(crate) struct StreamReader<S> {
    stream: S,
    chunk: Bytes,
    done: bool,
}

impl<S> StreamReader<S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            stream,
            chunk: Bytes::new(),
            done: false,
        }
    }
}

impl<S, E> AsyncRead for StreamReader<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
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

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Decodes a zstd stream (one or more frames). Malformed or truncated input is `InvalidData`.
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
            in_buf: vec![0u8; BUF].into_boxed_slice(),
            in_pos: 0,
            in_len: 0,
            inner_eof: false,
            in_frame: false,
        })
    }
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

/// Compresses `inner` into one zstd frame as it is read.
pub(crate) struct ZstdEncodeReader<R> {
    inner: R,
    encoder: Encoder<'static>,
    in_buf: Box<[u8]>,
    in_pos: usize,
    in_len: usize,
    inner_eof: bool,
    out_buf: Box<[u8]>,
    out_pos: usize,
    out_len: usize,
    finished: bool,
}

impl<R> ZstdEncodeReader<R> {
    pub(crate) fn new(inner: R, level: i32) -> io::Result<Self> {
        Ok(Self {
            inner,
            encoder: Encoder::new(level)?,
            in_buf: vec![0u8; BUF].into_boxed_slice(),
            in_pos: 0,
            in_len: 0,
            inner_eof: false,
            out_buf: vec![0u8; BUF].into_boxed_slice(),
            out_pos: 0,
            out_len: 0,
            finished: false,
        })
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for ZstdEncodeReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        loop {
            if this.out_pos < this.out_len {
                let n = (this.out_len - this.out_pos).min(out.remaining());
                out.put_slice(&this.out_buf[this.out_pos..this.out_pos + n]);
                this.out_pos += n;
                return Poll::Ready(Ok(()));
            }
            if this.finished {
                return Poll::Ready(Ok(()));
            }
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
            let mut outb = OutBuffer::around(&mut this.out_buf[..]);
            if this.in_pos < this.in_len {
                let mut inb = InBuffer::around(&this.in_buf[this.in_pos..this.in_len]);
                this.encoder.run(&mut inb, &mut outb)?;
                this.in_pos += inb.pos();
            } else {
                // Input is exhausted: flush the frame epilogue until nothing remains.
                let remaining = this.encoder.finish(&mut outb, true)?;
                this.finished = remaining == 0;
            }
            this.out_len = outb.pos();
            this.out_pos = 0;
        }
    }
}

/// Turns a reader into a stream of chunks, for request bodies.
pub(crate) fn reader_stream<R>(reader: R) -> impl Stream<Item = io::Result<Bytes>> + Send
where
    R: AsyncRead + Send + Unpin + 'static,
{
    futures::stream::try_unfold(reader, |mut r| async move {
        let mut buf = vec![0u8; BUF];
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

    async fn read_all<R: AsyncRead + Unpin>(mut r: R) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        r.read_to_end(&mut out).await?;
        Ok(out)
    }

    #[tokio::test]
    async fn encode_then_decode_round_trips() {
        for len in [0usize, 1, 1000, 300_000] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
            let enc = read_all(ZstdEncodeReader::new(&data[..], 3).unwrap())
                .await
                .unwrap();
            assert_eq!(zstd::decode_all(&enc[..]).unwrap(), data, "len {len}");
            let dec = read_all(ZstdDecodeReader::new(&enc[..]).unwrap())
                .await
                .unwrap();
            assert_eq!(dec, data);
        }
    }

    #[tokio::test]
    async fn truncated_zstd_is_invalid() {
        let enc = zstd::encode_all(&vec![9u8; 100_000][..], 3).unwrap();
        let err = read_all(ZstdDecodeReader::new(&enc[..enc.len() - 3]).unwrap())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
