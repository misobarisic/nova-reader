//! Transfer lifetime, independent of Slint frames or a retained Activity.
//! Nova's Java service/JNI remain unchanged; Reader supplies chapter progress.
use reader_downloads::DownloadStore;

pub(crate) struct Lease {
    #[cfg(target_os = "android")]
    inner: Option<ActivityLease>,
}

/// Acquire on the initiating UI action, before metadata/network work starts.
/// Android restricts starting a foreground service after the app backgrounds.
pub(crate) fn begin(store: &DownloadStore) -> Lease {
    #[cfg(target_os = "android")]
    {
        use std::sync::OnceLock;
        static MONITOR: OnceLock<std::sync::Arc<Monitor>> = OnceLock::new();
        let monitor = MONITOR.get_or_init(|| {
            Monitor::new(
                store.clone(),
                crate::android_downloads::set_download_service,
            )
        });
        Lease {
            inner: Some(monitor.begin()),
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = store;
        Lease {}
    }
}

#[cfg(target_os = "android")]
impl Drop for Lease {
    fn drop(&mut self) {
        self.inner.take();
    }
}

#[cfg(any(target_os = "android", test))]
use std::sync::{Arc, Condvar, Mutex};

#[cfg(any(target_os = "android", test))]
#[derive(Default)]
struct Activity {
    requests: usize,
    watching: bool,
}

#[cfg(any(target_os = "android", test))]
struct Monitor {
    store: DownloadStore,
    activity: Mutex<Activity>,
    changed: Condvar,
    publish: Box<dyn Fn(bool, &str, &str) + Send + Sync>,
}

#[cfg(any(target_os = "android", test))]
impl Monitor {
    fn new(
        store: DownloadStore,
        publish: impl Fn(bool, &str, &str) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            activity: Mutex::default(),
            changed: Condvar::new(),
            publish: Box::new(publish),
        })
    }

    fn begin(self: &Arc<Self>) -> ActivityLease {
        let mut activity = self.activity.lock().unwrap_or_else(|e| e.into_inner());
        activity.requests += 1;
        // No filesystem scan on the event loop. Promotion happens immediately;
        // the worker fills in page progress once metadata has been prepared.
        (self.publish)(true, &title(), &preparing());
        if !activity.watching {
            activity.watching = true;
            let monitor = self.clone();
            if let Err(error) = std::thread::Builder::new()
                .name("reader-download-notification".into())
                .spawn(move || monitor.watch())
            {
                activity.watching = false;
                eprintln!("Reader download notification worker: {error}");
            }
        }
        self.changed.notify_all();
        ActivityLease(self.clone())
    }

    fn watch(&self) {
        loop {
            // Read only atomic manifests, never hash or decode downloaded pages.
            // Do not hold the lifetime lock across filesystem I/O.
            let progress = self.store.progress();
            let mut activity = self.activity.lock().unwrap_or_else(|e| e.into_inner());
            if activity.requests == 0 {
                activity.watching = false;
                return;
            }
            let body = match progress {
                Ok(progress) if progress.total_pages > 0 => {
                    nova_reader_ui::backend_text::download_progress(
                        progress.queued + progress.running,
                        progress.completed_pages,
                        progress.total_pages,
                    )
                }
                _ => preparing(),
            };
            // Serialize the publication with last-lease stop. An old polling
            // result must never restart the service after work has ended.
            (self.publish)(true, &title(), &body);
            activity = self
                .changed
                .wait_timeout(activity, std::time::Duration::from_secs(1))
                .unwrap_or_else(|e| e.into_inner())
                .0;
            if activity.requests == 0 {
                activity.watching = false;
                return;
            }
        }
    }
}

#[cfg(any(target_os = "android", test))]
fn title() -> String {
    nova_reader_ui::backend_text::tr("Downloading chapters")
}
#[cfg(any(target_os = "android", test))]
fn preparing() -> String {
    nova_reader_ui::backend_text::tr("Preparing downloads…")
}

#[cfg(any(target_os = "android", test))]
struct ActivityLease(Arc<Monitor>);
#[cfg(any(target_os = "android", test))]
impl Drop for ActivityLease {
    fn drop(&mut self) {
        let mut activity = self.0.activity.lock().unwrap_or_else(|e| e.into_inner());
        activity.requests -= 1;
        if activity.requests == 0 {
            (self.0.publish)(false, "", "");
        }
        self.0.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{panic::AssertUnwindSafe, time::Duration};

    #[test]
    fn page_progress_and_completion_publish_without_a_window_or_ui_tick() {
        use reader_domain::{ChapterId, SeriesId};
        use reader_downloads::PageResource;
        use std::{collections::BTreeMap, io::Cursor, sync::mpsc};
        let root = tempfile::tempdir().unwrap();
        let store = DownloadStore::open(root.path(), 1024 * 1024).unwrap();
        let chapter = ChapterId::new(SeriesId::new("fixture", "series"), "chapter");
        store
            .enqueue(
                chapter.clone(),
                (0..2)
                    .map(|index| PageResource {
                        id: format!("page-{index}"),
                        url: format!("https://example.invalid/{index}.png"),
                        headers: BTreeMap::new(),
                    })
                    .collect(),
            )
            .unwrap();
        let (events, notifications) = mpsc::channel();
        let monitor = Monitor::new(store.clone(), move |busy, _, body| {
            let _ = events.send((busy, body.to_owned()));
        });
        let lease = monitor.begin();
        let (second_page, reached_second_page) = mpsc::channel();
        let (finish, may_finish) = mpsc::channel();
        let mut png = Cursor::new(Vec::new());
        image::RgbaImage::new(2, 2)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let download = std::thread::spawn({
            let store = store.clone();
            let chapter = chapter.clone();
            move || {
                let _lease = lease;
                let mut page = 0;
                store
                    .run(&chapter, |_| {
                        page += 1;
                        if page == 2 {
                            second_page.send(()).unwrap();
                            may_finish.recv_timeout(Duration::from_secs(5)).unwrap();
                        }
                        Ok(png.get_ref().clone())
                    })
                    .unwrap();
            }
        });
        reached_second_page
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "no background progress notification"
            );
            let (busy, body) = notifications.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(busy);
            if body.contains("1 / 2 pages") {
                break;
            }
        }
        finish.send(()).unwrap();
        download.join().unwrap();
        loop {
            if !notifications
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .0
            {
                break;
            }
        }
        assert_eq!(store.offline(&chapter).unwrap().pages.len(), 2);
    }

    #[test]
    fn service_lifetime_survives_navigation_and_stops_after_last_worker_or_panic() {
        let root = tempfile::tempdir().unwrap();
        let store = DownloadStore::open(root.path(), 1024 * 1024).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let monitor = Monitor::new(store, {
            let events = events.clone();
            move |busy, _, _| events.lock().unwrap().push(busy)
        });
        let first = monitor.begin();
        let second = monitor.begin();
        assert_eq!(events.lock().unwrap().last(), Some(&true));
        // No window, event loop, tick, or Activity exists in this test.
        drop(first);
        assert_eq!(events.lock().unwrap().last(), Some(&true));
        assert!(
            std::panic::catch_unwind(AssertUnwindSafe(|| {
                let _second = second;
                panic!("interrupted transfer");
            }))
            .is_err()
        );
        assert_eq!(events.lock().unwrap().last(), Some(&false));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while monitor.activity.lock().unwrap().watching {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        // A new queue can start after shutdown; stale pollers cannot stop it.
        let next = monitor.begin();
        assert_eq!(events.lock().unwrap().last(), Some(&true));
        drop(next);
        assert_eq!(events.lock().unwrap().last(), Some(&false));
    }
}
