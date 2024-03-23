use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

pub trait DlProgress: Send {
    fn start(&mut self, path: &Path, total_bytes: Option<u64>);

    fn update(&mut self, path: &Path, bytes_written: u64);

    fn finished(&mut self, path: &Path);
}

impl<P: DlProgress + ?Sized> DlProgress for &mut P {
    #[inline]
    fn start(&mut self, path: &Path, total_bytes: Option<u64>) {
        P::start(self, path, total_bytes)
    }

    #[inline]
    fn update(&mut self, path: &Path, bytes_written: u64) {
        P::update(self, path, bytes_written)
    }

    #[inline]
    fn finished(&mut self, path: &Path) {
        P::finished(self, path)
    }
}

impl<P: DlProgress + ?Sized> DlProgress for Box<P> {
    #[inline]
    fn start(&mut self, path: &Path, total_bytes: Option<u64>) {
        P::start(self, path, total_bytes)
    }

    #[inline]
    fn update(&mut self, path: &Path, bytes_written: u64) {
        P::update(self, path, bytes_written)
    }

    #[inline]
    fn finished(&mut self, path: &Path) {
        P::finished(self, path)
    }
}

impl<P> DlProgress for Arc<P>
where
    P: Send + Sync + ?Sized,
    for<'a> &'a P: DlProgress,
{
    #[inline]
    fn start(&mut self, path: &Path, total_bytes: Option<u64>) {
        <&P as DlProgress>::start(&mut &**self, path, total_bytes)
    }

    #[inline]
    fn update(&mut self, path: &Path, bytes_written: u64) {
        <&P as DlProgress>::update(&mut &**self, path, bytes_written)
    }

    #[inline]
    fn finished(&mut self, path: &Path) {
        <&P as DlProgress>::finished(&mut &**self, path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressContainer<
    Ctx,
    S = fn(&mut Ctx, &Path, Option<u64>),
    U = fn(&mut Ctx, &Path, u64),
    F = fn(&mut Ctx, &Path),
> {
    ctx: Ctx,
    start_fn: Option<S>,
    update_fn: U,
    finish_fn: Option<F>,
}

impl<Ctx, S, U, F> ProgressContainer<Ctx, S, U, F> {
    #[inline]
    pub const fn new(ctx: Ctx, start_fn: S, update_fn: U, finish_fn: F) -> Self {
        Self {
            ctx,
            start_fn: Some(start_fn),
            update_fn,
            finish_fn: Some(finish_fn),
        }
    }
}

impl<Ctx, S, U, F> DlProgress for ProgressContainer<Ctx, S, U, F>
where
    S: FnOnce(&mut Ctx, &Path, Option<u64>) + Send,
    U: FnMut(&mut Ctx, &Path, u64) + Send,
    F: FnOnce(&mut Ctx, &Path) + Send,
    Ctx: Send,
{
    #[inline]
    fn start(&mut self, path: &Path, total_bytes: Option<u64>) {
        if let Some(start_fn) = self.start_fn.take() {
            (start_fn)(&mut self.ctx, path, total_bytes);
        }
    }

    #[inline]
    fn update(&mut self, path: &Path, bytes_written: u64) {
        (self.update_fn)(&mut self.ctx, path, bytes_written);
    }

    #[inline]
    fn finished(&mut self, path: &Path) {
        if let Some(finish_fn) = self.finish_fn.take() {
            (finish_fn)(&mut self.ctx, path);
        }
    }
}

impl<Ctx, S, U, F> DlProgress for &ProgressContainer<Ctx, S, U, F>
where
    Ctx: Sync,
    S: Fn(&Ctx, &Path, Option<u64>) + Sync,
    U: Fn(&Ctx, &Path, u64) + Sync,
    F: Fn(&Ctx, &Path) + Sync,
{
    #[inline]
    fn start(&mut self, path: &Path, total_bytes: Option<u64>) {
        if let Some(ref start_fn) = self.start_fn {
            (start_fn)(&self.ctx, path, total_bytes);
        }
    }

    #[inline]
    fn update(&mut self, path: &Path, bytes_written: u64) {
        (self.update_fn)(&self.ctx, path, bytes_written);
    }

    #[inline]
    fn finished(&mut self, path: &Path) {
        if let Some(ref finish_fn) = self.finish_fn {
            (finish_fn)(&self.ctx, path);
        }
    }
}

#[derive(Debug)]
pub struct ProgressHandle<F = fn(&Path, u64, Option<u64>, DlState)> {
    shared: Arc<ProgressHandleShared>,
    on_update: F,
}

#[derive(Debug)]
pub struct ProgressHandleShared {
    total_bytes: AtomicU64,
    bytes_written: AtomicU64,
    finished: AtomicBool,
}

impl<F> ProgressHandle<F> {
    #[inline]
    pub fn new(on_update: F) -> Self {
        Self {
            shared: Arc::new(ProgressHandleShared {
                total_bytes: AtomicU64::new(0),
                bytes_written: AtomicU64::new(0),
                finished: AtomicBool::new(false),
            }),
            on_update,
        }
    }

    #[inline]
    pub fn shared(&self) -> &Arc<ProgressHandleShared> {
        &self.shared
    }
}

impl ProgressHandleShared {
    #[inline]
    pub fn state(&self) -> DlState {
        if self.is_finished() {
            DlState::Finished
        } else if self.get_bytes_written() == 0 {
            DlState::Starting
        } else {
            DlState::Running
        }
    }

    #[inline]
    pub fn is_finished(&self) -> bool {
        self.finished.load(Relaxed)
    }

    #[inline]
    pub fn get_bytes_written(&self) -> u64 {
        self.bytes_written.load(Relaxed)
    }

    #[inline]
    pub fn get_total_bytes(&self) -> Option<u64> {
        match self.total_bytes.load(Relaxed) {
            0 => None,
            non_zero => Some(non_zero),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DlState {
    Starting,
    Running,
    Finished,
}

impl<F> DlProgress for &ProgressHandle<F>
where
    F: Fn(&Path, u64, Option<u64>, DlState) + Sync,
{
    #[inline]
    fn start(&mut self, path: &Path, total_bytes: Option<u64>) {
        if let Some(total) = total_bytes {
            self.shared.total_bytes.store(total, Relaxed);
        }

        (self.on_update)(path, 0, total_bytes, DlState::Starting);
    }

    #[inline]
    fn update(&mut self, path: &Path, bytes_written: u64) {
        let max = self
            .shared
            .bytes_written
            .fetch_max(bytes_written, Relaxed)
            .max(bytes_written);

        (self.on_update)(path, max, self.shared.get_total_bytes(), DlState::Running);
    }

    #[inline]
    fn finished(&mut self, path: &Path) {
        self.shared.finished.store(true, Relaxed);

        (self.on_update)(
            path,
            self.shared.get_bytes_written(),
            self.shared.get_total_bytes(),
            DlState::Finished,
        );
    }
}
