//! On-demand, disposable page derivatives. No inventory fetch or progress writes.
use crate::{
    chapter_cache::ChapterCache,
    decode_admission::{Priority, SHARED},
    page_inventory::{Resource, ResourceKind},
    page_store::PageImage,
};
use reader_engine::PageDimensions;
use reader_sources::{CancelToken, PageChunkControl};
use sha2::{Digest, Sha256};
use slint::{Rgba8Pixel, SharedPixelBuffer};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
    sync::{Arc, Condvar, Mutex},
};

const PIXEL_BUDGET: usize = 8 * 1024 * 1024; // Other 8MiB reserves renderer's previous frame.
const WIDTH: u32 = 160;
const HEIGHT: u32 = 240;
const GALLERY_SIDE: u32 = 320;
const PREFIX_MAGIC: &[u8; 8] = b"NRTHJXL1";
type Current = Arc<dyn Fn() -> bool + Send + Sync>;
type Pixels = SharedPixelBuffer<Rgba8Pixel>;
type Publish = Arc<dyn Fn(Completion) + Send + Sync>;
type Result<T> = std::result::Result<T, String>;

#[derive(Default)]
struct Budget(Mutex<usize>, Condvar);
struct Reservation {
    budget: Arc<Budget>,
    bytes: usize,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        *self.budget.0.lock().unwrap_or_else(|e| e.into_inner()) -= self.bytes;
        self.budget.1.notify_all();
    }
}
#[derive(Clone)]
pub struct Thumbnail {
    pub pixels: Pixels,
    _reservation: Arc<Reservation>,
}
impl Thumbnail {
    fn new(image: image::RgbaImage, budget: &Arc<Budget>) -> Result<Self> {
        let bytes = image.as_raw().len();
        let mut used = budget.0.lock().unwrap_or_else(|e| e.into_inner());
        if used.saturating_add(bytes) > PIXEL_BUDGET {
            return Err("Thumbnail memory budget reached".into());
        }
        *used += bytes;
        drop(used);
        Ok(Self {
            pixels: Pixels::clone_from_slice(image.as_raw(), image.width(), image.height()),
            _reservation: Arc::new(Reservation {
                budget: budget.clone(),
                bytes,
            }),
        })
    }
}
pub struct Completion {
    pub generation: u64,
    pub index: usize,
    pub limits_revision: u64,
    pub dimensions: Option<PageDimensions>,
    pub top_preview: bool,
    pub result: Result<Thumbnail>,
    current: Current,
}
impl Completion {
    pub fn is_current(&self) -> bool {
        (self.current)() && reader_engine::limits_revision() == self.limits_revision
    }
}
#[derive(Clone)]
struct Ready {
    thumbnail: Thumbnail,
    dimensions: PageDimensions,
    top_preview: bool,
    current: Current,
    reusable: Current,
    revision: u64,
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum RenderingMode {
    #[default]
    Page,
    Gallery,
}
impl RenderingMode {
    fn width(self) -> u32 {
        if self == Self::Gallery {
            GALLERY_SIDE
        } else {
            WIDTH
        }
    }
    fn top_preview(self, dimensions: PageDimensions) -> bool {
        self == Self::Page && dimensions.height >= dimensions.width.saturating_mul(4)
    }
    fn cache_identity(self, mut identity: Vec<u8>) -> Vec<u8> {
        // Preserve existing manga derivatives. Photo squares have an independent
        // recipe, so they can never reuse a page contain/top-window derivative.
        if self == Self::Gallery {
            identity.extend_from_slice(b"\0gallery-square-center-320-v1");
        }
        identity
    }
}
struct State {
    mode: RenderingMode,
    generation: u64,
    epoch: u64,
    inventory: Arc<Vec<Resource>>,
    desired: BTreeSet<usize>,
    visible: BTreeSet<usize>,
    completed: BTreeSet<usize>,
    failed: BTreeSet<usize>,
    ready: BTreeMap<usize, Ready>,
    recycled: BTreeMap<usize, Ready>,
    paused: BTreeMap<String, String>,
    active: Option<(usize, CancelToken)>,
    stopped: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            mode: RenderingMode::Page,
            generation: 0,
            epoch: 0,
            inventory: Arc::new(Vec::new()),
            desired: BTreeSet::new(),
            visible: BTreeSet::new(),
            completed: BTreeSet::new(),
            failed: BTreeSet::new(),
            ready: BTreeMap::new(),
            recycled: BTreeMap::new(),
            paused: BTreeMap::new(),
            active: None,
            stopped: false,
        }
    }
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    budget: Arc<Budget>,
    cache: Arc<ChapterCache>,
    publish: Publish,
}
pub struct PageThumbnails {
    shared: Arc<Shared>,
}
impl PageThumbnails {
    pub fn new(cache: Arc<ChapterCache>, publish: Publish) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            budget: Arc::new(Budget::default()),
            cache,
            publish,
        });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("reader-page-thumbnails".into())
            .spawn(move || run(worker))
            .expect("thumbnail worker");
        Self { shared }
    }
    pub fn replace_inventory(&self, generation: u64, inventory: Arc<Vec<Resource>>) {
        self.replace_inventory_with_mode(generation, inventory, RenderingMode::Page);
    }
    /// Photo grids use square center crops; page overview keeps whole-page and
    /// explicitly labelled tall-page top previews.
    pub fn replace_inventory_for_gallery(&self, generation: u64, inventory: Arc<Vec<Resource>>) {
        self.replace_inventory_with_mode(generation, inventory, RenderingMode::Gallery);
    }
    fn replace_inventory_with_mode(
        &self,
        generation: u64,
        inventory: Arc<Vec<Resource>>,
        mode: RenderingMode,
    ) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, token)) = state.active.take() {
            token.cancel();
        }
        let epoch = state.epoch.wrapping_add(1);
        *state = State {
            mode,
            generation,
            epoch,
            inventory,
            ..Default::default()
        };
        self.shared.changed.notify_all();
    }
    /// The event loop first clears its old Image handles and Thumbnail leases.
    /// Keep decoded pixels as bounded recycle inputs, revoke old completions,
    /// and wait for the new viewport before selecting replacement resolutions.
    /// Disk-disabled galleries can reflow without refetching existing tiles.
    pub fn prepare_gallery_reflow(&self, generation: u64) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation {
            return;
        }
        state.epoch = state.epoch.wrapping_add(1);
        if let Some((_, token)) = state.active.take() {
            token.cancel();
        }
        let ready = std::mem::take(&mut state.ready);
        state.recycled.extend(ready);
        state.completed.clear();
        state.desired.clear();
        state.visible.clear();
        self.shared.changed.notify_all();
    }
    /// Visible tile range is half-open. Admit exactly its rows plus one adjacent
    /// row on either side; inventory size never expands this work window.
    pub fn viewport(&self, generation: u64, first: usize, last: usize, columns: usize) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation {
            return;
        }
        let columns = columns.clamp(1, 32);
        let count = state.inventory.len();
        let first = first.min(count);
        let last = last.min(count).max(first);
        let (desired, visible) = admitted(count, first, last, columns);
        if let Some((index, token)) = &state.active
            && !desired.contains(index)
        {
            token.cancel();
        }
        state.desired = desired;
        state.visible = visible;
        let desired = state.desired.clone();
        state.ready.retain(|index, entry| {
            desired.contains(index)
                && (entry.reusable)()
                && entry.revision == reader_engine::limits_revision()
        });
        let ready = state.ready.keys().copied().collect::<BTreeSet<_>>();
        state.completed.retain(|index| ready.contains(index));
        state.recycled.retain(|index, entry| {
            desired.contains(index)
                && (entry.reusable)()
                && entry.revision == reader_engine::limits_revision()
        });
        state.failed.retain(|index| desired.contains(index));
        let hits = state
            .ready
            .iter()
            .map(|(index, entry)| (*index, entry.clone()))
            .collect::<Vec<_>>();
        let epoch = state.epoch;
        drop(state);
        for (index, entry) in hits {
            let current = publication_guard(
                &self.shared,
                generation,
                epoch,
                index,
                entry.current.clone(),
            );
            (self.shared.publish)(Completion {
                generation,
                index,
                limits_revision: entry.revision,
                dimensions: Some(entry.dimensions),
                top_preview: entry.top_preview,
                result: Ok(entry.thumbnail),
                current,
            });
        }
        self.shared.changed.notify_all();
    }
    pub fn cancel(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.epoch = state.epoch.wrapping_add(1);
        if let Some((_, token)) = state.active.take() {
            token.cancel();
        }
        state.desired.clear();
        state.visible.clear();
        state.ready.clear();
        state.recycled.clear();
        state.completed.clear();
        state.failed.clear();
        self.shared.changed.notify_all();
    }
    pub fn invalidate(&self) {
        self.cancel();
    }
    pub fn retry(&self, generation: u64) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation {
            return;
        }
        state.paused.clear();
        state.failed.clear();
        self.shared.changed.notify_all();
    }
    pub fn paused(&self) -> Option<String> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .paused
            .values()
            .next()
            .cloned()
    }
    pub fn usage(&self) -> (u64, usize) {
        self.shared.cache.thumbnail_usage()
    }
    pub fn retained_bytes(&self) -> usize {
        *self
            .shared
            .budget
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
}
impl Drop for PageThumbnails {
    fn drop(&mut self) {
        self.cancel();
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopped = true;
        self.shared.changed.notify_all();
    }
}
fn admitted(
    count: usize,
    first: usize,
    last: usize,
    columns: usize,
) -> (BTreeSet<usize>, BTreeSet<usize>) {
    if first >= last {
        return (BTreeSet::new(), BTreeSet::new());
    }
    let row_first = first / columns * columns;
    let row_end = last.div_ceil(columns).saturating_mul(columns).min(count);
    (
        (row_first.saturating_sub(columns)..row_end.saturating_add(columns).min(count)).collect(),
        (first..last).collect(),
    )
}
fn publication_guard(
    shared: &Arc<Shared>,
    generation: u64,
    epoch: u64,
    index: usize,
    proof: Current,
) -> Current {
    let weak = Arc::downgrade(shared);
    Arc::new(move || {
        let valid = weak.upgrade().is_some_and(|shared| {
            let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.generation == generation
                && state.epoch == epoch
                && state.desired.contains(&index)
                && !state.stopped
        });
        valid && proof()
    })
}
fn source_id(resource: &Resource) -> Option<String> {
    match &resource.kind {
        ResourceKind::Remote { source, .. } => Some(source.descriptor().id),
        _ => None,
    }
}
fn run(shared: Arc<Shared>) {
    loop {
        let (generation, epoch, index, resource, token, priority, mode, recycled) = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if state.stopped {
                    return;
                }
                let next = state
                    .recycled
                    .keys()
                    .copied()
                    .find(|index| state.desired.contains(index))
                    .or_else(|| {
                        state
                            .visible
                            .iter()
                            .chain(state.desired.iter())
                            .copied()
                            .find(|index| {
                                !state.completed.contains(index)
                                    && !state.failed.contains(index)
                                    && source_id(&state.inventory[*index])
                                        .is_none_or(|source| !state.paused.contains_key(&source))
                            })
                    });
                if let Some(index) = next {
                    let token = CancelToken::default();
                    state.active = Some((index, token.clone()));
                    let recycled = state.recycled.remove(&index);
                    break (
                        state.generation,
                        state.epoch,
                        index,
                        state.inventory[index].clone(),
                        token,
                        if state.visible.contains(&index) {
                            Priority::VisibleThumbnail
                        } else {
                            Priority::Prefetch
                        },
                        state.mode,
                        recycled,
                    );
                }
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner());
            }
        };
        let token_current = token.clone();
        let current = publication_guard(
            &shared,
            generation,
            epoch,
            index,
            Arc::new(move || !token_current.is_cancelled()),
        );
        let revision = reader_engine::limits_revision();
        let result = reader_engine::with_current_limits(|| {
            if let Some(recycled) = recycled {
                recycle_ready(&shared, recycled, &current)
            } else {
                load(&shared, &resource, &token, current.clone(), priority, mode)
            }
        });
        if !current() || revision != reader_engine::limits_revision() {
            continue;
        }
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation != generation || state.epoch != epoch || !state.desired.contains(&index)
        {
            continue;
        }
        state.active = None;
        let completion = match result {
            Ok(loaded) => {
                let Ready {
                    thumbnail,
                    dimensions,
                    top_preview,
                    current: proof,
                    reusable,
                    ..
                } = loaded;
                let current = publication_guard(&shared, generation, epoch, index, proof.clone());
                state.completed.insert(index);
                state.ready.insert(
                    index,
                    Ready {
                        thumbnail: thumbnail.clone(),
                        dimensions,
                        top_preview,
                        current: proof,
                        reusable,
                        revision,
                    },
                );
                Completion {
                    generation,
                    index,
                    limits_revision: revision,
                    dimensions: Some(dimensions),
                    top_preview,
                    result: Ok(thumbnail),
                    current,
                }
            }
            Err(error) => {
                if matches!(
                    error.as_str(),
                    "interactive challenge required"
                        | "authentication required"
                        | "source rate limited the request"
                        | "source is disabled or unavailable"
                ) && let Some(source) = source_id(&resource)
                {
                    state.paused.insert(source, error.clone());
                }
                state.failed.insert(index);
                Completion {
                    generation,
                    index,
                    limits_revision: revision,
                    dimensions: None,
                    top_preview: false,
                    result: Err(error),
                    current,
                }
            }
        };
        drop(state);
        (shared.publish)(completion);
    }
}
fn recycle_ready(shared: &Arc<Shared>, old: Ready, current: &Current) -> Result<Ready> {
    if !current() || !(old.reusable)() || old.revision != reader_engine::current_limits_revision() {
        return Err("operation cancelled".into());
    }
    let count = shared
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .desired
        .len()
        .max(1);
    let allowance = PIXEL_BUDGET / count;
    if old.thumbnail.pixels.as_bytes().len() <= allowance {
        return Ok(old);
    }
    let pixels = &old.thumbnail.pixels;
    let (width, height) = budget_dimensions(pixels.width(), pixels.height(), allowance)?;
    let source = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(
        pixels.width(),
        pixels.height(),
        pixels.as_bytes(),
    )
    .ok_or_else(|| "Invalid thumbnail pixels".to_string())?;
    // Borrow the existing backing; only one small replacement is materialized.
    // Root has dropped UI ownership. Renderer references use the other8MiB.
    let replacement = image::imageops::resize(
        &source,
        width,
        height,
        image::imageops::FilterType::Triangle,
    );
    let Ready {
        thumbnail,
        dimensions,
        top_preview,
        current: proof,
        reusable,
        revision,
    } = old;
    drop(thumbnail); // Free the old reservation before admitting its replacement.
    let bytes = replacement.as_raw().len();
    loop {
        if !current() || !reusable() {
            return Err("operation cancelled".into());
        }
        let used = shared.budget.0.lock().unwrap_or_else(|e| e.into_inner());
        if used.saturating_add(bytes) <= PIXEL_BUDGET {
            break;
        }
        // Queued old-generation UI closures can briefly own their old lease.
        // Let the event loop discard those closures rather than failing tiles.
        drop(
            shared
                .budget
                .1
                .wait_timeout(used, std::time::Duration::from_millis(25))
                .unwrap_or_else(|e| e.into_inner()),
        );
    }
    Ok(Ready {
        thumbnail: Thumbnail::new(replacement, &shared.budget)?,
        dimensions,
        top_preview,
        current: proof,
        reusable,
        revision,
    })
}
#[cfg(test)]
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn local_stamp(path: &std::path::Path) -> Result<(u64, std::time::SystemTime)> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Saved page is unavailable or damaged".into());
    }
    Ok((
        metadata.len(),
        metadata.modified().map_err(|e| e.to_string())?,
    ))
}
/// Prefixes are disposable previews only. Full-resolution availability remains
/// owned by Resource::read, which independently verifies all original bytes.
fn read_local_preview(
    reader: &mut dyn Read,
    expected_bytes: u64,
    expected_sha256: &str,
    preview: &mut Preview,
    current: &Current,
) -> Result<Vec<u8>> {
    let cap = reader_engine::current_limits().max_encoded_bytes;
    if expected_bytes > cap as u64 {
        return Err("Image exceeds byte limit".into());
    }
    let mut bytes = Vec::new();
    let mut hash = Sha256::new();
    let mut chunk = [0; 16 * 1024];
    loop {
        if !current() {
            return Err("operation cancelled".into());
        }
        // Identify the codec before doing substantial filesystem IO or native
        // decode admission. The same buffer then streams bounded source chunks.
        let take = if bytes.is_empty() { 12 } else { chunk.len() };
        let count = reader.read(&mut chunk[..take]).map_err(|e| e.to_string())?;
        if !current() {
            return Err("operation cancelled".into());
        }
        if count == 0 {
            break;
        }
        if count > cap.saturating_sub(bytes.len()) {
            return Err("Image exceeds byte limit".into());
        }
        hash.update(&chunk[..count]);
        bytes.extend_from_slice(&chunk[..count]);
        match preview.feed(&chunk[..count]) {
            Ok(control) if control.is_break() => {
                if (bytes.len() as u64) < expected_bytes {
                    return Ok(bytes);
                }
                break;
            }
            Ok(_) => {}
            Err(_) if current() => preview.disable(),
            Err(error) => return Err(error),
        }
    }
    if bytes.len() as u64 != expected_bytes || format!("{:x}", hash.finalize()) != expected_sha256 {
        return Err("Saved page is unavailable or damaged".into());
    }
    Ok(bytes)
}
fn load(
    shared: &Arc<Shared>,
    resource: &Resource,
    token: &CancelToken,
    current: Current,
    priority: Priority,
    mode: RenderingMode,
) -> Result<Ready> {
    let cache_revision = shared.cache.revision();
    let cache = shared.cache.clone();
    let cache_current: Current = Arc::new(move || cache.revision() == cache_revision);
    let (identity, proof, reusable): (Vec<u8>, Current, Current) = match &resource.kind {
        ResourceKind::Unavailable { message } => return Err(message.clone()),
        ResourceKind::Local {
            path,
            expected_bytes,
            sha256,
        } => {
            let stamp = local_stamp(path)?;
            if stamp.0 != *expected_bytes {
                return Err("Saved page is unavailable or damaged".into());
            }
            let path_proof = path.clone();
            let proof: Current =
                Arc::new(move || local_stamp(&path_proof).is_ok_and(|now| now == stamp));
            (
                serde_json::to_vec(&(path, &resource.id, expected_bytes, sha256, stamp.1))
                    .map_err(|e| e.to_string())?,
                proof.clone(),
                proof,
            )
        }
        ResourceKind::Remote { source, page } => {
            let scope = source
                .page_cache_scope(&page.url, &page.headers)
                .map_err(|e| e.to_string())?;
            let access = source.page_access();
            let url = page.url.clone();
            let headers = page.headers.clone();
            let proof: Current =
                Arc::new(move || access.validate_page_request(&url, &headers).is_ok());
            let access = source.page_access();
            let url = page.url.clone();
            let headers = page.headers.clone();
            let before = scope.clone();
            let reusable: Current = Arc::new(move || {
                before.is_some()
                    && access
                        .page_cache_scope(&url, &headers)
                        .is_ok_and(|after| after == before)
            });
            (
                serde_json::to_vec(&(
                    &resource.chapter,
                    &resource.id,
                    &page.url,
                    &page.headers,
                    scope,
                ))
                .map_err(|e| e.to_string())?,
                proof,
                reusable,
            )
        }
    };
    let key = ChapterCache::thumbnail_key(&resource.chapter, &mode.cache_identity(identity));
    let active_current = current.clone();
    let active_proof = proof.clone();
    let active_cache = cache_current.clone();
    let active: Current = Arc::new(move || active_current() && active_proof() && active_cache());
    let mut stored = if reusable() {
        shared.cache.thumbnail_read(&key, inspect_derivative)?
    } else {
        None
    };
    let mut reused_resource = stored.is_some();
    let mut decoded = if let Some(bytes) = stored.as_ref() {
        Some(decode_derivative(bytes, priority, active.clone(), mode)?)
    } else {
        None
    };
    if decoded.is_none() {
        let mut preview = Preview::new(priority, active.clone(), mode);
        let page = match &resource.kind {
            ResourceKind::Unavailable { message } => return Err(message.clone()),
            ResourceKind::Local {
                path,
                expected_bytes,
                sha256,
            } => {
                // Recheck both the path and opened file before reading. A
                // cached derivative cannot make a missing/symlinked file valid.
                let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
                let metadata = file.metadata().map_err(|e| e.to_string())?;
                if !metadata.is_file() || metadata.len() != *expected_bytes || !active() {
                    return Err("Saved page is unavailable or damaged".into());
                }
                // A preview prefix is a truncated JPEG XL stream; a complete
                // file is whatever layout the download store wrote.
                PageImage::from_stored(read_local_preview(
                    &mut file,
                    *expected_bytes,
                    sha256,
                    &mut preview,
                    &active,
                )?)?
            }
            ResourceKind::Remote { source, page } => {
                let request = shared.cache.prepare(
                    &resource.chapter,
                    page,
                    source,
                    token.clone(),
                    active.clone(),
                )?;
                if let Some(read) = shared.cache.read_cached(&request)? {
                    reused_resource = true;
                    for chunk in read.page.stored_bytes().chunks(16 * 1024) {
                        if preview.feed(chunk)?.is_break() {
                            break;
                        }
                    }
                    read.page
                } else {
                    let transfer = source
                        .fetch_page_until(
                            &page.url,
                            &page.headers,
                            token,
                            reader_engine::current_limits().max_encoded_bytes,
                            &mut |chunk| {
                                if !active() {
                                    return Err(reader_sources::SourceError::Cancelled);
                                }
                                preview
                                    .feed(chunk)
                                    .map_err(reader_sources::SourceError::Network)
                            },
                        )
                        .map_err(|e| e.to_string())?;
                    if !transfer.complete && preview.image.is_none() {
                        return Err("Incomplete thumbnail response".into());
                    }
                    PageImage::from_encoded(transfer.bytes)
                }
            }
        };
        if let Some((image, dimensions, consumed)) = preview.take() {
            let top = mode.top_preview(dimensions);
            let image = if mode == RenderingMode::Gallery {
                center_square(image)?
            } else {
                fit_preview(image, top)
            };
            let mut payload = PREFIX_MAGIC.to_vec();
            payload.extend_from_slice(&dimensions.width.to_le_bytes());
            payload.extend_from_slice(&dimensions.height.to_le_bytes());
            payload.extend_from_slice(
                page.stored_bytes()
                    .get(..consumed)
                    .ok_or_else(|| "Incomplete thumbnail response".to_string())?,
            );
            stored = Some(payload);
            decoded = Some((image, dimensions, top));
        } else {
            let _permit = SHARED
                .acquire(priority, || active())
                .ok_or_else(|| "operation cancelled".to_string())?;
            let dimensions = page
                .dimensions(reader_engine::current_limits())
                .map_err(|e| e.to_string())?;
            let top = mode.top_preview(dimensions);
            let image = if mode == RenderingMode::Gallery {
                center_square(
                    page.decode(reader_engine::current_limits())
                        .map_err(|e| e.to_string())?,
                )?
            } else if top {
                // A page stored in strips decodes only the strips this window
                // touches: the first one, for a top-of-page preview.
                let mut decoder = reader_engine::DisplayDecoder::default();
                let height = (u64::from(dimensions.height)
                    * u64::from(WIDTH.min(dimensions.width))
                    / u64::from(dimensions.width))
                .max(1);
                decoder
                    .decode_window(
                        &page,
                        reader_engine::current_limits(),
                        WIDTH,
                        0.,
                        (f64::from(HEIGHT) / height as f64).min(1.),
                        WIDTH.min(dimensions.width) as usize * HEIGHT as usize * 4,
                    )
                    .map_err(|e| e.to_string())?
                    .image
            } else {
                reader_engine::decode_display_page(
                    &page,
                    reader_engine::current_limits(),
                    WIDTH,
                    HEIGHT,
                )
                .map_err(|e| e.to_string())?
            };
            let mut png = Cursor::new(Vec::new());
            image
                .write_to(&mut png, image::ImageFormat::Png)
                .map_err(|e| e.to_string())?;
            // Keep original geometry alongside the small PNG, separate from all
            // chapter-original lookup keys.
            let mut payload = b"NRTHPNG1".to_vec();
            payload.extend_from_slice(&dimensions.width.to_le_bytes());
            payload.extend_from_slice(&dimensions.height.to_le_bytes());
            payload.extend_from_slice(png.get_ref());
            stored = Some(payload);
            decoded = Some((image, dimensions, top));
        }
    }
    if !active() {
        return Err("operation cancelled".into());
    }
    let (image, dimensions, top_preview) =
        decoded.ok_or_else(|| "Thumbnail unavailable".to_string())?;
    // Dense or unusually tall windows can expose many tiles. Share the live
    // allowance across all outstanding admitted tiles rather than filling it
    // with the first full-resolution completions and failing later tiles.
    let allowance = {
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let outstanding = state
            .desired
            .iter()
            .filter(|index| !state.ready.contains_key(index) && !state.failed.contains(index))
            .count()
            .max(1);
        let used = *shared.budget.0.lock().unwrap_or_else(|e| e.into_inner());
        PIXEL_BUDGET.saturating_sub(used) / outstanding
    };
    let thumbnail = Thumbnail::new(fit_pixel_budget(image, allowance)?, &shared.budget)?;
    if reusable()
        && let Some(bytes) = stored
    {
        let _ =
            shared
                .cache
                .thumbnail_write(&key, &bytes, cache_revision, inspect_derivative, || {
                    active() && reusable()
                });
    }
    let reused_proof = reusable.clone();
    let reusable_proof = reusable;
    let reusable_cache = cache_current.clone();
    let reusable: Current = Arc::new(move || reusable_cache() && reusable_proof());
    // A source response may legitimately refresh cookies. Fresh pixels can
    // publish under that resulting live context, then revoke immediately if
    // the context changes again before the UI consumes the completion.
    let current_proof: Current = match &resource.kind {
        ResourceKind::Remote { .. } if reused_resource => reused_proof,
        ResourceKind::Remote { source, page } => {
            let expected = source
                .page_cache_scope(&page.url, &page.headers)
                .map_err(|e| e.to_string())?;
            let access = source.page_access();
            let url = page.url.clone();
            let headers = page.headers.clone();
            Arc::new(move || {
                access
                    .page_cache_scope(&url, &headers)
                    .is_ok_and(|scope| scope == expected)
            })
        }
        _ => proof,
    };
    let current_cache = cache_current;
    let proof: Current = Arc::new(move || current_cache() && current_proof());
    if !proof() {
        return Err("operation cancelled".into());
    }
    Ok(Ready {
        thumbnail,
        dimensions,
        top_preview,
        current: proof,
        reusable,
        revision: reader_engine::current_limits_revision(),
    })
}
struct Preview {
    probe: Vec<u8>,
    consumed: usize,
    decoder: Option<Box<nova_image::ProgressiveJxlDecoder>>,
    non_jxl: bool,
    image: Option<(image::RgbaImage, PageDimensions)>,
    priority: Priority,
    current: Current,
    mode: RenderingMode,
    permit: Option<crate::decode_admission::Permit>,
}
impl Preview {
    fn new(priority: Priority, current: Current, mode: RenderingMode) -> Self {
        Self {
            probe: Vec::new(),
            consumed: 0,
            decoder: None,
            non_jxl: false,
            image: None,
            priority,
            current,
            mode,
            permit: None,
        }
    }
    fn disable(&mut self) {
        self.decoder = None;
        self.permit.take();
        self.probe.clear();
        self.non_jxl = true;
    }
    fn feed(&mut self, chunk: &[u8]) -> Result<PageChunkControl> {
        if self.non_jxl {
            return Ok(PageChunkControl::Continue(()));
        }
        if self.image.is_some() {
            return Ok(PageChunkControl::Break(()));
        }
        self.consumed += chunk.len();
        if let Some(decoder) = self.decoder.as_mut() {
            decoder.feed(chunk).map_err(|e| e.to_string())?;
        } else {
            self.probe.extend_from_slice(chunk);
            if !nova_image::is_jpeg_xl(&self.probe) {
                if self.probe.len() >= 12 {
                    self.non_jxl = true;
                    self.probe.clear();
                }
                return Ok(PageChunkControl::Continue(()));
            }
            self.permit = SHARED.acquire(self.priority, || (self.current)());
            if self.permit.is_none() {
                return Err("operation cancelled".into());
            }
            let mut decoder = nova_image::ProgressiveJxlDecoder::new(
                reader_engine::current_limits(),
                nova_image::JxlPreviewOptions {
                    max_width: self.mode.width(),
                    max_height: 65536,
                    max_bytes: 4 * 1024 * 1024,
                },
            )
            .map_err(|e| e.to_string())?;
            decoder.feed(&self.probe).map_err(|e| e.to_string())?;
            self.probe.clear();
            self.decoder = Some(Box::new(decoder));
        }
        if let Some(preview) = self
            .decoder
            .as_mut()
            .unwrap()
            .render_preview()
            .map_err(|e| e.to_string())?
        {
            self.image = Some((preview.image, preview.source));
            return Ok(PageChunkControl::Break(()));
        }
        Ok(PageChunkControl::Continue(()))
    }
    fn take(self) -> Option<(image::RgbaImage, PageDimensions, usize)> {
        self.image
            .map(|(image, dimensions)| (image, dimensions, self.consumed))
    }
}
fn fit_pixel_budget(image: image::RgbaImage, max_bytes: usize) -> Result<image::RgbaImage> {
    if image.as_raw().len() <= max_bytes {
        return Ok(image);
    }
    let (width, height) = budget_dimensions(image.width(), image.height(), max_bytes)?;
    Ok(image::imageops::resize(
        &image,
        width,
        height,
        image::imageops::FilterType::Triangle,
    ))
}
fn budget_dimensions(
    source_width: u32,
    source_height: u32,
    max_bytes: usize,
) -> Result<(u32, u32)> {
    let pixels = max_bytes / 4;
    if pixels == 0 {
        return Err("Thumbnail memory budget reached".into());
    }
    let scale =
        (pixels as f64 / (u64::from(source_width) * u64::from(source_height)) as f64).sqrt();
    let width = ((f64::from(source_width) * scale).floor().max(1.) as u32)
        .min(pixels.min(u32::MAX as usize) as u32);
    let height = ((f64::from(source_height) * scale).floor().max(1.) as u32)
        .min((pixels / width as usize).min(u32::MAX as usize) as u32);
    Ok((width, height))
}
/// Render only the center square. Direct bilinear sampling avoids allocating a
/// native square crop or a long resized strip beside the admitted original.
fn center_square(original: image::RgbaImage) -> Result<image::RgbaImage> {
    let source_side = original.width().min(original.height());
    let side = source_side.min(GALLERY_SIDE);
    if side == 0 {
        return Err("Invalid thumbnail dimensions".into());
    }
    if original.width() == side && original.height() == side {
        return Ok(original);
    }
    let output_bytes = u64::from(side) * u64::from(side) * 4;
    if (original.as_raw().len() as u64).saturating_add(output_bytes)
        > reader_engine::current_limits().max_decoded_bytes
    {
        return Err("image exceeds supported limits".into());
    }
    let mut output = image::RgbaImage::new(side, side);
    let left = f64::from(original.width() - source_side) / 2.;
    let top = f64::from(original.height() - source_side) / 2.;
    let scale = f64::from(source_side) / f64::from(side);
    for y in 0..side {
        let sy =
            (top + (f64::from(y) + 0.5) * scale - 0.5).clamp(0., f64::from(original.height() - 1));
        let y0 = sy.floor() as u32;
        let y1 = (y0 + 1).min(original.height() - 1);
        let fy = sy - f64::from(y0);
        for x in 0..side {
            let sx = (left + (f64::from(x) + 0.5) * scale - 0.5)
                .clamp(0., f64::from(original.width() - 1));
            let x0 = sx.floor() as u32;
            let x1 = (x0 + 1).min(original.width() - 1);
            let fx = sx - f64::from(x0);
            let a = original.get_pixel(x0, y0);
            let b = original.get_pixel(x1, y0);
            let c = original.get_pixel(x0, y1);
            let d = original.get_pixel(x1, y1);
            let pixel = output.get_pixel_mut(x, y);
            for channel in 0..4 {
                let upper = f64::from(a[channel]) * (1. - fx) + f64::from(b[channel]) * fx;
                let lower = f64::from(c[channel]) * (1. - fx) + f64::from(d[channel]) * fx;
                pixel[channel] = (upper * (1. - fy) + lower * fy).round().clamp(0., 255.) as u8;
            }
        }
    }
    Ok(output)
}
fn fit_preview(image: image::RgbaImage, top: bool) -> image::RgbaImage {
    if top {
        image::imageops::crop_imm(
            &image,
            0,
            0,
            image.width().min(WIDTH),
            image.height().min(HEIGHT),
        )
        .to_image()
    } else if image.width() <= WIDTH && image.height() <= HEIGHT {
        image
    } else {
        image::DynamicImage::ImageRgba8(image)
            .thumbnail(WIDTH, HEIGHT)
            .into_rgba8()
    }
}
fn inspect_derivative(bytes: &[u8]) -> Result<(u32, u32)> {
    if bytes.len() < 17 || !matches!(&bytes[..8], b"NRTHJXL1" | b"NRTHPNG1") {
        return Err("Invalid thumbnail container".into());
    }
    let width = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let height = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    let limits = reader_engine::current_limits();
    if width == 0
        || height == 0
        || width > limits.max_width
        || height > limits.max_height
        || u64::from(width) * u64::from(height) > limits.max_pixels
        || u64::from(width) * u64::from(height) * 4 > limits.max_decoded_bytes
        || bytes.len() > limits.max_encoded_bytes.saturating_add(16)
    {
        return Err("Thumbnail exceeds image limits".into());
    }
    if &bytes[..8] == PREFIX_MAGIC && !nova_image::is_jpeg_xl(&bytes[16..]) {
        return Err("Invalid JPEG XL thumbnail prefix".into());
    }
    Ok((width, height))
}
fn decode_derivative(
    bytes: &[u8],
    priority: Priority,
    current: Current,
    mode: RenderingMode,
) -> Result<(image::RgbaImage, PageDimensions, bool)> {
    let (width, height) = inspect_derivative(bytes)?;
    let dimensions = PageDimensions { width, height };
    let top = mode.top_preview(dimensions);
    let image = if &bytes[..8] == PREFIX_MAGIC {
        let mut preview = Preview::new(priority, current.clone(), mode);
        for chunk in bytes[16..].chunks(16 * 1024) {
            if preview.feed(chunk)?.is_break() {
                break;
            }
        }
        let (image, source, _) = preview
            .take()
            .ok_or_else(|| "Unusable JPEG XL thumbnail prefix".to_string())?;
        if source != dimensions {
            return Err("Thumbnail dimensions changed".into());
        }
        if mode == RenderingMode::Gallery {
            center_square(image)?
        } else {
            fit_preview(image, top)
        }
    } else {
        let _permit = SHARED
            .acquire(priority, || current())
            .ok_or_else(|| "operation cancelled".to_string())?;
        reader_engine::decode_display_image(
            &bytes[16..],
            reader_engine::current_limits(),
            mode.width(),
            if mode == RenderingMode::Gallery {
                GALLERY_SIDE
            } else {
                HEIGHT
            },
        )
        .map_err(|e| e.to_string())?
    };
    Ok((image, dimensions, top))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reader_domain::{ChapterId, SeriesId};
    use reader_sources::SourceRegistry;
    use std::{io::Write, sync::mpsc, time::Duration};
    fn fixture() -> (
        tempfile::TempDir,
        SourceRegistry,
        reader_sources::Source,
        ChapterId,
        Vec<Resource>,
    ) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/addons/neutral-fixture");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
        let mut files = vec!["manifest.json".to_string(), "main.js".to_string()];
        files.extend(
            manifest["assets"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_string()),
        );
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for file in files {
            zip.start_file(&file, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&std::fs::read(root.join(file)).unwrap())
                .unwrap();
        }
        let directory = tempfile::tempdir().unwrap();
        let registry = SourceRegistry::open(directory.path().join("sources")).unwrap();
        registry
            .install_zip(&zip.finish().unwrap().into_inner())
            .unwrap();
        registry.set_enabled("neutral-fixture", true).unwrap();
        let source = registry.source("neutral-fixture").unwrap();
        let chapter = ChapterId::new(SeriesId::new("neutral-fixture", "neutral"), "hundreds");
        let pages = source
            .pages(&serde_json::json!({"chapterId":"hundreds"}))
            .unwrap()
            .pages;
        assert_eq!(pages.len(), 320);
        let resources = pages
            .into_iter()
            .map(|page| Resource {
                chapter: chapter.clone(),
                id: page.id.clone(),
                kind: ResourceKind::Remote {
                    source: Arc::new(source.clone()),
                    page,
                },
            })
            .collect();
        (directory, registry, source, chapter, resources)
    }
    struct Transport {
        status: std::sync::atomic::AtomicU16,
        calls: std::sync::atomic::AtomicUsize,
        body: Vec<u8>,
        scope: std::sync::atomic::AtomicU64,
        headers: Mutex<Vec<BTreeMap<String, String>>>,
    }
    impl reader_sources::SourceTransport for Transport {
        fn page_cache_partition(&self, _: &str) -> Option<String> {
            Some(format!(
                "profile-{}",
                self.scope.load(std::sync::atomic::Ordering::Acquire)
            ))
        }
        fn request(
            &self,
            request: &reader_sources::HttpRequest,
            _: usize,
            token: &CancelToken,
        ) -> reader_sources::Result<reader_sources::HttpResponse> {
            token.check()?;
            self.calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            self.headers.lock().unwrap().push(request.headers.clone());
            Ok(reader_sources::HttpResponse {
                status: self.status.load(std::sync::atomic::Ordering::Acquire),
                url: request.url.clone(),
                body: self.body.clone(),
                ..Default::default()
            })
        }
    }
    fn remote_fixture() -> (tempfile::TempDir, Vec<Resource>, Arc<Transport>) {
        let mut png = Cursor::new(Vec::new());
        image::RgbaImage::new(160, 240)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        remote_fixture_with_body(png.into_inner())
    }
    fn remote_fixture_with_body(
        body: Vec<u8>,
    ) -> (tempfile::TempDir, Vec<Resource>, Arc<Transport>) {
        let directory = tempfile::tempdir().unwrap();
        let manifest = serde_json::json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"thumb-network","name":"Thumbnail fixture","version":"1.0.0","language":"en","entry":"main.js","permissions":{"domains":["example.com"]},"capabilities":["pages"]});
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", manifest.to_string().into_bytes()),
            (
                "main.js",
                b"globalThis.novaProvider={handle(){return {pages:[]};}};".to_vec(),
            ),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        let registry = SourceRegistry::open(directory.path().join("sources")).unwrap();
        registry
            .install_zip(&zip.finish().unwrap().into_inner())
            .unwrap();
        registry.set_enabled("thumb-network", true).unwrap();
        let transport = Arc::new(Transport {
            status: std::sync::atomic::AtomicU16::new(429),
            calls: std::sync::atomic::AtomicUsize::new(0),
            body,
            scope: std::sync::atomic::AtomicU64::new(0),
            headers: Mutex::new(Vec::new()),
        });
        let source = registry
            .source("thumb-network")
            .unwrap()
            .with_transport(transport.clone());
        let chapter = ChapterId::new(SeriesId::new("thumb-network", "series"), "chapter");
        let resources = (0..320)
            .map(|index| Resource {
                chapter: chapter.clone(),
                id: index.to_string(),
                kind: ResourceKind::Remote {
                    source: Arc::new(source.clone()),
                    page: reader_sources::Page {
                        id: index.to_string(),
                        url: format!("https://example.com/{index}.png"),
                        headers: BTreeMap::from([(
                            "Referer".into(),
                            "https://example.com/chapter".into(),
                        )]),
                        width: Some(40),
                        height: Some(60),
                    },
                },
            })
            .collect();
        (directory, resources, transport)
    }
    fn bands(width: u32, height: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(width, height, |x, y| {
            let coordinate = if width > height { x } else { y };
            let total = width.max(height);
            image::Rgba(if coordinate < total / 3 {
                [220, 0, 0, 255]
            } else if coordinate >= total * 2 / 3 {
                [0, 220, 0, 255]
            } else {
                [0, 0, 220, 255]
            })
        })
    }
    fn png_bytes(image: &image::RgbaImage) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }
    #[test]
    fn photo_square_samples_landscape_and_portrait_centers_without_page_letterboxing() {
        for (width, height) in [(960, 160), (160, 480), (160, 960)] {
            let original = bands(width, height);
            let square = center_square(original.clone()).unwrap();
            assert_eq!((square.width(), square.height()), (160, 160));
            assert!(square.pixels().all(|pixel| pixel.0 == [0, 0, 220, 255]));
            let page = fit_preview(original, false);
            assert_ne!(page.width(), page.height());
            assert_eq!(page.get_pixel(0, 0).0, [220, 0, 0, 255]);
            assert_eq!(
                page.get_pixel(page.width() - 1, page.height() - 1).0,
                [0, 220, 0, 255]
            );
        }
    }
    #[test]
    fn remote_gallery_center_square_and_page_top_have_isolated_reusable_cache_recipes() {
        use std::sync::atomic::Ordering;
        let (directory, mut resources, transport) =
            remote_fixture_with_body(png_bytes(&bands(640, 3840)));
        resources.truncate(1);
        let inventory = Arc::new(resources);
        transport.status.store(200, Ordering::Release);
        let cache = Arc::new(ChapterCache::test_cache(
            directory.path().join("cache"),
            4 * 1024 * 1024,
        ));
        let (sender, receiver) = mpsc::channel();
        let service = PageThumbnails::new(
            cache.clone(),
            Arc::new(move |completion| {
                let _ = sender.send(completion);
            }),
        );
        service.replace_inventory(1, inventory.clone());
        service.viewport(1, 0, 1, 1);
        let page = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        let page_pixels = &page.result.as_ref().unwrap().pixels;
        assert!(page.top_preview);
        assert_eq!((page_pixels.width(), page_pixels.height()), (160, 240));
        assert_eq!(&page_pixels.as_bytes()[..4], &[220, 0, 0, 255]);
        service.replace_inventory_for_gallery(2, inventory.clone());
        service.viewport(2, 0, 1, 1);
        let gallery = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        let gallery_pixels = &gallery.result.as_ref().unwrap().pixels;
        assert!(!gallery.top_preview);
        assert_eq!(
            (gallery_pixels.width(), gallery_pixels.height()),
            (320, 320)
        );
        assert_eq!(&gallery_pixels.as_bytes()[..4], &[0, 0, 220, 255]);
        assert!(!page.is_current());
        assert_eq!(transport.calls.load(Ordering::Acquire), 2);
        assert_eq!(cache.thumbnail_usage().1, 2);
        service.replace_inventory(3, inventory.clone());
        service.viewport(3, 0, 1, 1);
        let restored = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(restored.top_preview);
        assert_eq!(
            &restored.result.as_ref().unwrap().pixels.as_bytes()[..4],
            &[220, 0, 0, 255]
        );
        assert_eq!(transport.calls.load(Ordering::Acquire), 2);
        service.replace_inventory_for_gallery(4, inventory);
        service.viewport(4, 0, 1, 1);
        let gallery_again = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        let pixels = &gallery_again.result.as_ref().unwrap().pixels;
        assert_eq!((pixels.width(), pixels.height()), (320, 320));
        assert!(!gallery_again.top_preview);
        assert_eq!(&pixels.as_bytes()[..4], &[0, 0, 220, 255]);
        assert_eq!(transport.calls.load(Ordering::Acquire), 2);
        service.cancel();
    }
    #[test]
    fn saved_gallery_square_works_offline_but_derivatives_cannot_replace_missing_originals() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("saved.png");
        let bytes = png_bytes(&bands(160, 960));
        std::fs::write(&path, &bytes).unwrap();
        let resource = Resource {
            chapter: ChapterId::new(SeriesId::new("removed-addon", "series"), "chapter"),
            id: "saved".into(),
            kind: ResourceKind::Local {
                path: path.clone(),
                expected_bytes: bytes.len() as u64,
                sha256: digest(&bytes),
            },
        };
        let inventory = Arc::new(vec![resource.clone()]);
        let cache = Arc::new(ChapterCache::test_cache(
            directory.path().join("cache"),
            4 * 1024 * 1024,
        ));
        let (sender, receiver) = mpsc::channel();
        let service = PageThumbnails::new(
            cache.clone(),
            Arc::new(move |completion| {
                let _ = sender.send(completion);
            }),
        );
        service.replace_inventory(1, inventory.clone());
        service.viewport(1, 0, 1, 1);
        let page = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(page.top_preview);
        service.replace_inventory_for_gallery(2, inventory.clone());
        service.viewport(2, 0, 1, 1);
        let gallery = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(!gallery.top_preview);
        assert_eq!(
            &gallery.result.as_ref().unwrap().pixels.as_bytes()[..4],
            &[0, 0, 220, 255]
        );
        assert!(resource.read(&CancelToken::default()).is_ok());
        assert_eq!(cache.thumbnail_usage().1, 2);
        std::fs::remove_file(path).unwrap();
        assert!(!gallery.is_current());
        service.replace_inventory_for_gallery(3, inventory);
        service.viewport(3, 0, 1, 1);
        assert!(
            receiver
                .recv_timeout(Duration::from_secs(20))
                .unwrap()
                .result
                .is_err()
        );
        assert!(resource.read(&CancelToken::default()).is_err());
        assert_eq!(cache.thumbnail_usage().1, 2);
        service.cancel();
    }
    #[test]
    fn source_rate_limit_pauses_all_queued_tiles_until_explicit_retry_preserving_headers() {
        use std::sync::atomic::Ordering;
        let (directory, resources, transport) = remote_fixture();
        let cache = Arc::new(ChapterCache::test_cache(
            directory.path().join("cache"),
            4 * 1024 * 1024,
        ));
        let (sender, receiver) = mpsc::channel();
        let service = PageThumbnails::new(
            cache,
            Arc::new(move |completion| {
                let _ = sender.send(completion);
            }),
        );
        service.replace_inventory(1, Arc::new(resources));
        service.viewport(1, 0, 6, 3);
        let failure = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(failure.result.is_err());
        assert!(service.paused().is_some());
        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        assert_eq!(transport.calls.load(Ordering::Acquire), 1);
        service.prepare_gallery_reflow(1);
        service.viewport(1, 0, 6, 3);
        assert!(service.paused().is_some());
        assert!(receiver.recv_timeout(Duration::from_millis(60)).is_err());
        assert_eq!(transport.calls.load(Ordering::Acquire), 1);
        transport.status.store(200, Ordering::Release);
        service.retry(1);
        let result = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(result.result.is_ok());
        assert!(result.is_current());
        service.cancel();
        assert!(transport.calls.load(Ordering::Acquire) <= 10); // Sole worker, bounded admitted rows.
        assert!(transport.headers.lock().unwrap().iter().all(|headers| {
            headers
                .get("Referer")
                .is_some_and(|value| value == "https://example.com/chapter")
        }));
    }
    #[test]
    fn authentication_scope_change_revokes_queued_pixels_and_requires_new_source_requests() {
        use std::sync::atomic::Ordering;
        let (directory, resources, transport) = remote_fixture();
        transport.status.store(200, Ordering::Release);
        let cache = Arc::new(ChapterCache::test_cache(
            directory.path().join("cache"),
            4 * 1024 * 1024,
        ));
        let (sender, receiver) = mpsc::channel();
        let service = PageThumbnails::new(
            cache,
            Arc::new(move |completion| {
                let _ = sender.send(completion);
            }),
        );
        service.replace_inventory(5, Arc::new(resources));
        service.viewport(5, 0, 1, 1);
        let first = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        let second = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(first.is_current());
        assert!(second.is_current());
        assert_eq!(transport.calls.load(Ordering::Acquire), 2);
        transport.scope.fetch_add(1, Ordering::AcqRel);
        assert!(!first.is_current());
        assert!(!second.is_current());
        service.viewport(5, 0, 1, 1);
        let replacement = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(replacement.result.is_ok());
        assert!(replacement.is_current());
        assert!(transport.calls.load(Ordering::Acquire) >= 3);
        service.cancel();
    }
    #[test]
    fn dense_viewport_adapts_all_tiles_without_exceeding_shared_ui_pixel_budget() {
        use std::sync::atomic::Ordering;
        let (directory, resources, transport) = remote_fixture();
        transport.status.store(200, Ordering::Release);
        let cache = Arc::new(ChapterCache::test_cache(
            directory.path().join("cache"),
            4 * 1024 * 1024,
        ));
        let (sender, receiver) = mpsc::channel();
        let service = PageThumbnails::new(
            cache,
            Arc::new(move |completion| {
                let _ = sender.send(completion);
            }),
        );
        service.replace_inventory(3, Arc::new(resources));
        service.viewport(3, 0, 120, 12);
        let mut ui_leases = Vec::new();
        let mut indices = BTreeSet::new();
        for _ in 0..132 {
            let completion = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
            assert!(completion.is_current());
            indices.insert(completion.index);
            ui_leases.push(completion.result.unwrap());
        }
        assert_eq!(indices, (0..132).collect::<BTreeSet<_>>());
        assert_eq!(transport.calls.load(Ordering::Acquire), 132);
        assert!(
            ui_leases
                .iter()
                .all(|tile| tile.pixels.width() > 0 && tile.pixels.height() > 0)
        );
        assert!(
            ui_leases
                .iter()
                .map(|tile| tile.pixels.as_bytes().len())
                .sum::<usize>()
                <= PIXEL_BUDGET
        );
        assert!(service.retained_bytes() <= PIXEL_BUDGET);
        service.cancel();
        assert_eq!(
            service.retained_bytes(),
            ui_leases
                .iter()
                .map(|tile| tile.pixels.as_bytes().len())
                .sum::<usize>()
        );
        drop(ui_leases);
        assert_eq!(service.retained_bytes(), 0);
    }
    #[test]
    fn dense_gallery_squares_keep_all_ui_leases_within_shared_live_pixel_allowance() {
        use std::sync::atomic::Ordering;
        let (directory, resources, transport) =
            remote_fixture_with_body(png_bytes(&image::RgbaImage::new(320, 480)));
        transport.status.store(200, Ordering::Release);
        let cache = Arc::new(ChapterCache::test_cache(
            directory.path().join("cache"),
            4 * 1024 * 1024,
        ));
        let (sender, receiver) = mpsc::channel();
        let service = PageThumbnails::new(
            cache,
            Arc::new(move |completion| {
                let _ = sender.send(completion);
            }),
        );
        service.replace_inventory_for_gallery(8, Arc::new(resources));
        service.viewport(8, 0, 120, 12);
        let mut ui_leases = Vec::new();
        for _ in 0..132 {
            let completion = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
            assert!(completion.is_current());
            assert!(!completion.top_preview);
            let tile = completion.result.unwrap();
            assert_eq!(tile.pixels.width(), tile.pixels.height());
            assert!(tile.pixels.width() <= GALLERY_SIDE);
            ui_leases.push(tile);
        }
        assert_eq!(transport.calls.load(Ordering::Acquire), 132);
        assert!(
            ui_leases
                .iter()
                .any(|tile| tile.pixels.width() < GALLERY_SIDE)
        );
        let bytes = ui_leases
            .iter()
            .map(|tile| tile.pixels.as_bytes().len())
            .sum::<usize>();
        assert!(bytes <= PIXEL_BUDGET);
        assert_eq!(service.retained_bytes(), bytes);
        service.cancel();
        assert_eq!(service.retained_bytes(), bytes);
        drop(ui_leases);
        assert_eq!(service.retained_bytes(), 0);
    }
    #[test]
    fn gallery_density_expansion_rebalances_retained_tiles_without_disk_or_refetching() {
        use std::sync::atomic::Ordering;
        let (directory, resources, transport) =
            remote_fixture_with_body(png_bytes(&image::RgbaImage::new(320, 480)));
        transport.status.store(200, Ordering::Release);
        let cache = Arc::new(ChapterCache::test_cache_without_disk(
            directory.path().join("cache"),
        ));
        let (sender, receiver) = mpsc::channel();
        let service = PageThumbnails::new(
            cache.clone(),
            Arc::new(move |completion| {
                let _ = sender.send(completion);
            }),
        );
        service.replace_inventory_for_gallery(12, Arc::new(resources));
        service.viewport(12, 0, 12, 12);
        let mut ui_leases = Vec::new();
        let mut old_publication_checks = Vec::new();
        for _ in 0..24 {
            let completion = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
            assert!(completion.is_current());
            old_publication_checks.push(completion.current.clone());
            ui_leases.push(completion.result.unwrap());
        }
        assert_eq!(transport.calls.load(Ordering::Acquire), 24);
        assert!(service.retained_bytes() >= 7 * 1024 * 1024);
        assert!(ui_leases.iter().all(|tile| tile.pixels.width() >= 290));
        assert_eq!(cache.thumbnail_usage(), (0, 0));
        // One stale queued publication may outlive the event loop's old Image
        // map. Reflow must wait for its lease, never fail tiles or exceed8MiB.
        let pending_old_publication = ui_leases[0].clone();
        drop(ui_leases);
        service.prepare_gallery_reflow(12);
        assert!(old_publication_checks.iter().all(|current| !current()));
        service.viewport(12, 0, 36, 12);
        assert!(receiver.recv_timeout(Duration::from_millis(60)).is_err());
        assert_eq!(transport.calls.load(Ordering::Acquire), 24);
        drop(pending_old_publication);
        let mut new_ui_leases = Vec::new();
        let mut indices = BTreeSet::new();
        for _ in 0..48 {
            let completion = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
            assert!(completion.is_current());
            assert!(!completion.top_preview);
            indices.insert(completion.index);
            let tile = completion.result.unwrap();
            assert_eq!(tile.pixels.width(), tile.pixels.height());
            assert!(tile.pixels.width() >= 200);
            new_ui_leases.push(tile);
        }
        assert_eq!(indices, (0..48).collect::<BTreeSet<_>>());
        assert_eq!(transport.calls.load(Ordering::Acquire), 48);
        let bytes = new_ui_leases
            .iter()
            .map(|tile| tile.pixels.as_bytes().len())
            .sum::<usize>();
        assert!(bytes <= PIXEL_BUDGET);
        assert_eq!(service.retained_bytes(), bytes);
        assert_eq!(cache.thumbnail_usage(), (0, 0));
        drop(new_ui_leases);
        service.prepare_gallery_reflow(12);
        service.viewport(12, 0, 0, 12);
        assert!(service.shared.state.lock().unwrap().recycled.is_empty());
        assert_eq!(service.retained_bytes(), 0);
        service.cancel();
    }
    #[test]
    fn progressive_jxl_stops_at_useful_pixels_and_replays_exact_unreencoded_prefix() {
        let bytes =
            include_bytes!("../crates/reader-engine/tests/fixtures/jxl/progressive-rgb.jxl");
        let current: Current = Arc::new(|| true);
        let mut preview = Preview::new(
            Priority::VisibleThumbnail,
            current.clone(),
            RenderingMode::Page,
        );
        let mut consumed = 0;
        for chunk in bytes.chunks(1024) {
            consumed += chunk.len();
            if preview.feed(chunk).unwrap().is_break() {
                break;
            }
        }
        let (image, dimensions, prefix_length) = preview
            .take()
            .expect("progressive fixture has useful low-resolution pixels");
        assert!(consumed < bytes.len());
        assert_eq!(prefix_length, consumed);
        let top = dimensions.height >= dimensions.width * 4;
        let expected = fit_preview(image, top);
        let mut stored = PREFIX_MAGIC.to_vec();
        stored.extend_from_slice(&dimensions.width.to_le_bytes());
        stored.extend_from_slice(&dimensions.height.to_le_bytes());
        stored.extend_from_slice(&bytes[..prefix_length]);
        let (replayed, replayed_dimensions, replayed_top) = decode_derivative(
            &stored,
            Priority::VisibleThumbnail,
            current.clone(),
            RenderingMode::Page,
        )
        .unwrap();
        assert_eq!(replayed_dimensions, dimensions);
        assert_eq!(replayed_top, top);
        assert_eq!(replayed, expected);
        assert_eq!(&stored[16..], &bytes[..consumed]);
        let mut gallery_preview = Preview::new(
            Priority::VisibleThumbnail,
            current.clone(),
            RenderingMode::Gallery,
        );
        for chunk in bytes.chunks(1024) {
            if gallery_preview.feed(chunk).unwrap().is_break() {
                break;
            }
        }
        let (gallery_image, gallery_source, gallery_consumed) = gallery_preview.take().unwrap();
        let expected_gallery = center_square(gallery_image).unwrap();
        let mut gallery_stored = PREFIX_MAGIC.to_vec();
        gallery_stored.extend_from_slice(&gallery_source.width.to_le_bytes());
        gallery_stored.extend_from_slice(&gallery_source.height.to_le_bytes());
        gallery_stored.extend_from_slice(&bytes[..gallery_consumed]);
        let (gallery, gallery_dimensions, gallery_top) = decode_derivative(
            &gallery_stored,
            Priority::VisibleThumbnail,
            current,
            RenderingMode::Gallery,
        )
        .unwrap();
        assert_eq!(gallery, expected_gallery);
        assert_eq!(gallery.width(), gallery.height());
        assert!(gallery.width() <= GALLERY_SIDE);
        assert_eq!(gallery_dimensions, dimensions);
        assert!(!gallery_top);
        assert!(gallery_consumed < bytes.len());
        assert_eq!(&gallery_stored[16..], &bytes[..gallery_consumed]);
    }
    #[test]
    fn derivative_identity_is_source_local_even_for_equal_chapter_and_page_ids() {
        let first = ChapterId::new(SeriesId::new("source-a", "series"), "chapter");
        let second = ChapterId::new(SeriesId::new("source-b", "series"), "chapter");
        assert_ne!(
            ChapterCache::thumbnail_key(&first, b"same-page-url-and-scope"),
            ChapterCache::thumbnail_key(&second, b"same-page-url-and-scope")
        );
    }
    #[test]
    fn rows_are_bounded_independently_of_three_hundred_twenty_page_inventory() {
        let (desired, visible) = admitted(320, 90, 108, 6);
        assert_eq!(visible.len(), 18);
        assert_eq!(desired.len(), 30);
        assert_eq!(*desired.first().unwrap(), 84);
        assert_eq!(*desired.last().unwrap(), 113);
        assert!(admitted(320, 0, 0, 6).0.is_empty());
    }
    #[test]
    fn shared_pixel_lease_survives_cache_eviction_and_enforces_budget() {
        let budget = Arc::new(Budget::default());
        let mut handles = Vec::new();
        while let Ok(thumbnail) = Thumbnail::new(image::RgbaImage::new(WIDTH, HEIGHT), &budget) {
            handles.push(thumbnail);
        }
        let bytes = *budget.0.lock().unwrap();
        assert!(bytes <= PIXEL_BUDGET);
        assert!(bytes + (WIDTH * HEIGHT * 4) as usize > PIXEL_BUDGET);
        let ui_handle = handles[0].clone();
        drop(handles);
        assert_eq!(*budget.0.lock().unwrap(), (WIDTH * HEIGHT * 4) as usize);
        drop(ui_handle);
        assert_eq!(*budget.0.lock().unwrap(), 0);
    }
    #[test]
    fn fixture_only_requested_rows_decode_cancelled_completions_cannot_publish() {
        let (directory, registry, _, _, resources) = fixture();
        let cache = Arc::new(ChapterCache::test_cache(
            directory.path().join("cache"),
            4 * 1024 * 1024,
        ));
        let (sender, receiver) = mpsc::channel();
        let service = PageThumbnails::new(
            cache.clone(),
            Arc::new(move |completion| {
                sender.send(completion).unwrap();
            }),
        );
        service.replace_inventory(4, Arc::new(resources));
        assert!(receiver.recv_timeout(Duration::from_millis(80)).is_err()); // Viewer/inventory alone creates zero jobs.
        service.viewport(4, 0, 3, 3);
        let first = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(first.result.is_ok());
        assert!(first.is_current());
        assert!(first.index < 3);
        service.cancel();
        assert!(!first.is_current());
        assert!(service.retained_bytes() <= PIXEL_BUDGET);
        registry.remove("neutral-fixture").unwrap();
        assert!(!first.is_current());
    }
    #[test]
    fn extreme_tall_fixture_uses_bounded_top_window_instead_of_a_narrow_full_strip() {
        let (directory, _registry, _, _, resources) = fixture();
        let cache = Arc::new(ChapterCache::test_cache(
            directory.path().join("cache"),
            4 * 1024 * 1024,
        ));
        let (sender, receiver) = mpsc::channel();
        let service = PageThumbnails::new(
            cache,
            Arc::new(move |completion| {
                let _ = sender.send(completion);
            }),
        );
        service.replace_inventory(9, Arc::new(resources));
        service.viewport(9, 2, 3, 1);
        let completion = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        assert_eq!(completion.index, 2);
        assert!(completion.top_preview);
        assert_eq!(
            completion.dimensions,
            Some(PageDimensions {
                width: 128,
                height: 12000
            })
        );
        let thumbnail = completion.result.as_ref().unwrap();
        assert_eq!(thumbnail.pixels.width(), 128);
        assert_eq!(thumbnail.pixels.height(), 240);
        service.cancel();
    }
    struct CountedReader<R> {
        inner: R,
        bytes_read: usize,
    }
    impl<R: Read> Read for CountedReader<R> {
        fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
            let count = self.inner.read(target)?;
            self.bytes_read += count;
            Ok(count)
        }
    }
    #[test]
    fn local_progressive_prefix_stops_filesystem_io_but_cannot_authorize_a_damaged_original() {
        let original =
            include_bytes!("../crates/reader-engine/tests/fixtures/jxl/progressive-rgb.jxl");
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("saved.jxl");
        let mut damaged = original.to_vec();
        *damaged.last_mut().unwrap() ^= 1;
        std::fs::write(&path, &damaged).unwrap();
        let expected_hash = digest(original);
        let current: Current = Arc::new(|| true);
        let mut preview = Preview::new(
            Priority::VisibleThumbnail,
            current.clone(),
            RenderingMode::Page,
        );
        let mut reader = CountedReader {
            inner: std::fs::File::open(&path).unwrap(),
            bytes_read: 0,
        };
        let prefix = read_local_preview(
            &mut reader,
            original.len() as u64,
            &expected_hash,
            &mut preview,
            &current,
        )
        .unwrap();
        assert_eq!(reader.bytes_read, prefix.len());
        assert!(prefix.len() < original.len());
        assert_eq!(prefix, original[..prefix.len()]);
        let (_, dimensions, consumed) = preview.take().expect("usable local progressive pixels");
        assert_eq!(consumed, prefix.len());
        let resource = Resource {
            chapter: ChapterId::new(SeriesId::new("neutral-local", "series"), "chapter"),
            id: "jxl".into(),
            kind: ResourceKind::Local {
                path: path.clone(),
                expected_bytes: original.len() as u64,
                sha256: expected_hash.clone(),
            },
        };
        assert!(resource.read(&CancelToken::default()).is_err());
        let cache = Arc::new(ChapterCache::test_cache(
            directory.path().join("cache"),
            4 * 1024 * 1024,
        ));
        let (sender, receiver) = mpsc::channel();
        let service = PageThumbnails::new(
            cache.clone(),
            Arc::new(move |completion| {
                let _ = sender.send(completion);
            }),
        );
        service.replace_inventory(6, Arc::new(vec![resource.clone()]));
        service.viewport(6, 0, 1, 1);
        let completion = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(completion.result.is_ok());
        assert!(completion.is_current());
        assert_eq!(completion.dimensions, Some(dimensions));
        let stamp = local_stamp(&path).unwrap();
        let identity = serde_json::to_vec(&(
            &path,
            &resource.id,
            &(original.len() as u64),
            &expected_hash,
            stamp.1,
        ))
        .unwrap();
        let key = ChapterCache::thumbnail_key(&resource.chapter, &identity);
        let stored = cache
            .thumbnail_read(&key, inspect_derivative)
            .unwrap()
            .unwrap();
        assert_eq!(&stored[..8], PREFIX_MAGIC);
        assert_eq!(&stored[16..], prefix);
        assert_eq!(cache.thumbnail_usage().1, 1);
        assert!(resource.read(&CancelToken::default()).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(!completion.is_current());
        service.viewport(6, 0, 1, 1);
        let missing = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(missing.result.is_err());
        assert_eq!(cache.thumbnail_usage().1, 1); // Derivative storage proves no original availability.
        assert!(resource.read(&CancelToken::default()).is_err());
        service.cancel();
    }
    #[test]
    fn non_jxl_local_fallback_reads_eof_and_verifies_full_hash() {
        let mut png = Cursor::new(Vec::new());
        image::RgbaImage::new(4, 4)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let original = png.into_inner();
        for damaged in [false, true] {
            let mut bytes = original.clone();
            if damaged {
                *bytes.last_mut().unwrap() ^= 1;
            }
            let mut reader = CountedReader {
                inner: Cursor::new(&bytes),
                bytes_read: 0,
            };
            let current: Current = Arc::new(|| true);
            let mut preview = Preview::new(
                Priority::VisibleThumbnail,
                current.clone(),
                RenderingMode::Page,
            );
            let result = read_local_preview(
                &mut reader,
                original.len() as u64,
                &digest(&original),
                &mut preview,
                &current,
            );
            assert_eq!(reader.bytes_read, original.len());
            assert_eq!(result.is_err(), damaged);
            assert!(preview.image.is_none());
        }
    }
    #[test]
    fn local_prefix_cancel_after_probe_stops_before_decoder_or_next_read() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct CancelAfterProbe {
            reads: usize,
            active: Arc<AtomicBool>,
        }
        impl Read for CancelAfterProbe {
            fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
                self.reads += 1;
                target.fill(0);
                self.active.store(false, Ordering::Release);
                Ok(target.len())
            }
        }
        let active = Arc::new(AtomicBool::new(true));
        let flag = active.clone();
        let current: Current = Arc::new(move || flag.load(Ordering::Acquire));
        let mut preview = Preview::new(
            Priority::VisibleThumbnail,
            current.clone(),
            RenderingMode::Page,
        );
        let mut reader = CancelAfterProbe { reads: 0, active };
        assert!(read_local_preview(&mut reader, 4096, "unused", &mut preview, &current).is_err());
        assert_eq!(reader.reads, 1);
        assert!(preview.decoder.is_none());
    }
    #[cfg(unix)]
    #[test]
    fn symlinked_local_original_never_authorizes_thumbnail_reuse() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original");
        let link = directory.path().join("link");
        std::fs::write(&original, b"not an image").unwrap();
        std::os::unix::fs::symlink(&original, &link).unwrap();
        assert!(local_stamp(&original).is_ok());
        assert!(local_stamp(&link).is_err());
    }
    #[test]
    fn saved_pages_work_without_addon_but_damage_and_unavailable_pages_are_isolated() {
        let (directory, registry, source, chapter, resources) = fixture();
        let ResourceKind::Remote { page, .. } = &resources[0].kind else {
            unreachable!()
        };
        let bytes = source
            .fetch_page(&page.url, &page.headers, &CancelToken::default())
            .unwrap();
        let path = directory.path().join("saved.png");
        std::fs::write(&path, &bytes).unwrap();
        registry.remove("neutral-fixture").unwrap();
        let local = Resource {
            chapter: chapter.clone(),
            id: "saved".into(),
            kind: ResourceKind::Local {
                path: path.clone(),
                expected_bytes: bytes.len() as u64,
                sha256: digest(&bytes),
            },
        };
        let unavailable = Resource {
            chapter,
            id: "missing".into(),
            kind: ResourceKind::Unavailable {
                message: "Page is unavailable offline".into(),
            },
        };
        let cache = Arc::new(ChapterCache::test_cache(
            directory.path().join("cache"),
            4 * 1024 * 1024,
        ));
        let (sender, receiver) = mpsc::channel();
        let service = PageThumbnails::new(
            cache,
            Arc::new(move |completion| {
                sender.send(completion).unwrap();
            }),
        );
        service.replace_inventory(1, Arc::new(vec![local, unavailable]));
        service.viewport(1, 0, 2, 2);
        let saved = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        assert_eq!(saved.index, 0);
        assert!(saved.result.is_ok());
        let missing = receiver.recv_timeout(Duration::from_secs(20)).unwrap();
        assert_eq!(missing.index, 1);
        assert!(missing.result.is_err());
        std::fs::write(path, b"damaged").unwrap();
        assert!(!saved.is_current());
    }
}
