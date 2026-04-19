//! `Task<T>`: a value computed on a background thread, polled by the UI.

use std::sync::mpsc::{Receiver, channel};

pub struct Task<T>(pub(super) Receiver<T>);

impl<T: Send + 'static> Task<T> {
    pub fn spawn(ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
        Self(rx)
    }

    /// Takes the result out of `slot` once it is ready.
    pub fn poll(slot: &mut Option<Self>) -> Option<T> {
        let v = slot.as_ref()?.0.try_recv().ok()?;
        *slot = None;
        Some(v)
    }
}
