//! Coalesced periodic checkpoints. Storage never runs on the scroll/UI thread;
//! explicit navigation and lifecycle flushes still wait for a durable result.
use reader_domain::{ChapterId, DomainStore, ReadingProgress};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
};

type Pending = Arc<Mutex<BTreeMap<ChapterId, ReadingProgress>>>;
type WriteResult = Result<(), String>;

enum Command {
    Wake,
    Barrier(mpsc::Sender<WriteResult>),
    Stop,
}

pub(crate) struct ProgressWriter {
    pending: Pending,
    sender: mpsc::SyncSender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl ProgressWriter {
    pub(crate) fn new(domain: DomainStore) -> std::io::Result<Self> {
        Self::spawn(move |batch| {
            domain
                .update(|state| {
                    for progress in batch {
                        state.set_progress(progress.clone());
                    }
                    Ok(())
                })
                .map_err(|error| error.to_string())
        })
    }

    fn spawn(
        write: impl Fn(&[ReadingProgress]) -> WriteResult + Send + 'static,
    ) -> std::io::Result<Self> {
        let pending = Pending::default();
        let worker_pending = pending.clone();
        // Repeated timer ticks need at most one wake-up; their latest values
        // live in the map, not in an unbounded queue of old scroll offsets.
        let (sender, receiver) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("reader-progress".into())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    let result = write_pending(&worker_pending, &write);
                    match command {
                        Command::Wake => {
                            if let Err(error) = result {
                                eprintln!("Nova Reader: could not save reading position: {error}");
                            }
                        }
                        Command::Barrier(ack) => {
                            let _ = ack.send(result);
                        }
                        Command::Stop => {
                            if let Err(error) = result {
                                eprintln!("Nova Reader: final reading checkpoint failed: {error}");
                            }
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            pending,
            sender,
            thread: Some(thread),
        })
    }

    pub(crate) fn enqueue(&self, progress: ReadingProgress) -> WriteResult {
        self.insert(progress);
        match self.sender.try_send(Command::Wake) {
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                Err("Reading checkpoint worker stopped".into())
            }
        }
    }

    pub(crate) fn flush(&self, progress: Option<ReadingProgress>) -> WriteResult {
        if let Some(progress) = progress {
            self.insert(progress);
        }
        let (ack, result) = mpsc::channel();
        // No pending-map lock is held while waiting. A barrier flushes the map
        // itself even when a coalesced Wake could not enter the bounded queue.
        self.sender
            .send(Command::Barrier(ack))
            .map_err(|_| "Reading checkpoint worker stopped".to_string())?;
        result
            .recv()
            .map_err(|_| "Reading checkpoint worker stopped".to_string())?
    }

    fn insert(&self, progress: ReadingProgress) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(progress.chapter.clone(), progress);
    }
}

impl Drop for ProgressWriter {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn write_pending(
    pending: &Pending,
    write: &impl Fn(&[ReadingProgress]) -> WriteResult,
) -> WriteResult {
    let batch = std::mem::take(&mut *pending.lock().unwrap_or_else(|e| e.into_inner()));
    if batch.is_empty() {
        return Ok(());
    }
    let batch = batch.into_values().collect::<Vec<_>>();
    let result = write(&batch);
    if result.is_err() {
        let mut pending = pending.lock().unwrap_or_else(|e| e.into_inner());
        for progress in batch {
            // An offset queued while storage was busy is newer than this
            // failed attempt. Retrying must never overwrite that newer value.
            pending.entry(progress.chapter.clone()).or_insert(progress);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use reader_domain::{ReadingMode, ResumeAnchor, SeriesId};
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    fn progress(chapter: &str, page: usize) -> ReadingProgress {
        ReadingProgress {
            chapter: ChapterId::new(SeriesId::new("fixture", "series"), chapter),
            anchor: ResumeAnchor::new(format!("page-{page}"), page, 0.).unwrap(),
            mode: ReadingMode::Webtoon,
            last_read_at: page as i64,
            completed: false,
        }
    }

    #[test]
    fn busy_storage_does_not_block_enqueue_and_latest_offsets_coalesce_per_chapter() {
        let (entered, entering) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let writes = Arc::new(Mutex::new(Vec::new()));
        let saved = writes.clone();
        let calls = AtomicUsize::new(0);
        let writer = ProgressWriter::spawn(move |batch| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            saved.lock().unwrap().extend_from_slice(batch);
            Ok(())
        })
        .unwrap();
        writer.enqueue(progress("a", 1)).unwrap();
        entering.recv_timeout(Duration::from_secs(5)).unwrap();
        // These calls finish while the write closure is deliberately blocked.
        writer.enqueue(progress("a", 2)).unwrap();
        writer.enqueue(progress("a", 3)).unwrap();
        writer.enqueue(progress("b", 4)).unwrap();
        assert_eq!(writer.pending.lock().unwrap().len(), 2);
        writer.insert(progress("a", 5));
        release.send(()).unwrap();
        writer.flush(None).unwrap();
        let writes = writes.lock().unwrap();
        assert_eq!(writes.last().unwrap().chapter.local_id, "b");
        assert_eq!(
            writes
                .iter()
                .rev()
                .find(|p| p.chapter.local_id == "a")
                .unwrap()
                .anchor
                .page_index,
            5
        );
        assert!(!writes.iter().any(|p| [2, 3].contains(&p.anchor.page_index)));
    }

    #[test]
    fn failure_is_reported_and_retained_for_retry_without_overwriting_newer_offsets() {
        let pending = Pending::default();
        let first = progress("a", 1);
        pending.lock().unwrap().insert(first.chapter.clone(), first);
        let newer = progress("a", 2);
        let result = write_pending(&pending, &|_| {
            pending
                .lock()
                .unwrap()
                .insert(newer.chapter.clone(), newer.clone());
            Err("disk full".into())
        });
        assert_eq!(result, Err("disk full".into()));
        assert_eq!(
            pending
                .lock()
                .unwrap()
                .values()
                .next()
                .unwrap()
                .anchor
                .page_index,
            2
        );
        let fail = Arc::new(AtomicUsize::new(0));
        let attempts = fail.clone();
        let writer = ProgressWriter::spawn(move |_| {
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                Err("disk full".into())
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(
            writer.flush(Some(progress("a", 9))),
            Err("disk full".into())
        );
        assert_eq!(writer.pending.lock().unwrap().len(), 1);
        writer.flush(None).unwrap();
        assert!(writer.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn dropping_writer_saves_the_final_pending_checkpoint() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let saved = writes.clone();
        let writer = ProgressWriter::spawn(move |batch| {
            saved.lock().unwrap().extend_from_slice(batch);
            Ok(())
        })
        .unwrap();
        writer.insert(progress("a", 7));
        drop(writer);
        assert_eq!(writes.lock().unwrap()[0].anchor.page_index, 7);
    }

    #[test]
    fn final_checkpoint_survives_a_process_restart() {
        const MODE: &str = "READER_CHECKPOINT_RESTART_MODE";
        const DIRECTORY: &str = "READER_CHECKPOINT_RESTART_DIRECTORY";
        if let Ok(mode) = std::env::var(MODE) {
            let domain = DomainStore::open(std::env::var(DIRECTORY).unwrap()).unwrap();
            if mode == "write" {
                let chapter = progress("a", 1).chapter;
                domain
                    .update(|state| {
                        state.series.push(reader_domain::Series {
                            publication_status: Default::default(),
                            id: chapter.series.clone(),
                            title: "Checkpoint fixture".into(),
                            description: String::new(),
                            authors: vec![],
                            genres: vec![],
                            cover_url: None,
                            canonical_url: None,
                        });
                        state.chapters.push(reader_domain::Chapter {
                            id: chapter.clone(),
                            title: "Chapter".into(),
                            label: "1".into(),
                            number: None,
                            volume: None,
                            source_order: 0,
                            language: None,
                            scanlator: None,
                            uploaded_at: None,
                        });
                        Ok(())
                    })
                    .unwrap();
                let writer = ProgressWriter::new(domain.clone()).unwrap();
                writer.enqueue(progress("a", 1)).unwrap();
                writer.flush(Some(progress("a", 17))).unwrap();
                drop(writer);
            }
            assert_eq!(domain.snapshot().unwrap().progress[0].anchor.page_index, 17);
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let test_name = concat!(
            module_path!(),
            "::final_checkpoint_survives_a_process_restart"
        )
        .split_once("::")
        .unwrap()
        .1;
        for mode in ["write", "read"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test_name, "--nocapture"])
                .env(MODE, mode)
                .env(DIRECTORY, directory.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{mode}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
    }
}
