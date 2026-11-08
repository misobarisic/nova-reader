//! Bounded cover loading. Only the event-loop publication creates Slint images.
use crate::{AppWindow, services::Services};
use nova_reader_ui::backend_text as text;
use reader_domain::Series;
use reader_engine::{DecodeLimits, decode_display_image};
use reader_sources::{CancelToken, PageAccess, SourceAvailability};
use sha2::{Digest, Sha256};
use slint::{Image, Model, Rgba8Pixel, SharedPixelBuffer};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashSet, VecDeque},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    time::Duration,
};

const MAX_REQUESTS: usize = 32;
const FETCH_WORKERS: usize = 4;
const CACHE_QUEUE_BYTES: usize = 16 * 1024 * 1024;
const ENCODED_LIMIT: usize = 8 * 1024 * 1024;
// Nova's decoded LRU default and retained-card allowance (media/src/cache.rs).
const DECODED_CACHE_LIMIT: usize = 128 * 1024 * 1024;
const UNLOAD_EXTRA_CARDS: usize = nova_cache::UNLOAD_EXTRA_CARDS;
const COVER_LIMITS: DecodeLimits = DecodeLimits {
    max_encoded_bytes: ENCODED_LIMIT,
    max_width: 8192,
    max_height: 8192,
    max_pixels: 8_000_000,
    max_decoded_bytes: 72 * 1024 * 1024,
};

// Posters retain their independent, tighter admission while respecting a lower
// device-wide image policy.
fn cover_limits() -> DecodeLimits {
    let current = reader_engine::current_limits();
    DecodeLimits {
        max_encoded_bytes: current
            .max_encoded_bytes
            .min(COVER_LIMITS.max_encoded_bytes),
        max_width: current.max_width.min(COVER_LIMITS.max_width),
        max_height: current.max_height.min(COVER_LIMITS.max_height),
        max_pixels: current.max_pixels.min(COVER_LIMITS.max_pixels),
        max_decoded_bytes: current
            .max_decoded_bytes
            .min(COVER_LIMITS.max_decoded_bytes),
    }
}

type Pixels = SharedPixelBuffer<Rgba8Pixel>;
static NEXT_LOADER: AtomicU64 = AtomicU64::new(1);
thread_local! {
    // Image handles remain on the event loop. Their pixels are shared with the
    // single decoded LRU, never copied into a second full-size image cache.
    static UI_IMAGES: RefCell<BTreeMap<u64, BTreeMap<String, Image>>> = RefCell::default();
}

/// Source identity, local identity and the complete URL all isolate cache entries.
/// Length prefixes avoid ambiguity even when source-local IDs contain separators.
pub fn key(series: &Series) -> String {
    let Some(url) = series.cover_url.as_deref().filter(|url| !url.is_empty()) else {
        return String::new();
    };
    let mut digest = Sha256::new();
    digest.update(b"nova-reader-cover-v1");
    for field in [
        series.id.source_id.as_str(),
        series.id.local_id.as_str(),
        url,
    ] {
        digest.update((field.len() as u64).to_le_bytes());
        digest.update(field.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

#[derive(Default)]
struct Requests {
    pending: BTreeMap<String, CancelToken>,
    failed: BTreeSet<String>,
}

#[derive(Default)]
struct Shared {
    generation: AtomicU64,
    requests: Mutex<Requests>,
    maintenance: Mutex<Maintenance>,
    clearing: AtomicBool,
    worker_available: AtomicBool,
    shutdown: AtomicBool,
    decoded: Mutex<DecodedCache>,
    decode: Mutex<()>,
    cache_writes: Mutex<CacheWrites>,
}

#[derive(Clone, Copy, Default)]
struct Maintenance {
    revision: u64,
    pending: bool,
    clear: bool,
}

impl Shared {
    fn retain(&self, desired: &BTreeSet<String>) {
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        requests.pending.retain(|key, cancel| {
            if desired.contains(key) {
                true
            } else {
                cancel.cancel();
                false
            }
        });
        requests.failed.retain(|key| desired.contains(key));
    }

    fn queue_maintenance(&self, clear: bool) {
        let mut maintenance = self.maintenance.lock().unwrap_or_else(|e| e.into_inner());
        maintenance.revision = maintenance.revision.wrapping_add(1);
        maintenance.pending = true;
        maintenance.clear |= clear;
        if clear {
            self.clearing.store(true, Ordering::Release);
        }
    }

    fn take_maintenance(&self) -> Option<Maintenance> {
        let mut maintenance = self.maintenance.lock().unwrap_or_else(|e| e.into_inner());
        if !maintenance.pending {
            return None;
        }
        let command = *maintenance;
        maintenance.pending = false;
        maintenance.clear = false;
        Some(command)
    }

    fn invalidate(&self) {
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        self.generation.fetch_add(1, Ordering::AcqRel);
        for cancel in requests.pending.values() {
            cancel.cancel();
        }
        requests.pending.clear();
        requests.failed.clear();
    }

    fn current(&self, task: &Task) -> bool {
        !task.cancel.is_cancelled() && self.generation.load(Ordering::Acquire) == task.generation
    }

    fn finish(&self, task: &Task, failed: bool) {
        let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        if self.current(task) {
            requests.pending.remove(&task.key);
            if failed {
                requests.failed.insert(task.key.clone());
            }
        }
    }
}

#[derive(Clone)]
struct Task {
    key: String,
    source_id: String,
    url: String,
    generation: u64,
    cancel: CancelToken,
}

/// Bounded concurrent fetches, one display decoder and one optional cache worker.
pub struct ArtworkLoader {
    sender: SyncSender<Task>,
    shared: Arc<Shared>,
    ui: slint::Weak<AppWindow>,
    id: u64,
    encoded: nova_cache::blob::Cache,
}

impl ArtworkLoader {
    pub fn new(services: Services, ui: slint::Weak<AppWindow>, cache_dir: PathBuf) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Task>(MAX_REQUESTS);
        let shared = Arc::new(Shared::default());
        shared
            .decoded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_budget(crate::image_storage::memory_limit_bytes());
        let encoded = nova_cache::blob::Cache::new(cache_dir, ENCODED_LIMIT as u64);
        let id = NEXT_LOADER.fetch_add(1, Ordering::Relaxed);
        start_cache_worker(
            services.clone(),
            encoded.clone(),
            shared.clone(),
            ui.clone(),
        );
        let load_shared = shared.clone();
        let load_encoded = encoded.clone();
        start_fetch_workers(receiver, ui.clone(), id, shared.clone(), move |task| {
            reader_engine::with_current_limits(|| {
                load(&services, &load_encoded, &load_shared, task)
            })
        });
        Self {
            sender,
            shared,
            ui,
            id,
            encoded,
        }
    }

    /// Cancel before replacing rows, changing profiles, or changing source access.
    pub fn invalidate(&self) {
        self.shared.invalidate();
    }

    /// Drop transient poster admissions and retry failures after a device policy edit.
    /// Encoded disk files remain disposable and are validated on their next read.
    pub fn apply_image_limits(&self) {
        self.invalidate();
        let mut cache = self
            .shared
            .decoded
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let keys = cache.keys();
        cache.clear();
        drop(cache);
        if let Some(ui) = self.ui.upgrade() {
            forget_images(&ui, self.id, &keys);
        }
    }

    /// Cancel obsolete queued, active, and not-yet-published requests while
    /// keeping the generation of still-visible artwork intact.
    pub fn retain(&self, desired: &BTreeSet<String>) {
        self.shared.retain(desired);
        self.shared
            .decoded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain_visible(desired);
        if let Some(ui) = self.ui.upgrade() {
            trim_ui(&ui, self.id, &self.shared);
        }
    }

    /// Restore an authorized decoded poster synchronously on the event loop.
    /// This performs no filesystem, network or decoding work.
    pub fn cached_image(&self, series: &Series) -> Option<Image> {
        // Weak upgrades also enforce the component's event-loop thread.
        self.ui.upgrade()?;
        let key = key(series);
        let image = cached_image(&self.shared, self.id, &key, &series.id.source_id)?;
        if let Some(permit) = self
            .shared
            .decoded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .permits
            .get(&key)
            && let Some(encoded_key) = permit.encoded_key(&key)
        {
            self.encoded.touch(&encoded_key);
        }
        Some(image)
    }

    /// Apply a committed policy without filesystem access on the event loop.
    pub fn apply_policy(&self) {
        let evicted = self
            .shared
            .decoded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set_budget(crate::image_storage::memory_limit_bytes());
        if let Some(ui) = self.ui.upgrade() {
            forget_images(&ui, self.id, &evicted);
            trim_ui(&ui, self.id, &self.shared);
        }
        nova_cache::gc::wake();
    }

    /// Read actual poster cache usage on the worker. Repeated requests coalesce.
    pub fn refresh_usage(&self) {
        self.maintenance(false);
    }

    /// Cancel all older artwork work before clearing only owned poster files.
    pub fn clear(&self) {
        self.invalidate();
        if let Some(ui) = self.ui.upgrade() {
            let keys = self
                .shared
                .decoded
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .keys();
            forget_images(&ui, self.id, &keys);
        }
        self.maintenance(true);
    }

    fn maintenance(&self, clear: bool) {
        if !self.shared.worker_available.load(Ordering::Acquire) {
            if let Some(ui) = self.ui.upgrade() {
                ui.set_image_cache_busy(false);
                ui.set_image_cache_status(text::tr("Image cache worker unavailable.").into());
            }
            return;
        }
        self.shared.queue_maintenance(clear);
        if let Some(ui) = self.ui.upgrade() {
            ui.set_image_cache_busy(true);
        }
    }

    /// Enqueue a visible missing poster without blocking the event loop.
    /// A full queue is retried by the caller's next viewport update.
    pub fn request(&self, series: &Series) {
        if self.shared.clearing.load(Ordering::Acquire) {
            return;
        }
        let key = key(series);
        if key.is_empty() {
            return;
        }
        self.shared
            .decoded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .protect(key.clone());
        if let Some(image) = self.cached_image(series) {
            if let Some(ui) = self.ui.upgrade() {
                publish(&ui, &key, image);
            }
            return;
        }
        let mut requests = self
            .shared
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if requests.pending.len() >= MAX_REQUESTS
            || requests.pending.contains_key(&key)
            || requests.failed.contains(&key)
        {
            return;
        }
        let cancel = CancelToken::default();
        requests.pending.insert(key.clone(), cancel.clone());
        let task = Task {
            key: key.clone(),
            source_id: series.id.source_id.clone(),
            url: series.cover_url.clone().unwrap_or_default(),
            generation: self.shared.generation.load(Ordering::Acquire),
            cancel,
        };
        if self.sender.try_send(task).is_err() {
            requests.pending.remove(&key);
        }
    }
}

impl Drop for ArtworkLoader {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.invalidate();
        let id = self.id;
        if self.ui.upgrade().is_some() {
            UI_IMAGES.with(|images| {
                images.borrow_mut().remove(&id);
            });
        } else {
            let _ = self.ui.upgrade_in_event_loop(move |_| {
                UI_IMAGES.with(|images| {
                    images.borrow_mut().remove(&id);
                });
            });
        }
    }
}

// Encoded originals never enter UI closures. Bound queued cache data separately
// from four in-flight (8 MiB each) responses and the two bounded codec stages.
#[derive(Default)]
struct CacheWrites {
    pending: VecDeque<(Task, DeferredCacheWrite)>,
    bytes: usize,
}
impl CacheWrites {
    fn push(&mut self, task: Task, write: DeferredCacheWrite) {
        if self.pending.len() >= MAX_REQUESTS
            || write.bytes.len() > CACHE_QUEUE_BYTES.saturating_sub(self.bytes)
        {
            return; // Disposable cache work must never delay a visible poster.
        }
        self.bytes += write.bytes.len();
        self.pending.push_back((task, write));
    }
    fn pop(&mut self) -> Option<(Task, DeferredCacheWrite)> {
        let job = self.pending.pop_front()?;
        self.bytes -= job.1.bytes.len();
        Some(job)
    }
}

fn start_fetch_workers(
    receiver: mpsc::Receiver<Task>,
    ui: slint::Weak<AppWindow>,
    id: u64,
    shared: Arc<Shared>,
    load: impl Fn(&Task) -> LoadResult + Send + Sync + 'static,
) -> Vec<std::thread::JoinHandle<()>> {
    let mut workers = Vec::new();
    let receiver = Arc::new(Mutex::new(receiver));
    let load = Arc::new(load);
    for index in 0..FETCH_WORKERS {
        let receiver = receiver.clone();
        let shared = shared.clone();
        let ui = ui.clone();
        let load = load.clone();
        let worker = std::thread::Builder::new()
            .name(format!("reader-artwork-fetch-{index}"))
            .spawn(move || {
                while !shared.shutdown.load(Ordering::Acquire) {
                    // Only dequeue under this lock; network and decode run outside it.
                    let task = receiver
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .recv_timeout(Duration::from_millis(100));
                    let task = match task {
                        Ok(task) => task,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    if !shared.current(&task) {
                        continue;
                    }
                    let (result, deferred) = match load(&task) {
                        Ok((loaded, deferred)) => (Ok(loaded), deferred),
                        Err(error) => (Err(error), None),
                    };
                    if !shared.current(&task) {
                        continue;
                    }
                    let write_task = task.clone();
                    let publish_shared = shared.clone();
                    let (acknowledge, receipt) = mpsc::sync_channel(1);
                    let _ = ui.upgrade_in_event_loop(move |ui| {
                        // At most one small decoded completion per fetch worker.
                        let _receipt = Completion(acknowledge);
                        if !publish_shared.current(&task) {
                            return;
                        }
                        publish_shared.finish(&task, result.is_err());
                        if let Ok(loaded) = result {
                            admit(&ui, id, &publish_shared, &task.key, loaded);
                        }
                    });
                    after_publication(receipt, &shared, &write_task, || {
                        if shared.worker_available.load(Ordering::Acquire)
                            && let Some(deferred) = deferred
                        {
                            shared
                                .cache_writes
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push(write_task.clone(), deferred);
                        }
                    });
                }
            });
        if let Ok(worker) = worker {
            workers.push(worker);
        }
    }
    workers
}

fn start_cache_worker(
    services: Services,
    encoded: nova_cache::blob::Cache,
    shared: Arc<Shared>,
    ui: slint::Weak<AppWindow>,
) {
    let worker_shared = shared.clone();
    let started = std::thread::Builder::new()
        .name("reader-artwork-cache".into())
        .spawn(move || {
            if crate::image_storage::policy_snapshot().disk_enabled {
                migrate_legacy(&encoded);
            }
            // Joining the GC stays on this worker, never the event loop.
            let _gc = nova_cache::gc::start(
                encoded.directory().to_owned(),
                Arc::new(ArtworkBackend {
                    services,
                    shared: worker_shared.clone(),
                }),
            )
            .ok();
            let mut maintenance_status = None::<String>;
            while !worker_shared.shutdown.load(Ordering::Acquire) {
                if let Some(command) = worker_shared.take_maintenance() {
                    if command.clear {
                        {
                            let mut writes = worker_shared
                                .cache_writes
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            writes.pending.clear();
                            writes.bytes = 0;
                        }
                        clear_cache(&encoded, &worker_shared.decoded);
                        maintenance_status = Some(text::tr("Image cache cleared."));
                        let pending = worker_shared
                            .maintenance
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        if !pending.clear {
                            worker_shared.clearing.store(false, Ordering::Release);
                        }
                    }
                    let usage = cache_usage(&encoded);
                    let shared = worker_shared.clone();
                    let status = maintenance_status.clone();
                    let _ = ui.upgrade_in_event_loop(move |ui| {
                        if shared
                            .maintenance
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .revision
                            != command.revision
                        {
                            return;
                        }
                        ui.set_image_cache_info(text::image_cache_usage(usage.0, usage.1).into());
                        if let Some(status) = status {
                            ui.set_image_cache_status(status.into());
                        }
                        ui.set_image_cache_busy(false);
                    });
                    continue;
                }
                // Start optional recompression after visible requests finish. A new
                // request can still fetch/decode while an older encoder is running.
                let waiting = !worker_shared
                    .requests
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pending
                    .is_empty();
                if !waiting {
                    let job = worker_shared
                        .cache_writes
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .pop();
                    if let Some((task, write)) = job {
                        write.write(&encoded, &worker_shared, &task);
                        continue;
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
        .is_ok();
    shared.worker_available.store(started, Ordering::Release);
}

struct Completion(SyncSender<()>);
impl Drop for Completion {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}

/// Keep only one decoded completion and one bounded original on the worker.
/// A disconnected UI is not an acknowledgment and cannot trigger cache work.
fn after_publication(
    receipt: mpsc::Receiver<()>,
    shared: &Shared,
    task: &Task,
    optional_work: impl FnOnce(),
) {
    let acknowledged = loop {
        match receipt.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => break true,
            Err(mpsc::RecvTimeoutError::Disconnected) => break false,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if shared.shutdown.load(Ordering::Acquire) {
            break false;
        }
    };
    if acknowledged && shared.current(task) && !shared.shutdown.load(Ordering::Acquire) {
        optional_work();
    }
}

fn cached_image(shared: &Shared, id: u64, key: &str, source_id: &str) -> Option<Image> {
    if shared.clearing.load(Ordering::Acquire) {
        return None;
    }
    let pixels = shared
        .decoded
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(key, source_id)?;
    Some(UI_IMAGES.with(|images| {
        images
            .borrow_mut()
            .entry(id)
            .or_default()
            .entry(key.into())
            .or_insert_with(|| Image::from_rgba8(pixels))
            .clone()
    }))
}

fn admit(ui: &AppWindow, id: u64, shared: &Shared, key: &str, loaded: Loaded) {
    if !loaded.is_current() {
        return;
    }
    let source_id = loaded.source_id.clone();
    let uncached_pixels = loaded
        .proof
        .as_ref()
        .is_some_and(|proof| proof.scope.is_none())
        .then(|| loaded.pixels.clone());
    let evicted = shared
        .decoded
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key.into(), loaded);
    // Release every UI reference to evicted pixels in this same event-loop
    // turn; the LRU budget cannot hide a second cache of older model images.
    forget_images(ui, id, &evicted);
    if let Some(image) = cached_image(shared, id, key, &source_id) {
        publish(ui, key, image);
    } else if let Some(pixels) = uncached_pixels {
        let image = Image::from_rgba8(pixels);
        UI_IMAGES.with(|images| {
            images
                .borrow_mut()
                .entry(id)
                .or_default()
                .insert(key.into(), image.clone());
        });
        publish(ui, key, image);
    }
    trim_ui(ui, id, shared);
}

/// Updates/History lines and the History shelf also carry covers: set the
/// image on every entry whose key `matches` (and whose cover differs).
pub(crate) fn set_timeline_posters(ui: &AppWindow, matches: impl Fn(&str) -> bool, image: &Image) {
    use slint::ComponentHandle;
    let bridge = ui.global::<crate::TimelineBridge>();
    // The whole list and the window the page renders from it.
    for entries in [bridge.get_entries(), bridge.get_shown()] {
        for index in 0..entries.row_count() {
            if let Some(mut row) = entries.row_data(index)
                && matches(row.artwork_key.as_str())
                && row.poster != *image
            {
                row.poster = image.clone();
                entries.set_row_data(index, row);
            }
        }
    }
    let shelf = bridge.get_shelf();
    for index in 0..shelf.row_count() {
        if let Some(mut row) = shelf.row_data(index)
            && matches(row.artwork_key.as_str())
            && row.poster != *image
        {
            row.poster = image.clone();
            shelf.set_row_data(index, row);
        }
    }
}

fn publish(ui: &AppWindow, key: &str, image: Image) {
    let model = ui.get_items();
    for index in 0..model.row_count() {
        if let Some(mut row) = model.row_data(index)
            && row.artwork_key.as_str() == key
            && row.poster != image
        {
            row.poster = image.clone();
            model.set_row_data(index, row);
        }
    }
    for model in [ui.get_downloaded_rows(), ui.get_download_queue()] {
        for index in 0..model.row_count() {
            if let Some(mut row) = model.row_data(index)
                && row.artwork_key.as_str() == key
                && row.poster != image
            {
                row.poster = image.clone();
                model.set_row_data(index, row);
            }
        }
    }
    set_timeline_posters(ui, |row| row == key, &image);
    crate::app::set_global_posters(ui, |row| row == key, &image);
    if ui.get_series_artwork_key().as_str() == key {
        set_series_poster(ui, image);
    }
}

/// The detail screen's poster, and the few-pixel copy its desktop hero
/// scales up smoothly into a blurred backdrop.
pub(crate) fn set_series_poster(ui: &AppWindow, image: Image) {
    ui.set_series_backdrop(backdrop(&image).unwrap_or_default());
    ui.set_series_poster(image);
}

/// Average the poster down to `WIDTH` pixels across (keeping its shape).
fn backdrop(image: &Image) -> Option<Image> {
    const WIDTH: u32 = 12;
    let source = image.to_rgba8()?;
    let (w, h) = (source.width(), source.height());
    if w == 0 || h == 0 {
        return None;
    }
    let height = (WIDTH * h / w).clamp(1, 48);
    let pixels = source.as_slice();
    let mut out = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(WIDTH, height);
    for (i, pixel) in out.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (i as u32 % WIDTH, i as u32 / WIDTH);
        let (x0, x1) = (x * w / WIDTH, ((x + 1) * w / WIDTH).max(x * w / WIDTH + 1));
        let (y0, y1) = (
            y * h / height,
            ((y + 1) * h / height).max(y * h / height + 1),
        );
        // Every fourth pixel each way is plenty for an average.
        let (mut sum, mut count) = ([0u64; 3], 0u64);
        for sy in (y0..y1.min(h)).step_by(4) {
            for sx in (x0..x1.min(w)).step_by(4) {
                let p = pixels[(sy * w + sx) as usize];
                sum[0] += u64::from(p.r);
                sum[1] += u64::from(p.g);
                sum[2] += u64::from(p.b);
                count += 1;
            }
        }
        let count = count.max(1);
        *pixel = slint::Rgba8Pixel::new(
            (sum[0] / count) as u8,
            (sum[1] / count) as u8,
            (sum[2] / count) as u8,
            255,
        );
    }
    Some(Image::from_rgba8(out))
}

fn forget_images(ui: &AppWindow, id: u64, keys: &BTreeSet<String>) {
    if keys.is_empty() {
        return;
    }
    UI_IMAGES.with(|images| {
        if let Some(images) = images.borrow_mut().get_mut(&id) {
            images.retain(|key, _| !keys.contains(key));
        }
    });
    let model = ui.get_items();
    for index in 0..model.row_count() {
        if let Some(mut row) = model.row_data(index)
            && keys.contains(row.artwork_key.as_str())
            && row.poster.size().width > 0
        {
            row.poster = Image::default();
            model.set_row_data(index, row);
        }
    }
    for model in [ui.get_downloaded_rows(), ui.get_download_queue()] {
        for index in 0..model.row_count() {
            if let Some(mut row) = model.row_data(index)
                && keys.contains(row.artwork_key.as_str())
                && row.poster.size().width > 0
            {
                row.poster = Image::default();
                model.set_row_data(index, row);
            }
        }
    }
    set_timeline_posters(ui, |row| keys.contains(row), &Image::default());
    crate::app::set_global_posters(ui, |row| keys.contains(row), &Image::default());
    if keys.contains(ui.get_series_artwork_key().as_str()) {
        set_series_poster(ui, Image::default());
    }
}

fn trim_ui(ui: &AppWindow, id: u64, shared: &Shared) {
    let keep = shared
        .decoded
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .ui_keys(ui.get_series_artwork_key().as_str());
    let mut remove = UI_IMAGES.with(|images| {
        images
            .borrow()
            .get(&id)
            .into_iter()
            .flat_map(|images| images.keys())
            .filter(|key| !keep.contains(*key))
            .cloned()
            .collect::<BTreeSet<_>>()
    });
    remove.extend(
        ui.get_items()
            .iter()
            .filter(|row| !keep.contains(row.artwork_key.as_str()))
            .map(|row| row.artwork_key.to_string()),
    );
    for model in [ui.get_downloaded_rows(), ui.get_download_queue()] {
        remove.extend(
            model
                .iter()
                .filter(|row| !keep.contains(row.artwork_key.as_str()))
                .map(|row| row.artwork_key.to_string()),
        );
    }
    if !keep.contains(ui.get_series_artwork_key().as_str()) {
        remove.insert(ui.get_series_artwork_key().to_string());
    }
    forget_images(ui, id, &remove);
}

fn decode(bytes: &[u8]) -> Result<Pixels, String> {
    let _permit = crate::decode_admission::SHARED
        .acquire(crate::decode_admission::Priority::Prefetch, || true)
        .ok_or_else(|| "operation cancelled".to_string())?;
    let image = decode_display_image(bytes, cover_limits(), 460, 690).map_err(|e| e.to_string())?;
    Ok(Pixels::clone_from_slice(
        image.as_raw(),
        image.width(),
        image.height(),
    ))
}

#[derive(Clone)]
struct RequestProof {
    access: PageAccess,
    url: String,
    scope: Option<String>,
}
impl RequestProof {
    fn new(source: &reader_sources::Source, url: &str) -> Result<Self, String> {
        let access = source.page_access();
        let scope = access
            .page_cache_scope(url, &BTreeMap::new())
            .map_err(|e| e.to_string())?;
        Ok(Self {
            access,
            url: url.into(),
            scope,
        })
    }
    fn current(&self) -> bool {
        self.access
            .page_cache_scope(&self.url, &BTreeMap::new())
            .is_ok_and(|scope| scope == self.scope)
    }
    fn encoded_key(&self, key: &str) -> Option<String> {
        self.scope.as_ref().map(|scope| format!("{key}:{scope}"))
    }
}

/// Worker-owned only: never attach this to Loaded or the UI completion closure.
struct DeferredCacheWrite {
    key: String,
    epoch: u64,
    bytes: Vec<u8>,
    compression: crate::image_storage::ImageCompression,
    max_side: Option<u32>,
    access: SourceAvailability,
    proof: RequestProof,
}
impl DeferredCacheWrite {
    fn write(self, encoded: &nova_cache::blob::Cache, shared: &Shared, task: &Task) {
        let still_current =
            || shared.current(task) && self.access.is_available() && self.proof.current();
        if encoded.generation() != self.epoch || !still_current() {
            return;
        }
        write_fetched(
            encoded,
            &self.key,
            self.epoch,
            &self.bytes,
            self.compression,
            self.max_side,
            still_current,
        );
    }
}

type LoadResult = Result<(Loaded, Option<DeferredCacheWrite>), String>;

struct Loaded {
    pixels: Pixels,
    source_id: String,
    access: SourceAvailability,
    proof: Option<RequestProof>,
}
impl Loaded {
    fn is_current(&self) -> bool {
        self.access.is_available() && self.proof.as_ref().is_none_or(RequestProof::current)
    }
}

struct Permit {
    source_id: String,
    access: SourceAvailability,
    proof: Option<RequestProof>,
}
impl Permit {
    fn current(&self) -> bool {
        self.access.is_available() && self.proof.as_ref().is_none_or(RequestProof::current)
    }
    fn reusable(&self) -> bool {
        self.current()
            && self
                .proof
                .as_ref()
                .is_none_or(|proof| proof.scope.is_some())
    }
    fn encoded_key(&self, key: &str) -> Option<String> {
        if !self.reusable() {
            return None;
        }
        self.proof
            .as_ref()
            .map_or_else(|| Some(key.into()), |proof| proof.encoded_key(key))
    }
}

/// Reader owns source authorization and UI admission. Nova Kit owns the actual
/// shared-pixel LRU, its byte accounting, access clock and eviction selection.
struct DecodedCache {
    inner: nova_cache::DecodedCache,
    permits: BTreeMap<String, Permit>,
    protected: BTreeSet<String>,
}

impl Default for DecodedCache {
    fn default() -> Self {
        Self {
            inner: nova_cache::DecodedCache::new(DECODED_CACHE_LIMIT),
            permits: BTreeMap::new(),
            protected: BTreeSet::new(),
        }
    }
}

impl DecodedCache {
    fn get(&mut self, key: &str, source_id: &str) -> Option<Pixels> {
        let permit = self.permits.get(key)?;
        if permit.source_id != source_id || !permit.reusable() {
            return None;
        }
        self.inner.get(key)
    }

    fn insert(&mut self, key: String, loaded: Loaded) -> BTreeSet<String> {
        let Loaded {
            pixels,
            source_id,
            access,
            proof,
        } = loaded;
        let evicted = self.inner.insert(key.clone(), pixels);
        for key in &evicted {
            self.permits.remove(key);
        }
        if self.inner.contains(&key) {
            self.permits.insert(
                key,
                Permit {
                    source_id,
                    access,
                    proof,
                },
            );
        } else {
            self.permits.remove(&key);
        }
        evicted.into_iter().collect()
    }

    fn keys(&self) -> BTreeSet<String> {
        self.inner.keys_by_recency().into_iter().collect()
    }

    fn protect(&mut self, key: String) {
        self.protected.insert(key);
        self.inner
            .set_protected(self.protected.iter().cloned().collect());
    }

    fn retain_visible(&mut self, desired: &BTreeSet<String>) {
        self.protected = desired.clone();
        let evicted = self.inner.set_protected(desired.iter().cloned().collect());
        for key in evicted {
            self.permits.remove(&key);
        }
        for key in desired {
            let _ = self.inner.get(key);
        }
    }

    fn ui_keys(&self, selected: &str) -> BTreeSet<String> {
        let available = |key: &str| self.permits.get(key).is_some_and(Permit::current);
        let recent = self.inner.keys_by_recency();
        let mut keep = recent
            .iter()
            .filter(|key| {
                available(key) && (self.protected.contains(*key) || key.as_str() == selected)
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        let extra = recent
            .into_iter()
            .rev()
            .filter(|key| available(key) && !keep.contains(key))
            .take(UNLOAD_EXTRA_CARDS)
            .collect::<Vec<_>>();
        keep.extend(extra);
        keep
    }

    fn set_budget(&mut self, bytes: usize) -> BTreeSet<String> {
        let evicted = self.inner.set_budget(bytes);
        for key in &evicted {
            self.permits.remove(key);
        }
        evicted.into_iter().collect()
    }

    fn clear(&mut self) {
        self.inner.clear();
        self.permits.clear();
        self.protected.clear();
    }
}

fn load(
    services: &Services,
    encoded: &nova_cache::blob::Cache,
    shared: &Shared,
    task: &Task,
) -> LoadResult {
    task.cancel.check().map_err(|e| e.to_string())?;
    // Cached artwork must still respect source availability/adult/profile gates.
    let source = services.source(&task.source_id)?;
    load_source(source, encoded, shared, task)
}

fn load_source(
    source: reader_sources::Source,
    encoded: &nova_cache::blob::Cache,
    shared: &Shared,
    task: &Task,
) -> LoadResult {
    task.cancel.check().map_err(|e| e.to_string())?;
    let proof = RequestProof::new(&source, &task.url)?;
    let encoded_key = proof.encoded_key(&task.key);
    let access = source.availability();
    {
        let mut cache = shared.decoded.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = cache.permits.get_mut(&task.key).filter(|entry| {
            entry.source_id == task.source_id
                && entry
                    .proof
                    .as_ref()
                    .is_some_and(|old| old.scope == proof.scope && old.url == proof.url)
        }) {
            entry.access = access.clone();
            entry.proof = Some(proof.clone());
        }
        let same_scope = cache.permits.get(&task.key).is_some_and(|entry| {
            entry
                .proof
                .as_ref()
                .is_some_and(|old| old.scope == proof.scope && old.url == proof.url)
        });
        if same_scope && let Some(pixels) = cache.get(&task.key, &task.source_id) {
            if let Some(key) = &encoded_key {
                encoded.touch(key);
            }
            return Ok((
                Loaded {
                    pixels,
                    source_id: task.source_id.clone(),
                    access,
                    proof: Some(proof),
                },
                None,
            ));
        }
    }
    let settings = crate::image_storage::effective_for_source(&source.descriptor().id);
    let epoch = encoded.generation();
    let cached = if settings.policy.disk_enabled {
        let _decode = shared.decode.lock().unwrap_or_else(|e| e.into_inner());
        task.cancel.check().map_err(|e| e.to_string())?;
        encoded_key
            .as_ref()
            .and_then(|key| read_cached_pixels(encoded, key))
    } else {
        None
    };
    if let Some(pixels) = cached {
        task.cancel.check().map_err(|e| e.to_string())?;
        return Ok((
            Loaded {
                pixels,
                source_id: task.source_id.clone(),
                access,
                proof: Some(proof),
            },
            None,
        ));
    }
    let bytes = source
        .fetch_page_limited(
            &task.url,
            &BTreeMap::new(),
            &task.cancel,
            cover_limits().max_encoded_bytes,
        )
        .map_err(|e| e.to_string())?;
    task.cancel.check().map_err(|e| e.to_string())?;
    let pixels = {
        let _decode = shared.decode.lock().unwrap_or_else(|e| e.into_inner());
        task.cancel.check().map_err(|e| e.to_string())?;
        decode(&bytes)?
    };
    task.cancel.check().map_err(|e| e.to_string())?;
    if !proof.current() {
        return Err("Artwork authorization context changed".into());
    }
    let deferred = if settings.policy.disk_enabled && bytes.len() <= ENCODED_LIMIT {
        encoded_key.map(|key| DeferredCacheWrite {
            key,
            epoch,
            bytes,
            compression: settings.cache,
            max_side: settings.policy.poster_max_side,
            access: access.clone(),
            proof: proof.clone(),
        })
    } else {
        None
    };
    Ok((
        Loaded {
            pixels,
            source_id: task.source_id.clone(),
            access,
            proof: Some(proof),
        },
        deferred,
    ))
}

fn read_cached_pixels(cache: &nova_cache::blob::Cache, key: &str) -> Option<Pixels> {
    let decoded = RefCell::new(None);
    // Full validation happens exactly once on the worker. A cancelled request
    // must not make Kit mistake a valid existing variant for a corrupt one.
    cache
        .get(key, |bytes| {
            let dimensions = inspect(bytes)?;
            *decoded.borrow_mut() = Some(decode(bytes)?);
            Ok(dimensions)
        })
        .ok()
        .flatten()?;
    decoded.into_inner()
}

fn inspect(bytes: &[u8]) -> Result<(u32, u32), String> {
    let dimensions =
        reader_engine::inspect_image(bytes, cover_limits()).map_err(|e| e.to_string())?;
    Ok((dimensions.width, dimensions.height))
}

/// Only a newly fetched image is transformed; Kit owns atomic publication and
/// refuses stale epochs or overwriting an existing validated variant.
fn write_fetched(
    cache: &nova_cache::blob::Cache,
    key: &str,
    epoch: u64,
    bytes: &[u8],
    compression: crate::image_storage::ImageCompression,
    max_side: Option<u32>,
    still_current: impl Fn() -> bool,
) {
    if !still_current() || bytes.len() > ENCODED_LIMIT {
        return;
    }
    let converted = transform_new_image(bytes, compression, max_side);
    let stored = match &converted {
        Ok(Some(bytes)) if bytes.len() <= ENCODED_LIMIT => bytes.as_slice(),
        _ => bytes,
    };
    // Optional disk failures cannot hide a successfully decoded source poster.
    let _ = cache.put_if_current(key, stored, epoch, inspect, still_current);
}

/// Explicit resolution edits use Nova's actual aspect-preserving Triangle fit.
/// Original resolution avoids this intermediate conversion entirely.
fn transform_new_image(
    bytes: &[u8],
    mut compression: crate::image_storage::ImageCompression,
    max_side: Option<u32>,
) -> Result<Option<Vec<u8>>, String> {
    // Classify before resizing: its intermediate PNG must not make a lossy
    // source look like a lossless input.
    compression.lossless_inputs &= nova_image::is_lossless_input(bytes);
    let resized = if let Some(max_side) = max_side {
        let image =
            reader_engine::decode_image(bytes, cover_limits()).map_err(|e| e.to_string())?;
        if image.width().max(image.height()) > max_side {
            let image =
                nova_cache::fit_rgba(image, Some(max_side)).ok_or("Invalid artwork resolution")?;
            let mut png = Vec::new();
            use image::ImageEncoder;
            image::codecs::png::PngEncoder::new(&mut png)
                .write_image(
                    image.as_raw(),
                    image.width(),
                    image.height(),
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| e.to_string())?;
            Some(png)
        } else {
            None
        }
    } else {
        None
    };
    if compression.enabled {
        // JPEG alpha/unsupported codec failures retain the lossless resized
        // source when available, otherwise the admitted original bytes.
        match crate::reader_export::reencode_storage_image(
            resized.as_deref().unwrap_or(bytes),
            compression,
            cover_limits(),
        ) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(_) => Ok(resized),
        }
    } else {
        Ok(resized)
    }
}

fn clear_cache(cache: &nova_cache::blob::Cache, decoded: &Mutex<DecodedCache>) {
    decoded.lock().unwrap_or_else(|e| e.into_inner()).clear();
    cache.clear();
    for path in legacy_paths(cache) {
        let _ = fs::remove_file(path);
    }
}

fn legacy_paths(cache: &nova_cache::blob::Cache) -> Vec<PathBuf> {
    fs::read_dir(cache.directory())
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let key = path.file_stem()?.to_str()?;
            if key.len() == 64
                && key.bytes().all(|b| b.is_ascii_hexdigit())
                && path
                    .extension()
                    .is_some_and(|ext| ext == "cover" || ext == "pending")
                && fs::symlink_metadata(&path)
                    .is_ok_and(|stat| stat.is_file() || stat.file_type().is_symlink())
            {
                Some(path)
            } else {
                None
            }
        })
        .collect()
}
fn cache_usage(cache: &nova_cache::blob::Cache) -> (u64, usize) {
    let mut usage = cache.usage();
    for path in legacy_paths(cache) {
        if let Ok(stat) = fs::symlink_metadata(path)
            && stat.is_file()
        {
            usage.0 = usage.0.saturating_add(stat.len());
            usage.1 += 1;
        }
    }
    usage
}

/// Compatibility with Reader's previous raw files is deliberately limited to
/// importing that format once. All subsequent ownership and maintenance is Kit.
fn migrate_legacy(cache: &nova_cache::blob::Cache) {
    let Ok(entries) = fs::read_dir(cache.directory()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(key) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let Ok(stat) = fs::symlink_metadata(&path) else {
            continue;
        };
        let legacy = path
            .extension()
            .is_some_and(|ext| ext == "cover" || ext == "pending");
        if legacy && stat.file_type().is_symlink() {
            // Dispose only of the owned link, never read or remove its target.
            let _ = fs::remove_file(&path);
            continue;
        }
        if !stat.is_file() {
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "pending") {
            let _ = fs::remove_file(&path);
        } else if path.extension().is_some_and(|ext| ext == "cover") {
            let valid = (|| {
                if stat.len() > ENCODED_LIMIT as u64 {
                    return None;
                }
                let mut bytes = Vec::new();
                fs::File::open(&path)
                    .ok()?
                    .take(ENCODED_LIMIT as u64 + 1)
                    .read_to_end(&mut bytes)
                    .ok()?;
                inspect(&bytes).ok()?;
                Some(bytes)
            })();
            if valid.as_ref().is_none_or(|bytes| {
                cache
                    .put_if_current(key, bytes, cache.generation(), inspect, || true)
                    .is_ok()
            }) {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

// The application adapter follows Nova's actual app/cache_gc.rs markers and
// Library retention policy; cache traversal, scheduling and eviction live in Kit.
#[derive(serde::Serialize, serde::Deserialize)]
struct Marker {
    directory: PathBuf,
    completed: u64,
}
struct ArtworkBackend {
    services: Services,
    shared: Arc<Shared>,
}
impl nova_cache::gc::Backend for ArtworkBackend {
    fn policy(&self) -> nova_cache::gc::DiskPolicy {
        crate::image_storage::cache_policy(crate::image_storage::CacheKind::Artwork)
    }
    fn protected_urls(&self) -> std::io::Result<HashSet<String>> {
        let decoded = self
            .shared
            .decoded
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        Ok(decoded
            .protected
            .iter()
            .filter_map(|key| {
                decoded
                    .permits
                    .get(key)
                    .and_then(|permit| permit.encoded_key(key))
            })
            .collect())
    }
    fn last_cleanup(&self, directory: &Path) -> std::io::Result<Option<u64>> {
        let raw = nova_storage::try_get_str("reader:cache-gc:artwork:v1")
            .map_err(std::io::Error::other)?;
        Ok(raw
            .and_then(|raw| serde_json::from_str::<Marker>(&raw).ok())
            .filter(|marker| marker.directory == directory)
            .map(|marker| marker.completed))
    }
    fn save_cleanup(&self, directory: &Path, completed: u64) -> std::io::Result<()> {
        nova_storage::try_set_str(
            "reader:cache-gc:artwork:v1",
            &serde_json::to_string(&Marker {
                directory: directory.into(),
                completed,
            })?,
        )
        .map_err(std::io::Error::other)
    }
    fn preferred_urls(&self) -> std::io::Result<HashSet<String>> {
        let state = self
            .services
            .domain
            .snapshot()
            .map_err(std::io::Error::other)?;
        Ok(state
            .series
            .iter()
            .filter(|series| state.library.iter().any(|entry| entry.series == series.id))
            .filter_map(|series| {
                let key = key(series);
                if key.is_empty() {
                    return None;
                }
                let source = self.services.source(&series.id.source_id).ok()?;
                RequestProof::new(&source, series.cover_url.as_deref()?)
                    .ok()?
                    .encoded_key(&key)
            })
            .collect())
    }
    fn completed(&self, _: nova_cache::gc::Report) {
        self.shared.queue_maintenance(false);
    }
    fn failed(&self, error: &std::io::Error) {
        eprintln!("artwork cache maintenance failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reader_domain::SeriesId;
    use slint::ComponentHandle;

    fn series(source: &str, id: &str, url: &str) -> Series {
        Series {
            publication_status: Default::default(),
            id: SeriesId::new(source, id),
            title: String::new(),
            description: String::new(),
            authors: vec![],
            genres: vec![],
            cover_url: Some(url.into()),
            canonical_url: None,
        }
    }

    #[test]
    fn poster_pool_paints_fast_cards_without_waiting_for_slow_fetch_or_cache_encoder() {
        const CHILD: &str = "NOVA_POSTER_POOL_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "artwork::tests::poster_pool_paints_fast_cards_without_waiting_for_slow_fetch_or_cache_encoder", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            eprint!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use slint::{ModelRc, VecModel};
        use std::{cell::Cell, rc::Rc, sync::atomic::AtomicUsize, time::Instant};
        struct Latency {
            active: AtomicUsize,
            peak: AtomicUsize,
            release: AtomicBool,
            slow_started: AtomicBool,
        }
        impl reader_sources::SourceTransport for Latency {
            fn page_cache_partition(&self, _: &str) -> Option<String> {
                Some("latency-fixture".into())
            }
            fn request(
                &self,
                request: &reader_sources::HttpRequest,
                cap: usize,
                cancel: &CancelToken,
            ) -> reader_sources::Result<reader_sources::HttpResponse> {
                assert_eq!(cap, ENCODED_LIMIT);
                let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
                self.peak.fetch_max(active, Ordering::AcqRel);
                struct Active<'a>(&'a AtomicUsize);
                impl Drop for Active<'_> {
                    fn drop(&mut self) {
                        self.0.fetch_sub(1, Ordering::AcqRel);
                    }
                }
                let _active = Active(&self.active);
                if request.url.ends_with("slow.png") {
                    self.slow_started.store(true, Ordering::Release);
                    while !self.release.load(Ordering::Acquire) {
                        cancel.check()?;
                        std::thread::sleep(Duration::from_millis(5));
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(40));
                }
                reader_sources::SourceTransport::request(&NeutralPosters, request, cap, cancel)
            }
        }
        i_slint_backend_testing::init_integration_test_with_system_time();
        let directory = tempfile::tempdir().unwrap();
        let transport = Arc::new(Latency {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            release: AtomicBool::new(false),
            slow_started: AtomicBool::new(false),
        });
        let (_registry, source) =
            poster_source(&directory.path().join("sources"), transport.clone());
        let items: Vec<_> = (0..9)
            .map(|i| {
                series(
                    "poster-fixture",
                    &i.to_string(),
                    &format!(
                        "https://example.test/{}.png",
                        if i == 0 {
                            "slow".to_owned()
                        } else {
                            i.to_string()
                        }
                    ),
                )
            })
            .collect();
        let cache =
            nova_cache::blob::Cache::new(directory.path().join("posters"), ENCODED_LIMIT as u64);
        let shared = Arc::new(Shared::default());
        let serial = {
            let source = source.clone();
            let shared = shared.clone();
            let cache = cache.clone();
            let items = items.clone();
            std::thread::spawn(move || {
                let start = Instant::now();
                for item in &items[1..] {
                    let task = Task {
                        key: key(item),
                        source_id: item.id.source_id.clone(),
                        url: item.cover_url.clone().unwrap(),
                        generation: 0,
                        cancel: CancelToken::default(),
                    };
                    load_source(source.clone(), &cache, &shared, &task).unwrap();
                }
                start.elapsed()
            })
            .join()
            .unwrap()
        };
        transport.peak.store(0, Ordering::Release);
        let ui = AppWindow::new().unwrap();
        ui.set_items(ModelRc::new(VecModel::from(
            items
                .iter()
                .map(|item| crate::ItemRow {
                    artwork_key: key(item).into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        )));
        let (sender, receiver) = mpsc::sync_channel(MAX_REQUESTS);
        let loader = ArtworkLoader {
            sender,
            shared: shared.clone(),
            ui: ui.as_weak(),
            id: NEXT_LOADER.fetch_add(1, Ordering::Relaxed),
            encoded: cache.clone(),
        };
        // Deliberately leave optional cache writes unconsumed, as if an encoder
        // were busy with an older page. Visible work must still finish.
        shared.worker_available.store(true, Ordering::Release);
        let load_shared = shared.clone();
        let workers = start_fetch_workers(
            receiver,
            ui.as_weak(),
            loader.id,
            shared.clone(),
            move |task| load_source(source.clone(), &cache, &load_shared, task),
        );
        for item in &items {
            loader.request(item);
        }
        let started = Instant::now();
        let fast_ready = Rc::new(Cell::new(None));
        let completed = Rc::new(Cell::new(false));
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(5), {
            let weak = ui.as_weak();
            let transport = transport.clone();
            let fast_ready = fast_ready.clone();
            let completed = completed.clone();
            move || {
                let ui = weak.upgrade().unwrap();
                let rows = ui.get_items();
                if fast_ready.get().is_none()
                    && rows.iter().skip(1).all(|row| row.poster.size().width > 0)
                {
                    assert!(transport.slow_started.load(Ordering::Acquire));
                    assert_eq!(
                        rows.row_data(0).unwrap().poster.size().width,
                        0,
                        "fast cards paint while the first response is blocked"
                    );
                    fast_ready.set(Some(started.elapsed()));
                    transport.release.store(true, Ordering::Release);
                }
                if rows.iter().all(|row| row.poster.size().width > 0) {
                    completed.set(true);
                    slint::quit_event_loop().unwrap();
                } else if started.elapsed() > Duration::from_secs(5) {
                    transport.release.store(true, Ordering::Release);
                    slint::quit_event_loop().unwrap();
                }
            }
        });
        slint::run_event_loop().unwrap();
        assert!(completed.get(), "poster pipeline stalled");
        assert!(
            transport.peak.load(Ordering::Acquire) > 1,
            "network requests must overlap"
        );
        assert!(transport.peak.load(Ordering::Acquire) <= FETCH_WORKERS);
        assert!(
            !shared.cache_writes.lock().unwrap().pending.is_empty(),
            "unprocessed optional cache writes cannot block the grid"
        );
        eprintln!(
            "8 neutral posters with40ms transport: serial {serial:?}, concurrent fast-card paint {:?}, peak requests {}",
            fast_ready.get().unwrap(),
            transport.peak.load(Ordering::Acquire)
        );
        drop(loader);
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn optional_cache_queue_is_bounded_by_bytes_and_count() {
        let directory = tempfile::tempdir().unwrap();
        let (_registry, source) = poster_source(directory.path(), Arc::new(NeutralPosters));
        let job = |bytes| {
            (
                Task {
                    key: "cover".into(),
                    source_id: "poster-fixture".into(),
                    url: "https://example.test/cover.png".into(),
                    generation: 0,
                    cancel: CancelToken::default(),
                },
                DeferredCacheWrite {
                    key: "cover".into(),
                    epoch: 0,
                    bytes: vec![0; bytes],
                    compression: Default::default(),
                    max_side: None,
                    access: source.availability(),
                    proof: RequestProof::new(&source, "https://example.test/cover.png").unwrap(),
                },
            )
        };
        let mut queue = CacheWrites::default();
        for _ in 0..3 {
            let (task, write) = job(ENCODED_LIMIT);
            queue.push(task, write);
        }
        assert_eq!(queue.pending.len(), 2);
        assert_eq!(queue.bytes, CACHE_QUEUE_BYTES);
        queue.pop();
        assert_eq!(queue.bytes, ENCODED_LIMIT);
        let mut queue = CacheWrites::default();
        for _ in 0..MAX_REQUESTS + 1 {
            let (task, write) = job(1);
            queue.push(task, write);
        }
        assert_eq!(queue.pending.len(), MAX_REQUESTS);
        assert_eq!(queue.bytes, MAX_REQUESTS);
    }

    #[test]
    fn cover_keys_isolate_source_identity_and_expiring_urls() {
        let original = series("source", "one", "https://example.test/cover?a=1");
        let key = key(&original);
        assert_eq!(key.len(), 64);
        assert_ne!(
            key,
            super::key(&series("other", "one", "https://example.test/cover?a=1"))
        );
        assert_ne!(
            key,
            super::key(&series("source", "two", "https://example.test/cover?a=1"))
        );
        assert_ne!(
            key,
            super::key(&series("source", "one", "https://example.test/cover?a=2"))
        );
        assert_ne!(
            super::key(&series("a", "bc", "d")),
            super::key(&series("ab", "c", "d"))
        );
        assert!(super::key(&series("source", "one", "")).is_empty());
        let mut renamed = original;
        renamed.title = "A changed title".into();
        assert_eq!(key, super::key(&renamed));
    }

    #[test]
    fn invalidation_cancels_work_and_rejects_stale_failure_publication() {
        let shared = Shared::default();
        let task = Task {
            key: "cover".into(),
            source_id: String::new(),
            url: String::new(),
            generation: 0,
            cancel: CancelToken::default(),
        };
        shared
            .requests
            .lock()
            .unwrap()
            .pending
            .insert(task.key.clone(), task.cancel.clone());
        assert!(shared.current(&task));
        shared.finish(&task, true);
        assert!(shared.requests.lock().unwrap().failed.contains("cover"));
        shared
            .requests
            .lock()
            .unwrap()
            .pending
            .insert(task.key.clone(), task.cancel.clone());
        shared.invalidate();
        assert!(task.cancel.is_cancelled());
        assert!(!shared.current(&task));
        shared.finish(&task, true);
        let requests = shared.requests.lock().unwrap();
        assert!(requests.pending.is_empty());
        assert!(requests.failed.is_empty());
    }

    #[test]
    fn retaining_visible_keys_cancels_obsolete_work_without_invalidating_visible_work() {
        let shared = Shared::default();
        let task = |key: &str| Task {
            key: key.into(),
            source_id: String::new(),
            url: String::new(),
            generation: 0,
            cancel: CancelToken::default(),
        };
        let active = task("active-offscreen");
        let queued = task("queued-offscreen");
        let visible = task("visible");
        {
            let mut requests = shared.requests.lock().unwrap();
            for task in [&active, &queued, &visible] {
                requests
                    .pending
                    .insert(task.key.clone(), task.cancel.clone());
            }
            requests.failed.insert("failed-offscreen".into());
            requests.failed.insert("failed-visible".into());
        }
        shared.retain(&BTreeSet::from(["visible".into(), "failed-visible".into()]));
        assert!(!shared.current(&active));
        assert!(!shared.current(&queued));
        assert!(shared.current(&visible));
        assert_eq!(shared.generation.load(Ordering::Acquire), 0);
        let retry = task("active-offscreen");
        {
            let mut requests = shared.requests.lock().unwrap();
            assert!(!requests.pending.contains_key(&active.key));
            assert!(!requests.failed.contains("failed-offscreen"));
            assert!(requests.failed.contains("failed-visible"));
            requests
                .pending
                .insert(retry.key.clone(), retry.cancel.clone());
        }
        // A cancelled task cannot remove or fail a newer request for its key.
        shared.finish(&active, true);
        assert!(shared.current(&retry));
        let requests = shared.requests.lock().unwrap();
        assert!(requests.pending.contains_key(&retry.key));
        assert!(!requests.failed.contains(&retry.key));
    }

    fn package(version: &str) -> Vec<u8> {
        let domains = if version == "1.0.0" {
            vec!["example.test"]
        } else {
            vec!["example.test", "other.test"]
        };
        package_domains(version, domains)
    }

    fn package_domains(version: &str, domains: Vec<&str>) -> Vec<u8> {
        use std::io::{Cursor, Write};
        let manifest = serde_json::json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"fixture","name":"Fixture","version":version,"language":"en","entry":"main.js","permissions":{"domains":domains},"capabilities":["browse","search","filters","details","chapters","pages"]});
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("main.js", b"globalThis.source = {};".to_vec()),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn source_fixture() -> (
        tempfile::TempDir,
        reader_sources::SourceRegistry,
        SourceAvailability,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let registry = reader_sources::SourceRegistry::open(directory.path()).unwrap();
        registry.install_zip(&package("1.0.0")).unwrap();
        registry.set_enabled("fixture", true).unwrap();
        let access = registry.source("fixture").unwrap().availability();
        (directory, registry, access)
    }

    fn loaded(pixels: Pixels, access: &SourceAvailability) -> Loaded {
        Loaded {
            pixels,
            source_id: "fixture".into(),
            access: access.clone(),
            proof: None,
        }
    }

    #[test]
    fn decoded_lru_evicts_oldest_and_preserves_visible_pixels_within_one_budget() {
        let (_directory, _registry, access) = source_fixture();
        let mut cache = DecodedCache::default();
        cache.set_budget(2 * 32 * 32 * 4);
        cache.insert("first".into(), loaded(Pixels::new(32, 32), &access));
        cache.insert("second".into(), loaded(Pixels::new(32, 32), &access));
        assert!(cache.get("first", "fixture").is_some());
        let evicted = cache.insert("third".into(), loaded(Pixels::new(32, 32), &access));
        assert_eq!(evicted, BTreeSet::from(["second".into()]));
        assert!(cache.get("second", "fixture").is_none());
        cache.retain_visible(&BTreeSet::from(["first".into()]));
        cache.insert("fourth".into(), loaded(Pixels::new(32, 32), &access));
        assert!(cache.get("first", "fixture").is_some());
        assert!(cache.get("third", "fixture").is_none());
        assert_eq!(cache.inner.stats().1, 2 * 32 * 32 * 4);
    }

    #[test]
    fn budget_eviction_releases_model_and_ui_handle_references_together() {
        use slint::{ModelRc, VecModel};
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let (_directory, _registry, access) = source_fixture();
        let shared = Shared::default();
        shared.decoded.lock().unwrap().set_budget(2 * 32 * 32 * 4);
        let id = NEXT_LOADER.fetch_add(1, Ordering::Relaxed);
        ui.set_items(ModelRc::new(VecModel::from(
            ["first", "second", "third"]
                .into_iter()
                .map(|key| crate::ItemRow {
                    artwork_key: key.into(),
                    ..crate::ItemRow::default()
                })
                .collect::<Vec<_>>(),
        )));
        admit(
            &ui,
            id,
            &shared,
            "first",
            loaded(Pixels::new(32, 32), &access),
        );
        admit(
            &ui,
            id,
            &shared,
            "second",
            loaded(Pixels::new(32, 32), &access),
        );
        shared
            .decoded
            .lock()
            .unwrap()
            .retain_visible(&BTreeSet::from(["first".into()]));
        let first = ui.get_items().row_data(0).unwrap().poster;
        admit(
            &ui,
            id,
            &shared,
            "third",
            loaded(Pixels::new(32, 32), &access),
        );
        assert_eq!(ui.get_items().row_data(0).unwrap().poster, first);
        assert_eq!(ui.get_items().row_data(1).unwrap().poster.size().width, 0);
        assert_eq!(ui.get_items().row_data(2).unwrap().poster.size().width, 32);
        let cache = shared.decoded.lock().unwrap();
        assert_eq!(cache.inner.stats().1, 2 * 32 * 32 * 4);
        assert_eq!(
            UI_IMAGES.with(|images| images.borrow()[&id]
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>()),
            cache.keys()
        );
        UI_IMAGES.with(|images| {
            images.borrow_mut().remove(&id);
        });
    }

    #[test]
    fn sixty_posters_scroll_and_tab_restore_synchronously_without_worker_requests() {
        use slint::{ModelRc, VecModel};
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let (_directory, _registry, access) = source_fixture();
        let shared = Arc::new(Shared::default());
        let (sender, receiver) = mpsc::sync_channel(MAX_REQUESTS);
        let loader = ArtworkLoader {
            sender,
            shared: shared.clone(),
            ui: ui.as_weak(),
            id: NEXT_LOADER.fetch_add(1, Ordering::Relaxed),
            encoded: nova_cache::blob::Cache::new(
                _directory.path().join("posters"),
                ENCODED_LIMIT as u64,
            ),
        };
        let series = (0..60)
            .map(|i| {
                series(
                    "fixture",
                    &i.to_string(),
                    &format!("https://example.test/{i}.png"),
                )
            })
            .collect::<Vec<_>>();
        for item in &series {
            shared
                .decoded
                .lock()
                .unwrap()
                .insert(key(item), loaded(Pixels::new(460, 690), &access));
        }
        let original = series
            .iter()
            .map(|series| loader.cached_image(series).unwrap())
            .collect::<Vec<_>>();
        assert!(
            !loader.encoded.directory().exists(),
            "memory hits and GC touches cannot create disk cache files"
        );
        assert_eq!(
            shared
                .decoded
                .lock()
                .unwrap()
                .inner
                .keys_by_recency()
                .last(),
            Some(&key(&series[59]))
        );
        assert!(loader.cached_image(&series[0]).is_some());
        assert_eq!(
            shared
                .decoded
                .lock()
                .unwrap()
                .inner
                .keys_by_recency()
                .last(),
            Some(&key(&series[0]))
        );
        ui.set_items(ModelRc::new(VecModel::from(
            series
                .iter()
                .zip(&original)
                .map(|(series, poster)| crate::ItemRow {
                    artwork_key: key(series).into(),
                    poster: poster.clone(),
                    ..crate::ItemRow::default()
                })
                .collect::<Vec<_>>(),
        )));
        for visible in [48..60, 0..12] {
            loader.retain(&visible.clone().map(|index| key(&series[index])).collect());
            for index in visible {
                loader.request(&series[index]);
                assert_eq!(
                    ui.get_items().row_data(index).unwrap().poster,
                    original[index]
                );
            }
        }
        // A new Updates/History model receives pixels before publication, even
        // when the old and new viewport ranges are identical and no timer fires.
        let restored = series
            .iter()
            .map(|series| crate::ItemRow {
                artwork_key: key(series).into(),
                poster: loader.cached_image(series).unwrap(),
                ..crate::ItemRow::default()
            })
            .collect::<Vec<_>>();
        assert!(restored.iter().all(|row| row.poster.size().width == 460));
        ui.set_items(ModelRc::new(VecModel::from(restored)));
        assert!(
            receiver.try_recv().is_err(),
            "cached scrolling must not enqueue fetch/decode work"
        );
        assert_eq!(
            shared.decoded.lock().unwrap().inner.stats().1,
            60 * 460 * 690 * 4
        );
        assert!(shared.decoded.lock().unwrap().inner.stats().1 <= DECODED_CACHE_LIMIT);
        drop(original);
        shared.worker_available.store(true, Ordering::Release);
        loader.clear();
        assert!(shared.take_maintenance().unwrap().clear);
        clear_cache(&loader.encoded, &shared.decoded);
        shared.clearing.store(false, Ordering::Release);
        assert!(loader.cached_image(&series[0]).is_none());
        assert!(
            ui.get_items()
                .iter()
                .all(|row| row.poster.size().width == 0)
        );
        assert!(UI_IMAGES.with(|images| {
            images
                .borrow()
                .get(&loader.id)
                .is_none_or(|images| images.is_empty())
        }));
    }

    #[test]
    fn source_scroll_return_paints_cached_posters_before_any_retry_timer() {
        use slint::{ModelRc, VecModel};
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let (_directory, _registry, access) = source_fixture();
        let shared = Arc::new(Shared::default());
        let (sender, receiver) = mpsc::sync_channel(MAX_REQUESTS);
        let loader = std::rc::Rc::new(ArtworkLoader {
            sender,
            shared: shared.clone(),
            ui: ui.as_weak(),
            id: NEXT_LOADER.fetch_add(1, Ordering::Relaxed),
            encoded: nova_cache::blob::Cache::new(
                _directory.path().join("posters"),
                ENCODED_LIMIT as u64,
            ),
        });
        let series = std::rc::Rc::new(
            (0..60)
                .map(|i| {
                    series(
                        "fixture",
                        &i.to_string(),
                        &format!("https://example.test/{i}.png"),
                    )
                })
                .collect::<Vec<_>>(),
        );
        for item in series.iter() {
            shared
                .decoded
                .lock()
                .unwrap()
                .insert(key(item), loaded(Pixels::new(460, 690), &access));
        }
        ui.on_request_artwork({
            let weak = ui.as_weak();
            let loader = loader.clone();
            let series = series.clone();
            move |first, last| {
                let ui = weak.unwrap();
                for index in first.max(0) as usize..(last.max(0) as usize).min(series.len()) {
                    if ui
                        .get_items()
                        .row_data(index)
                        .is_some_and(|row| row.poster.size().width == 0)
                    {
                        loader.request(&series[index]);
                    }
                }
            }
        });
        ui.set_items(ModelRc::new(VecModel::from(
            series
                .iter()
                .map(|series| crate::ItemRow {
                    title: series.id.local_id.clone().into(),
                    artwork_key: key(series).into(),
                    ..crate::ItemRow::default()
                })
                .collect::<Vec<_>>(),
        )));
        ui.set_section(3);
        ui.set_source_catalog_open(true);
        ui.show().unwrap();
        ui.window().set_size(slint::LogicalSize::new(390., 844.));
        for _ in 0..3 {
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        }
        let grid_height =
            i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui, "SeriesGrid")
                .next()
                .unwrap()
                .size()
                .height;
        ui.set_catalogue_scroll_y(-grid_height * 0.85);
        for _ in 0..3 {
            i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
        }
        let _ =
            i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui, "LibraryCard")
                .map(|card| card.absolute_position())
                .collect::<Vec<_>>();
        assert_eq!(
            ui.get_items().row_data(55).unwrap().poster.size().width,
            460
        );
        let model = ui.get_items();
        let mut first = model.row_data(0).unwrap();
        first.poster = Image::default();
        model.set_row_data(0, first);
        ui.set_catalogue_scroll_y(0.);
        for _ in 0..3 {
            i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
        }
        let _ =
            i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui, "LibraryCard")
                .map(|card| card.absolute_position())
                .collect::<Vec<_>>();
        assert_eq!(
            ui.get_items().row_data(0).unwrap().poster.size().width,
            460,
            "scroll return must paint without advancing the mock clock"
        );
        assert!(
            receiver.try_recv().is_err(),
            "cached viewport changes must not fetch or decode again"
        );
    }

    #[test]
    fn cached_pixels_cannot_bypass_source_disable_remove_or_update() {
        i_slint_backend_testing::init_no_event_loop();
        for action in ["disable", "remove", "update"] {
            let (_directory, registry, access) = source_fixture();
            let shared = Shared::default();
            shared
                .decoded
                .lock()
                .unwrap()
                .insert("poster".into(), loaded(Pixels::new(1, 1), &access));
            assert!(cached_image(&shared, 0, "poster", "fixture").is_some());
            match action {
                "disable" => registry.set_enabled("fixture", false).unwrap(),
                "remove" => registry.remove("fixture").unwrap(),
                "update" => {
                    registry.install_zip(&package("1.0.1")).unwrap();
                    assert!(
                        registry.source("fixture").is_err(),
                        "expanded permissions require renewed source approval"
                    );
                }
                _ => unreachable!(),
            }
            assert!(!access.is_available());
            assert!(
                cached_image(&shared, 0, "poster", "fixture").is_none(),
                "{action} must revoke immediate cache hits"
            );
            UI_IMAGES.with(|images| {
                images.borrow_mut().remove(&0);
            });
        }
    }

    #[test]
    fn removed_cover_host_cannot_reauthorize_ram_disk_or_pending_publication() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let (directory, registry, _) = source_fixture();
        let source = registry.source("fixture").unwrap();
        let item = series("fixture", "one", "https://example.test/private.png");
        let key = key(&item);
        let proof = RequestProof::new(&source, item.cover_url.as_deref().unwrap()).unwrap();
        let encoded_key = proof.encoded_key(&key).unwrap();
        let cache =
            nova_cache::blob::Cache::new(directory.path().join("posters"), ENCODED_LIMIT as u64);
        let bytes = png(16, 24, [130, 130, 130, 255]);
        assert!(
            cache
                .put_if_current(&encoded_key, &bytes, cache.generation(), inspect, || true)
                .unwrap()
        );
        let shared = Shared::default();
        let make_loaded = || Loaded {
            pixels: Pixels::new(16, 24),
            source_id: "fixture".into(),
            access: source.availability(),
            proof: Some(proof.clone()),
        };
        shared
            .decoded
            .lock()
            .unwrap()
            .insert(key.clone(), make_loaded());
        assert!(cached_image(&shared, 0, &key, "fixture").is_some());
        registry
            .install_zip(&package_domains("1.0.1", vec!["replacement.test"]))
            .unwrap();
        registry.set_enabled("fixture", true).unwrap();
        let fresh_source = registry.source("fixture").unwrap();
        assert!(fresh_source.availability().is_available());
        let task = Task {
            key: key.clone(),
            source_id: "fixture".into(),
            url: item.cover_url.unwrap(),
            generation: 0,
            cancel: CancelToken::default(),
        };
        assert!(load_source(fresh_source, &cache, &shared, &task).is_err());
        assert!(cached_image(&shared, 0, &key, "fixture").is_none());
        ui.set_series_artwork_key(key.clone().into());
        admit(&ui, 0, &shared, &key, make_loaded());
        assert_eq!(ui.get_series_poster().size().width, 0);
        assert_eq!(
            cache.get(&encoded_key, inspect).unwrap().unwrap(),
            bytes,
            "retained data is not published under removed permissions"
        );
        UI_IMAGES.with(|images| {
            images.borrow_mut().remove(&0);
        });
    }

    #[test]
    fn artwork_scopes_partition_profiles_before_ram_or_encoded_reuse() {
        let (directory, registry, _) = source_fixture();
        let source = registry.source("fixture").unwrap();
        let other = registry.with_profile("other").source("fixture").unwrap();
        let url = "https://example.test/private.png";
        let first = RequestProof::new(&source, url).unwrap();
        let second = RequestProof::new(&other, url).unwrap();
        assert_ne!(first.encoded_key("poster"), second.encoded_key("poster"));
        let cache =
            nova_cache::blob::Cache::new(directory.path().join("posters"), ENCODED_LIMIT as u64);
        let bytes = png(16, 24, [130, 130, 130, 255]);
        assert!(
            cache
                .put_if_current(
                    &first.encoded_key("poster").unwrap(),
                    &bytes,
                    cache.generation(),
                    inspect,
                    || true
                )
                .unwrap()
        );
        assert!(
            cache
                .get(&second.encoded_key("poster").unwrap(), inspect)
                .unwrap()
                .is_none()
        );
        let shared = Shared::default();
        shared.decoded.lock().unwrap().insert(
            "poster".into(),
            Loaded {
                pixels: Pixels::new(16, 24),
                source_id: "fixture".into(),
                access: source.availability(),
                proof: Some(first),
            },
        );
        // Distinct current-profile disk pixels must win over an old-profile RAM hit.
        let other_bytes = png(16, 24, [210, 210, 210, 255]);
        assert!(
            cache
                .put_if_current(
                    &second.encoded_key("poster").unwrap(),
                    &other_bytes,
                    cache.generation(),
                    inspect,
                    || true
                )
                .unwrap()
        );
        let task = Task {
            key: "poster".into(),
            source_id: "fixture".into(),
            url: url.into(),
            generation: 0,
            cancel: CancelToken::default(),
        };
        let (current, deferred) = load_source(other, &cache, &shared, &task).unwrap();
        assert!(
            deferred.is_none(),
            "a disk hit must not schedule another write"
        );
        assert_eq!(&current.pixels.as_bytes()[..4], &[210, 210, 210, 255]);
    }

    #[test]
    fn legacy_originals_migrate_into_shared_bounded_cache_without_reencoding() {
        let directory = tempfile::tempdir().unwrap();
        let cache = nova_cache::blob::Cache::new(directory.path().into(), ENCODED_LIMIT as u64);
        let key = format!("{:064x}", 0);
        let bytes = png(16, 24, [130, 130, 130, 255]);
        let original = directory.path().join(format!("{key}.cover"));
        fs::write(&original, &bytes).unwrap();
        let interrupted = directory.path().join(format!("{:064x}.pending", 99));
        fs::write(&interrupted, b"partial").unwrap();
        let oversized = directory.path().join(format!("{:064x}.cover", 100));
        fs::File::create(&oversized)
            .unwrap()
            .set_len(ENCODED_LIMIT as u64 + 1)
            .unwrap();
        let outside = directory.path().join("chapter-page.bin");
        fs::write(&outside, b"downloaded chapter").unwrap();
        #[cfg(unix)]
        let legacy_link = {
            let path = directory.path().join(format!("{:064x}.cover", 101));
            std::os::unix::fs::symlink(&outside, &path).unwrap();
            path
        };
        migrate_legacy(&cache);
        assert_eq!(fs::read(&outside).unwrap(), b"downloaded chapter");
        #[cfg(unix)]
        assert!(!legacy_link.exists());
        assert!(!original.exists());
        assert!(!interrupted.exists());
        assert!(!oversized.exists());
        assert_eq!(cache.get(&key, inspect).unwrap().unwrap(), bytes);
        assert!(
            cache
                .put_if_current(
                    "huge",
                    &vec![0; ENCODED_LIMIT + 1],
                    cache.generation(),
                    inspect,
                    || true
                )
                .is_err()
        );
        assert_eq!(cache.usage().1, 1);
    }

    #[test]
    fn invalid_pixel_payload_is_removed_by_shared_cache_and_can_be_repaired() {
        let directory = tempfile::tempdir().unwrap();
        let cache = nova_cache::blob::Cache::new(directory.path().into(), ENCODED_LIMIT as u64);
        let valid = png(16, 24, [130, 130, 130, 255]);
        let mut corrupt = valid.clone();
        let idat = corrupt
            .windows(4)
            .position(|bytes| bytes == b"IDAT")
            .unwrap();
        // Break the compressed stream while preserving its dimension headers.
        corrupt[idat + 4] ^= 0xff;
        assert_eq!(inspect(&corrupt).unwrap(), (16, 24));
        assert!(decode(&corrupt).is_err());
        assert!(
            cache
                .put_if_current("poster", &corrupt, cache.generation(), inspect, || true)
                .unwrap()
        );
        assert!(read_cached_pixels(&cache, "poster").is_none());
        assert!(cache.get("poster", inspect).unwrap().is_none());
        assert!(
            cache
                .put_if_current("poster", &valid, cache.generation(), inspect, || true)
                .unwrap()
        );
        let pixels = read_cached_pixels(&cache, "poster").unwrap();
        assert_eq!((pixels.width(), pixels.height()), (16, 24));
    }

    fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        use image::ImageEncoder;
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                &color.repeat((width * height) as usize),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn clear_is_admitted_when_cover_queue_is_full_and_cancels_old_work() {
        let shared = Shared::default();
        let (sender, receiver) = mpsc::sync_channel(MAX_REQUESTS);
        for index in 0..MAX_REQUESTS {
            let cancel = CancelToken::default();
            let key = index.to_string();
            shared
                .requests
                .lock()
                .unwrap()
                .pending
                .insert(key.clone(), cancel.clone());
            assert!(
                sender
                    .try_send(Task {
                        key,
                        source_id: String::new(),
                        url: String::new(),
                        generation: 0,
                        cancel,
                    })
                    .is_ok()
            );
        }
        shared.invalidate();
        shared.queue_maintenance(true);
        shared.queue_maintenance(false);
        let command = shared.take_maintenance().unwrap();
        assert!(command.clear);
        assert!(shared.clearing.load(Ordering::Acquire));
        assert!(shared.take_maintenance().is_none());
        for _ in 0..MAX_REQUESTS {
            let task = receiver.try_recv().unwrap();
            assert!(task.cancel.is_cancelled());
            assert!(!shared.current(&task));
        }
    }

    #[test]
    fn clear_removes_only_kit_owned_posters_and_cpu_cache_and_rejects_stale_writer() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("cache/posters");
        let cache = nova_cache::blob::Cache::new(directory.clone(), ENCODED_LIMIT as u64);
        let chapters = root.path().join("chapters");
        fs::create_dir_all(&directory).unwrap();
        fs::create_dir_all(&chapters).unwrap();
        let chapter = chapters.join("page.bin");
        fs::write(&chapter, b"downloaded chapter").unwrap();
        let bytes = png(16, 24, [130, 130, 130, 255]);
        let epoch = cache.generation();
        assert!(
            cache
                .put_if_current("poster", &bytes, epoch, inspect, || true)
                .unwrap()
        );
        let legacy = directory.join(format!("{:064x}.cover", 1));
        let pending = directory.join(format!("{:064x}.pending", 2));
        fs::write(&legacy, b"legacy unscoped bytes").unwrap();
        fs::write(&pending, b"interrupted").unwrap();
        assert_eq!(
            cache_usage(&cache).1,
            3,
            "maintenance includes legacy files even when automatic migration is disabled"
        );
        let unrelated = directory.join("notes.txt");
        fs::write(&unrelated, b"keep").unwrap();
        let nested = directory.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("keep.txt"), b"keep nested files").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&chapter, directory.join("neighbor-link")).unwrap();
        let mut decoded = DecodedCache::default();
        decoded.insert("old".into(), loaded(Pixels::new(1, 1), &source_fixture().2));
        let decoded = Mutex::new(decoded);
        clear_cache(&cache, &decoded);
        assert_eq!(cache_usage(&cache), (0, 0));
        assert!(!legacy.exists());
        assert!(!pending.exists());
        assert_eq!(decoded.lock().unwrap().inner.stats(), (0, 0));
        assert!(
            !cache
                .put_if_current("late", &bytes, epoch, inspect, || true)
                .unwrap()
        );
        assert_eq!(fs::read(chapter).unwrap(), b"downloaded chapter");
        assert_eq!(fs::read(unrelated).unwrap(), b"keep");
        assert!(nested.join("keep.txt").exists());
        clear_cache(&cache, &decoded);
    }

    #[test]
    fn resized_artwork_lossless_policy_classifies_original_bytes() {
        use crate::image_storage::ImageCompression;
        let pixels: Vec<u8> = (0..64 * 96 * 3).map(|i| (i * 17) as u8).collect();
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80)
            .encode(&pixels, 64, 96, image::ExtendedColorType::Rgb8)
            .unwrap();
        let compression = ImageCompression {
            quality: 20,
            lossless_inputs: true,
            ..ImageCompression::default()
        };
        let low = transform_new_image(&jpeg, compression, Some(48))
            .unwrap()
            .unwrap();
        assert_eq!(
            low,
            transform_new_image(
                &jpeg,
                ImageCompression {
                    lossless_inputs: false,
                    ..compression
                },
                Some(48)
            )
            .unwrap()
            .unwrap()
        );
        assert_ne!(
            low,
            transform_new_image(
                &jpeg,
                ImageCompression {
                    quality: 95,
                    ..compression
                },
                Some(48)
            )
            .unwrap()
            .unwrap()
        );
        let source = png(64, 96, [137, 28, 201, 255]);
        let low = transform_new_image(&source, compression, Some(48))
            .unwrap()
            .unwrap();
        assert_eq!(
            low,
            transform_new_image(
                &source,
                ImageCompression {
                    quality: 95,
                    ..compression
                },
                Some(48)
            )
            .unwrap()
            .unwrap()
        );
        let expected = nova_cache::fit_rgba(
            reader_engine::decode_image(&source, COVER_LIMITS).unwrap(),
            Some(48),
        )
        .unwrap();
        assert_eq!(
            reader_engine::decode_image(&low, COVER_LIMITS).unwrap(),
            expected
        );
    }

    #[test]
    fn fetched_cache_compression_changes_new_files_and_preserves_existing_png() {
        use crate::image_storage::{ImageCompression, ImageStorageFormat};
        let png = png(64, 96, [130, 130, 130, 255]);
        let directory = tempfile::tempdir().unwrap();
        let cache = nova_cache::blob::Cache::new(directory.path().into(), ENCODED_LIMIT as u64);
        let epoch = cache.generation();
        assert!(
            cache
                .put_if_current("old", &png, epoch, inspect, || true)
                .unwrap()
        );
        for (index, format) in [
            ImageStorageFormat::Jpeg,
            ImageStorageFormat::Webp,
            ImageStorageFormat::Avif,
        ]
        .into_iter()
        .enumerate()
        {
            let compression = ImageCompression {
                enabled: true,
                format,
                quality: 85,
                jxl_lossless_jpeg: false,
                lossless_inputs: false,
                ..ImageCompression::default()
            };
            let key = format!("new-{index}");
            write_fetched(&cache, &key, epoch, &png, compression, None, || true);
            let bytes = cache.get(&key, inspect).unwrap().unwrap();
            match format {
                ImageStorageFormat::Jpeg => assert!(bytes.starts_with(&[0xff, 0xd8, 0xff])),
                ImageStorageFormat::Webp => {
                    assert_eq!(&bytes[..4], b"RIFF");
                    assert_eq!(&bytes[8..12], b"WEBP");
                }
                ImageStorageFormat::JxlLossless | ImageStorageFormat::JxlLossy => {
                    assert!(nova_image::is_jpeg_xl(&bytes))
                }
                ImageStorageFormat::Avif => {
                    assert_eq!(&bytes[4..8], b"ftyp");
                    assert!(bytes.windows(4).any(|brand| brand == b"avif"));
                }
            }
            assert_eq!(inspect(&bytes).unwrap(), (64, 96));
            let rendered = decode(&bytes).unwrap();
            assert_eq!((rendered.width(), rendered.height()), (64, 96));
            write_fetched(
                &cache,
                "old",
                epoch,
                &png,
                ImageCompression {
                    lossless_inputs: true,
                    ..compression
                },
                None,
                || true,
            );
            assert_eq!(cache.get("old", inspect).unwrap().unwrap(), png);
        }
        write_fetched(
            &cache,
            "disabled",
            epoch,
            &png,
            ImageCompression {
                enabled: false,
                ..ImageCompression::default()
            },
            None,
            || true,
        );
        assert_eq!(cache.get("disabled", inspect).unwrap().unwrap(), png);
        let cancel = CancelToken::default();
        cancel.cancel();
        write_fetched(
            &cache,
            "cancelled",
            epoch,
            &png,
            ImageCompression::default(),
            None,
            || !cancel.is_cancelled(),
        );
        assert!(cache.get("cancelled", inspect).unwrap().is_none());
        write_fetched(
            &cache,
            "resized",
            epoch,
            &png,
            ImageCompression {
                enabled: false,
                ..ImageCompression::default()
            },
            Some(48),
            || true,
        );
        assert_eq!(
            inspect(&cache.get("resized", inspect).unwrap().unwrap()).unwrap(),
            (32, 48)
        );
        // Applying a new resolution preference never rewrites an existing key.
        write_fetched(
            &cache,
            "old",
            epoch,
            &png,
            ImageCompression::default(),
            Some(48),
            || true,
        );
        assert_eq!(cache.get("old", inspect).unwrap().unwrap(), png);
        write_fetched(
            &cache,
            "revoked",
            epoch,
            &png,
            ImageCompression::default(),
            None,
            || false,
        );
        assert!(cache.get("revoked", inspect).unwrap().is_none());
        let transparent = self::png(16, 16, [0, 0, 0, 0]);
        write_fetched(
            &cache,
            "transparent",
            epoch,
            &transparent,
            ImageCompression {
                enabled: true,
                format: ImageStorageFormat::Jpeg,
                quality: 85,
                jxl_lossless_jpeg: false,
                lossless_inputs: false,
                ..ImageCompression::default()
            },
            None,
            || true,
        );
        assert_eq!(
            cache.get("transparent", inspect).unwrap().unwrap(),
            transparent
        );
    }

    #[test]
    fn artwork_decode_bounds_display_and_rejects_invalid_data() {
        use image::ImageEncoder;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(
                &vec![40; 1000 * 1500 * 4],
                1000,
                1500,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        let pixels = decode(&png).unwrap();
        assert_eq!((pixels.width(), pixels.height()), (460, 690));
        assert_eq!(pixels.as_bytes().len(), 460 * 690 * 4);
        assert!(decode(b"not an image").is_err());
        assert!(decode(&vec![0; ENCODED_LIMIT + 1]).is_err());
        let mut too_wide = Vec::new();
        image::codecs::png::PngEncoder::new(&mut too_wide)
            .write_image(&vec![0; 8193 * 4], 8193, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        assert!(decode(&too_wide).is_err());
    }
    // Authored source package: exercises the real JS host without depending on
    // any website implementation, captured response, or network service.
    fn poster_package() -> Vec<u8> {
        use std::io::{Cursor, Write};
        let manifest = serde_json::json!({
            "formatVersion": 2, "contract": "nova-reader1", "api": "^1.0",
            "id": "poster-fixture", "name": "Neutral poster fixture", "version": "1.0.0",
            "language": "en", "entry": "main.js",
            "permissions": {"domains": ["example.test"]},
            "capabilities": ["browse", "details"]
        });
        let script = br#"
            globalThis.novaProvider = {handle(request) {
                const operation = request.op || request.operation;
                const path = operation === 'browse' ? '/catalogue.json' : '/details.json';
                const response = JSON.parse(nova.http.get('https://example.test' + path,
                    JSON.stringify({Origin: 'https://example.test'})));
                return JSON.parse(response.body);
            }};
        "#;
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("main.js", script.to_vec()),
        ] {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(&bytes).unwrap();
        }
        archive.finish().unwrap().into_inner()
    }

    fn poster_source(
        directory: &Path,
        transport: Arc<dyn reader_sources::SourceTransport>,
    ) -> (reader_sources::SourceRegistry, reader_sources::Source) {
        let registry = reader_sources::SourceRegistry::open(directory).unwrap();
        registry.install_zip(&poster_package()).unwrap();
        registry.set_enabled("poster-fixture", true).unwrap();
        let source = registry
            .source("poster-fixture")
            .unwrap()
            .with_transport(transport);
        (registry, source)
    }

    /// Exercise Source DTOs, queue admission, request authorization, native
    /// decoding, retained permits and the actual Slint row publication together.
    fn check_poster_pipeline(source: reader_sources::Source, directory: &Path, mode: &str) {
        use slint::{ModelRc, VecModel};
        let ui = AppWindow::new().unwrap();
        let catalogue = source
            .browse(&serde_json::json!({"catalogue":mode}))
            .unwrap();
        let entry = catalogue.items.first().expect("neutral catalogue is empty");
        for label in ["catalogue", "details"] {
            // The first pass must be untouched Browse: do not fetch any post
            // details before its poster request.
            let dto = if label == "catalogue" {
                entry.clone()
            } else {
                source.details(&serde_json::json!({"id":entry.id})).unwrap()
            };
            let item = series(
                "poster-fixture",
                &dto.id,
                dto.cover_url.as_deref().expect("poster URL missing"),
            );
            let shared = Arc::new(Shared::default());
            let (sender, receiver) = mpsc::sync_channel(MAX_REQUESTS);
            let loader = ArtworkLoader {
                sender,
                shared: shared.clone(),
                ui: ui.as_weak(),
                id: NEXT_LOADER.fetch_add(1, Ordering::Relaxed),
                encoded: nova_cache::blob::Cache::new(directory.join(label), ENCODED_LIMIT as u64),
            };
            ui.set_items(ModelRc::new(VecModel::from(vec![crate::ItemRow {
                artwork_key: key(&item).into(),
                ..crate::ItemRow::default()
            }])));
            assert_eq!(ui.get_items().row_data(0).unwrap().poster.size().width, 0);
            loader.request(&item);
            let task = receiver
                .try_recv()
                .expect("visible poster was not scheduled");
            let (loaded, deferred) = load_source(source.clone(), &loader.encoded, &shared, &task)
                .unwrap_or_else(|error| panic!("{label} poster load failed: {error}"));
            assert!(
                loaded.is_current(),
                "poster authorization was revoked before publication"
            );
            shared.finish(&task, false);
            admit(&ui, loader.id, &shared, &task.key, loaded);
            let image = ui.get_items().row_data(0).unwrap().poster;
            let (acknowledge, receipt) = mpsc::sync_channel(1);
            acknowledge.send(()).unwrap();
            after_publication(receipt, &shared, &task, || {
                if let Some(deferred) = deferred {
                    deferred.write(&loader.encoded, &shared, &task);
                }
            });
            assert!(
                image.size().width > 0 && image.size().height > 0,
                "{label} decoded poster never reached its Slint row"
            );
            assert!(image.size().width <= 460 && image.size().height <= 690);
            assert!(loader.cached_image(&item).is_some());
            assert!(shared.requests.lock().unwrap().failed.is_empty());
            eprintln!(
                "{mode} {label} poster decoded and published: {}x{}",
                image.size().width,
                image.size().height
            );
        }
    }

    struct NeutralPosters;
    impl reader_sources::SourceTransport for NeutralPosters {
        fn page_cache_partition(&self, _: &str) -> Option<String> {
            Some("neutral-posters".into())
        }
        fn request(
            &self,
            request: &reader_sources::HttpRequest,
            cap: usize,
            cancel: &CancelToken,
        ) -> reader_sources::Result<reader_sources::HttpResponse> {
            cancel.check()?;
            let url = url::Url::parse(&request.url).unwrap();
            assert_eq!(url.host_str(), Some("example.test"));
            let body = if url.path().ends_with(".png") {
                // Artwork supplies no addon-defined Page headers. Prove that
                // the same path works for catalogue and fresh details posters.
                assert!(request.headers.is_empty());
                png(120, 180, [20, 100, 160, 255])
            } else {
                assert_eq!(
                    request.headers.get("Origin").map(String::as_str),
                    Some("https://example.test")
                );
                let mut entry = serde_json::json!({
                    "id": "shapes", "title": "Neutral shapes",
                    "url": "https://example.test/series/shapes",
                    "coverUrl": "https://example.test/images/catalogue.png"
                });
                let response = match url.path() {
                    "/catalogue.json" => {
                        serde_json::json!({"items": [entry], "hasNextPage": false})
                    }
                    "/details.json" => {
                        entry["coverUrl"] =
                            serde_json::json!("https://example.test/images/details.png");
                        entry
                    }
                    path => panic!("unexpected neutral fixture request: {path}"),
                };
                serde_json::to_vec(&response).unwrap()
            };
            assert!(body.len() <= cap);
            Ok(reader_sources::HttpResponse {
                status: 200,
                url: request.url.clone(),
                headers: BTreeMap::new(),
                body,
            })
        }
    }

    #[test]
    fn catalogue_and_details_posters_reach_the_ui_through_native_pipeline() {
        i_slint_backend_testing::init_no_event_loop();
        let directory = tempfile::tempdir().unwrap();
        let (_registry, source) =
            poster_source(&directory.path().join("sources"), Arc::new(NeutralPosters));
        check_poster_pipeline(source, &directory.path().join("posters"), "popular");
    }

    #[test]
    fn decoded_poster_is_visible_while_optional_cache_work_is_gated() {
        use slint::{ModelRc, VecModel};
        i_slint_backend_testing::init_no_event_loop();
        let directory = tempfile::tempdir().unwrap();
        let (_registry, source) =
            poster_source(&directory.path().join("sources"), Arc::new(NeutralPosters));
        let dto = source
            .browse(&serde_json::json!({}))
            .unwrap()
            .items
            .remove(0);
        let item = series("poster-fixture", &dto.id, dto.cover_url.as_deref().unwrap());
        let shared = Arc::new(Shared::default());
        let task = Task {
            key: key(&item),
            source_id: "poster-fixture".into(),
            url: item.cover_url.unwrap(),
            generation: 0,
            cancel: CancelToken::default(),
        };
        let cache =
            nova_cache::blob::Cache::new(directory.path().join("posters"), ENCODED_LIMIT as u64);
        let (loaded, deferred) = load_source(source, &cache, &shared, &task).unwrap();
        let deferred = deferred.expect("a new reusable poster must defer its optional cache write");
        assert!(deferred.bytes.len() <= ENCODED_LIMIT);
        let encoded_key = deferred.key.clone();
        assert!(
            cache.get(&encoded_key, inspect).unwrap().is_none(),
            "loading pixels must not recompress or publish the disk variant"
        );
        let ui = AppWindow::new().unwrap();
        let id = NEXT_LOADER.fetch_add(1, Ordering::Relaxed);
        ui.set_items(ModelRc::new(VecModel::from(vec![crate::ItemRow {
            artwork_key: task.key.clone().into(),
            ..crate::ItemRow::default()
        }])));
        let (acknowledge, receipt) = mpsc::sync_channel(1);
        let (cache_entered, entered) = mpsc::sync_channel(1);
        let (cache_release, release) = mpsc::sync_channel(1);
        let worker_shared = shared.clone();
        let worker_cache = cache.clone();
        let worker_task = task.clone();
        let worker = std::thread::spawn(move || {
            after_publication(receipt, &worker_shared, &worker_task, || {
                cache_entered.send(()).unwrap();
                release.recv_timeout(Duration::from_secs(5)).unwrap();
                deferred.write(&worker_cache, &worker_shared, &worker_task);
            });
        });
        admit(&ui, id, &shared, &task.key, loaded);
        drop(Completion(acknowledge));
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(ui.get_items().row_data(0).unwrap().poster.size().width, 120);
        assert!(
            cache.get(&encoded_key, inspect).unwrap().is_none(),
            "poster must already be visible while cache work is blocked"
        );
        cache_release.send(()).unwrap();
        worker.join().unwrap();
        assert!(cache.get(&encoded_key, inspect).unwrap().is_some());
        UI_IMAGES.with(|images| {
            images.borrow_mut().remove(&id);
        });
    }

    #[test]
    fn deferred_poster_cache_writes_keep_cancellation_source_and_epoch_guards() {
        struct MutableScope(AtomicU64);
        impl reader_sources::SourceTransport for MutableScope {
            fn page_cache_partition(&self, _: &str) -> Option<String> {
                Some(self.0.load(Ordering::Acquire).to_string())
            }
            fn request(
                &self,
                request: &reader_sources::HttpRequest,
                cap: usize,
                cancel: &CancelToken,
            ) -> reader_sources::Result<reader_sources::HttpResponse> {
                reader_sources::SourceTransport::request(&NeutralPosters, request, cap, cancel)
            }
        }
        for action in [
            "cancel",
            "generation",
            "source",
            "scope",
            "epoch",
            "disconnected",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let transport = Arc::new(MutableScope(AtomicU64::new(0)));
            let (registry, source) =
                poster_source(&directory.path().join("sources"), transport.clone());
            let shared = Shared::default();
            let cache = nova_cache::blob::Cache::new(
                directory.path().join("posters"),
                ENCODED_LIMIT as u64,
            );
            let task = Task {
                key: "poster".into(),
                source_id: "poster-fixture".into(),
                url: "https://example.test/images/cover.png".into(),
                generation: 0,
                cancel: CancelToken::default(),
            };
            let (_loaded, deferred) = load_source(source, &cache, &shared, &task).unwrap();
            let deferred = deferred.unwrap();
            let encoded_key = deferred.key.clone();
            match action {
                "cancel" => task.cancel.cancel(),
                "generation" => shared.invalidate(),
                "source" => registry.disable("poster-fixture").unwrap(),
                "scope" => {
                    transport.0.fetch_add(1, Ordering::AcqRel);
                }
                "epoch" => {
                    cache.clear();
                }
                "disconnected" => {}
                _ => unreachable!(),
            }
            let (acknowledge, receipt) = mpsc::sync_channel(1);
            if action != "disconnected" {
                acknowledge.send(()).unwrap();
            }
            drop(acknowledge);
            after_publication(receipt, &shared, &task, || {
                deferred.write(&cache, &shared, &task)
            });
            assert!(
                cache.get(&encoded_key, inspect).unwrap().is_none(),
                "{action} must reject the deferred optional write"
            );
        }
    }
}
