//! Moving bodies between the network and the store without buffering them.
//!
//! The store is synchronous: it reads uploads through [`std::io::Read`] and
//! serves objects as files. Requests and responses are async streams. Two
//! bridges connect them through a small bounded channel, so at most a few
//! chunks of any body are in memory at once:
//!
//! - [`upload`] runs a store operation on a blocking thread, feeding it the
//!   request body chunk by chunk and stopping it when the body exceeds the
//!   size limit;
//! - [`download`] reads an object file on a blocking thread into a
//!   response body.

use std::io::{self, Read};
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf as _, Bytes};
use http_body_util::BodyExt as _;
use hyper::body::{Body, Frame, SizeHint};
use tokio::sync::mpsc;

use crate::route;

/// Chunks in flight between a body and the store.
const DEPTH: usize = 4;

/// How much of a file one download chunk holds.
const CHUNK: usize = 256 * 1024;

/// Files up to this size are read whole rather than streamed.
const SMALL: u64 = 64 * 1024;

/// Why an upload did not reach the store.
#[derive(Debug)]
pub(crate) enum UploadError<E> {
    /// The body was larger than the limit.
    TooLarge,
    /// The client's body failed, such as a dropped connection.
    Body,
    /// The store operation failed.
    Store(E),
}

/// Runs `operation` on a blocking thread with `body` as its reader, and
/// returns its result together with how many bytes it received.
pub(crate) async fn upload<B, T, E>(
    body: B,
    limit: u64,
    operation: impl FnOnce(&mut dyn Read) -> Result<T, E> + Send + 'static,
) -> (Result<T, UploadError<E>>, u64)
where
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    T: Send + 'static,
    E: Send + 'static,
{
    let (sender, mut reader) = pipe();
    let worker = tokio::task::spawn_blocking(move || operation(&mut reader));

    let mut body = body;
    let mut total: u64 = 0;
    let mut failure = None;
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            failure = Some(UploadError::Body);
            break;
        };
        let Ok(data) = frame.into_data() else {
            continue; // trailers carry nothing the store needs
        };
        total += data.len() as u64;
        if total > limit {
            failure = Some(UploadError::TooLarge);
            break;
        }
        if sender.send(Ok(data)).await.is_err() {
            break; // the operation stopped reading; its result says why
        }
    }
    if failure.is_some() {
        // Make the operation's next read fail, so it stores nothing.
        let _ = sender.send(Err(io::Error::other("upload aborted"))).await;
    }
    drop(sender);

    let result = match worker.await {
        Ok(result) => result.map_err(UploadError::Store),
        Err(_) => Err(UploadError::Body),
    };
    match failure {
        Some(failure) => (Err(failure), total),
        None => (result, total),
    }
}

/// A blocking reader and the sender that feeds it, for streaming a body
/// that does not arrive as an HTTP body, such as a `ByteStream.Write`.
///
/// Sending an error makes the reader's next read fail with it; dropping
/// the sender ends the stream.
pub(crate) fn pipe() -> (mpsc::Sender<io::Result<Bytes>>, ChannelReader) {
    let (sender, receiver) = mpsc::channel(DEPTH);
    let reader = ChannelReader {
        receiver,
        current: Bytes::new(),
    };
    (sender, reader)
}

/// A blocking reader over the chunks a [`pipe`] receives.
pub(crate) struct ChannelReader {
    receiver: mpsc::Receiver<io::Result<Bytes>>,
    current: Bytes,
}

impl Read for ChannelReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        while self.current.is_empty() {
            match self.receiver.blocking_recv() {
                Some(Ok(chunk)) => self.current = chunk,
                Some(Err(error)) => return Err(error),
                None => return Ok(0),
            }
        }
        let len = buffer.len().min(self.current.len());
        buffer[..len].copy_from_slice(&self.current[..len]);
        self.current.advance(len);
        Ok(len)
    }
}

/// A response body holding the file at `path`, and its length, or `None`
/// when the file is gone.
///
/// Small files are read in one go; larger ones stream from a blocking
/// thread.
pub(crate) async fn download(path: PathBuf) -> Option<(route::Body, u64)> {
    let opened = tokio::task::spawn_blocking(move || -> io::Result<_> {
        let file = std::fs::File::open(&path)?;
        let len = file.metadata()?.len();
        if len <= SMALL {
            let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
            let mut file = file;
            file.read_to_end(&mut bytes)?;
            return Ok((Some(Bytes::from(bytes)), None, len));
        }
        Ok((None, Some(file), len))
    })
    .await
    .ok()?
    .ok()?;

    match opened {
        (Some(bytes), _, len) => Some((
            http_body_util::Full::new(bytes)
                .map_err(|never| match never {})
                .boxed_unsync(),
            len,
        )),
        (None, Some(file), len) => {
            let (sender, receiver) = mpsc::channel(DEPTH);
            tokio::task::spawn_blocking(move || stream_file(file, &sender));
            Some((
                ChannelBody {
                    receiver,
                    remaining: len,
                }
                .boxed_unsync(),
                len,
            ))
        }
        (None, None, _) => None,
    }
}

fn stream_file(mut file: std::fs::File, sender: &mpsc::Sender<io::Result<Bytes>>) {
    loop {
        let mut chunk = vec![0_u8; CHUNK];
        match file.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => {
                chunk.truncate(read);
                if sender.blocking_send(Ok(Bytes::from(chunk))).is_err() {
                    return; // the client went away
                }
            }
            Err(error) => {
                let _ = sender.blocking_send(Err(error));
                return;
            }
        }
    }
}

/// A response body fed from a channel, with a known length.
struct ChannelBody {
    receiver: mpsc::Receiver<io::Result<Bytes>>,
    remaining: u64,
}

impl Body for ChannelBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        match self.receiver.poll_recv(context) {
            Poll::Ready(Some(Ok(chunk))) => {
                self.remaining = self.remaining.saturating_sub(chunk.len() as u64);
                Poll::Ready(Some(Ok(Frame::data(chunk))))
            }
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining)
    }
}

#[cfg(test)]
mod test;
