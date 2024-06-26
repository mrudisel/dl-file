use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::{fmt, io};

use bytes::Buf;
use futures::{Stream, TryStreamExt};
use reqwest::StatusCode;
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWrite};
use tokio::sync::Semaphore;

mod builder;
mod driver;
mod writer;

pub use writer::DlFileWriter;
pub mod progress;
pub use builder::DlFileBuilder;

pub struct DlFile<P: AsRef<Path> = PathBuf> {
    path: P,
    semaphore: Option<Arc<Semaphore>>,
    delete: Delete,
    progress: Option<Box<dyn progress::DlProgress>>,
    on_drop_error: fn(&Path, DropError),
    file: ManuallyDrop<File>,
}

#[derive(Debug, Default, Clone, Copy)]
pub enum Delete {
    Yes,
    No,
    #[default]
    IfEmptyOnDrop,
}

impl Delete {
    fn should_delete(&self, path: &Path) -> io::Result<bool> {
        match self {
            Self::Yes => Ok(true),
            Self::No => Ok(false),
            Self::IfEmptyOnDrop => {
                let meta = std::fs::metadata(path)?;
                Ok(meta.len() == 0)
            }
        }
    }
}

pub enum DropError {
    Metadata(io::Error),
    Deleting(io::Error),
}

impl<P: AsRef<Path>> Deref for DlFile<P> {
    type Target = File;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

impl<P: AsRef<Path>> DerefMut for DlFile<P> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.file
    }
}

impl<P: AsRef<Path>> fmt::Debug for DlFile<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DlFile")
            .field("path", &self.path.as_ref().display())
            .field("delete", &self.delete)
            .field("semaphore", &self.semaphore)
            .field(
                "progress",
                match self.progress.as_ref() {
                    None => &"None",
                    Some(_) => &"Some(...)",
                },
            )
            .field("file", &*self.file)
            .finish()
    }
}

impl<P: AsRef<Path>> Drop for DlFile<P> {
    fn drop(&mut self) {
        let should_delete = match self.delete.should_delete(self.path.as_ref()) {
            Ok(should_delete) => should_delete,
            Err(error) => {
                (self.on_drop_error)(self.path.as_ref(), DropError::Metadata(error));
                // SAFETY: We're only deleting this once, then returning.
                unsafe { ManuallyDrop::drop(&mut self.file) }
                return;
            }
        };

        if should_delete {
            // SAFETY: this only gets called once, and then we return early to prevent
            // the drop call at the bottom of this drop impl from being called.
            //
            // if something panics before we can return, this is still safe from a double
            // free.
            unsafe { ManuallyDrop::drop(&mut self.file) };

            if let Err(error) = std::fs::remove_file(self.path.as_ref()) {
                (self.on_drop_error)(self.path.as_ref(), DropError::Deleting(error));
            }
            // bail, so we dont drop twice
            return;
        }

        // SAFETY: this only gets called once, since we returned early if we deleted the file
        // or ran into an error;
        unsafe { ManuallyDrop::drop(&mut self.file) }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OverwriteBehavior {
    Do,
    Dont,
    #[default]
    DoIfEmpty,
}

impl<P: AsRef<Path>> DlFile<P> {
    #[inline]
    pub fn builder(path: P) -> DlFileBuilder<P> {
        DlFileBuilder::new(path)
    }

    /// If downloading encountered an error, this can be used to reset the file without
    /// having to close + reopen it.
    ///
    /// In this context, 'reset' means seeking to the start of the file, and truncating to 0 bytes.
    #[inline]
    pub async fn reset(&mut self) -> io::Result<()> {
        self.file.seek(io::SeekFrom::Start(0)).await?;
        self.file.set_len(0).await
    }

    #[inline]
    pub fn into_async_writer(self, estimated_size: Option<u64>) -> DlFileWriter<P> {
        DlFileWriter::new(self, estimated_size)
    }

    pub fn set_delete(&mut self, delete: Delete) {
        self.delete = delete;
    }

    #[inline]
    pub async fn download_from_io_stream<S, B>(
        &mut self,
        size: Option<u64>,
        stream: S,
    ) -> io::Result<u64>
    where
        S: Stream<Item = io::Result<B>>,
        B: Buf,
    {
        futures::pin_mut!(stream);

        let download = driver::DownloadDriver::new(self, stream, size).await;

        futures::pin_mut!(download);

        download.await
    }

    #[inline]
    pub async fn download_from_response(&mut self, response: reqwest::Response) -> io::Result<u64> {
        #[inline]
        fn reqwest_error_to_io_error(error: reqwest::Error) -> io::Error {
            let kind = if let Some(status) = error.status() {
                match status {
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                        io::ErrorKind::PermissionDenied
                    }
                    StatusCode::CONFLICT => io::ErrorKind::AlreadyExists,
                    StatusCode::NOT_FOUND | StatusCode::GONE => io::ErrorKind::NotFound,
                    _ if (400..500).contains(&status.as_u16()) => io::ErrorKind::InvalidInput,
                    _ if (500..600).contains(&status.as_u16()) => io::ErrorKind::ConnectionAborted,
                    _ => io::ErrorKind::Other,
                }
            } else if error.is_timeout() {
                io::ErrorKind::TimedOut
            } else if error.is_connect() {
                io::ErrorKind::ConnectionAborted
            } else if error.is_decode() || error.is_body() {
                io::ErrorKind::InvalidData
            } else if error.is_request() || error.is_builder() {
                io::ErrorKind::InvalidInput
            } else if error.is_redirect() {
                io::ErrorKind::ConnectionReset
            } else {
                io::ErrorKind::Other
            };

            io::Error::new(kind, error)
        }

        self.download_from_stream(
            response.content_length(),
            response.bytes_stream(),
            reqwest_error_to_io_error,
        )
        .await
    }

    #[inline]
    pub async fn download_from_stream<S, B, E, F>(
        &mut self,
        size: Option<u64>,
        stream: S,
        map_err: F,
    ) -> io::Result<u64>
    where
        S: Stream<Item = Result<B, E>>,
        B: Buf,
        F: FnMut(E) -> io::Error,
    {
        self.download_from_io_stream(size, stream.map_err(map_err))
            .await
    }
}
