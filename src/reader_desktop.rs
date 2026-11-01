//! Desktop display-idle inhibition. Following Nova's player idle worker, the
//! UI publishes only a latest desired state; one worker owns native resources.
//! No player, rendering, or persisted state is involved.

use std::sync::{Arc, Mutex, OnceLock, mpsc};

#[derive(Clone, Copy, Default)]
struct Request {
    wanted: bool,
    revision: u64,
}
struct Worker {
    latest: Arc<Mutex<Request>>,
    wakeup: mpsc::SyncSender<()>,
}
impl Worker {
    fn spawn(
        mut converge: impl FnMut(bool) + Send + 'static,
    ) -> std::io::Result<(Self, std::thread::JoinHandle<()>)> {
        let latest = Arc::new(Mutex::new(Request::default()));
        let requested = latest.clone();
        let (wakeup, receiver) = mpsc::sync_channel(1);
        let join = std::thread::Builder::new()
            .name("reader-keep-awake".into())
            .spawn(move || {
                let mut applied_revision = 0;
                while receiver.recv().is_ok() {
                    let request = *requested.lock().unwrap_or_else(|error| error.into_inner());
                    if request.revision != applied_revision {
                        applied_revision = request.revision;
                        converge(request.wanted);
                    }
                }
                // Tests and nonstatic owners may close the mailbox while an
                // acquisition is in flight. Release after it returns, too.
                converge(false);
            })?;
        Ok((Self { latest, wakeup }, join))
    }
    fn request(&self, wanted: bool) {
        {
            let mut latest = self
                .latest
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if latest.wanted == wanted {
                return;
            }
            latest.wanted = wanted;
            latest.revision = latest.revision.wrapping_add(1);
        }
        // One pending wake is enough; never wait for the native service here.
        let _ = self.wakeup.try_send(());
    }
}
fn converge<G>(
    lease: &mut Option<G>,
    wanted: bool,
    acquire: &mut impl FnMut() -> Result<G, String>,
) {
    if !wanted {
        *lease = None;
    } else if lease.is_none() {
        match acquire() {
            Ok(acquired) => *lease = Some(acquired),
            Err(error) => eprintln!("Reader keep awake unavailable: {error}"),
        }
    }
}

/// Keep the desktop display awake only while the caller requests it. False at
/// startup creates no worker or native connections. Failed acquisition retries
/// on the next off/on interval, rather than on every scrolling or OSD update.
pub fn keep_awake(wanted: bool) {
    static WORKER: OnceLock<Option<Worker>> = OnceLock::new();
    if !wanted && WORKER.get().is_none() {
        return;
    }
    let worker = WORKER.get_or_init(|| {
        let mut lease = None;
        match Worker::spawn(move |wanted| converge(&mut lease, wanted, &mut platform::acquire)) {
            Ok((worker, _)) => Some(worker),
            Err(error) => {
                eprintln!("Reader keep awake worker unavailable: {error}");
                None
            }
        }
    });
    if let Some(worker) = worker {
        worker.request(wanted);
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::{collections::HashMap, time::Duration};
    use zbus::{
        blocking::{Connection, Proxy, connection::Builder},
        zvariant::{OwnedObjectPath, Value},
    };

    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
    const SCREEN_SAVER: &str = "org.freedesktop.ScreenSaver";
    const SCREEN_SAVER_PATH: &str = "/org/freedesktop/ScreenSaver";
    // Bound native method calls, including release. Connection establishment
    // uses the local session bus; zbus exposes no separate handshake timeout.
    const METHOD_TIMEOUT: Duration = Duration::from_secs(2);

    pub(super) enum Guard {
        Portal {
            connection: Connection,
            request: OwnedObjectPath,
        },
        ScreenSaver {
            connection: Connection,
            cookie: u32,
        },
    }
    pub(super) fn acquire() -> Result<Guard, String> {
        let connection = Builder::session()
            .and_then(|builder| builder.method_timeout(METHOD_TIMEOUT).build())
            .map_err(|error| error.to_string())?;
        let portal = (|| -> zbus::Result<OwnedObjectPath> {
            let proxy = Proxy::new(
                &connection,
                PORTAL,
                PORTAL_PATH,
                "org.freedesktop.portal.Inhibit",
            )?;
            let options = HashMap::from([("reason", Value::from("Reading in Nova Reader"))]);
            // Only idle/display inhibition: never inhibit suspend or logout.
            proxy.call("Inhibit", &("", 8_u32, options))
        })();
        match portal {
            Ok(request) => Ok(Guard::Portal {
                connection,
                request,
            }),
            Err(portal_error) => {
                let fallback = (|| -> zbus::Result<u32> {
                    let proxy =
                        Proxy::new(&connection, SCREEN_SAVER, SCREEN_SAVER_PATH, SCREEN_SAVER)?;
                    proxy.call("Inhibit", &("Nova Reader", "Reading"))
                })();
                fallback
                    .map(|cookie| Guard::ScreenSaver { connection, cookie })
                    .map_err(|error| format!("portal: {portal_error}; screensaver: {error}"))
            }
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let result = match self {
                Self::Portal {
                    connection,
                    request,
                } => Proxy::new(
                    connection,
                    PORTAL,
                    request.as_str(),
                    "org.freedesktop.portal.Request",
                )
                .and_then(|proxy| proxy.call::<_, _, ()>("Close", &())),
                Self::ScreenSaver { connection, cookie } => {
                    Proxy::new(connection, SCREEN_SAVER, SCREEN_SAVER_PATH, SCREEN_SAVER)
                        .and_then(|proxy| proxy.call::<_, _, ()>("UnInhibit", &(*cookie,)))
                }
            };
            if let Err(error) = result {
                eprintln!("Reader keep awake release: {error}");
            }
            // The guard also owns the last connection reference. Disconnecting
            // releases its request/cookie even if the explicit close failed.
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetThreadExecutionState(flags: u32) -> u32;
    }
    // Acquisition and Drop both execute on the same dedicated worker thread;
    // Windows execution state belongs to that thread, not to the UI thread.
    pub(super) struct Guard;
    pub(super) fn acquire() -> Result<Guard, String> {
        // SAFETY: Windows accepts these documented execution-state flags.
        if unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED) } == 0 {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(Guard)
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: release the state on the thread that acquired it.
            if unsafe { SetThreadExecutionState(ES_CONTINUOUS) } == 0 {
                eprintln!(
                    "Reader keep awake release: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::process::{Child, Command, Stdio};
    pub(super) struct Guard(Child);
    pub(super) fn acquire() -> Result<Guard, String> {
        // -w also terminates caffeinate if Reader exits without running Drop.
        Command::new("/usr/bin/caffeinate")
            .args(["-d", "-w", &std::process::id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(Guard)
            .map_err(|error| error.to_string())
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod platform {
    pub(super) struct Guard;
    pub(super) fn acquire() -> Result<Guard, String> {
        Err("display inhibition is unavailable on this desktop".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    const DEADLINE: Duration = Duration::from_secs(2);
    struct Lease(mpsc::Sender<()>);
    impl Drop for Lease {
        fn drop(&mut self) {
            self.0.send(()).unwrap();
        }
    }

    #[test]
    fn leases_release_on_off_and_failures_do_not_claim_a_lease() {
        let (released, releases) = mpsc::channel();
        let mut lease = None;
        let mut attempts = 0;
        let mut acquire = || {
            attempts += 1;
            Ok(Lease(released.clone()))
        };
        converge(&mut lease, false, &mut acquire);
        converge(&mut lease, true, &mut acquire);
        converge(&mut lease, true, &mut acquire);
        converge(&mut lease, false, &mut acquire);
        converge(&mut lease, false, &mut acquire);
        assert_eq!(attempts, 1);
        releases.recv_timeout(DEADLINE).unwrap();
        assert!(releases.try_recv().is_err());
        converge(&mut lease, true, &mut || Err("denied".into()));
        assert!(lease.is_none());
    }

    #[test]
    fn slow_acquisition_coalesces_to_latest_off_and_worker_drop_releases() {
        let (entered, entries) = mpsc::channel();
        let (finish, finishes) = mpsc::channel();
        let (released, releases) = mpsc::channel();
        let mut lease = None;
        let (worker, join) = Worker::spawn(move |wanted| {
            converge(&mut lease, wanted, &mut || {
                entered.send(()).unwrap();
                finishes.recv().unwrap();
                Ok(Lease(released.clone()))
            });
        })
        .unwrap();
        worker.request(false);
        worker.request(true);
        entries.recv_timeout(DEADLINE).unwrap();
        for _ in 0..1000 {
            worker.request(false);
            worker.request(true);
        }
        worker.request(false);
        finish.send(()).unwrap();
        releases.recv_timeout(DEADLINE).unwrap();
        worker.request(true);
        entries.recv_timeout(DEADLINE).unwrap();
        drop(worker);
        finish.send(()).unwrap();
        join.join().unwrap();
        releases.recv_timeout(DEADLINE).unwrap();
        assert!(entries.try_recv().is_err());
        assert!(releases.try_recv().is_err());
    }

    #[test]
    fn coalesced_off_on_retries_failed_interval_and_steady_ticks_do_no_work() {
        let (entered, entries) = mpsc::channel();
        let (finish, finishes) = mpsc::channel();
        let (released, releases) = mpsc::channel();
        let mut lease = None;
        let (worker, join) = Worker::spawn(move |wanted| {
            converge(&mut lease, wanted, &mut || {
                entered.send(()).unwrap();
                if finishes.recv().unwrap() {
                    Ok(Lease(released.clone()))
                } else {
                    Err("denied".into())
                }
            });
        })
        .unwrap();
        worker.request(true);
        entries.recv_timeout(DEADLINE).unwrap();
        worker.request(false);
        worker.request(true);
        finish.send(false).unwrap();
        entries.recv_timeout(DEADLINE).unwrap();
        for _ in 0..100 {
            worker.request(true);
        }
        finish.send(true).unwrap();
        drop(worker);
        join.join().unwrap();
        releases.recv_timeout(DEADLINE).unwrap();
        assert!(entries.try_recv().is_err());
    }
}
