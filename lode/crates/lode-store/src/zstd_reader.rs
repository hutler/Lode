//! A streaming zstd decoder that enforces the decoded length from the object header.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use lode_core::ObjectId;
use tokio::io::{AsyncRead, ReadBuf};
use zstd::stream::raw::{Decoder, InBuffer, Operation, OutBuffer};

use crate::StoreError;

const IN_BUF: usize = 64 * 1024;

/// Decodes one zstd frame from `inner`, yielding exactly `raw_len` bytes or an error.
pub(crate) struct ZstdReader<R> {
    id: ObjectId,
    inner: R,
    decoder: Decoder<'static>,
    in_buf: Box<[u8]>,
    in_pos: usize,
    in_len: usize,
    inner_eof: bool,
    remaining: u64,
    frame_done: bool,
}

impl<R> ZstdReader<R> {
    pub(crate) fn new(id: ObjectId, inner: R, raw_len: u64) -> io::Result<Self> {
        Ok(Self {
            id,
            inner,
            decoder: Decoder::new()?,
            in_buf: vec![0u8; IN_BUF].into_boxed_slice(),
            in_pos: 0,
            in_len: 0,
            inner_eof: false,
            remaining: raw_len,
            frame_done: false,
        })
    }

    fn corrupt(&self, reason: &str) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            StoreError::Corrupt {
                id: self.id,
                reason: reason.to_owned(),
            },
        )
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for ZstdReader<R> {
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
            if this.frame_done {
                if this.remaining > 0 {
                    return Poll::Ready(Err(
                        this.corrupt("payload decodes to fewer bytes than the header says")
                    ));
                }
                // Nothing may follow the frame.
                if this.in_pos < this.in_len {
                    return Poll::Ready(Err(this.corrupt("data after the end of the zstd frame")));
                }
                if this.inner_eof {
                    return Poll::Ready(Ok(()));
                }
                let mut probe = [0u8; 1];
                let mut rb = ReadBuf::new(&mut probe);
                match Pin::new(&mut this.inner).poll_read(cx, &mut rb) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Ready(Ok(())) if rb.filled().is_empty() => {
                        this.inner_eof = true;
                        return Poll::Ready(Ok(()));
                    }
                    Poll::Ready(Ok(())) => {
                        return Poll::Ready(Err(
                            this.corrupt("data after the end of the zstd frame")
                        ))
                    }
                }
            }

            if this.in_pos == this.in_len && !this.inner_eof {
                let mut rb = ReadBuf::new(&mut this.in_buf);
                match Pin::new(&mut this.inner).poll_read(cx, &mut rb) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Ready(Ok(())) => {
                        let n = rb.filled().len();
                        this.in_pos = 0;
                        this.in_len = n;
                        if n == 0 {
                            this.inner_eof = true;
                        }
                    }
                }
            }

            // When the header's length is reached, decode into a scratch byte: any output
            // there means the payload is longer than the header claims.
            let mut scratch = [0u8; 1];
            let limit = usize::try_from(this.remaining).unwrap_or(usize::MAX);
            let dst: &mut [u8] = if this.remaining == 0 {
                &mut scratch
            } else {
                let unfilled = out.initialize_unfilled();
                let n = unfilled.len().min(limit);
                &mut unfilled[..n]
            };
            let mut inb = InBuffer::around(&this.in_buf[this.in_pos..this.in_len]);
            let mut outb = OutBuffer::around(dst);
            let hint = this
                .decoder
                .run(&mut inb, &mut outb)
                .map_err(|e| this.corrupt(&format!("zstd: {e}")))?;
            let consumed = inb.pos();
            let produced = outb.pos();
            this.in_pos += consumed;
            if hint == 0 {
                this.frame_done = true;
            }
            if produced > 0 {
                if this.remaining == 0 {
                    return Poll::Ready(Err(
                        this.corrupt("payload decodes to more bytes than the header says")
                    ));
                }
                this.remaining -= produced as u64;
                out.advance(produced);
                return Poll::Ready(Ok(()));
            }
            if this.inner_eof && this.in_pos == this.in_len && !this.frame_done {
                return Poll::Ready(Err(this.corrupt("zstd frame is truncated")));
            }
            if consumed == 0 && !this.frame_done && this.in_pos < this.in_len {
                // The decoder made no progress with input available; it needs output room
                // that `remaining == 0` denies, which means extra output.
                return Poll::Ready(Err(
                    this.corrupt("payload decodes to more bytes than the header says")
                ));
            }
        }
    }
}
