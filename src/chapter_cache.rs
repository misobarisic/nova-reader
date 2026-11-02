//! Disposable viewed-page storage, backed by Nova Kit's real encoded-image
//! container, atomic publication, batched usage index, grouped GC and pin leases.
//! Explicit downloads/imports remain owned by reader-downloads in data storage.
use crate::{
    image_storage::{self, CacheKind, ImageCompression, ImageStorageFormat},
    services::Result,
};
use nova_cache::{blob, gc};
use reader_domain::ChapterId;
use reader_sources::{CancelToken, Page, Source};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

const ENTRY_BYTES: usize = 32 * 1024 * 1024;
const QUEUED_BYTES: usize = 32 * 1024 * 1024;
type Current = Arc<dyn Fn() -> bool + Send + Sync>;
type Encoder = Arc<dyn Fn(&[u8], ImageCompression) -> Result<Vec<u8>> + Send + Sync>;
type Policy = Arc<dyn Fn() -> gc::DiskPolicy + Send + Sync>;
type Notify = Arc<dyn Fn() + Send + Sync>;
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn chapter_group(chapter: &ChapterId) -> String {
    format!(
        "reader-page:v1:{}",
        digest(&serde_json::to_vec(chapter).expect("chapter identity serializes"))
    )
}
fn inspect(bytes: &[u8]) -> Result<(u32, u32)> {
    let dimensions = reader_engine::inspect_image(bytes, reader_engine::current_limits())
        .map_err(|e| e.to_string())?;
    Ok((dimensions.width, dimensions.height))
}
fn private_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::other(
            "chapter cache directory cannot be a symlink",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
struct Backend {
    policy: Policy,
    marker: String,
    notify: Arc<Mutex<Option<Notify>>>,
}
impl gc::Backend for Backend {
    fn policy(&self) -> gc::DiskPolicy {
        (self.policy)()
    }
    fn eviction_group(&self, key: &str) -> String {
        key.split(':').take(3).collect::<Vec<_>>().join(":")
    }
    fn last_cleanup(&self, _: &Path) -> io::Result<Option<u64>> {
        Ok(nova_storage::try_get_str(&self.marker)
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok()))
    }
    fn save_cleanup(&self, _: &Path, completed: u64) -> io::Result<()> {
        let _ = nova_storage::try_set_str(&self.marker, &completed.to_string());
        Ok(())
    }
    fn preferred_urls(&self) -> io::Result<HashSet<String>> {
        Ok(HashSet::new())
    }
    fn completed(&self, _: gc::Report) {
        let callback = self
            .notify
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(callback) = callback {
            callback();
        }
    }
    fn failed(&self, error: &io::Error) {
        eprintln!("Nova Reader chapter cache: {error}");
    }
}
#[derive(Clone)]
pub struct Request {
    key: Option<String>,
    source: Source,
    page: Page,
    scope: Option<String>,
    cancel: CancelToken,
    current: Current,
    cache_epoch: u64,
    write_epoch: u64,
}
impl Request {
    pub fn memory_key(&self) -> Option<String> {
        self.key
            .as_ref()
            .map(|key| format!("{}:{key}", self.cache_epoch))
    }
    pub fn validate_active(&self) -> Result<()> {
        self.cancel.check().map_err(|e| e.to_string())?;
        if !(self.current)() {
            return Err("operation cancelled".into());
        }
        self.source
            .validate_page_request(&self.page.url, &self.page.headers)
            .map_err(|e| e.to_string())
    }
    pub fn validate_scope(&self) -> Result<()> {
        self.validate_active()?;
        if self
            .source
            .page_cache_scope(&self.page.url, &self.page.headers)
            .map_err(|e| e.to_string())?
            != self.scope
        {
            return Err("chapter cache authorization context changed".into());
        }
        Ok(())
    }
    pub fn validate_for_publication(&self, reused: bool) -> Result<()> {
        if reused {
            self.validate_scope()
        } else {
            self.validate_active()
        }
    }
}
pub struct Read {
    pub bytes: Vec<u8>,
    pub pending: Option<PendingWrite>,
    pub from_cache: bool,
}
pub struct PendingWrite {
    request: Request,
    compression: ImageCompression,
    _reservation: Reservation,
}
#[derive(Default)]
struct Queue {
    bytes: Mutex<usize>,
    idle: Condvar,
}
struct Reservation {
    queue: Arc<Queue>,
    bytes: usize,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut used = self.queue.bytes.lock().unwrap_or_else(|e| e.into_inner());
        *used -= self.bytes;
        self.queue.idle.notify_all();
    }
}
struct Job {
    pending: PendingWrite,
    bytes: Arc<Vec<u8>>,
}
struct Active {
    generation: u64,
    chapter: ChapterId,
    _lease: blob::Lease,
}
pub struct ChapterCache {
    cache: blob::Cache,
    policy: Policy,
    usable: bool,
    active: Mutex<Option<Active>>,
    write_epoch: Arc<AtomicU64>,
    queue: Arc<Queue>,
    sender: Mutex<Option<mpsc::SyncSender<Job>>>,
    writer: Mutex<Option<thread::JoinHandle<()>>>,
    _gc: Option<gc::Worker>,
    pub maintenance_busy: AtomicBool,
    notify: Arc<Mutex<Option<Notify>>>,
}
impl ChapterCache {
    pub fn new(directory: PathBuf) -> Self {
        Self::with_hooks(
            directory,
            Arc::new(|| image_storage::cache_policy(CacheKind::Chapters)),
            Arc::new(compress),
        )
    }
    fn with_hooks(directory: PathBuf, policy: Policy, encoder: Encoder) -> Self {
        let usable = private_directory(&directory).is_ok();
        let cache = blob::Cache::new(directory.clone(), ENTRY_BYTES as u64);
        let notify = Arc::new(Mutex::new(None));
        let backend = Arc::new(Backend {
            policy: policy.clone(),
            marker: format!(
                "reader:chapter-cache:cleanup:{}",
                digest(directory.as_os_str().as_encoded_bytes())
            ),
            notify: notify.clone(),
        });
        let gc = usable.then(|| gc::start(directory, backend).ok()).flatten();
        let queue = Arc::new(Queue::default());
        let epoch = Arc::new(AtomicU64::new(0));
        let (sender, receiver) = mpsc::sync_channel::<Job>(1);
        let worker_cache = cache.clone();
        let worker_epoch = epoch.clone();
        let worker_policy = policy.clone();
        let writer = thread::Builder::new()
            .name("reader-chapter-cache-writer".into())
            .spawn(move || {
                for job in receiver {
                    let request = &job.pending.request;
                    let current = || {
                        request.write_epoch == worker_epoch.load(Ordering::Acquire)
                            && (worker_policy)().enabled
                            && request.validate_scope().is_ok()
                    };
                    if !current() {
                        continue;
                    }
                    let Some(key) = request.key.as_deref() else {
                        continue;
                    };
                    if worker_cache.get(key, inspect).ok().flatten().is_some() {
                        continue;
                    }
                    let transformed = job
                        .pending
                        .compression
                        .enabled
                        .then(|| encoder(&job.bytes, job.pending.compression));
                    let stored = match transformed {
                        Some(Ok(ref bytes)) if bytes.len() <= ENTRY_BYTES => bytes.as_slice(),
                        _ => job.bytes.as_slice(),
                    };
                    if !current() {
                        continue;
                    }
                    // Active chapter leases protect current reading, so enforce
                    // admission as well as GC: a pinned chapter cannot grow the
                    // disposable namespace without bound. This single writer
                    // owns admission; a ready tracker avoids filesystem work.
                    if (worker_policy)().limit_bytes.is_some_and(|limit| {
                        let used = worker_cache
                            .tracked_usage()
                            .unwrap_or_else(|| worker_cache.usage())
                            .0;
                        used.saturating_add(stored.len() as u64)
                            .saturating_add(64 * 1024 + 12)
                            > limit
                    }) {
                        gc::wake();
                        continue;
                    }
                    let _ = worker_cache.put_if_current(
                        key,
                        stored,
                        request.cache_epoch,
                        inspect,
                        current,
                    );
                }
            })
            .ok();
        Self {
            cache,
            policy,
            usable,
            active: Mutex::new(None),
            write_epoch: epoch,
            queue,
            sender: Mutex::new(writer.as_ref().map(|_| sender)),
            writer: Mutex::new(writer),
            _gc: gc,
            maintenance_busy: AtomicBool::new(false),
            notify,
        }
    }
    pub fn prepare(
        &self,
        chapter: &ChapterId,
        page: &Page,
        source: &Source,
        cancel: CancelToken,
        current: Current,
    ) -> Result<Request> {
        cancel.check().map_err(|e| e.to_string())?;
        let scope = source
            .page_cache_scope(&page.url, &page.headers)
            .map_err(|e| e.to_string())?;
        let key = scope.as_ref().map(|scope| {
            format!(
                "{}:{}",
                chapter_group(chapter),
                digest(&serde_json::to_vec(&(&page.id, scope)).expect("page scope serializes"))
            )
        });
        Ok(Request {
            key,
            scope,
            source: source.clone(),
            page: page.clone(),
            cancel,
            current,
            cache_epoch: self.cache.generation(),
            write_epoch: self.write_epoch.load(Ordering::Acquire),
        })
    }
    pub fn read(&self, request: &Request) -> Result<Read> {
        self.read_with_chunks(request, &mut |_| {})
    }
    /// Observe only newly fetched source bytes. Disk hits are already complete
    /// responses and deliberately produce no progressive notifications.
    pub fn read_with_chunks(
        &self,
        request: &Request,
        on_chunk: &mut dyn FnMut(&[u8]),
    ) -> Result<Read> {
        reader_engine::with_current_limits(|| {
            request.validate_active()?;
            let enabled = self.usable && (self.policy)().enabled;
            if enabled
                && request.validate_scope().is_ok()
                && let Some(key) = request.key.as_deref()
                && let Some(bytes) = self.cache.get(key, inspect).ok().flatten()
                && request.validate_scope().is_ok()
            {
                return Ok(Read {
                    bytes,
                    pending: None,
                    from_cache: true,
                });
            }
            let bytes = request
                .source
                .fetch_page_with_chunks_limited(
                    &request.page.url,
                    &request.page.headers,
                    &request.cancel,
                    reader_engine::current_limits().max_encoded_bytes,
                    on_chunk,
                )
                .map_err(|e| e.to_string())?;
            request.validate_active()?;
            inspect(&bytes)?;
            let pending = if enabled && request.key.is_some() && request.validate_scope().is_ok() {
                self.reserve(bytes.len()).map(|reservation| PendingWrite {
                    request: request.clone(),
                    compression: image_storage::effective_for_source(
                        &request.source.descriptor().id,
                    )
                    .cache,
                    _reservation: reservation,
                })
            } else {
                None
            };
            // A response may legitimately update its cookie jar. The original
            // fetched bytes may display, but never publish under the old scope.
            request.validate_active()?;
            Ok(Read {
                bytes,
                pending,
                from_cache: false,
            })
        })
    }
    pub fn touch(&self, request: &Request) {
        if let Some(key) = request.key.as_deref() {
            self.cache.touch(key);
        }
    }
    /// Called only after foreground decode and successful UI model publication.
    /// Active+pending writes together admit at most32MiB of encoded source data.
    pub fn enqueue(&self, pending: PendingWrite, bytes: Arc<Vec<u8>>) {
        if !self.usable
            || bytes.is_empty()
            || bytes.len() > ENTRY_BYTES
            || pending.request.validate_scope().is_err()
        {
            return;
        }
        let job = Job { pending, bytes };
        if let Some(sender) = self
            .sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = sender.try_send(job);
        }
    }
    fn reserve(&self, bytes: usize) -> Option<Reservation> {
        if bytes == 0 || bytes > ENTRY_BYTES {
            return None;
        }
        let mut used = self.queue.bytes.lock().unwrap_or_else(|e| e.into_inner());
        if used.saturating_add(bytes) > QUEUED_BYTES {
            return None;
        }
        *used += bytes;
        Some(Reservation {
            queue: self.queue.clone(),
            bytes,
        })
    }
    pub fn activate(&self, generation: u64, chapter: &ChapterId) {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if active
            .as_ref()
            .is_some_and(|old| old.generation == generation && old.chapter == *chapter)
        {
            return;
        }
        let group = chapter_group(chapter);
        *active = Some(Active {
            generation,
            chapter: chapter.clone(),
            _lease: self.cache.pin([format!("{group}:active")]),
        });
    }
    pub fn deactivate(&self) {
        self.active.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
    pub fn clear(&self) -> (u64, usize) {
        self.write_epoch.fetch_add(1, Ordering::AcqRel);
        if self.usable {
            self.cache.clear()
        } else {
            (0, 0)
        }
    }
    pub fn invalidate_writes(&self) {
        self.write_epoch.fetch_add(1, Ordering::AcqRel);
    }
    pub fn usage(&self) -> (u64, usize) {
        if self.usable {
            self.cache.usage()
        } else {
            (0, 0)
        }
    }
    pub fn wake(&self) {
        gc::wake();
    }
    pub fn on_gc(&self, callback: Notify) {
        *self.notify.lock().unwrap_or_else(|e| e.into_inner()) = Some(callback);
    }
    /// Orderly shutdown/tests can wait for the bounded asynchronous write stage.
    pub fn flush_writes(&self) -> bool {
        let used = self.queue.bytes.lock().unwrap_or_else(|e| e.into_inner());
        self.queue
            .idle
            .wait_timeout_while(used, Duration::from_secs(30), |bytes| *bytes > 0)
            .map(|(bytes, _)| *bytes == 0)
            .unwrap_or(false)
    }
}
impl Drop for ChapterCache {
    fn drop(&mut self) {
        self.sender.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(writer) = self.writer.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = writer.join();
        }
    }
}
fn compress(bytes: &[u8], compression: ImageCompression) -> Result<Vec<u8>> {
    let dimensions = reader_engine::inspect_image(bytes, reader_engine::current_limits())
        .map_err(|e| e.to_string())?;
    if u64::from(dimensions.width) * u64::from(dimensions.height) > 8_000_000
        || (compression.format == ImageStorageFormat::Webp
            && (dimensions.width > 16383 || dimensions.height > 16383))
    {
        return Err("cache codec admission retains original image".into());
    }
    crate::reader_export::reencode_storage_image(
        bytes,
        compression,
        reader_engine::DecodeLimits {
            max_pixels: 8_000_000u64.min(reader_engine::current_limits().max_pixels),
            max_decoded_bytes: (64u64 * 1024 * 1024)
                .min(reader_engine::current_limits().max_decoded_bytes),
            ..reader_engine::current_limits()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use reader_sources::{HttpRequest, HttpResponse, SourceRegistry, SourceTransport};
    use std::{io::Write, sync::atomic::AtomicUsize, time::Instant};
    struct Transport {
        body: Vec<u8>,
        reads: AtomicUsize,
        scope: AtomicU64,
        rotate: AtomicBool,
    }
    impl SourceTransport for Transport {
        fn page_cache_partition(&self, _: &str) -> Option<String> {
            Some(format!(
                "test-profile-{}",
                self.scope.load(Ordering::Acquire)
            ))
        }
        fn request(
            &self,
            request: &HttpRequest,
            _: usize,
            cancel: &CancelToken,
        ) -> reader_sources::Result<HttpResponse> {
            cancel.check()?;
            self.reads.fetch_add(1, Ordering::AcqRel);
            if self.rotate.swap(false, Ordering::AcqRel) {
                self.scope.fetch_add(1, Ordering::AcqRel);
            }
            Ok(HttpResponse {
                status: 200,
                url: request.url.clone(),
                body: self.body.clone(),
                ..Default::default()
            })
        }
    }
    fn png(color: u8) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(4, 4, image::Rgba([color, 30, 90, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }
    fn source(path: &Path, body: Vec<u8>) -> (SourceRegistry, Source, Arc<Transport>) {
        let registry = SourceRegistry::open(path).unwrap();
        if registry.source("cache-fixture").is_err() {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            let manifest = serde_json::json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"cache-fixture","name":"Cache fixture","version":"1.0.0","language":"en","nsfw":false,"entry":"main.js","permissions":{"domains":["example.com"],"httpPost":false},"capabilities":["pages"]});
            zip.start_file("manifest.json", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(manifest.to_string().as_bytes()).unwrap();
            zip.start_file("main.js", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"globalThis.nova={request:()=>({pages:[]})};")
                .unwrap();
            registry
                .install_zip(&zip.finish().unwrap().into_inner())
                .unwrap();
        }
        registry.set_enabled("cache-fixture", true).unwrap();
        let transport = Arc::new(Transport {
            body,
            reads: AtomicUsize::new(0),
            scope: AtomicU64::new(0),
            rotate: AtomicBool::new(false),
        });
        let source = registry
            .source("cache-fixture")
            .unwrap()
            .with_transport(transport.clone());
        (registry, source, transport)
    }
    fn chapter(id: &str) -> ChapterId {
        ChapterId::new(reader_domain::SeriesId::new("cache-fixture", "series"), id)
    }
    fn page(id: &str) -> Page {
        Page {
            id: id.into(),
            url: format!("https://example.com/{id}.png?private=secret-fixture-query"),
            headers: std::collections::BTreeMap::from([(
                "Authorization".into(),
                "Bearer super-secret-fixture-auth".into(),
            )]),
            width: Some(4),
            height: Some(4),
        }
    }
    fn cache(path: PathBuf) -> ChapterCache {
        ChapterCache::with_hooks(
            path,
            Arc::new(|| gc::DiskPolicy {
                enabled: true,
                limit_bytes: None,
            }),
            Arc::new(|_, _| Err("retain original".into())),
        )
    }
    fn prepare(cache: &ChapterCache, source: &Source, chapter_id: &str, page_id: &str) -> Request {
        cache
            .prepare(
                &chapter(chapter_id),
                &page(page_id),
                source,
                CancelToken::default(),
                Arc::new(|| true),
            )
            .unwrap()
    }
    fn persist(cache: &ChapterCache, request: &Request) -> Vec<u8> {
        let read = cache.read(request).unwrap();
        let bytes = read.bytes.clone();
        if let Some(pending) = read.pending {
            cache.enqueue(pending, Arc::new(read.bytes));
            assert!(cache.flush_writes());
        }
        bytes
    }
    fn entries(cache: &ChapterCache) -> usize {
        fs::read_dir(cache.cache.directory())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "variant"))
            .count()
    }
    fn until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !predicate() {
            assert!(Instant::now() < deadline, "cache worker did not settle");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn viewed_pages_survive_cold_process_without_download_jobs_or_secret_keys() {
        const PHASE: &str = "READER_PAGE_CACHE_PHASE";
        const ROOT: &str = "READER_PAGE_CACHE_ROOT";
        if let Ok(phase) = std::env::var(PHASE) {
            let root = PathBuf::from(std::env::var(ROOT).unwrap());
            let original = png(200);
            let (_registry, source, transport) = source(&root.join("source"), original.clone());
            let cache = cache(root.join("cache/viewed-chapters"));
            let request = prepare(&cache, &source, "first", "page");
            assert_eq!(persist(&cache, &request), original);
            assert_eq!(
                transport.reads.load(Ordering::Acquire),
                usize::from(phase == "write")
            );
            let downloads =
                reader_downloads::DownloadStore::open(root.join("downloads"), 1024 * 1024).unwrap();
            assert!(downloads.jobs().unwrap().is_empty());
            assert_eq!(entries(&cache), 1);
            for file in fs::read_dir(cache.cache.directory()).unwrap() {
                let file = file.unwrap();
                if file.file_type().unwrap().is_file() {
                    let bytes = fs::read(file.path()).unwrap();
                    assert!(
                        !bytes
                            .windows(b"Bearer super-secret-fixture-auth".len())
                            .any(|w| w == b"Bearer super-secret-fixture-auth")
                    );
                }
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(cache.cache.directory())
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
            }
            return;
        }
        let root = tempfile::tempdir().unwrap();
        for phase in ["write", "reload"] {
            let output=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","chapter_cache::tests::viewed_pages_survive_cold_process_without_download_jobs_or_secret_keys","--nocapture"]).env(PHASE,phase).env(ROOT,root.path()).output().unwrap();
            assert!(
                output.status.success(),
                "{phase}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    #[test]
    fn cookie_rotation_displays_original_and_never_cancels_or_publishes_old_scope() {
        let root = tempfile::tempdir().unwrap();
        let original = png(120);
        let (_registry, source, transport) = source(&root.path().join("source"), original.clone());
        let cache = cache(root.path().join("cache"));
        let request = prepare(&cache, &source, "c", "p");
        transport.rotate.store(true, Ordering::Release);
        let read = cache.read(&request).unwrap();
        assert_eq!(read.bytes, original);
        assert!(read.pending.is_none());
        assert!(!read.from_cache);
        assert!(request.validate_for_publication(false).is_ok());
        assert!(!request.cancel.is_cancelled());
        assert_eq!(entries(&cache), 0);
        let next = prepare(&cache, &source, "c", "p");
        assert_ne!(request.memory_key(), next.memory_key());
        assert_eq!(persist(&cache, &next), original);
        assert_eq!(entries(&cache), 1);
        let cached = cache.read(&next).unwrap();
        assert!(cached.from_cache);
        transport.scope.fetch_add(1, Ordering::AcqRel);
        assert!(next.validate_scope().is_err());
        assert!(
            next.validate_for_publication(cached.from_cache).is_err(),
            "a hit held during decode cannot publish after auth context changes"
        );
        assert!(!next.cancel.is_cancelled());
        let fresh = cache.read(&next).unwrap();
        assert_eq!(fresh.bytes, original);
        assert!(fresh.pending.is_none());
        assert!(!next.cancel.is_cancelled());
    }
    #[test]
    fn current_permissions_and_profiles_gate_all_persisted_hits() {
        let root = tempfile::tempdir().unwrap();
        let (registry, source, transport) = source(&root.path().join("source"), png(80));
        let cache = cache(root.path().join("cache"));
        let request = prepare(&cache, &source, "c", "p");
        persist(&cache, &request);
        let reads = transport.reads.load(Ordering::Acquire);
        assert_eq!(cache.read(&request).unwrap().bytes, png(80));
        assert_eq!(transport.reads.load(Ordering::Acquire), reads);
        let first = registry
            .source("cache-fixture")
            .unwrap()
            .page_cache_scope(&page("p").url, &page("p").headers)
            .unwrap();
        let other = registry
            .with_profile("other")
            .source("cache-fixture")
            .unwrap()
            .page_cache_scope(&page("p").url, &page("p").headers)
            .unwrap();
        assert_ne!(first, other);
        registry.set_enabled("cache-fixture", false).unwrap();
        assert!(cache.read(&request).is_err());
        assert_eq!(transport.reads.load(Ordering::Acquire), reads);
        let mut denied = page("p");
        denied.url = "https://denied.example/private".into();
        assert!(
            cache
                .prepare(
                    &chapter("c"),
                    &denied,
                    &source,
                    CancelToken::default(),
                    Arc::new(|| true)
                )
                .is_err()
        );
    }
    #[test]
    fn transformed_new_writes_never_delay_or_replace_original_first_display() {
        let root = tempfile::tempdir().unwrap();
        let original = png(210);
        let (_registry, source, transport) = source(&root.path().join("source"), original.clone());
        let transformed = png(100);
        let output = transformed.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let cache = ChapterCache::with_hooks(
            root.path().join("cache"),
            Arc::new(|| gc::DiskPolicy {
                enabled: true,
                limit_bytes: None,
            }),
            Arc::new(move |_, _| {
                count.fetch_add(1, Ordering::AcqRel);
                Ok(output.clone())
            }),
        );
        let request = prepare(&cache, &source, "c", "p");
        let mut read = cache.read(&request).unwrap();
        assert_eq!(read.bytes, original);
        assert_eq!(calls.load(Ordering::Acquire), 0);
        let mut pending = read.pending.take().unwrap();
        pending.compression.enabled = true;
        cache.enqueue(pending, Arc::new(read.bytes));
        assert!(cache.flush_writes());
        assert_eq!(cache.read(&request).unwrap().bytes, transformed);
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(transport.reads.load(Ordering::Acquire), 1);
    }
    #[test]
    fn clear_or_changed_credentials_during_blocked_encode_cannot_resurrect_cache() {
        for clear in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let (_registry, source, transport) = source(&root.path().join("source"), png(90));
            let (entered, start) = mpsc::channel();
            let (release, gate) = mpsc::channel();
            let gate = Mutex::new(gate);
            let cache = ChapterCache::with_hooks(
                root.path().join("cache"),
                Arc::new(|| gc::DiskPolicy {
                    enabled: true,
                    limit_bytes: None,
                }),
                Arc::new(move |bytes, _| {
                    entered.send(()).unwrap();
                    gate.lock().unwrap().recv().unwrap();
                    Ok(bytes.to_vec())
                }),
            );
            let request = prepare(&cache, &source, "c", "p");
            let mut read = cache.read(&request).unwrap();
            let mut pending = read.pending.take().unwrap();
            pending.compression.enabled = true;
            cache.enqueue(pending, Arc::new(read.bytes));
            start.recv_timeout(Duration::from_secs(2)).unwrap();
            if clear {
                cache.clear();
            } else {
                transport.scope.fetch_add(1, Ordering::AcqRel);
            }
            release.send(()).unwrap();
            assert!(cache.flush_writes());
            assert_eq!(entries(&cache), 0);
            assert!(!request.cancel.is_cancelled());
            let fresh = prepare(&cache, &source, "c", "p");
            assert!(cache.read(&fresh).is_ok());
            assert!(!fresh.cancel.is_cancelled());
        }
    }
    #[test]
    fn pending_and_active_write_bytes_are_bounded_before_ui_handoff() {
        let root = tempfile::tempdir().unwrap();
        let mut body = png(90);
        body.resize(20 * 1024 * 1024, 0);
        let (_registry, source, _) = source(&root.path().join("source"), body);
        let (entered, start) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let gate = Mutex::new(gate);
        let cache = ChapterCache::with_hooks(
            root.path().join("cache"),
            Arc::new(|| gc::DiskPolicy {
                enabled: true,
                limit_bytes: None,
            }),
            Arc::new(move |_, _| {
                entered.send(()).unwrap();
                gate.lock().unwrap().recv().unwrap();
                Err("original".into())
            }),
        );
        let first = prepare(&cache, &source, "c", "first");
        let mut read = cache.read(&first).unwrap();
        assert_eq!(
            *cache.queue.bytes.lock().unwrap(),
            20 * 1024 * 1024,
            "reservation exists BEFORE UI queues publication"
        );
        let mut pending = read.pending.take().unwrap();
        pending.compression.enabled = true;
        cache.enqueue(pending, Arc::new(read.bytes));
        start.recv_timeout(Duration::from_secs(2)).unwrap();
        let second = prepare(&cache, &source, "c", "second");
        let other = cache.read(&second).unwrap();
        assert_eq!(other.bytes.len(), 20 * 1024 * 1024);
        assert!(
            other.pending.is_none(),
            "cache pressure must skip write, never block foreground bytes"
        );
        assert!(*cache.queue.bytes.lock().unwrap() <= QUEUED_BYTES);
        release.send(()).unwrap();
        assert!(cache.flush_writes());
        assert_eq!(*cache.queue.bytes.lock().unwrap(), 0);
    }
    #[test]
    fn pinned_chapter_stops_new_writes_at_quota_without_blocking_display() {
        let root = tempfile::tempdir().unwrap();
        let original = png(75);
        let (_registry, source, transport) = source(&root.path().join("source"), original.clone());
        let limit = Arc::new(AtomicU64::new(u64::MAX));
        let policy_limit = limit.clone();
        let cache = ChapterCache::with_hooks(
            root.path().join("chapters"),
            Arc::new(move || gc::DiskPolicy {
                enabled: true,
                limit_bytes: Some(policy_limit.load(Ordering::Acquire)),
            }),
            Arc::new(|_, _| Err("original".into())),
        );
        cache.activate(1, &chapter("active"));
        assert_eq!(
            persist(&cache, &prepare(&cache, &source, "active", "first")),
            original
        );
        assert_eq!(entries(&cache), 1);
        limit.store(cache.usage().0 + original.len() as u64, Ordering::Release);
        assert_eq!(
            persist(&cache, &prepare(&cache, &source, "active", "next")),
            original
        );
        assert_eq!(
            entries(&cache),
            1,
            "active leases must not bypass disk admission"
        );
        assert_eq!(transport.reads.load(Ordering::Acquire), 2);
    }
    #[test]
    fn grouped_chapter_eviction_preserves_active_chapter_posters_and_downloads() {
        let root = tempfile::tempdir().unwrap();
        let (_registry, source, _) = source(&root.path().join("source"), png(75));
        let limit = Arc::new(AtomicU64::new(u64::MAX));
        let policy_limit = limit.clone();
        let directory = root.path().join("chapters");
        {
            let seeded = cache(directory.clone());
            persist(&seeded, &prepare(&seeded, &source, "old", "p0"));
            persist(&seeded, &prepare(&seeded, &source, "old", "p1"));
            persist(&seeded, &prepare(&seeded, &source, "active", "p"));
            until(|| seeded.cache.tracked_usage().is_some());
        }
        let index = directory.join(".image-usage-v1.json");
        let mut records: serde_json::Value =
            serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
        for record in records["records"].as_object_mut().unwrap().values_mut() {
            record["last_used"] = 1.into();
        }
        fs::write(index, serde_json::to_vec(&records).unwrap()).unwrap();
        let cache = ChapterCache::with_hooks(
            directory,
            Arc::new(move || gc::DiskPolicy {
                enabled: true,
                limit_bytes: Some(policy_limit.load(Ordering::Acquire)),
            }),
            Arc::new(|_, _| Err("original".into())),
        );
        let old0 = prepare(&cache, &source, "old", "p0");
        let old1 = prepare(&cache, &source, "old", "p1");
        cache.activate(1, &chapter("active"));
        let poster = blob::Cache::new(root.path().join("posters"), ENTRY_BYTES as u64);
        poster
            .put_if_current("poster", &png(75), poster.generation(), inspect, || true)
            .unwrap();
        let downloads = root.path().join("downloads");
        fs::create_dir(&downloads).unwrap();
        fs::write(downloads.join("keep.page"), b"explicit download").unwrap();
        limit.store(1, Ordering::Release);
        cache.wake();
        until(|| entries(&cache) == 1);
        assert!(
            cache
                .cache
                .get(old0.key.as_deref().unwrap(), inspect)
                .unwrap()
                .is_none()
        );
        assert!(
            cache
                .cache
                .get(old1.key.as_deref().unwrap(), inspect)
                .unwrap()
                .is_none()
        );
        assert!(poster.get("poster", inspect).unwrap().is_some());
        cache.deactivate();
        until(|| entries(&cache) == 0);
        assert_eq!(
            fs::read(downloads.join("keep.page")).unwrap(),
            b"explicit download"
        );
        assert!(poster.get("poster", inspect).unwrap().is_some());
    }
}
