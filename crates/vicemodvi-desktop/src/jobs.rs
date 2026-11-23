//! Background work that reports back to the UI thread.

use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use eframe::egui;
use vicemodvi_core::Cancel;

pub enum Poll<T> {
    Pending,
    Done(T),
    /// The worker panicked or stopped without a result.
    Failed,
}

pub struct Job<T> {
    rx: Receiver<T>,
    pub cancel: Cancel,
}

impl<T: Send + 'static> Job<T> {
    /// Runs `work` on its own thread and repaints when it finishes.
    pub fn spawn(ctx: &egui::Context, work: impl FnOnce(&Cancel) -> T + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancel = Cancel::new();
        let token = cancel.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            if let Ok(value) = catch_unwind(AssertUnwindSafe(|| work(&token))) {
                let _ = tx.send(value);
            }
            ctx.request_repaint();
        });
        Self { rx, cancel }
    }

    pub fn poll(&self) -> Poll<T> {
        match self.rx.try_recv() {
            Ok(value) => Poll::Done(value),
            Err(TryRecvError::Empty) => Poll::Pending,
            Err(TryRecvError::Disconnected) => Poll::Failed,
        }
    }
}
