use std::{path::Path, sync::Arc};

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
