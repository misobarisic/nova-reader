//! Bounded cover loading. Only the event-loop publication creates Slint images.
use crate::{AppWindow, services::Services};
use nova_reader_ui::backend_text as text;
use reader_domain::Series;
use reader_engine::{DecodeLimits, decode_display_image};
use reader_sources::CancelToken;
use sha2::{Digest, Sha256};
use slint::{Image, Model, Rgba8Pixel, SharedPixelBuffer};
use std::{
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
const DECODED_CACHE_LIMIT: usize = 12 * 1024 * 1024;
const UI_IMAGE_LIMIT: u64 = 32 * 1024 * 1024;
const DISK_CACHE_LIMIT: u64 = 32 * 1024 * 1024;
const DISK_CACHE_FILES: usize = 256;
const COVER_LIMITS: DecodeLimits = DecodeLimits {
    max_encoded_bytes: ENCODED_LIMIT,
    max_width: 8192,
    max_height: 8192,
    max_pixels: 8_000_000,
    max_decoded_bytes: 72 * 1024 * 1024,
};

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
}

impl ArtworkLoader {
    pub fn new(services: Services, ui: slint::Weak<AppWindow>, cache_dir: PathBuf) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Task>(MAX_REQUESTS);
        let shared = Arc::new(Shared::default());
        let worker_shared = shared.clone();
        let worker_ui = ui.clone();
        // Failure to start this optional worker leaves the UI's title fallback intact.
        let started = std::thread::Builder::new()
            .name("reader-artwork".into())
            .spawn(move || {
                let mut decoded = DecodedCache::default();
                let mut maintenance_status = None::<String>;
                prune_disk(&cache_dir);
                loop {
                    if let Some(command) = worker_shared.take_maintenance() {
                        if command.clear {
                            maintenance_status =
                                Some(match clear_cache(&cache_dir, &mut decoded) {
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
                    let result = load(&services, &cache_dir, &mut decoded, &task);
                    if !worker_shared.current(&task) {
                        continue;
                    }
                    let shared = worker_shared.clone();
                    // Keep pending entries until publication so queued event-loop
                    // closures count toward the same 32-request memory bound.
                    let _ = worker_ui.upgrade_in_event_loop(move |ui| {
                        if !shared.current(&task) {
                            return;
                        }
                        shared.finish(&task, result.is_err());
                        let Ok(pixels) = result else { return };
                        let image =
                            Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                                &pixels.bytes,
                                pixels.width,
                                pixels.height,
                            ));
                        let model = ui.get_items();
                        let mut target = 0;
                        for index in 0..model.row_count() {
                            if let Some(mut row) = model.row_data(index)
                                && row.artwork_key.as_str() == task.key
                            {
                                target = index;
                                row.poster = image.clone();
                                model.set_row_data(index, row);
                            }
                        }
                        if ui.get_series_artwork_key().as_str() == task.key {
                            ui.set_series_poster(image);
                        }
                        trim_ui(&ui, &task.key, target);
                    });
                }
            })
            .is_ok();
        shared.worker_available.store(started, Ordering::Release);
        Self { sender, shared, ui }
    }

    /// Cancel before replacing rows, changing profiles, or changing source access.
    pub fn invalidate(&self) {
        self.shared.invalidate();
    }

    /// Cancel obsolete queued, active, and not-yet-published requests while
    /// keeping the generation of still-visible artwork intact.
    pub fn retain(&self, desired: &BTreeSet<String>) {
        self.shared.retain(desired);
    }

    /// Read actual poster cache usage on the worker. Repeated requests coalesce.
    pub fn refresh_usage(&self) {
        self.maintenance(false);
    }

    /// Cancel all older artwork work before clearing only owned poster files.
    pub fn clear(&self) {
        self.invalidate();
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
        self.invalidate();
    }
}

fn image_bytes(image: &Image) -> u64 {
    let size = image.size();
    u64::from(size.width) * u64::from(size.height) * 4
}

/// Model images are shared by key; evict complete groups so duplicate history or
/// update rows cannot keep an otherwise evicted texture alive.
fn trim_ui(ui: &AppWindow, incoming: &str, target: usize) {
    let selected = ui.get_series_artwork_key();
    let model = ui.get_items();
    let mut groups = BTreeMap::<String, (u64, Vec<usize>)>::new();
    for index in 0..model.row_count() {
        if let Some(row) = model.row_data(index) {
            let bytes = image_bytes(&row.poster);
            if bytes == 0 {
                continue;
            }
            let group = groups.entry(row.artwork_key.to_string()).or_default();
            group.0 = group.0.max(bytes);
            group.1.push(index);
        }
    }
    // Counting the selected image separately is conservative when it shares the
    // same image as a row, and covers the detail-only presentation too.
    let mut bytes = image_bytes(&ui.get_series_poster())
        + groups.values().map(|(bytes, _)| *bytes).sum::<u64>();
    let mut victims = groups
        .into_iter()
        .filter(|(key, _)| key != incoming && key != selected.as_str())
        .collect::<Vec<_>>();
    victims.sort_by_key(|(_, (_, indices))| {
        std::cmp::Reverse(
            indices
                .iter()
                .map(|index| index.abs_diff(target))
                .min()
                .unwrap_or(0),
        )
    });
    for (_, (size, indices)) in victims {
        if bytes <= UI_IMAGE_LIMIT {
            break;
        }
        for index in indices {
            if let Some(mut row) = model.row_data(index) {
                row.poster = Image::default();
                model.set_row_data(index, row);
            }
        }
        bytes = bytes.saturating_sub(size);
    }
}

struct Pixels {
    width: u32,
    height: u32,
    bytes: Vec<u8>,
}

fn decode(bytes: &[u8]) -> Result<Arc<Pixels>, String> {
    let image = decode_display_image(bytes, COVER_LIMITS, 460, 690).map_err(|e| e.to_string())?;
    Ok(Arc::new(Pixels {
        width: image.width(),
        height: image.height(),
        bytes: image.into_raw(),
    }))
}

#[derive(Default)]
struct DecodedCache {
    entries: VecDeque<(String, Arc<Pixels>)>,
    bytes: usize,
}

impl DecodedCache {
    fn get(&mut self, key: &str) -> Option<Arc<Pixels>> {
        let index = self.entries.iter().position(|(entry, _)| entry == key)?;
        let entry = self.entries.remove(index)?;
        let pixels = entry.1.clone();
        self.entries.push_back(entry);
        Some(pixels)
    }

    fn insert(&mut self, key: String, pixels: Arc<Pixels>) {
        if pixels.bytes.len() > DECODED_CACHE_LIMIT {
            return;
        }
        if let Some(index) = self.entries.iter().position(|(entry, _)| entry == &key)
            && let Some((_, old)) = self.entries.remove(index)
        {
            self.bytes -= old.bytes.len();
        }
        while self.bytes + pixels.bytes.len() > DECODED_CACHE_LIMIT {
            let Some((_, old)) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old.bytes.len();
        }
        self.bytes += pixels.bytes.len();
        self.entries.push_back((key, pixels));
    }
}

fn load(
    services: &Services,
    cache_dir: &Path,
    decoded: &mut DecodedCache,
    task: &Task,
) -> Result<Arc<Pixels>, String> {
    task.cancel.check().map_err(|e| e.to_string())?;
    // Cached artwork must still respect source availability/adult/profile gates.
    let source = services.source(&task.source_id)?;
    if let Some(pixels) = decoded.get(&task.key) {
        return Ok(pixels);
    }
    let path = cache_dir.join(format!("{}.cover", task.key));
    if let Some(bytes) = read_cached(&path) {
        task.cancel.check().map_err(|e| e.to_string())?;
        if let Ok(pixels) = decode(&bytes) {
            task.cancel.check().map_err(|e| e.to_string())?;
            decoded.insert(task.key.clone(), pixels.clone());
            return Ok(pixels);
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
    decoded.insert(task.key.clone(), pixels.clone());
    Ok(pixels)
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

fn clear_cache(directory: &Path, decoded: &mut DecodedCache) -> std::io::Result<()> {
    decoded.entries.clear();
    decoded.bytes = 0;
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

    #[test]
    fn decoded_lru_evicts_oldest_and_bounds_bytes() {
        let pixels = || {
            Arc::new(Pixels {
                width: 1,
                height: 1,
                bytes: vec![0; DECODED_CACHE_LIMIT / 2],
            })
        };
        let mut cache = DecodedCache::default();
        cache.insert("first".into(), pixels());
        cache.insert("second".into(), pixels());
        assert!(cache.get("first").is_some());
        cache.insert("third".into(), pixels());
        assert!(cache.get("second").is_none());
        assert!(cache.get("first").is_some());
        assert!(cache.get("third").is_some());
        assert_eq!(cache.bytes, DECODED_CACHE_LIMIT);
    }

    #[test]
    fn ui_retention_evicts_distant_artwork_and_preserves_selected_and_incoming() {
        use slint::{ModelRc, VecModel};
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let mut rows = (0..32)
            .map(|index| crate::ItemRow {
                artwork_key: index.to_string().into(),
                poster: Image::from_rgba8(SharedPixelBuffer::new(460, 690)),
                ..crate::ItemRow::default()
            })
            .collect::<Vec<_>>();
        ui.set_series_artwork_key("0".into());
        ui.set_series_poster(rows[0].poster.clone());
        // A duplicate near the incoming row keeps that shared image nearby.
        rows.push(rows[1].clone());
        ui.set_items(ModelRc::new(VecModel::from(rows)));
        trim_ui(&ui, "31", 31);
        let model = ui.get_items();
        assert!(image_bytes(&model.row_data(0).unwrap().poster) > 0);
        assert!(image_bytes(&model.row_data(31).unwrap().poster) > 0);
        assert!(image_bytes(&model.row_data(1).unwrap().poster) > 0);
        assert_eq!(image_bytes(&model.row_data(2).unwrap().poster), 0);
        let mut retained = BTreeMap::new();
        for row in model.iter() {
            retained.insert(row.artwork_key.to_string(), image_bytes(&row.poster));
        }
        assert!(
            retained.values().sum::<u64>() + image_bytes(&ui.get_series_poster()) <= UI_IMAGE_LIMIT
        );
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
        decoded.insert(
            "old".into(),
            Arc::new(Pixels {
                width: 1,
                height: 1,
                bytes: vec![0; 4],
            }),
        );
        clear_cache(&directory, &mut decoded).unwrap();
        assert_eq!(disk_usage(&directory).unwrap(), (0, 0));
        assert_eq!(decoded.bytes, 0);
        assert!(decoded.entries.is_empty());
        assert_eq!(fs::read(chapter).unwrap(), b"downloaded chapter");
        assert_eq!(fs::read(unrelated).unwrap(), b"keep");
        assert!(nested.join("keep.txt").exists());
        clear_cache(&directory, &mut decoded).unwrap();
        clear_cache(&root.path().join("missing"), &mut decoded).unwrap();
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
        for (index, format) in [ImageStorageFormat::Jpeg, ImageStorageFormat::Webp]
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
            }
            let dimensions = reader_engine::inspect_image(&bytes, COVER_LIMITS).unwrap();
            assert_eq!((dimensions.width, dimensions.height), (64, 96));
            write_fetched(
                directory.path(),
                &old,
                &png,
                &CancelToken::default(),
                compression,
            );
            assert_eq!(fs::read(&old).unwrap(), png);
        }
        let disabled = directory.path().join(format!("{:064x}.cover", 3));
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
        let cancelled = directory.path().join(format!("{:064x}.cover", 4));
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
        let transparent = directory.path().join(format!("{:064x}.cover", 5));
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
        assert_eq!((pixels.width, pixels.height), (460, 690));
        assert_eq!(pixels.bytes.len(), 460 * 690 * 4);
        assert!(decode(b"not an image").is_err());
        assert!(decode(&vec![0; ENCODED_LIMIT + 1]).is_err());
        let mut too_wide = Vec::new();
        image::codecs::png::PngEncoder::new(&mut too_wide)
            .write_image(&vec![0; 8193 * 4], 8193, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        assert!(decode(&too_wide).is_err());
    }
}
