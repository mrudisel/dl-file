use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

pub trait DlProgress {
    fn start(&mut self, path: &Path, total_bytes: Option<u64>);

    fn update(&mut self, path: &Path, bytes_written: u64);

    fn finished(&mut self, path: &Path);
}

impl<P: DlProgress + ?Sized> DlProgress for &mut P {
    fn start(&mut self, path: &Path, total_bytes: Option<u64>) {
        P::start(self, path, total_bytes)
    }

    fn update(&mut self, path: &Path, bytes_written: u64) {
        P::update(self, path, bytes_written)
    }

    fn finished(&mut self, path: &Path) {
        P::finished(self, path)
    }
}

impl<P: DlProgress + ?Sized> DlProgress for Box<P> {
    fn start(&mut self, path: &Path, total_bytes: Option<u64>) {
        P::start(self, path, total_bytes)
    }

    fn update(&mut self, path: &Path, bytes_written: u64) {
        P::update(self, path, bytes_written)
    }

    fn finished(&mut self, path: &Path) {
        P::finished(self, path)
    }
}

impl<P> DlProgress for Arc<P>
where
    P: ?Sized,
    for<'a> &'a P: DlProgress,
{
    fn start(&mut self, path: &Path, total_bytes: Option<u64>) {
        <&P as DlProgress>::start(&mut &**self, path, total_bytes)
    }

    fn update(&mut self, path: &Path, bytes_written: u64) {
        <&P as DlProgress>::update(&mut &**self, path, bytes_written)
    }

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
    S: FnOnce(&mut Ctx, &Path, Option<u64>),
    U: FnMut(&mut Ctx, &Path, u64),
    F: FnOnce(&mut Ctx, &Path),
{
    fn start(&mut self, path: &Path, total_bytes: Option<u64>) {
        if let Some(start_fn) = self.start_fn.take() {
            (start_fn)(&mut self.ctx, path, total_bytes);
        }
    }

    fn update(&mut self, path: &Path, bytes_written: u64) {
        (self.update_fn)(&mut self.ctx, path, bytes_written);
    }

    fn finished(&mut self, path: &Path) {
        if let Some(finish_fn) = self.finish_fn.take() {
            (finish_fn)(&mut self.ctx, path);
        }
    }
}

impl<Ctx, S, U, F> DlProgress for &ProgressContainer<Ctx, S, U, F>
where
    S: Fn(&Ctx, &Path, Option<u64>),
    U: Fn(&Ctx, &Path, u64),
    F: Fn(&Ctx, &Path),
{
    fn start(&mut self, path: &Path, total_bytes: Option<u64>) {
        if let Some(ref start_fn) = self.start_fn {
            (start_fn)(&self.ctx, path, total_bytes);
        }
    }

    fn update(&mut self, path: &Path, bytes_written: u64) {
        (self.update_fn)(&self.ctx, path, bytes_written);
    }

    fn finished(&mut self, path: &Path) {
        if let Some(ref finish_fn) = self.finish_fn {
            (finish_fn)(&self.ctx, path);
        }
    }
}

#[derive(Debug)]
pub struct ProgressHandle<F = fn(&Path, u64, Option<u64>, DlState)> {
    total_bytes: AtomicU64,
    bytes_written: AtomicU64,
    finished: AtomicBool,
    caller: F,
}

impl<F> ProgressHandle<F> {
    pub const fn new(caller: F) -> Self {
        Self {
            total_bytes: AtomicU64::new(0),
            bytes_written: AtomicU64::new(0),
            finished: AtomicBool::new(false),
            caller,
        }
    }

    pub fn state(&self) -> DlState {
        if self.is_finished() {
            DlState::Finished
        } else if self.get_bytes_written() == 0 {
            DlState::Starting
        } else {
            DlState::Running
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Relaxed)
    }

    pub fn get_bytes_written(&self) -> u64 {
        self.bytes_written.load(Relaxed)
    }

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
    F: Fn(&Path, u64, Option<u64>, DlState),
{
    fn start(&mut self, path: &Path, total_bytes: Option<u64>) {
        if let Some(total) = total_bytes {
            self.total_bytes.store(total, Relaxed);
        }

        (self.caller)(path, 0, total_bytes, DlState::Starting);
    }

    fn update(&mut self, path: &Path, bytes_written: u64) {
        let max = self
            .bytes_written
            .fetch_max(bytes_written, Relaxed)
            .max(bytes_written);

        (self.caller)(path, max, self.get_total_bytes(), DlState::Running);
    }

    fn finished(&mut self, path: &Path) {
        self.finished.store(true, Relaxed);

        (self.caller)(
            path,
            self.get_bytes_written(),
            self.get_total_bytes(),
            DlState::Finished,
        );
    }
}
