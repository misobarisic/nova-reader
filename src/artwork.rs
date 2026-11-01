//! Bounded cover loading. Only the event-loop publication creates Slint images.
use crate::{AppWindow, services::Services};
use nova_reader_ui::backend_text as text;
use reader_domain::Series;
use reader_engine::{DecodeLimits, decode_display_image};
use reader_sources::{CancelToken, SourceAvailability};
use sha2::{Digest, Sha256};
use slint::{Image, Model, Rgba8Pixel, SharedPixelBuffer};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
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
const ENCODED_LIMIT: usize = 8 * 1024 * 1024;
// Nova's decoded LRU default and retained-card allowance (media/src/cache.rs).
const DECODED_CACHE_LIMIT: usize = 128 * 1024 * 1024;
const UNLOAD_EXTRA_CARDS: usize = 48;
const DISK_CACHE_LIMIT: u64 = 32 * 1024 * 1024;
const DISK_CACHE_FILES: usize = 256;
const COVER_LIMITS: DecodeLimits = DecodeLimits {
    max_encoded_bytes: ENCODED_LIMIT,
    max_width: 8192,
    max_height: 8192,
    max_pixels: 8_000_000,
    max_decoded_bytes: 72 * 1024 * 1024,
};

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

struct Task {
    key: String,
    source_id: String,
    url: String,
    generation: u64,
    cancel: CancelToken,
}

/// A single worker owns all filesystem, network and image decoding work.
pub struct ArtworkLoader {
    sender: SyncSender<Task>,
    shared: Arc<Shared>,
    ui: slint::Weak<AppWindow>,
    id: u64,
}

impl ArtworkLoader {
    pub fn new(services: Services, ui: slint::Weak<AppWindow>, cache_dir: PathBuf) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Task>(MAX_REQUESTS);
        let shared = Arc::new(Shared::default());
        let id = NEXT_LOADER.fetch_add(1, Ordering::Relaxed);
        let worker_shared = shared.clone();
        let worker_ui = ui.clone();
        // Failure to start this optional worker leaves the UI's title fallback intact.
        let started = std::thread::Builder::new()
            .name("reader-artwork".into())
            .spawn(move || {
                let mut maintenance_status = None::<String>;
                prune_disk(&cache_dir);
                loop {
                    if let Some(command) = worker_shared.take_maintenance() {
                        if command.clear {
                            maintenance_status =
                                Some(match clear_cache(&cache_dir, &worker_shared.decoded) {
                                    Ok(()) => text::tr("Image cache cleared."),
                                    Err(error) => format!(
                                        "{}: {error}",
                                        text::tr("Could not clear image cache")
                                    ),
                                });
                            // Old queued tasks are discarded by the generation guard.
                            // Do not unblock requests if a newer clear is pending.
                            let pending = worker_shared
                                .maintenance
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            if !pending.clear {
                                worker_shared.clearing.store(false, Ordering::Release);
                            }
                        }
                        let usage = disk_usage(&cache_dir);
                        let shared = worker_shared.clone();
                        let status = maintenance_status.clone();
                        let _ = worker_ui.upgrade_in_event_loop(move |ui| {
                            if shared
                                .maintenance
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .revision
                                != command.revision
                            {
                                return;
                            }
                            match usage {
                                Ok((bytes, files)) => {
                                    ui.set_image_cache_info(
                                        text::image_cache_usage(bytes, files).into(),
                                    );
                                    if let Some(status) = status {
                                        ui.set_image_cache_status(status.into());
                                    }
                                }
                                Err(error) => ui.set_image_cache_status(
                                    format!(
                                        "{}: {error}",
                                        text::tr("Could not inspect image cache.")
                                    )
                                    .into(),
                                ),
                            }
                            ui.set_image_cache_busy(false);
                        });
                        continue;
                    }
                    let task = match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(task) => task,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    if !worker_shared.current(&task) {
                        continue;
                    }
                    let result = load(&services, &cache_dir, &worker_shared, &task);
                    if !worker_shared.current(&task) {
                        continue;
                    }
                    let shared = worker_shared.clone();
                    let (acknowledge, receipt) = mpsc::sync_channel(1);
                    // Keep pending entries until publication so queued event-loop
                    // closures count toward the same 32-request memory bound.
                    let _ = worker_ui.upgrade_in_event_loop(move |ui| {
                        // One decoded completion at a time keeps transient pixel
                        // ownership bounded independently of the request queue.
                        let _receipt = Completion(acknowledge);
                        if !shared.current(&task) {
                            return;
                        }
                        shared.finish(&task, result.is_err());
                        let Ok(loaded) = result else { return };
                        admit(&ui, id, &shared, &task.key, loaded);
                    });
                    // Do not pile up decoded closures while the UI is busy.
                    loop {
                        match receipt.recv_timeout(Duration::from_millis(100)) {
                            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                        }
                        if worker_shared.shutdown.load(Ordering::Acquire) {
                            break;
                        }
                    }
                }
            })
            .is_ok();
        shared.worker_available.store(started, Ordering::Release);
        Self {
            sender,
            shared,
            ui,
            id,
        }
    }

    /// Cancel before replacing rows, changing profiles, or changing source access.
    pub fn invalidate(&self) {
        self.shared.invalidate();
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
        cached_image(&self.shared, self.id, &key(series), &series.id.source_id)
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
            .protected
            .insert(key.clone());
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

struct Completion(SyncSender<()>);
impl Drop for Completion {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
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
    if !loaded.access.is_available() {
        return;
    }
    let source_id = loaded.source_id.clone();
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
    }
    trim_ui(ui, id, shared);
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
    if ui.get_series_artwork_key().as_str() == key {
        ui.set_series_poster(image);
    }
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
    if keys.contains(ui.get_series_artwork_key().as_str()) {
        ui.set_series_poster(Image::default());
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
    if !keep.contains(ui.get_series_artwork_key().as_str()) {
        remove.insert(ui.get_series_artwork_key().to_string());
    }
    forget_images(ui, id, &remove);
}

fn decode(bytes: &[u8]) -> Result<Pixels, String> {
    let image = decode_display_image(bytes, COVER_LIMITS, 460, 690).map_err(|e| e.to_string())?;
    Ok(Pixels::clone_from_slice(
        image.as_raw(),
        image.width(),
        image.height(),
    ))
}

struct Loaded {
    pixels: Pixels,
    source_id: String,
    access: SourceAvailability,
}

struct DecodedCache {
    entries: VecDeque<(String, Loaded)>,
    bytes: usize,
    max_bytes: usize,
    protected: BTreeSet<String>,
}

impl Default for DecodedCache {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            max_bytes: DECODED_CACHE_LIMIT,
            protected: BTreeSet::new(),
        }
    }
}

impl DecodedCache {
    fn get(&mut self, key: &str, source_id: &str) -> Option<Pixels> {
        let index = self.entries.iter().position(|(entry, _)| entry == key)?;
        let entry = &self.entries[index].1;
        if entry.source_id != source_id || !entry.access.is_available() {
            return None;
        }
        let entry = self.entries.remove(index)?;
        let pixels = entry.1.pixels.clone();
        self.entries.push_back(entry);
        Some(pixels)
    }

    fn insert(&mut self, key: String, loaded: Loaded) -> BTreeSet<String> {
        let mut evicted = BTreeSet::new();
        if loaded.pixels.as_bytes().len() > self.max_bytes {
            return evicted;
        }
        if let Some(index) = self.entries.iter().position(|(entry, _)| entry == &key)
            && let Some((_, old)) = self.entries.remove(index)
        {
            self.bytes -= old.pixels.as_bytes().len();
        }
        while self.bytes + loaded.pixels.as_bytes().len() > self.max_bytes {
            let index = self
                .entries
                .iter()
                .position(|(key, _)| !self.protected.contains(key))
                .unwrap_or(0);
            let Some((key, old)) = self.entries.remove(index) else {
                break;
            };
            self.bytes -= old.pixels.as_bytes().len();
            evicted.insert(key);
        }
        self.bytes += loaded.pixels.as_bytes().len();
        self.entries.push_back((key, loaded));
        evicted
    }

    fn keys(&self) -> BTreeSet<String> {
        self.entries.iter().map(|(key, _)| key.clone()).collect()
    }

    fn retain_visible(&mut self, desired: &BTreeSet<String>) {
        self.protected = desired.clone();
        for key in desired {
            if let Some(index) = self.entries.iter().position(|(entry, _)| entry == key)
                && let Some(entry) = self.entries.remove(index)
            {
                self.entries.push_back(entry);
            }
        }
    }

    fn ui_keys(&self, selected: &str) -> BTreeSet<String> {
        let mut keep = self
            .entries
            .iter()
            .filter(|(key, entry)| {
                entry.access.is_available() && (self.protected.contains(key) || key == selected)
            })
            .map(|(key, _)| key.clone())
            .collect::<BTreeSet<_>>();
        let extra = self
            .entries
            .iter()
            .rev()
            .filter(|(key, entry)| entry.access.is_available() && !keep.contains(key))
            .take(UNLOAD_EXTRA_CARDS)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        keep.extend(extra);
        keep
    }
}

fn load(
    services: &Services,
    cache_dir: &Path,
    shared: &Shared,
    task: &Task,
) -> Result<Loaded, String> {
    task.cancel.check().map_err(|e| e.to_string())?;
    // Cached artwork must still respect source availability/adult/profile gates.
    let source = services.source(&task.source_id)?;
    let access = source.availability();
    {
        let mut cache = shared.decoded.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, entry)) = cache
            .entries
            .iter_mut()
            .find(|(key, entry)| key == &task.key && entry.source_id == task.source_id)
        {
            entry.access = access.clone();
        }
        if let Some(pixels) = cache.get(&task.key, &task.source_id) {
            return Ok(Loaded {
                pixels,
                source_id: task.source_id.clone(),
                access,
            });
        }
    }
    let path = cache_dir.join(format!("{}.cover", task.key));
    if let Some(bytes) = read_cached(&path) {
        task.cancel.check().map_err(|e| e.to_string())?;
        if let Ok(pixels) = decode(&bytes) {
            task.cancel.check().map_err(|e| e.to_string())?;
            return Ok(Loaded {
                pixels,
                source_id: task.source_id.clone(),
                access,
            });
        }
        let _ = fs::remove_file(&path);
    }
    let bytes = source
        .fetch_page(&task.url, &BTreeMap::new(), &task.cancel)
        .map_err(|e| e.to_string())?;
    task.cancel.check().map_err(|e| e.to_string())?;
    let pixels = decode(&bytes)?;
    task.cancel.check().map_err(|e| e.to_string())?;
    write_fetched(
        cache_dir,
        &path,
        &bytes,
        &task.cancel,
        crate::image_storage::load().cache,
    );
    Ok(Loaded {
        pixels,
        source_id: task.source_id.clone(),
        access,
    })
}

/// Compression applies only when a newly fetched image is first persisted.
/// A changed preference never rewrites either cache hits or an existing file.
fn write_fetched(
    cache_dir: &Path,
    path: &Path,
    bytes: &[u8],
    cancel: &CancelToken,
    compression: crate::image_storage::ImageCompression,
) {
    if cancel.is_cancelled() || path.exists() || bytes.len() > ENCODED_LIMIT {
        return;
    }
    let converted = compression.enabled.then(|| {
        crate::reader_export::reencode_image(
            bytes,
            compression.format,
            compression.quality,
            COVER_LIMITS,
        )
    });
    // A valid poster remains available when a particular encoding cannot be
    // produced within its bounds. Conversion never relaxes the source limits.
    let stored = match converted {
        Some(Ok(ref encoded)) if encoded.len() <= ENCODED_LIMIT => encoded.as_slice(),
        _ => bytes,
    };
    if cancel.is_cancelled() || fs::create_dir_all(cache_dir).is_err() {
        return;
    }
    let temporary = path.with_extension("pending");
    if !cancel.is_cancelled()
        && fs::write(&temporary, stored).is_ok()
        && !cancel.is_cancelled()
        && !path.exists()
    {
        let _ = fs::rename(&temporary, path);
    }
    let _ = fs::remove_file(temporary);
    prune_disk(cache_dir);
}

fn read_cached(path: &Path) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > ENCODED_LIMIT as u64 {
        let _ = fs::remove_file(path);
        return None;
    }
    let mut bytes = Vec::new();
    file.take(ENCODED_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= ENCODED_LIMIT).then_some(bytes)
}

fn cache_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext == "cover" || ext == "pending")
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| {
                stem.len() == 64 && stem.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
}

fn disk_usage(directory: &Path) -> std::io::Result<(u64, usize)> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(error) => return Err(error),
    };
    let mut bytes = 0u64;
    let mut files = 0;
    for entry in entries {
        let entry = entry?;
        if cache_file(&entry.path()) {
            let metadata = entry.metadata()?;
            if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
                files += 1;
            }
        }
    }
    Ok((bytes, files))
}

fn clear_cache(directory: &Path, decoded: &Mutex<DecodedCache>) -> std::io::Result<()> {
    {
        let mut decoded = decoded.lock().unwrap_or_else(|e| e.into_inner());
        decoded.entries.clear();
        decoded.bytes = 0;
        decoded.protected.clear();
    }
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut failure = None;
    for entry in entries {
        let result = (|| {
            let entry = entry?;
            if cache_file(&entry.path()) {
                let kind = entry.file_type()?;
                if kind.is_file() || kind.is_symlink() {
                    fs::remove_file(entry.path())?;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            failure.get_or_insert(error);
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn prune_disk(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut files = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if !cache_file(&path) {
                return None;
            }
            // A process interrupted before rename must not leave uncounted
            // encoded files behind on every subsequent launch.
            if path.extension().is_some_and(|ext| ext == "pending") {
                let _ = fs::remove_file(path);
                return None;
            }
            let metadata = entry.metadata().ok()?;
            metadata
                .is_file()
                .then(|| (metadata.modified().ok(), metadata.len(), path))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(modified, _, _)| *modified);
    let mut bytes: u64 = files.iter().map(|(_, size, _)| size).sum();
    let mut count = files.len();
    for (_, size, path) in files {
        if bytes <= DISK_CACHE_LIMIT && count <= DISK_CACHE_FILES {
            break;
        }
        if fs::remove_file(path).is_ok() {
            bytes = bytes.saturating_sub(size);
            count -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reader_domain::SeriesId;
    use slint::ComponentHandle;

    fn series(source: &str, id: &str, url: &str) -> Series {
        Series {
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
        use std::io::{Cursor, Write};
        let domains = if version == "1.0.0" {
            vec!["example.test"]
        } else {
            vec!["example.test", "other.test"]
        };
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
        }
    }

    #[test]
    fn decoded_lru_evicts_oldest_and_preserves_visible_pixels_within_one_budget() {
        let (_directory, _registry, access) = source_fixture();
        let mut cache = DecodedCache {
            max_bytes: 2 * 32 * 32 * 4,
            ..DecodedCache::default()
        };
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
        assert_eq!(cache.bytes, cache.max_bytes);
    }

    #[test]
    fn budget_eviction_releases_model_and_ui_handle_references_together() {
        use slint::{ModelRc, VecModel};
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let (_directory, _registry, access) = source_fixture();
        let shared = Shared::default();
        shared.decoded.lock().unwrap().max_bytes = 2 * 32 * 32 * 4;
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
        assert_eq!(cache.bytes, cache.max_bytes);
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
        assert_eq!(shared.decoded.lock().unwrap().bytes, 60 * 460 * 690 * 4);
        assert!(shared.decoded.lock().unwrap().bytes <= DECODED_CACHE_LIMIT);
        drop(original);
        shared.worker_available.store(true, Ordering::Release);
        loader.clear();
        assert!(shared.take_maintenance().unwrap().clear);
        clear_cache(&_directory.path().join("posters"), &shared.decoded).unwrap();
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
    fn disk_cache_prunes_to_budget_and_rejects_large_files() {
        let directory = tempfile::tempdir().unwrap();
        let interrupted = directory.path().join(format!("{:064x}.pending", 99));
        fs::write(&interrupted, b"interrupted cache write").unwrap();
        for index in 0..5 {
            let path = directory.path().join(format!("{index:064x}.cover"));
            fs::File::create(path)
                .unwrap()
                .set_len(ENCODED_LIMIT as u64)
                .unwrap();
        }
        prune_disk(directory.path());
        assert!(!interrupted.exists());
        let bytes: u64 = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert!(bytes <= DISK_CACHE_LIMIT);
        let large = directory.path().join("large.cover");
        fs::File::create(&large)
            .unwrap()
            .set_len(ENCODED_LIMIT as u64 + 1)
            .unwrap();
        assert!(read_cached(&large).is_none());
        assert!(!large.exists());
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
    fn clear_removes_only_owned_poster_files_and_cpu_cache() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("cache/posters");
        let chapters = root.path().join("chapters");
        fs::create_dir_all(&directory).unwrap();
        fs::create_dir_all(&chapters).unwrap();
        let chapter = chapters.join("page.bin");
        fs::write(&chapter, b"downloaded chapter").unwrap();
        fs::write(directory.join(format!("{:064x}.cover", 0)), b"data").unwrap();
        fs::write(directory.join(format!("{:064x}.pending", 1)), b"pending").unwrap();
        let unrelated = directory.join("notes.txt");
        fs::write(&unrelated, b"keep").unwrap();
        let nested = directory.join(format!("{:064x}.cover", 2));
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("keep.txt"), b"keep nested files").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&chapter, directory.join(format!("{:064x}.cover", 3))).unwrap();
        assert_eq!(disk_usage(&directory).unwrap(), (11, 2));
        let mut decoded = DecodedCache::default();
        decoded.insert("old".into(), loaded(Pixels::new(1, 1), &source_fixture().2));
        let decoded = Mutex::new(decoded);
        clear_cache(&directory, &decoded).unwrap();
        assert_eq!(disk_usage(&directory).unwrap(), (0, 0));
        assert_eq!(decoded.lock().unwrap().bytes, 0);
        assert!(decoded.lock().unwrap().entries.is_empty());
        assert_eq!(fs::read(chapter).unwrap(), b"downloaded chapter");
        assert_eq!(fs::read(unrelated).unwrap(), b"keep");
        assert!(nested.join("keep.txt").exists());
        clear_cache(&directory, &decoded).unwrap();
        clear_cache(&root.path().join("missing"), &decoded).unwrap();
    }

    #[test]
    fn fetched_cache_compression_changes_new_files_and_preserves_existing_png() {
        use crate::image_storage::{ImageCompression, ImageStorageFormat};
        use image::ImageEncoder;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(
                &[130, 130, 130, 255].repeat(64 * 96),
                64,
                96,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join(format!("{:064x}.cover", 0));
        fs::write(&old, &png).unwrap();
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
            };
            let path = directory.path().join(format!("{:064x}.cover", index + 1));
            write_fetched(
                directory.path(),
                &path,
                &png,
                &CancelToken::default(),
                compression,
            );
            let bytes = fs::read(&path).unwrap();
            match format {
                ImageStorageFormat::Jpeg => assert!(bytes.starts_with(&[0xff, 0xd8, 0xff])),
                ImageStorageFormat::Webp => {
                    assert_eq!(&bytes[..4], b"RIFF");
                    assert_eq!(&bytes[8..12], b"WEBP");
                }
                ImageStorageFormat::Avif => {
                    assert_eq!(&bytes[4..8], b"ftyp");
                    assert!(bytes.windows(4).any(|brand| brand == b"avif"));
                }
            }
            let dimensions = reader_engine::inspect_image(&bytes, COVER_LIMITS).unwrap();
            assert_eq!((dimensions.width, dimensions.height), (64, 96));
            let rendered = decode(&bytes).unwrap();
            assert_eq!((rendered.width(), rendered.height()), (64, 96));
            write_fetched(
                directory.path(),
                &old,
                &png,
                &CancelToken::default(),
                compression,
            );
            assert_eq!(fs::read(&old).unwrap(), png);
        }
        let disabled = directory.path().join(format!("{:064x}.cover", 4));
        write_fetched(
            directory.path(),
            &disabled,
            &png,
            &CancelToken::default(),
            ImageCompression {
                enabled: false,
                ..ImageCompression::default()
            },
        );
        assert_eq!(fs::read(disabled).unwrap(), png);
        let cancelled = directory.path().join(format!("{:064x}.cover", 5));
        let cancel = CancelToken::default();
        cancel.cancel();
        write_fetched(
            directory.path(),
            &cancelled,
            &png,
            &cancel,
            ImageCompression::default(),
        );
        assert!(!cancelled.exists());
        assert!(!cancelled.with_extension("pending").exists());
        let mut transparent_png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut transparent_png)
            .write_image(
                &[0, 0, 0, 0].repeat(16 * 16),
                16,
                16,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        let transparent = directory.path().join(format!("{:064x}.cover", 6));
        write_fetched(
            directory.path(),
            &transparent,
            &transparent_png,
            &CancelToken::default(),
            ImageCompression {
                enabled: true,
                format: ImageStorageFormat::Jpeg,
                quality: 85,
            },
        );
        assert_eq!(
            fs::read(transparent).unwrap(),
            transparent_png,
            "unsupported transparent JPEG conversion keeps original artwork"
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
}
