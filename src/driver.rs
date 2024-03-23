use std::io;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Buf;
use futures::Stream;
use tokio::fs::File;
use tokio::io::AsyncWrite;
use tokio::sync::OwnedSemaphorePermit;

use crate::progress::DlProgress;
use crate::DlFile;

pin_project_lite::pin_project! {
    pub(super) struct DownloadDriver<'a, S: Stream<Item = io::Result<B>>, B: Buf> {
        permit: Option<OwnedSemaphorePermit>,
        path: &'a Path,
        stream: Option<Pin<&'a mut S>>,
        current_buf: Option<B>,
        progress: Option<&'a mut dyn DlProgress>,
        file: Pin<&'a mut File>,
        bytes_copied: u64,
    }
}

impl<'a, S, B> DownloadDriver<'a, S, B>
where
    S: Stream<Item = io::Result<B>>,
    B: Buf,
{
    pub(super) async fn new<P: AsRef<Path>>(
        file: &'a mut DlFile<P>,
        stream: Pin<&'a mut S>,
        size: Option<u64>,
    ) -> Self {
        let permit = match file.semaphore.take() {
            Some(semaphore) => Some(semaphore.acquire_owned().await.unwrap()),
            None => None,
        };

        if let Some(ref mut prog) = file.progress {
            prog.start(file.path.as_ref(), size);
        }

        Self {
            path: file.path.as_ref(),
            file: Pin::new(&mut file.file),
            bytes_copied: 0,
            permit,
            stream: Some(stream),
            current_buf: None,
            progress: match file.progress {
                Some(ref mut prog) => Some(&mut *prog),
                None => None,
            },
        }
    }
}

impl<S, B> std::future::Future for DownloadDriver<'_, S, B>
where
    S: Stream<Item = io::Result<B>>,
    B: Buf,
{
    type Output = io::Result<u64>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        'outer: loop {
            // work towards exhausting the current buffer
            if let Some(ref mut current) = this.current_buf {
                'current_buf: loop {
                    let written =
                        std::task::ready!(this.file.as_mut().poll_write(cx, current.chunk()))?;

                    if written > 0 {
                        current.advance(written);
                        *this.bytes_copied += written as u64;

                        if let Some(ref mut prog) = this.progress {
                            prog.update(this.path, *this.bytes_copied);
                        }
                    }

                    if !current.has_remaining() {
                        *this.current_buf = None;
                        break 'current_buf;
                    }
                }
            }

            // if empty, poll more bytes from the stream
            if let Some(ref mut stream) = this.stream {
                'poll_stream: loop {
                    let chunk = match std::task::ready!(stream.as_mut().poll_next(cx)) {
                        Some(result) => result?,
                        None => {
                            *this.stream = None;
                            break 'outer;
                        }
                    };

                    // skip empty chunks
                    if !chunk.has_remaining() {
                        continue 'poll_stream;
                    }

                    // set the current buf, and start back at the top to start writing
                    *this.current_buf = Some(chunk);
                    continue 'outer;
                }
            }

            break 'outer;
        }

        // if we made it here, there's no stream left and no current chunk, so we need to flush.
        std::task::ready!(this.file.as_mut().poll_flush(cx))?;

        if let Some(ref mut prog) = this.progress {
            prog.finished(this.path);
        }

        Poll::Ready(Ok(*this.bytes_copied))
    }
}
