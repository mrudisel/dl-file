use std::io;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::{DlFile, DlProgress, OverwriteBehavior};

pub struct DlFileBuilder<P: AsRef<Path> = PathBuf> {
    path: P,
    semaphore: Option<Arc<Semaphore>>,
    delete_if_empty: bool,
    on_drop_error: Option<fn(io::Error)>,
    progress: Option<Box<dyn DlProgress>>,
}

impl<P: AsRef<Path>> DlFileBuilder<P> {
    pub fn new(path: P) -> Self {
        Self {
            path,
            semaphore: None,
            on_drop_error: None,
            delete_if_empty: true,
            progress: None,
        }
    }

    pub fn delete_if_empty(mut self, delete_if_empty: bool) -> Self {
        self.delete_if_empty = delete_if_empty;
        self
    }

    pub fn with_semaphore(mut self, semaphore: Arc<Semaphore>) -> Self {
        self.semaphore = Some(semaphore);
        self
    }

    pub fn with_semaphore_ref(self, semaphore: &Arc<Semaphore>) -> Self {
        self.with_semaphore(Arc::clone(semaphore))
    }

    pub fn with_progress(mut self, progress: impl DlProgress + 'static) -> Self {
        self.progress = Some(Box::new(progress));
        self
    }

    pub fn on_drop_error(mut self, on_drop_error: fn(io::Error)) -> Self {
        self.on_drop_error = Some(on_drop_error);
        self
    }

    pub async fn open(self, overwrite_behavior: OverwriteBehavior) -> io::Result<DlFile<P>> {
        let path = self.path.as_ref();

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
                        format!("non-empty ({} bytes) file '{}' already exists", meta.len(), self.path.as_ref().display()),
                    ));
                },
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    tokio::fs::File::create(path).await?
                }
                Err(error) => return Err(error),
            },
        };

        Ok(DlFile {
            path: self.path,
            semaphore: self.semaphore,
            on_drop_error: self.on_drop_error,
            delete_if_empty: self.delete_if_empty,
            progress: self.progress,
            file: ManuallyDrop::new(file),
        })
    }

    pub async fn open_overwrite(self) -> io::Result<DlFile<P>> {
        self.open(OverwriteBehavior::Do).await
    }

    pub async fn open_new(self) -> io::Result<DlFile<P>> {
        self.open(OverwriteBehavior::Dont).await
    }

    pub async fn open_overwrite_if_empty(self) -> io::Result<DlFile<P>> {
        self.open(OverwriteBehavior::DoIfEmpty).await
    }
}
