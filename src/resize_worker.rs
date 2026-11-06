//! Coalesce geometry work outside the low-level mouse hook. The final queued rect
//! is applied before a session becomes available to the next gesture.
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use crate::engine::{geometry::Rect, state::WindowId};

#[derive(Default)]
struct Pending {
    hwnd: Option<WindowId>,
    resizing: bool,
    rect: Option<Rect>,
    last: Option<Rect>,
    ending: bool,
    failed: bool,
}
impl Pending {
    fn begin(&mut self, hwnd: WindowId, rect: Rect, resizing: bool) -> bool {
        if self.hwnd.is_some() { return false; }
        *self = Self { hwnd: Some(hwnd), resizing, last: Some(rect), ..Self::default() };
        true
    }
    fn update(&mut self, rect: Rect) -> bool {
        if self.hwnd.is_none() || self.ending || self.failed { return false; }
        self.rect = Some(rect);
        true
    }
    fn settle(&mut self) {
        if self.ending && self.rect.is_none() { *self = Self::default(); }
    }
    fn take(&mut self) -> Option<(WindowId, Rect)> {
        let rect = self.rect.take()?;
        if self.last == Some(rect) { return None; }
        Some((self.hwnd?, rect))
    }
}
struct Worker { pending: Mutex<Pending>, ready: Condvar }
static WORKER: OnceLock<Option<Arc<Worker>>> = OnceLock::new();
fn worker() -> Option<&'static Arc<Worker>> {
    WORKER.get_or_init(|| {
        let state = Arc::new(Worker { pending: Mutex::new(Pending::default()), ready: Condvar::new() });
        let thread_state = state.clone();
        match std::thread::Builder::new().name("window-resize".into()).spawn(move || run(thread_state)) {
            Ok(_) => Some(state),
            Err(error) => { log::error!("Could not start resize worker: {error}"); None }
        }
    }).as_ref()
}
pub fn finishing() -> bool {
    WORKER.get().and_then(Option::as_ref)
        .is_some_and(|w| w.pending.lock().unwrap().ending)
}
pub fn begin(hwnd: WindowId, rect: Rect, resizing: bool) -> bool {
    worker().is_some_and(|w| w.pending.lock().unwrap().begin(hwnd, rect, resizing))
}
pub fn update(rect: Rect) -> bool {
    worker().is_some_and(|w| {
        let ok = w.pending.lock().unwrap().update(rect);
        w.ready.notify_one();
        ok
    })
}
pub fn end() {
    if let Some(w) = worker() {
        let mut pending = w.pending.lock().unwrap();
        if pending.hwnd.is_some() { pending.ending = true; }
        w.ready.notify_one();
    }
}
fn run(worker: Arc<Worker>) {
    loop {
        let mut pending = worker.pending.lock().unwrap();
        while pending.rect.is_none() && !pending.ending {
            pending = worker.ready.wait(pending).unwrap();
        }
        if let Some((hwnd, rect)) = pending.take() {
            let resizing = pending.resizing;
            drop(pending);
            let ok = unsafe {
                let hwnd = crate::win_api::id_to_hwnd(hwnd);
                if resizing { crate::win_api::resize_window_rect(hwnd, rect) }
                else { crate::win_api::set_window_rect(hwnd, rect) }
            };
            pending = worker.pending.lock().unwrap();
            if ok { pending.last = Some(rect); }
            else {
                pending.failed = true;
                pending.rect = None;
                log::warn!("Could not update the target window geometry");
            }
        }
        // New moves can arrive while the target processes SetWindowPos.
        pending.settle();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn rect(n: i32) -> Rect { Rect { left: 0, top: 0, right: n, bottom: n } }
    #[test]
    fn final_pending_size_precedes_next_session() {
        let mut p = Pending::default();
        assert!(p.begin(WindowId(1), rect(100), true));
        assert!(p.update(rect(110)));
        assert!(p.update(rect(120)));
        p.ending = true;
        assert!(!p.begin(WindowId(2), rect(200), true));
        assert!(!p.update(rect(130)));
        assert_eq!(p.take(), Some((WindowId(1), rect(120))));
        assert!(p.rect.is_none());
        p.settle();
        assert!(p.begin(WindowId(2), rect(200), true));
    }
    #[test]
    fn release_during_in_flight_update_preserves_latest_rectangle() {
        let mut p = Pending::default();
        assert!(p.begin(WindowId(1), rect(100), false));
        assert!(!p.resizing);
        assert!(p.update(rect(110)));
        let (_, in_flight) = p.take().unwrap();
        assert!(p.update(rect(130)));
        p.ending = true;
        p.last = Some(in_flight);
        p.settle();
        assert!(!p.begin(WindowId(2), rect(200), true));
        assert_eq!(p.take(), Some((WindowId(1), rect(130))));
        p.last = Some(rect(130));
        p.settle();
        assert!(p.begin(WindowId(2), rect(200), true));
    }
    #[test]
    fn unchanged_and_failed_updates_are_not_applied() {
        let mut p = Pending::default();
        assert!(!p.update(rect(90)));
        assert!(p.begin(WindowId(1), rect(100), true));
        assert!(p.update(rect(100)));
        assert!(p.take().is_none());
        p.failed = true;
        assert!(!p.update(rect(120)));
    }
}
