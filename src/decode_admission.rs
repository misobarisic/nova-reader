//! One non-preemptible heavy image decoder, with foreground-first admission.
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::Duration;
#[derive(Clone, Copy)]
pub(crate) enum Priority {
    Viewer = 0,
    VisibleThumbnail = 1,
    Prefetch = 2,
}
#[derive(Default)]
struct State {
    active: bool,
    waiting: [usize; 3],
}
#[derive(Default)]
pub(crate) struct Admission {
    state: Mutex<State>,
    changed: Condvar,
}
pub(crate) static SHARED: LazyLock<Arc<Admission>> =
    LazyLock::new(|| Arc::new(Admission::default()));
pub(crate) struct Permit(Arc<Admission>);
impl Admission {
    /// Take the decoder only if it is free right now and nothing of higher
    /// priority is waiting for it; never waits.
    pub(crate) fn try_acquire(self: &Arc<Self>, priority: Priority) -> Option<Permit> {
        let index = priority as usize;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.active || state.waiting[..=index].iter().any(|count| *count != 0) {
            return None;
        }
        state.active = true;
        Some(Permit(self.clone()))
    }
    pub(crate) fn acquire(
        self: &Arc<Self>,
        priority: Priority,
        current: impl Fn() -> bool,
    ) -> Option<Permit> {
        let index = priority as usize;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.waiting[index] += 1;
        loop {
            if !current() {
                state.waiting[index] -= 1;
                self.changed.notify_all();
                return None;
            }
            if !state.active && state.waiting[..index].iter().all(|count| *count == 0) {
                state.waiting[index] -= 1;
                state.active = true;
                return Some(Permit(self.clone()));
            }
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(25))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active = false;
        self.0.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    #[test]
    fn foreground_wins_queued_thumbnail_and_prefetch_and_waits_cancel() {
        let admission = Arc::new(Admission::default());
        let active = admission.acquire(Priority::Viewer, || true).unwrap();
        let (sender, receiver) = mpsc::channel();
        let mut workers = Vec::new();
        for (priority, value) in [
            (Priority::Prefetch, 2),
            (Priority::VisibleThumbnail, 1),
            (Priority::Viewer, 0),
        ] {
            let admission = admission.clone();
            let sender = sender.clone();
            workers.push(std::thread::spawn(move || {
                let _permit = admission.acquire(priority, || true).unwrap();
                sender.send(value).unwrap();
            }));
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while admission.state.lock().unwrap().waiting != [1, 1, 1] {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let token = cancelled.clone();
        let waiting_admission = admission.clone();
        let waiting = std::thread::spawn(move || {
            waiting_admission
                .acquire(Priority::Prefetch, || !token.load(Ordering::Acquire))
                .is_none()
        });
        cancelled.store(true, Ordering::Release);
        assert!(waiting.join().unwrap());
        drop(active);
        assert_eq!(receiver.recv_timeout(Duration::from_secs(5)).unwrap(), 0);
        assert_eq!(receiver.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
        assert_eq!(receiver.recv_timeout(Duration::from_secs(5)).unwrap(), 2);
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn try_acquire_never_waits_and_never_jumps_the_queue() {
        let admission = Arc::new(Admission::default());
        let active = admission.try_acquire(Priority::Prefetch).unwrap();
        assert!(admission.try_acquire(Priority::Prefetch).is_none());
        assert!(admission.try_acquire(Priority::Viewer).is_none());
        drop(active);
        // A waiting viewer (or an equal-priority waiter) comes first even
        // while the decoder is free; a waiting lower priority does not.
        for (waiter, priority, free) in [
            (0, Priority::Prefetch, false),
            (1, Priority::Prefetch, false),
            (2, Priority::Prefetch, false),
            (2, Priority::VisibleThumbnail, true),
        ] {
            admission.state.lock().unwrap().waiting[waiter] += 1;
            assert_eq!(admission.try_acquire(priority).is_some(), free);
            admission.state.lock().unwrap().waiting[waiter] -= 1;
        }
        assert!(admission.try_acquire(Priority::Prefetch).is_some());
    }
}
