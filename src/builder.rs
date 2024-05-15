use std::io;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::progress::DlProgress;
use crate::{Delete, DlFile, DlFileWriter, DropError, OverwriteBehavior};

pub struct DlFileBuilder<P: AsRef<Path> = PathBuf> {
    path: P,
    semaphore: Option<Arc<Semaphore>>,
    delete: Delete,
    on_drop_error: Option<fn(&Path, DropError)>,
    progress: Option<Box<dyn DlProgress>>,
}

impl<P: AsRef<Path>> DlFileBuilder<P> {
    #[inline]
    pub fn new(path: P) -> Self {
        Self {
            path,
            semaphore: None,
            on_drop_error: None,
            delete: Delete::default(),
            progress: None,
        }
    }

    #[inline]
    pub fn delete(mut self, delete: Delete) -> Self {
        self.delete = delete;
        self
    }

    #[inline]
    pub fn with_semaphore(mut self, semaphore: Arc<Semaphore>) -> Self {
        self.semaphore = Some(semaphore);
        self
    }

    #[inline]
    pub fn with_semaphore_ref(self, semaphore: &Arc<Semaphore>) -> Self {
        self.with_semaphore(Arc::clone(semaphore))
    }

    #[inline]
    pub fn with_progress(mut self, progress: impl DlProgress + 'static) -> Self {
        self.progress = Some(Box::new(progress));
        self
    }

    #[inline]
    pub fn on_drop_error(mut self, on_drop_error: fn(&Path, DropError)) -> Self {
        self.on_drop_error = Some(on_drop_error);
        self
    }

    #[cfg(feature = "tracing")]
    #[inline]
    pub fn trace_on_drop_error(self, level: tracing::Level) -> Self {
        self.on_drop_error(match level {
            tracing::Level::TRACE => default_trace_on_drop_error,
            tracing::Level::DEBUG => default_debug_on_drop_error,
            tracing::Level::INFO => default_info_on_drop_error,
            tracing::Level::WARN => default_warn_on_drop_error,
            tracing::Level::ERROR => default_error_on_drop_error,
        })
    }

    pub async fn open_as_writer(
        self,
        overwrite_behavior: OverwriteBehavior,
        estimated_size: Option<u64>,
    ) -> io::Result<DlFileWriter<P>> {
        let file = self.open(overwrite_behavior).await?;

        Ok(file.into_async_writer(estimated_size))
    }

    pub async fn open(self, overwrite_behavior: OverwriteBehavior) -> io::Result<DlFile<P>> {
        let path = self.path.as_ref();

        if let Some(parent) = path.parent() {
            if !tokio::fs::try_exists(parent).await? {
                tokio::fs::create_dir_all(parent).await?;
            }
        }

        let file = match overwrite_behavior {
            OverwriteBehavior::Do => tokio::fs::File::create(path).await?,
            OverwriteBehavior::Dont => {
                tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .await?
            }
            OverwriteBehavior::DoIfEmpty => match tokio::fs::metadata(path).await {
                Ok(meta) if meta.len() == 0 => tokio::fs::File::create(path).await?,
                Ok(meta) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!(
                            "non-empty ({} bytes) file '{}' already exists",
                            meta.len(),
                            self.path.as_ref().display()
                        ),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    tokio::fs::File::create(path).await?
                }
                Err(error) => return Err(error),
            },
        };

        Ok(DlFile {
            path: self.path,
            semaphore: self.semaphore,
            #[cfg(not(feature = "tracing"))]
            on_drop_error: self.on_drop_error.unwrap_or(default_on_drop_error),
            #[cfg(feature = "tracing")]
            on_drop_error: self.on_drop_error.unwrap_or(default_error_on_drop_error),
            delete: self.delete,
            progress: self.progress,
            file: ManuallyDrop::new(file),
        })
    }

    #[inline]
    pub async fn open_overwrite(self) -> io::Result<DlFile<P>> {
        self.open(OverwriteBehavior::Do).await
    }

    #[inline]
    pub async fn open_new(self) -> io::Result<DlFile<P>> {
        self.open(OverwriteBehavior::Dont).await
    }

    #[inline]
    pub async fn open_overwrite_if_empty(self) -> io::Result<DlFile<P>> {
        self.open(OverwriteBehavior::DoIfEmpty).await
    }
}

#[cfg(not(feature = "tracing"))]
#[inline]
fn default_on_drop_error(path: &Path, error: DropError) {
    match error {
        DropError::Deleting(error) => {
            eprintln!("{}: error deleting file on drop: {error}", path.display())
        }
        DropError::Metadata(error) => eprintln!(
            "{}: error getting file metadata on drop: {error}",
            path.display()
        ),
    }
}

#[cfg(feature = "tracing")]
macro_rules! define_tracing_error_fns {
    ($($fn_name:ident($macro_ident:ident)),* $(,)?) => {
        $(
            #[inline]
            fn $fn_name(path: &Path, error: DropError) {
                let (message, error) = match error {
                    DropError::Deleting(error) => ("error deleting file on drop", error),
                    DropError::Metadata(error) => ("error getting file metadata on drop", error),
                };

                tracing::$macro_ident!(message = ?message, path = %path.display(), error = %error);
            }
        )*
    };
}

#[cfg(feature = "tracing")]
define_tracing_error_fns! {
    default_trace_on_drop_error(trace),
    default_debug_on_drop_error(debug),
    default_info_on_drop_error(info),
    default_warn_on_drop_error(warn),
    default_error_on_drop_error(error),
}
