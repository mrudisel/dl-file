use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fmt, io};

use bytes::Buf;
use futures::{Stream, TryStreamExt};
use reqwest::StatusCode;
use tokio::fs::File;
use tokio::sync::Semaphore;

mod builder;
mod driver;
pub mod progress;
pub use builder::DlFileBuilder;

pub struct DlFile<P: AsRef<Path> = PathBuf> {
    path: P,
    semaphore: Option<Arc<Semaphore>>,
    delete_if_empty: bool,
    progress: Option<Box<dyn progress::DlProgress>>,
    on_drop_error: Option<fn(io::Error)>,
    file: ManuallyDrop<File>,
}

impl<P: AsRef<Path>> Deref for DlFile<P> {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

impl<P: AsRef<Path>> DerefMut for DlFile<P> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.file
    }
}

impl<P: AsRef<Path>> fmt::Debug for DlFile<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DlFile")
            .field("path", &self.path.as_ref().display())
            .field("delete_if_empty", &self.delete_if_empty)
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
        if self.delete_if_empty {
            fn default_on_metadata_error(error: io::Error) {
                eprintln!("error getting file metadata: {error}");
            }

            fn default_on_delete_error(error: io::Error) {
                eprintln!("error getting file metadata: {error}");
            }

            match std::fs::metadata(self.path.as_ref()) {
                Ok(meta) if meta.len() == 0 => {
                    // SAFETY: this only gets called once, and then we return early to prevent
                    // the drop call at the bottom of this drop impl from being called.
                    unsafe { ManuallyDrop::drop(&mut self.file) };

                    if let Err(error) = std::fs::remove_file(self.path.as_ref()) {
                        self.on_drop_error.unwrap_or(default_on_delete_error)(error);
                    }
                    // bail, so we dont drop twice
                    return;
                }
                Ok(_) => (),
                Err(error) => self.on_drop_error.unwrap_or(default_on_metadata_error)(error),
            }
        }

        // SAFETY: this only gets called once, since we returned early if we deleted the file.
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
    pub fn builder(path: P) -> DlFileBuilder<P> {
        DlFileBuilder::new(path)
    }

    pub async fn download_from_io_stream<S, B>(
        mut self,
        size: Option<u64>,
        stream: S,
    ) -> io::Result<u64>
    where
        S: Stream<Item = io::Result<B>>,
        B: Buf,
    {
        futures::pin_mut!(stream);

        let download = driver::DownloadDriver::new(&mut self, stream, size).await;

        futures::pin_mut!(download);

        download.await
    }

    pub async fn download_from_response(self, response: reqwest::Response) -> io::Result<u64> {
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

    pub async fn download_from_stream<S, B, E, F>(
        self,
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
