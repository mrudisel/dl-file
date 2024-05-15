use std::io;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use tokio::io::AsyncWrite;

use crate::DlFile;

pub struct DlFileWriter<P: AsRef<Path>> {
    dst: DlFile<P>,
    written: u64,
}

impl<P: AsRef<Path>> Deref for DlFileWriter<P> {
    type Target = DlFile<P>;

    fn deref(&self) -> &Self::Target {
        &self.dst
    }
}

impl<P: AsRef<Path>> DerefMut for DlFileWriter<P> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.dst
    }
}

impl<P: AsRef<Path>> DlFileWriter<P> {
    pub(super) fn new(mut dst: DlFile<P>, est_size: Option<u64>) -> Self {
        if let Some(ref mut prog) = dst.progress {
            prog.start(dst.path.as_ref(), est_size);
        }

        Self { dst, written: 0 }
    }

    #[inline]
    fn handle_write(&mut self, count: usize) {
        self.written += count as u64;

        if count > 0 {
            if let Some(ref mut prog) = self.dst.progress {
                prog.update(self.dst.path.as_ref(), self.written);
            }
        }
    }
}

impl<P: AsRef<Path> + Unpin> AsyncWrite for DlFileWriter<P> {
    #[inline]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let this = self.get_mut();
        let written = ready!(Pin::new(&mut *this.dst.file).poll_write(cx, buf))?;
        this.handle_write(written);
        Poll::Ready(Ok(written))
    }

    #[inline]
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.get_mut().dst.file).poll_flush(cx)
    }

    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let this = self.get_mut();

        ready!(Pin::new(&mut *this.dst.file).poll_shutdown(cx))?;

        if let Some(ref mut prog) = this.dst.progress {
            prog.finished(this.dst.path.as_ref());
        }

        Poll::Ready(Ok(()))
    }

    #[inline]
    fn is_write_vectored(&self) -> bool {
        self.dst.file.is_write_vectored()
    }

    #[inline]
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        let this = self.get_mut();
        let written = ready!(Pin::new(&mut *this.dst.file).poll_write_vectored(cx, bufs))?;
        this.handle_write(written);
        Poll::Ready(Ok(written))
    }
}
