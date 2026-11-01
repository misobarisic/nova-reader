use crate::reader_modes::{self, ModeChoice};
use crate::{
    AppWindow, Appearance, EntryCategoryRow, ExtensionRow, ItemRow, PageRow, SettingRow, TileRow,
    services::{Result, Services, error, now},
};
use nova_reader_ui::backend_text as text;
use reader_domain::{
    Bookmark, Chapter, ChapterId, ReadingMode, ReadingProgress, ResumeAnchor, Series,
};
use reader_sources::{CancelToken, Source, SourceDescriptor};
use serde_json::json;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
};

static LIFECYCLE: OnceLock<Weak<Controller>> = OnceLock::new();
#[cfg(target_os = "android")]
pub fn flush_lifecycle() {
    if let Some(app) = LIFECYCLE.get().and_then(Weak::upgrade) {
        app.metadata_refresh.pause();
        app.flush();
    }
}

#[derive(Clone)]
struct Choice {
    package: String,
    id: String,
    name: String,
    enabled: bool,
    filters: bool,
    language: String,
    latest: bool,
}
fn catalogue_rows((choices, disabled): (Vec<Choice>, BTreeSet<String>)) -> Vec<SettingRow> {
    choices
        .into_iter()
        .map(|choice| SettingRow {
            checked: !disabled.contains(&choice.id),
            key: choice.id.into(),
            name: choice.name.into(),
            kind: "bool".into(),
            ..Default::default()
        })
        .collect()
}
struct BrowseSources {
    choices: Vec<Choice>,
    extensions: Vec<SourceDescriptor>,
    icons: Vec<crate::addon_icons::PreparedIcon>,
}
#[derive(Clone)]
enum Row {
    Series(Series),
    Chapter(Chapter),
    Download(ChapterId),
}
#[derive(Clone)]
enum PageInput {
    Local { path: PathBuf, id: String },
    Remote(reader_sources::Page),
}
impl PageInput {
    fn read(&self, source: Option<&Source>, cancel: &CancelToken) -> Result<Vec<u8>> {
        cancel.check().map_err(error)?;
        let bytes = match self {
            Self::Local { path, .. } => {
                use std::io::Read;
                let file = std::fs::File::open(path).map_err(error)?;
                if file.metadata().map_err(error)?.len() > 32 * 1024 * 1024 {
                    return Err(text::tr("Image exceeds byte limit"));
                }
                let mut bytes = Vec::new();
                file.take(32 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(error)?;
                bytes
            }
            Self::Remote(page) => source
                .ok_or_else(|| text::tr("Source unavailable"))?
                .fetch_page(&page.url, &page.headers, cancel)
                .map_err(error)?,
        };
        cancel.check().map_err(error)?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(text::tr("Image exceeds byte limit"));
        }
        Ok(bytes)
    }
}
// Leave half of the 64MiB display allowance for replacements: renderer/model
// references can survive until the next frame even after a model update.
const READER_MODEL_BYTES: usize = 32 * 1024 * 1024;
struct Reader {
    chapter: ChapterId,
    source: Option<Source>,
    pages: Vec<PageInput>,
    ratios: Vec<f32>,
    requested: BTreeSet<usize>,
    retained: BTreeSet<usize>,
    desired: BTreeSet<usize>,
    failed: BTreeSet<usize>,
    budget_blocked: BTreeSet<usize>,
    desired_anchor: usize,
    decoded_bytes: BTreeMap<usize, usize>,
    // Track the requested quality, since source dimensions and the display
    // pixel limit can produce a texture narrower than that request.
    decoded_widths: BTreeMap<usize, u32>,
    decoded_windows: BTreeMap<usize, reader_engine::DisplayWindowPlan>,
    // Weak identity never extends the encoded-response memory cache budget.
    decoded_responses: BTreeMap<usize, Weak<Vec<u8>>>,
    native_dimensions: BTreeMap<usize, reader_engine::PageDimensions>,
    target_width: u32,
    viewport_width: f32,
    viewport_height: f32,
    anchor: ResumeAnchor,
    mode: ReadingMode,
    mode_choice: ModeChoice,
    automatic_mode: ReadingMode,
    mode_switching: bool,
    completed: bool,
}
impl Reader {
    fn decode_width(width: f32, scale_factor: f32) -> u32 {
        // Small layout changes share a decode, while high-density screens get
        // physical-pixel detail within a bounded display allocation.
        let physical = (width * scale_factor).clamp(128., 16384.);
        ((physical / 128.).ceil() as u32 * 128).clamp(128, 16384)
    }

    fn visible_end(&self) -> usize {
        let index = self.anchor.page_index.min(self.pages.len() - 1);
        let mut end = index;
        if self.mode == ReadingMode::Webtoon && self.viewport_width > 0. {
            let mut covered =
                self.ratios[index] * self.viewport_width * (1. - self.anchor.offset() as f32);
            while end + 1 < self.pages.len() && covered < self.viewport_height {
                end += 1;
                covered += self.ratios[end] * self.viewport_width;
            }
        }
        end
    }
    fn visible_span(&self, index: usize) -> (f64, f64) {
        self.span_with_ratio(index, self.ratios[index])
    }
    fn span_with_ratio(&self, index: usize, actual_ratio: f32) -> (f64, f64) {
        if self.mode != ReadingMode::Webtoon {
            return if index == self.anchor.page_index {
                (0., 1.)
            } else {
                (0., 0.)
            };
        }
        let page_start = self.ratios[..index]
            .iter()
            .map(|r| f64::from(*r))
            .sum::<f64>();
        let anchor = self.anchor.page_index.min(self.pages.len() - 1);
        let correction = if index < anchor {
            f64::from(actual_ratio - self.ratios[index])
        } else {
            0.
        };
        let viewport_start = self.ratios[..anchor]
            .iter()
            .map(|r| f64::from(*r))
            .sum::<f64>()
            + correction
            + f64::from(if index == anchor {
                actual_ratio
            } else {
                self.ratios[anchor]
            }) * self.anchor.offset();
        let viewport_end =
            viewport_start + f64::from(self.viewport_height / self.viewport_width.max(1.));
        let ratio = f64::from(actual_ratio);
        (
            ((viewport_start - page_start) / ratio).clamp(0., 1.),
            ((viewport_end - page_start) / ratio).clamp(0., 1.),
        )
    }
    fn window_request(
        &self,
        index: usize,
        dimensions: reader_engine::PageDimensions,
    ) -> Option<(u32, (f64, f64), usize)> {
        if index >= self.pages.len()
            || self.ratios.len() != self.pages.len()
            || dimensions.width == 0
            || dimensions.height == 0
        {
            return None;
        }
        let ratio = dimensions.height as f32 / dimensions.width as f32;
        let width = if self.mode == ReadingMode::Webtoon {
            self.target_width
        } else {
            Self::decode_width(
                self.target_width as f32
                    * (self.viewport_height / (ratio * self.viewport_width.max(1.))).min(1.),
                1.,
            )
        };
        let anchor = self.anchor.page_index.min(self.pages.len() - 1);
        let mut end = anchor;
        if self.mode == ReadingMode::Webtoon {
            let ratio_at = |i| if i == index { ratio } else { self.ratios[i] };
            let mut covered =
                ratio_at(anchor) * self.viewport_width * (1. - self.anchor.offset() as f32);
            while end + 1 < self.pages.len() && covered < self.viewport_height {
                end += 1;
                covered += ratio_at(end) * self.viewport_width;
            }
        }
        Some((
            width,
            self.span_with_ratio(index, ratio),
            (READER_MODEL_BYTES / (end - anchor + 1)).min(reader_engine::DISPLAY_WINDOW_BYTES),
        ))
    }
    fn has_visible_pixels(&self, index: usize) -> bool {
        self.retained.contains(&index)
            && self.decoded_windows.get(&index).is_some_and(|plan| {
                let (start, end) = self.visible_span(index);
                plan.covers(start, end)
            })
    }
    fn quality_width(&self, index: usize) -> u32 {
        if self.mode == ReadingMode::Webtoon {
            return self.target_width;
        }
        let fitted =
            (self.viewport_height / (self.ratios[index] * self.viewport_width.max(1.))).min(1.);
        Self::decode_width(self.target_width as f32 * fitted, 1.)
    }
    fn window_budget(&self) -> usize {
        let visible = self.visible_end() + 1 - self.anchor.page_index.min(self.pages.len() - 1);
        (READER_MODEL_BYTES / visible).min(reader_engine::DISPLAY_WINDOW_BYTES)
    }
    fn needs_window(&self, index: usize) -> bool {
        self.decoded_windows.get(&index).is_some_and(|plan| {
            let (start, end) = self.visible_span(index);
            !plan.covers_with_margin(start, end) || plan.byte_len() > self.window_budget()
        })
    }

    // The worker retains one decoded original. Decoding an offscreen page
    // before a long visible page's final window would evict that original and
    // make its next scroll window decompress the entire source again.
    fn defers_prefetch(&self, target: usize) -> bool {
        if self.pages.is_empty() {
            return false;
        }
        let visible = self.anchor.page_index.min(self.pages.len() - 1)..=self.visible_end();
        !visible.contains(&target)
            && visible.into_iter().any(|index| {
                self.retained.contains(&index)
                    && self
                        .decoded_windows
                        .get(&index)
                        .is_some_and(|plan| plan.first_row + plan.rows < plan.full_height)
            })
    }
    fn start_image(&mut self, index: usize) -> bool {
        if !self.desired.contains(&index) || self.defers_prefetch(index) {
            self.requested.remove(&index);
            false
        } else {
            true
        }
    }

    fn retained_bytes_without(&self, index: usize) -> usize {
        self.decoded_bytes
            .iter()
            .filter(|(page, _)| **page != index)
            .map(|(_, bytes)| *bytes)
            .sum()
    }

    fn page_id(&self, index: usize) -> String {
        match &self.pages[index] {
            PageInput::Local { id, .. } => id.clone(),
            PageInput::Remote(p) => p.id.clone(),
        }
    }
}
fn task_window_request(
    reader: &Reader,
    generation: u64,
    task: &ImageTask,
    dimensions: reader_engine::PageDimensions,
) -> Result<(u32, (f64, f64), usize)> {
    if generation != task.generation {
        return Err(text::tr("Operation cancelled"));
    }
    reader
        .window_request(task.index, dimensions)
        .ok_or_else(|| text::tr("Operation cancelled"))
}

/// Encoded chapter bytes are lossless, generation-scoped and memory-only.
/// Re-windowing a long page must not repeat a potentially slow source request.
#[derive(Default)]
struct EncodedPages {
    generation: u64,
    pages: BTreeMap<usize, Arc<Vec<u8>>>,
    order: std::collections::VecDeque<usize>,
    bytes: usize,
    contexts: BTreeMap<usize, String>,
}
impl EncodedPages {
    fn read(
        &mut self,
        generation: u64,
        index: usize,
        fetch: impl FnOnce() -> Result<Vec<u8>>,
    ) -> Result<Arc<Vec<u8>>> {
        self.read_scoped(generation, index, "", fetch)
    }
    fn read_scoped(
        &mut self,
        generation: u64,
        index: usize,
        context: &str,
        fetch: impl FnOnce() -> Result<Vec<u8>>,
    ) -> Result<Arc<Vec<u8>>> {
        if self.generation != generation {
            self.pages.clear();
            self.order.clear();
            self.bytes = 0;
            self.generation = generation;
            self.contexts.clear();
        }
        if self.contexts.get(&index).is_some_and(|old| old != context) {
            if let Some(old) = self.pages.remove(&index) {
                self.bytes -= old.len();
            }
            self.order.retain(|i| *i != index);
            self.contexts.remove(&index);
        }
        if let Some(bytes) = self.pages.get(&index) {
            self.order.retain(|i| *i != index);
            self.order.push_back(index);
            return Ok(bytes.clone());
        }
        let bytes = Arc::new(fetch()?);
        while self.bytes + bytes.len() > 32 * 1024 * 1024 {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(old) = self.pages.remove(&old) {
                self.bytes -= old.len();
            }
            self.contexts.remove(&old);
        }
        if bytes.len() <= 32 * 1024 * 1024 {
            self.bytes += bytes.len();
            self.pages.insert(index, bytes.clone());
            self.order.push_back(index);
            self.contexts.insert(index, context.into());
        }
        Ok(bytes)
    }
}
fn resume_chapter<'a>(
    chapters: &'a [Chapter],
    progress: &[ReadingProgress],
) -> Option<&'a Chapter> {
    progress
        .iter()
        .filter(|p| !p.completed)
        .filter_map(|p| {
            chapters
                .iter()
                .find(|c| c.id == p.chapter)
                .map(|c| (p.last_read_at, c))
        })
        .max_by_key(|(read_at, _)| *read_at)
        .map(|(_, c)| c)
        .or_else(|| {
            chapters
                .iter()
                .find(|c| !progress.iter().any(|p| p.chapter == c.id && p.completed))
        })
        .or_else(|| chapters.first())
}
fn resume_label(chapter: &Chapter) -> String {
    chapter
        .number
        .as_deref()
        .filter(|n| !n.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| chapter.title.clone())
}
struct CatalogueState {
    source_id: String,
    rows: Vec<Series>,
    query: String,
    catalogue: String,
    page: usize,
    has_more: bool,
    page_error: String,
    selected: usize,
    filters_active: bool,
}
#[derive(Default)]
struct View {
    section: i32,
    rows: Vec<Row>,
    download_states: BTreeMap<ChapterId, reader_downloads::JobStatus>,
    download_refreshing: bool,
    download_refresh_again: bool,
    download_revision: u64,
    choices: Vec<Choice>,
    selected: usize,
    series: Option<Series>,
    chapters: Vec<Chapter>,
    reader: Option<Reader>,
    catalogue: String,
    query: String,
    page: usize,
    has_more: bool,
    catalogue_page_error: String,
    pending: Option<ReadingProgress>,
    category: usize,
    settings_package: Option<String>,
    source_filters: crate::source_filters::FilterSet,
    source_browser: crate::source_browser::SourceBrowser,
    source_filters_loaded: bool,
    filters_active: bool,
    saved_catalogue: Option<CatalogueState>,
    artwork_rows: Vec<Option<Series>>,
    artwork_seen: BTreeMap<usize, std::time::Instant>,
    artwork_pruned: Option<std::time::Instant>,
}
fn reader_series_title(view: &View) -> Option<&str> {
    let reader = view.reader.as_ref()?;
    view.series
        .as_ref()
        .filter(|series| series.id == reader.chapter.series)
        .map(|series| series.title.as_str())
}
struct ImageTask {
    generation: u64,
    index: usize,
}
struct ImageCompletion(mpsc::SyncSender<()>);
impl Drop for ImageCompletion {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}
fn wait_image_completion<T>(
    pending: &Arc<Mutex<Option<T>>>,
    ack: &mpsc::Receiver<()>,
    current: impl Fn() -> bool,
) -> bool {
    loop {
        match ack.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return true,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !current() {
                    // A cancelled generation can release queued pixel buffers even
                    // while its UI callback is waiting behind another event.
                    pending.lock().unwrap_or_else(|e| e.into_inner()).take();
                    return false;
                }
            }
        }
    }
}
fn update_entry_membership(
    state: &mut reader_domain::ReaderState,
    series: &reader_domain::SeriesId,
    action: &str,
) -> reader_domain::Result<()> {
    if let Some(status) = action.strip_prefix("status:") {
        let status = reader_domain::ReadingStatus::from_action(status.parse().unwrap_or(-1))
            .ok_or_else(|| reader_domain::Error::Invalid("Unknown reading status".into()))?;
        let entry = state
            .library
            .iter_mut()
            .find(|entry| &entry.series == series)
            .ok_or_else(|| {
                reader_domain::Error::Invalid("Title is no longer in your library.".into())
            })?;
        entry.reading_status = status;
    } else if let Some(category) = action.strip_prefix("category:") {
        if !state.categories.iter().any(|entry| entry.id == category) {
            return Err(reader_domain::Error::Invalid(text::tr(
                "Category is no longer available.",
            )));
        }
        let entry = state
            .library
            .iter_mut()
            .find(|entry| &entry.series == series)
            .ok_or_else(|| {
                reader_domain::Error::Invalid(text::tr("Title is no longer in your library."))
            })?;
        if !entry.categories.remove(category) {
            entry.categories.insert(category.to_owned());
        }
    } else if action == "remove" {
        state.library.retain(|entry| &entry.series != series);
    } else if action == "add" {
        if !state.library.iter().any(|entry| &entry.series == series) {
            state.library.push(reader_domain::LibraryEntry {
                reading_status: Default::default(),
                series: series.clone(),
                categories: BTreeSet::new(),
                preferences: None,
                bookmarked: false,
                added_at: now(),
            });
        }
    } else {
        return Err(reader_domain::Error::Invalid(text::tr(
            "Unknown title action.",
        )));
    }
    Ok(())
}
struct Controller {
    services: Services,
    progress_checkpoint: crate::progress_checkpoint::ProgressWriter,
    artwork: crate::artwork::ArtworkLoader,
    chapter_cache: Arc<crate::chapter_cache::ChapterCache>,
    image_worker_live: AtomicBool,
    view: Mutex<View>,
    generation: AtomicU64,
    cancel: Mutex<CancelToken>,
    images: mpsc::Sender<ImageTask>,
    download_serial: Mutex<()>,
    active_image: Mutex<Option<(usize, CancelToken)>>,
    metadata_refresh: Arc<crate::metadata_refresh::Scheduler>,
}
impl Controller {
    fn metadata_refresh_allowed(&self, ui: &AppWindow) -> bool {
        crate::metadata_refresh::foreground()
            && crate::metadata_refresh::surface_allowed(self.view().section)
            && ui.window().is_visible()
            && !ui.window().is_minimized()
            && !ui.get_busy()
    }
    fn refresh_metadata(self: &Arc<Self>, ui: &AppWindow) {
        if !self.metadata_refresh_allowed(ui) {
            self.metadata_refresh.pause();
            return;
        }
        let weak = ui.as_weak();
        let app = Arc::downgrade(self);
        self.metadata_refresh.tick(self.services.clone(), move || {
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if let Some(app) = app.upgrade()
                    && app.metadata_refresh_allowed(&ui)
                {
                    app.render(&ui);
                }
            });
        });
    }
    fn view(&self) -> std::sync::MutexGuard<'_, View> {
        self.view.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn invalidate(&self) -> (u64, CancelToken) {
        self.metadata_refresh.pause();
        self.artwork.invalidate();
        self.chapter_cache.deactivate();
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let mut current = self.cancel.lock().unwrap_or_else(|e| e.into_inner());
        current.cancel();
        if let Some((_, cancel)) = self
            .active_image
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            cancel.cancel();
        }
        *current = CancelToken::default();
        (generation, current.clone())
    }
    fn chapter_cache_maintenance(self: &Arc<Self>, ui: &AppWindow, clear: bool) {
        if self
            .chapter_cache
            .maintenance_busy
            .swap(true, Ordering::AcqRel)
        {
            return;
        }
        if clear {
            self.chapter_cache.invalidate_writes();
        }
        let settings = ui.global::<crate::ImageStorageSettings>();
        settings.set_chapter_cache_busy(true);
        settings.set_chapter_cache_status("".into());
        let cache = self.chapter_cache.clone();
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let removed = clear.then(|| cache.clear());
            let (bytes, files) = cache.usage();
            let failed = cache.clone();
            if weak
                .upgrade_in_event_loop(move |ui| {
                    cache.maintenance_busy.store(false, Ordering::Release);
                    let settings = ui.global::<crate::ImageStorageSettings>();
                    settings.set_chapter_cache_busy(false);
                    settings.set_chapter_cache_info(
                        format!(
                            "{}: {files} {} · {:.1} MiB",
                            text::tr("Viewed chapter cache"),
                            text::tr("files"),
                            bytes as f64 / (1024. * 1024.)
                        )
                        .into(),
                    );
                    if let Some((_, count)) = removed {
                        settings.set_chapter_cache_status(
                            format!("{}: {count}", text::tr("Temporary chapter files removed"))
                                .into(),
                        );
                    }
                })
                .is_err()
            {
                failed.maintenance_busy.store(false, Ordering::Release);
            }
        });
    }
    fn flush(&self) {
        self.checkpoint_progress();
        if let Err(error) = self.progress_checkpoint.flush(None) {
            eprintln!("Nova Reader: could not save reading position: {error}");
        }
    }
    fn checkpoint_progress(&self) {
        // Keep the take/enqueue handoff ordered with scroll updates and Android
        // lifecycle flushes. Enqueue only changes memory; the blocking barrier
        // in flush() runs after this view guard has been released.
        let mut view = self.view();
        if let Some(progress) = view.pending.take()
            && let Err(error) = self.progress_checkpoint.enqueue(progress)
        {
            eprintln!("Nova Reader: could not queue reading position: {error}");
        }
    }
    fn task<T: Send + 'static>(
        self: &Arc<Self>,
        ui: &AppWindow,
        work: impl FnOnce(&Arc<Self>, &CancelToken) -> Result<T> + Send + 'static,
        complete: impl FnOnce(&Arc<Self>, &AppWindow, T) + Send + 'static,
    ) {
        self.flush();
        let (generation, cancel) = self.invalidate();
        ui.set_busy(true);
        ui.set_status("".into());
        let app = self.clone();
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let result = work(&app, &cancel);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if app.generation.load(Ordering::Acquire) != generation {
                    return;
                }
                ui.set_busy(false);
                match result {
                    Ok(value) => complete(&app, &ui, value),
                    Err(error) => ui.set_status(text::tr(&error).into()),
                }
            });
        });
    }
    fn sources(&self, adult: bool) -> Result<Vec<Choice>> {
        self.source_choices(adult, false)
    }
    fn source_choices(&self, adult: bool, include_hidden: bool) -> Result<Vec<Choice>> {
        let mut choices = Vec::new();
        for package in self
            .services
            .sources
            .list()
            .map_err(error)?
            .into_iter()
            .filter(|s| !s.nsfw || adult)
        {
            let variants = if package.enabled {
                self.services
                    .sources
                    .source(&package.id)
                    .and_then(|s| s.call("capabilities", &json!({})))
                    .ok()
                    .and_then(|r| r.get("sources").and_then(|s| s.as_array()).cloned())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            if variants.is_empty() {
                choices.push(Choice {
                    id: package.id.clone(),
                    package: package.id,
                    name: format!(
                        "{}{}",
                        package.name,
                        if package.enabled { "" } else { " (disabled)" }
                    ),
                    enabled: package.enabled,
                    filters: false,
                    language: package.language.clone(),
                    latest: false,
                });
            } else {
                for variant in variants {
                    let id = variant
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&package.id);
                    // A package cannot claim another package's source identity.
                    if id != package.id && !id.starts_with(&format!("{}.", package.id)) {
                        return Err("Source advertised an invalid identity".into());
                    }
                    choices.push(Choice {
                        id: id.into(),
                        package: package.id.clone(),
                        name: format!(
                            "{} · {}",
                            package.name,
                            variant
                                .get("language")
                                .and_then(|v| v.as_str())
                                .unwrap_or(&package.language)
                        ),
                        enabled: package.enabled,
                        language: variant
                            .get("language")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&package.language)
                            .to_owned(),
                        latest: variant
                            .get("operations")
                            .and_then(|v| v.as_array())
                            .is_some_and(|v| v.iter().any(|op| op.as_str() == Some("browse"))),
                        filters: variant
                            .get("operations")
                            .and_then(|v| v.as_array())
                            .is_some_and(|v| v.iter().any(|op| op.as_str() == Some("filters"))),
                    });
                }
            }
        }
        if !include_hidden {
            let mut disabled = BTreeMap::new();
            choices.retain(|choice| {
                !disabled
                    .entry(choice.package.clone())
                    .or_insert_with(|| crate::source_catalogs::disabled(&choice.package))
                    .contains(&choice.id)
            });
        }
        Ok(choices)
    }
    fn catalogue_choices(&self, package: &str) -> Result<(Vec<Choice>, BTreeSet<String>)> {
        Ok((
            self.source_choices(true, true)?
                .into_iter()
                .filter(|choice| choice.package == package)
                .collect(),
            crate::source_catalogs::disabled(package),
        ))
    }
    fn render(&self, ui: &AppWindow) {
        let snapshot = match self.services.domain.snapshot() {
            Ok(s) => s,
            Err(e) => {
                ui.set_status(error(e).into());
                return;
            }
        };
        let mut view = self.view();
        if !crate::metadata_refresh::surface_allowed(view.section) {
            self.metadata_refresh.pause();
        }
        ui.set_section(view.section);
        if view.section == 6 {
            ui.set_series_title(reader_series_title(&view).unwrap_or("Series").into());
        }
        let builtins = reader_domain::ReadingStatus::BUILTINS;
        if view.category > builtins.len() + snapshot.categories.len() {
            view.category = 0;
        }
        ui.set_category_index(view.category as i32);
        let category_id = view
            .category
            .checked_sub(builtins.len() + 1)
            .and_then(|index| snapshot.categories.get(index))
            .map(|category| category.id.clone());
        let status_filter = view
            .category
            .checked_sub(1)
            .and_then(|index| builtins.get(index))
            .copied();
        ui.set_categories(ModelRc::new(VecModel::from(
            std::iter::once(text::tr("All").into())
                .chain(
                    builtins
                        .iter()
                        .map(|status| text::tr(status.label()).into()),
                )
                .chain(
                    snapshot
                        .categories
                        .iter()
                        .map(|category| category.name.clone().into()),
                )
                .collect::<Vec<_>>(),
        )));
        let (heading, caption) = match view.section {
            0 => ("My Library", ""),
            1 => ("Updates", ""),
            2 => ("History", ""),
            3 => ("Browse", ""),
            4 => ("More", "Make this reading space your own."),
            5 => (
                "Chapters",
                "Reading position and downloads stay with you when a source goes away.",
            ),
            7 => ("Downloads", "Your chapters, ready whenever you are."),
            8 => ("Source settings", "Settings belong to this profile."),
            9 => (
                "Local chapters",
                "Import chapter archives and image folders.",
            ),
            10 => ("Settings", "Make Nova Reader yours."),
            11 => ("Categories", "Organize your library."),
            12 => ("Data", "Backup and import."),
            13 => ("About", "Information about Nova Reader."),
            _ => ("Reader", ""),
        };
        ui.set_heading(text::tr(heading).into());
        ui.set_caption(text::tr(caption).into());
        match view.section {
            0 => {
                let query = view.query.to_lowercase();
                let mut series: Vec<_> = snapshot
                    .library
                    .iter()
                    .filter(|l| {
                        category_id
                            .as_ref()
                            .is_none_or(|id| l.categories.contains(id))
                            && status_filter.is_none_or(|status| {
                                reader_domain::library_bucket(
                                    l,
                                    &snapshot.chapters,
                                    &snapshot.progress,
                                    snapshot
                                        .series
                                        .iter()
                                        .find(|series| series.id == l.series)
                                        .map(|series| series.publication_status)
                                        .unwrap_or_default(),
                                ) == status
                            })
                    })
                    .filter_map(|l| snapshot.series.iter().find(|s| s.id == l.series))
                    .filter(|s| s.title.to_lowercase().contains(&query))
                    .cloned()
                    .collect();
                match ui.global::<Appearance>().get_library_sort() {
                    0 => series.sort_by_key(|s| {
                        std::cmp::Reverse(
                            snapshot
                                .library
                                .iter()
                                .find(|l| l.series == s.id)
                                .map(|l| l.added_at)
                                .unwrap_or(0),
                        )
                    }),
                    2 => series.sort_by_key(|s| {
                        std::cmp::Reverse(
                            snapshot
                                .chapters
                                .iter()
                                .filter(|c| {
                                    c.id.series == s.id
                                        && !snapshot
                                            .progress
                                            .iter()
                                            .any(|p| p.chapter == c.id && p.completed)
                                })
                                .count(),
                        )
                    }),
                    _ => series.sort_by_key(|s| s.title.to_lowercase()),
                }
                view.rows = series.into_iter().map(Row::Series).collect();
            }
            1 => {
                view.rows = snapshot
                    .chapters
                    .iter()
                    .filter(|c| {
                        snapshot.library.iter().any(|l| l.series == c.id.series)
                            && !snapshot
                                .progress
                                .iter()
                                .any(|p| p.chapter == c.id && p.completed)
                    })
                    .rev()
                    .cloned()
                    .map(Row::Chapter)
                    .collect()
            }
            2 => {
                view.rows = snapshot
                    .history
                    .iter()
                    .rev()
                    .filter_map(|h| snapshot.chapters.iter().find(|c| c.id == h.chapter))
                    .cloned()
                    .map(Row::Chapter)
                    .collect()
            }
            5 => {
                if let Some(series) = &view.series {
                    let detail_key =
                        serde_json::to_string(&series.id).expect("SeriesId serializes");
                    if ui.get_detail_series_key().as_str() != detail_key {
                        ui.set_chapters_descending(crate::reading_preferences::chapter_descending(
                            &series.id,
                        ));
                    }
                    ui.set_detail_series_key(detail_key.into());
                    ui.set_heading(series.title.clone().into());
                    ui.set_series_title(series.title.clone().into());
                    ui.set_series_description(series.description.clone().into());
                    ui.set_series_metadata(
                        std::iter::once(series.authors.join(", "))
                            .filter(|s| !s.is_empty())
                            .chain(std::iter::once(series.id.source_id.clone()))
                            .collect::<Vec<_>>()
                            .join(" · ")
                            .into(),
                    );
                    ui.set_series_genres(ModelRc::new(VecModel::from(
                        series
                            .genres
                            .iter()
                            .map(|s| s.clone().into())
                            .collect::<Vec<_>>(),
                    )));
                    let key = crate::artwork::key(series);
                    ui.set_series_artwork_key(key.into());
                    ui.set_series_poster(self.artwork.cached_image(series).unwrap_or_default());
                    ui.set_in_library(snapshot.library.iter().any(|l| l.series == series.id));
                    view.chapters = snapshot
                        .chapters
                        .iter()
                        .filter(|c| c.id.series == series.id)
                        .cloned()
                        .collect();
                    view.chapters.sort_by_key(|c| c.source_order);
                    ui.set_resume_chapter(
                        resume_chapter(&view.chapters, &snapshot.progress)
                            .map(resume_label)
                            .unwrap_or_default()
                            .into(),
                    );
                    view.rows = view.chapters.iter().cloned().map(Row::Chapter).collect();
                }
            }
            _ => {}
        }
        if view.section == 1 {
            view.rows.sort_by_key(|row| match row {
                Row::Chapter(c) => (
                    std::cmp::Reverse(c.uploaded_at.unwrap_or(0)),
                    std::cmp::Reverse(c.source_order),
                ),
                _ => (std::cmp::Reverse(0), std::cmp::Reverse(0)),
            });
            let label = nova_storage::get_str("reader:last-update")
                .and_then(|s| s.parse::<i64>().ok())
                .map(|t| {
                    let stamp = crate::timeline::TimelineClock::now().history(t);
                    format!(
                        "{}: {} {}",
                        text::tr("Library last checked"),
                        stamp.group,
                        stamp.timestamp
                    )
                })
                .unwrap_or_else(|| text::tr("Refresh your library to check for new chapters."));
            ui.set_caption(label.into());
        }
        if view.section == 2 {
            view.rows.sort_by_key(|row| match row {
                Row::Chapter(c) => std::cmp::Reverse(
                    snapshot
                        .history
                        .iter()
                        .find(|h| h.chapter == c.id)
                        .map(|h| h.read_at)
                        .unwrap_or(0),
                ),
                _ => std::cmp::Reverse(0),
            });
            let query = view.query.to_lowercase();
            if !query.is_empty() {
                view.rows.retain(|row| match row {
                    Row::Chapter(c) => {
                        c.title.to_lowercase().contains(&query)
                            || snapshot
                                .series
                                .iter()
                                .find(|s| s.id == c.id.series)
                                .is_some_and(|s| s.title.to_lowercase().contains(&query))
                    }
                    _ => false,
                });
            }
        }
        if view.section == 5 && ui.get_chapters_descending() {
            view.rows.reverse();
        }
        let clock = crate::timeline::TimelineClock::now();
        view.artwork_rows = view
            .rows
            .iter()
            .map(|row| match row {
                Row::Series(s) => Some(s.clone()),
                Row::Chapter(c) => snapshot
                    .series
                    .iter()
                    .find(|s| s.id == c.id.series)
                    .cloned(),
                Row::Download(c) => snapshot.series.iter().find(|s| s.id == c.series).cloned(),
            })
            .collect();
        view.artwork_seen.clear();
        let items: Vec<_> = view
            .rows
            .iter()
            .map(|row| match row {
                Row::Series(s) => {
                    let chapters: Vec<_> = snapshot
                        .chapters
                        .iter()
                        .filter(|c| c.id.series == s.id)
                        .collect();
                    let unread = chapters
                        .iter()
                        .filter(|c| {
                            !snapshot
                                .progress
                                .iter()
                                .any(|p| p.chapter == c.id && p.completed)
                        })
                        .count();
                    let key = crate::artwork::key(s);
                    ItemRow {
                        series_key: serde_json::to_string(&s.id).unwrap_or_default().into(),
                        title: s.title.clone().into(),
                        subtitle: snapshot
                            .library
                            .iter()
                            .find(|entry| entry.series == s.id)
                            .map(|entry| {
                                format!(
                                    "{} · {}",
                                    text::tr(
                                        reader_domain::library_bucket(
                                            entry,
                                            &snapshot.chapters,
                                            &snapshot.progress,
                                            s.publication_status
                                        )
                                        .label()
                                    ),
                                    text::chapter_count(unread, chapters.len())
                                )
                            })
                            .unwrap_or_else(|| text::chapter_count(unread, chapters.len()))
                            .into(),
                        badge: s.id.source_id.clone().into(),
                        bookmarked: snapshot.library.iter().any(|l| l.series == s.id),
                        poster: self.artwork.cached_image(s).unwrap_or_default(),
                        artwork_key: key.into(),
                        completed: (chapters.len() - unread) as i32,
                        total: chapters.len() as i32,
                        ..ItemRow::default()
                    }
                }
                Row::Chapter(c) => {
                    let local_import = c.id.series.source_id == "local";
                    let download = if local_import {
                        None
                    } else {
                        view.download_states.get(&c.id)
                    };
                    let downloaded = download == Some(&reader_downloads::JobStatus::Complete);
                    let series = snapshot.series.iter().find(|s| s.id == c.id.series);
                    let key = series.map(crate::artwork::key).unwrap_or_default();
                    let read = snapshot
                        .progress
                        .iter()
                        .any(|p| p.chapter == c.id && p.completed);
                    let stamp = if view.section == 2 {
                        clock.history(
                            snapshot
                                .history
                                .iter()
                                .find(|h| h.chapter == c.id)
                                .map(|h| h.read_at)
                                .unwrap_or(0),
                        )
                    } else {
                        clock.release(c.uploaded_at)
                    };
                    let mut meta = Vec::new();
                    if c.uploaded_at.is_some() {
                        meta.push(clock.release_date(c.uploaded_at));
                    }
                    if let Some(scanlator) = &c.scanlator {
                        meta.push(scanlator.clone());
                    }
                    if let Some(language) = &c.language {
                        meta.push(language.clone());
                    }
                    if downloaded {
                        meta.push(text::tr("Downloaded"));
                    }
                    ItemRow {
                        title: if view.section == 1 || view.section == 2 {
                            series
                                .map(|s| s.title.clone())
                                .unwrap_or_else(|| c.title.clone())
                        } else {
                            c.title.clone()
                        }
                        .into(),
                        subtitle: if view.section == 1 || view.section == 2 {
                            c.title.clone()
                        } else {
                            String::new()
                        }
                        .into(),
                        badge: text::tr(if read { "Read" } else { "Unread" }).into(),
                        read,
                        downloaded,
                        has_download: download.is_some(),
                        local_import,
                        chapter_meta: meta.join(" · ").into(),
                        group: stamp.group.into(),
                        timestamp: stamp.timestamp.into(),
                        bookmarked: snapshot.bookmarks.iter().any(|b| b.chapter == c.id),
                        poster: series
                            .and_then(|series| self.artwork.cached_image(series))
                            .unwrap_or_default(),
                        artwork_key: key.into(),
                        ..ItemRow::default()
                    }
                }
                Row::Download(c) => ItemRow {
                    local_import: c.series.source_id == "local",
                    title: snapshot
                        .chapters
                        .iter()
                        .find(|ch| ch.id == *c)
                        .map(|ch| ch.title.clone())
                        .unwrap_or_else(|| c.local_id.clone())
                        .into(),
                    subtitle: "".into(),
                    badge: "".into(),
                    bookmarked: false,
                    ..ItemRow::default()
                },
            })
            .collect();
        if view.section != 7 {
            ui.set_items(ModelRc::new(VecModel::from(items)));
        }
        ui.set_has_more(view.section == 3 && view.has_more);
        ui.set_catalogue_page_error(view.catalogue_page_error.clone().into());
        ui.set_sources(ModelRc::new(VecModel::from(
            view.choices
                .iter()
                .map(|c| c.name.clone().into())
                .collect::<Vec<_>>(),
        )));
        let source_choices = view
            .choices
            .iter()
            .enumerate()
            .map(|(i, c)| crate::source_browser::SourceChoice {
                id: c.id.clone(),
                name: c
                    .name
                    .strip_suffix(&format!(" · {}", c.language))
                    .unwrap_or(&c.name)
                    .to_owned(),
                language: c.language.clone(),
                enabled: c.enabled,
                latest: c.latest,
                index: i as i32,
            })
            .collect::<Vec<_>>();
        let mut source_rows = view.source_browser.rows(&source_choices);
        for row in &mut source_rows {
            if let Some(choice) = view.choices.get(row.index.max(0) as usize) {
                row.icon = crate::addon_icons::image(&choice.package);
            }
        }
        ui.set_source_rows(ModelRc::new(VecModel::from(source_rows)));
        ui.set_source_index(if view.choices.is_empty() {
            -1
        } else {
            view.selected.min(view.choices.len() - 1) as i32
        });
        if let Some(choice) = view.choices.get(view.selected) {
            ui.set_source_title(choice.name.clone().into());
            ui.set_source_has_filters(choice.filters && choice.enabled);
        }
        if view.section == 3 && ui.get_source_catalog_open() {
            ui.set_source_query(view.query.clone().into());
            ui.set_source_catalogue_kind(if !view.query.is_empty() {
                "search".into()
            } else if view.catalogue.is_empty() {
                "popular".into()
            } else {
                view.catalogue.clone().into()
            });
        }
    }
    fn extension_action(self: &Arc<Self>, ui: &AppWindow, package: &str, action: &str) {
        if action == "settings" {
            let package = package.to_owned();
            self.task(
                ui,
                move |app, _| {
                    let schema = app
                        .services
                        .sources
                        .settings_schema(&package)
                        .map_err(error)?;
                    let values = app.services.sources.settings(&package).map_err(error)?;
                    let catalogues = app.catalogue_choices(&package)?;
                    Ok((package, schema, values, catalogues))
                },
                |app, ui, (package, schema, values, catalogues)| {
                    {
                        let mut view = app.view();
                        view.section = 8;
                        view.settings_package = Some(package);
                    }
                    let rows = schema
                        .into_iter()
                        .map(|f| {
                            let value = values
                                .get(&f.key)
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            let selected = f
                                .options
                                .iter()
                                .position(|o| Some(o.as_str()) == value.as_str())
                                .unwrap_or(0) as i32;
                            SettingRow {
                                key: f.key.into(),
                                name: f.name.into(),
                                kind: match f.kind {
                                    reader_sources::SettingKind::Bool => "bool",
                                    reader_sources::SettingKind::Select => "select",
                                    reader_sources::SettingKind::Secret => "secret",
                                    reader_sources::SettingKind::Text => "text",
                                }
                                .into(),
                                value: value.as_str().unwrap_or_default().into(),
                                checked: value.as_bool().unwrap_or(false),
                                options: ModelRc::new(VecModel::from(
                                    f.options.into_iter().map(Into::into).collect::<Vec<_>>(),
                                )),
                                selected,
                            }
                        })
                        .collect::<Vec<_>>();
                    ui.set_source_catalog_settings(ModelRc::new(VecModel::from(catalogue_rows(
                        catalogues,
                    ))));
                    ui.set_settings(ModelRc::new(VecModel::from(rows)));
                    app.render(ui);
                },
            );
            return;
        }
        let current = self
            .services
            .sources
            .list()
            .ok()
            .and_then(|items| items.into_iter().find(|e| e.id == package));
        let Some(current) = current else {
            ui.set_status(text::tr("Source unavailable").into());
            return;
        };
        let result = match action {
            "remove" => self.services.sources.remove(package),
            "toggle" => self.services.sources.set_enabled(package, !current.enabled),
            _ => return,
        };
        if let Err(e) = result {
            ui.set_status(error(e).into());
        } else {
            self.navigate(ui, 3);
        }
    }
    fn select_source(self: &Arc<Self>, ui: &AppWindow, index: i32, kind: &str) {
        // Cancel before an unavailable/disabled choice can return early.
        self.flush();
        self.invalidate();
        ui.set_busy(false);
        ui.set_status("".into());
        ui.invoke_reset_catalogue_scroll();
        ui.set_catalogue_page_error("".into());
        ui.set_source_query("".into());
        {
            let mut view = self.view();
            let index = index.max(0) as usize;
            let Some(choice) = view.choices.get(index).cloned() else {
                return;
            };
            view.selected = index;
            if let Err(e) = view.source_browser.used(&choice.id) {
                eprintln!("Nova Reader: {e}");
            }
            view.source_filters = crate::source_filters::FilterSet::default();
            view.source_filters_loaded = false;
            view.filters_active = false;
            view.query.clear();
            view.page = 0;
            view.has_more = false;
            view.saved_catalogue = None;
            view.catalogue_page_error.clear();
            view.rows.clear();
            ui.set_source_title(choice.name.into());
            ui.set_source_has_filters(choice.filters && choice.enabled);
        }
        ui.set_items(ModelRc::default());
        ui.set_has_more(false);
        ui.set_source_catalog_open(true);
        ui.set_source_filters_open(false);
        self.browse(ui, kind, Some(String::new()), false);
    }
    fn request_artwork(&self, ui: &AppWindow, first: i32, last: i32) {
        // Viewport changes can arrive while render publishes a new model.
        // That render hydrates cached pixels itself; cold loads retry next turn.
        let mut view = match self.view.try_lock() {
            Ok(view) => view,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        if view.section == 5 {
            let series = view.series.clone();
            drop(view);
            if ui.get_series_poster().size().width == 0
                && let Some(series) = series
            {
                self.artwork.request(&series);
            }
            return;
        }
        if ![0, 1, 2, 3].contains(&view.section) {
            return;
        }
        let first = (first.max(0) as usize).min(view.artwork_rows.len());
        let last = (last.max(0) as usize).min(view.artwork_rows.len());
        let now = std::time::Instant::now();
        let model = ui.get_items();
        let mut requests = Vec::new();
        for i in first..last {
            view.artwork_seen.insert(i, now);
            if model
                .row_data(i)
                .is_some_and(|r| r.poster.size().width == 0)
                && let Some(s) = view.artwork_rows[i].clone()
            {
                requests.push(s);
            }
        }
        if view
            .artwork_pruned
            .is_none_or(|t| now.duration_since(t).as_secs() >= 1)
        {
            let desired = view
                .artwork_seen
                .iter()
                .filter(|(_, t)| now.duration_since(**t).as_millis() <= 600)
                .filter_map(|(i, _)| view.artwork_rows.get(*i).and_then(|s| s.as_ref()))
                .map(crate::artwork::key)
                .filter(|key| !key.is_empty())
                .collect::<BTreeSet<_>>();
            self.artwork.retain(&desired);
            view.artwork_pruned = Some(now);
        }
        drop(view);
        for series in requests {
            self.artwork.request(&series);
        }
    }
    fn navigate(self: &Arc<Self>, ui: &AppWindow, section: i32) {
        if section == 3 {
            ui.set_source_catalog_open(false);
        }
        self.flush();
        self.invalidate();
        ui.set_busy(false);
        ui.set_status("".into());
        if section != 6 {
            ui.set_pages(ModelRc::default());
            ui.set_total_ratio(0.0);
            self.view().reader = None;
        }
        {
            let mut view = self.view();
            view.section = section;
            view.has_more = false;
            if section == 0 {
                view.query = ui.get_library_query().to_string();
            } else if section != 3 {
                view.query.clear();
            }
        }
        if section == 3 {
            let adult = ui.get_show_adult();
            self.task(
                ui,
                move |app, _| app.browse_sources(adult),
                |app, ui, sources| app.publish_sources(ui, sources),
            );
        } else if section == 7 {
            self.download_list(ui);
        } else if section == 4 || section >= 9 {
            // These retained pages own their content. Publishing their route
            // must not deserialize/rebuild a potentially large library first.
            ui.set_section(section);
        } else {
            self.render(ui);
            if section == 5 {
                self.refresh_download_states(ui);
            }
        }
        self.refresh_metadata(ui);
    }
    fn return_to_catalogue(&self, ui: &AppWindow) {
        ui.invoke_prepare_catalogue_return();
        self.flush();
        self.invalidate();
        {
            let mut view = self.view();
            view.rows.clear();
            view.has_more = false;
            view.page = 0;
            view.catalogue_page_error.clear();
            if let Some(mut saved) = view.saved_catalogue.take()
                && let Some(selected) = view.choices.iter().position(|c| c.id == saved.source_id)
            {
                saved.selected = selected;
                view.rows = saved.rows.iter().cloned().map(Row::Series).collect();
                view.query = saved.query.clone();
                view.catalogue = saved.catalogue.clone();
                view.page = saved.page;
                view.has_more = saved.has_more;
                view.catalogue_page_error = saved.page_error.clone();
                view.selected = saved.selected;
                view.filters_active = saved.filters_active;
                view.saved_catalogue = Some(saved);
            }
            view.section = 3;
            view.reader = None;
        }
        ui.set_pages(ModelRc::default());
        ui.set_total_ratio(0.);
        ui.set_busy(false);
        ui.set_status("".into());
        ui.set_source_catalog_open(true);
        self.render(ui);
    }
    fn browse(self: &Arc<Self>, ui: &AppWindow, kind: &str, query: Option<String>, next: bool) {
        if next && ui.get_busy() {
            return;
        }
        if next {
            let view = self.view();
            if view.section != 3
                || !ui.get_source_catalog_open()
                || (!view.has_more && view.page > 0)
            {
                return;
            }
        }
        let choice = {
            let view = self.view();
            view.choices.get(view.selected).cloned()
        };
        let Some(choice) = choice else {
            ui.set_status(text::tr("Install and choose a source first.").into());
            return;
        };
        if !choice.enabled {
            ui.set_status(text::tr("Enable this source to browse it.").into());
            return;
        }
        let (op, query, page, catalogue, filters) = {
            let mut view = self.view();
            view.section = 3;
            view.catalogue_page_error.clear();
            if let Some(saved) = &mut view.saved_catalogue {
                saved.page_error.clear();
            }
            ui.set_catalogue_page_error("".into());
            if let Some(query) = query {
                view.query = query;
            }
            if !next {
                view.page = 0;
                view.has_more = false;
                view.saved_catalogue = None;
                ui.set_has_more(false);
                ui.set_catalogue_scroll_y(0.);
                view.rows.clear();
                ui.set_items(ModelRc::default());
                if !kind.is_empty() {
                    view.catalogue = kind.into();
                    view.filters_active = kind == "filtered";
                }
            }
            (
                if view.query.is_empty() && !view.filters_active {
                    "browse"
                } else {
                    "search"
                },
                view.query.clone(),
                if next {
                    view.page.saturating_add(1).max(1)
                } else {
                    1
                },
                view.catalogue.clone(),
                if view.filters_active {
                    view.source_filters.values()
                } else {
                    json!({})
                },
            )
        };
        let source_id = choice.id.clone();
        self.task(
            ui,
            move |app, cancel| {
                // Keep source failures inside the successful task envelope so
                // generation-checked completion owns pagination's retry state.
                Ok((|| {
                    let source = app
                        .services
                        .sources
                        .source(&choice.package)
                        .map_err(error)?;
                    let response = source
                        .call_cancelled(
                            op,
                            &json!({
                                "sourceId": choice.id,
                                "catalogue": catalogue,
                                "query": query,
                                "page": page,
                                "filters": filters,
                            }),
                            cancel,
                        )
                        .map_err(error)?;
                    let listing: reader_sources::Listing =
                        serde_json::from_value(response).map_err(error)?;
                    let series = listing
                        .items
                        .into_iter()
                        .map(|s| app.services.series(&choice.id, s))
                        .collect::<Vec<_>>();
                    Ok((series, listing.has_next_page))
                })())
            },
            move |app, ui, result| app.complete_browse(ui, source_id, page, next, result),
        );
    }
    fn complete_browse(
        &self,
        ui: &AppWindow,
        source_id: String,
        page: usize,
        next: bool,
        result: Result<(Vec<Series>, bool)>,
    ) {
        let mut view = self.view();
        let result = result.and_then(|(series, more)| {
            if more
                && !series.iter().any(|series| {
                    !view
                        .rows
                        .iter()
                        .any(|r| matches!(r, Row::Series(s) if s.id == series.id))
                })
            {
                Err(text::tr(
                    "The source returned no new titles. Retry to load this page again.",
                ))
            } else {
                Ok((series, more))
            }
        });
        let (series, more) = match result {
            Ok(result) => result,
            Err(error) => {
                view.catalogue_page_error = text::tr(&error);
                if let Some(saved) = &mut view.saved_catalogue {
                    saved.page_error = text::tr(&error);
                }
                ui.set_catalogue_page_error(view.catalogue_page_error.clone().into());
                return;
            }
        };
        view.catalogue_page_error.clear();
        if !next {
            view.rows.clear();
        }
        for series in series {
            if !view
                .rows
                .iter()
                .any(|r| matches!(r,Row::Series(s) if s.id == series.id))
            {
                view.rows.push(Row::Series(series));
            }
        }
        view.has_more = more;
        view.page = page;
        view.saved_catalogue = Some(CatalogueState {
            source_id,
            rows: view
                .rows
                .iter()
                .filter_map(|r| {
                    if let Row::Series(s) = r {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
                .collect(),
            query: view.query.clone(),
            catalogue: view.catalogue.clone(),
            page: view.page,
            has_more: more,
            selected: view.selected,
            filters_active: view.filters_active,
            page_error: String::new(),
        });
        drop(view);
        self.render(ui);
    }
    fn open_series(self: &Arc<Self>, ui: &AppWindow, series: Series) {
        ui.set_detail_origin(self.view().section);
        self.flush();
        {
            let mut view = self.view();
            view.series = Some(series.clone());
            view.section = 5;
        }
        let cached = self
            .services
            .domain
            .snapshot()
            .map(|s| s.chapters.iter().any(|c| c.id.series == series.id))
            .unwrap_or(false);
        // The catalogue already gives us enough to show the hero. Publish it
        // before starting provider work, so slow chapter lists never hold the
        // user on Browse or leave the previous entry's chapters on screen.
        ui.set_status("".into());
        ui.set_busy(false);
        self.render(ui);
        if cached {
            self.invalidate();
            self.refresh_download_states(ui);
            return;
        }
        self.task(
            ui,
            move |app, cancel| app.services.details(&series, cancel),
            |app, ui, (series, chapters)| {
                let mut view = app.view();
                view.series = Some(series);
                view.chapters = chapters;
                view.section = 5;
                drop(view);
                app.render(ui);
                app.refresh_download_states(ui);
            },
        );
    }
    fn open_chapter(self: &Arc<Self>, ui: &AppWindow, chapter: ChapterId) {
        ui.set_reader_osd(false);
        let origin = self.view().section;
        if origin != 5 && origin != 6 {
            ui.set_detail_origin(origin);
        }
        self.task(
            ui,
            move |app, cancel| {
                let state = app.services.domain.snapshot().map_err(error)?;
                let progress = state
                    .progress
                    .iter()
                    .find(|p| p.chapter == chapter)
                    .cloned();
                let completed = progress.as_ref().is_some_and(|p| p.completed);
                let mode_choice = reader_modes::load(
                    &chapter.series,
                    reader_modes::legacy_mode(&state, &chapter.series),
                )
                .map_err(error)?;
                let automatic_mode =
                    reader_modes::automatic_for_state(&state, &chapter.series, None);
                let mode = mode_choice.resolve(automatic_mode);
                let anchor = progress.map(|p| p.anchor).unwrap_or_default();
                if let Ok(offline) = app.services.downloads.offline(&chapter) {
                    let pages = offline
                        .pages
                        .into_iter()
                        .zip(app.services.downloads.job(&chapter).map_err(error)?.pages)
                        .map(|(path, page)| PageInput::Local {
                            path,
                            id: page.resource.id,
                        })
                        .collect::<Vec<_>>();
                    let ratios = pages
                        .iter()
                        .map(|p| match p {
                            PageInput::Local { path, .. } => std::fs::read(path)
                                .ok()
                                .and_then(|bytes| {
                                    reader_engine::inspect_image(
                                        &bytes,
                                        reader_engine::DecodeLimits::default(),
                                    )
                                    .ok()
                                })
                                .map(|d| d.height as f32 / d.width as f32)
                                .unwrap_or(1.5),
                            _ => 1.5,
                        })
                        .collect();
                    Ok(Reader {
                        chapter,
                        source: None,
                        pages,
                        ratios,
                        requested: BTreeSet::new(),
                        retained: BTreeSet::new(),
                        desired: BTreeSet::new(),
                        failed: BTreeSet::new(),
                        budget_blocked: BTreeSet::new(),
                        desired_anchor: usize::MAX,
                        decoded_bytes: BTreeMap::new(),
                        decoded_widths: BTreeMap::new(),
                        decoded_windows: BTreeMap::new(),
                        decoded_responses: BTreeMap::new(),
                        native_dimensions: BTreeMap::new(),
                        target_width: 0,
                        viewport_width: 0.,
                        viewport_height: 0.,
                        anchor,
                        mode,
                        mode_choice,
                        automatic_mode,
                        mode_switching: false,
                        completed,
                    })
                } else {
                    let (source, pages) = app.services.pages(&chapter, cancel)?;
                    let automatic_mode = reader_modes::automatic_for_state(
                        &state,
                        &chapter.series,
                        Some(&source.descriptor().name),
                    );
                    let mode = mode_choice.resolve(automatic_mode);

                    let ratios = pages
                        .iter()
                        .map(|p| {
                            p.width
                                .zip(p.height)
                                .filter(|(w, h)| *w > 0 && *h > 0)
                                .map(|(w, h)| h as f32 / w as f32)
                                .unwrap_or(1.5)
                        })
                        .collect();
                    Ok(Reader {
                        chapter,
                        source: Some(source),
                        pages: pages.into_iter().map(PageInput::Remote).collect(),
                        ratios,
                        requested: BTreeSet::new(),
                        retained: BTreeSet::new(),
                        desired: BTreeSet::new(),
                        failed: BTreeSet::new(),
                        budget_blocked: BTreeSet::new(),
                        desired_anchor: usize::MAX,
                        decoded_bytes: BTreeMap::new(),
                        decoded_widths: BTreeMap::new(),
                        decoded_windows: BTreeMap::new(),
                        decoded_responses: BTreeMap::new(),
                        native_dimensions: BTreeMap::new(),
                        target_width: 0,
                        viewport_width: 0.,
                        viewport_height: 0.,
                        anchor,
                        mode,
                        mode_choice,
                        automatic_mode,
                        mode_switching: false,
                        completed,
                    })
                }
            },
            |app, ui, mut reader| {
                let ids = (0..reader.pages.len())
                    .map(|i| reader.page_id(i))
                    .collect::<Vec<_>>();
                reader.anchor =
                    reader_engine::resolve_anchor(&reader.anchor, &ids).unwrap_or_default();
                let state = app.services.domain.snapshot().unwrap_or_default();
                reader.mode_choice = match reader_modes::load(
                    &reader.chapter.series,
                    reader_modes::legacy_mode(&state, &reader.chapter.series),
                ) {
                    Ok(choice) => choice,
                    Err(message) => {
                        ui.set_status(message.into());
                        return;
                    }
                };
                reader.mode = reader.mode_choice.resolve(reader.automatic_mode);
                // A manual selection while chapter metadata was loading must
                // win, and old-visible-reader image tasks need a fresh epoch.
                app.flush();
                app.invalidate();

                let mut view = app.view();
                view.series = state
                    .series
                    .iter()
                    .find(|s| s.id == reader.chapter.series)
                    .cloned();
                view.chapters = state
                    .chapters
                    .iter()
                    .filter(|c| c.id.series == reader.chapter.series)
                    .cloned()
                    .collect();
                view.chapters.sort_by_key(|c| c.source_order);
                let index = reader.anchor.page_index;
                let mode = reader.mode;
                let choice = reader.mode_choice;
                view.reader = Some(reader);
                view.section = 6;
                drop(view);
                app.mount_pages(ui);
                ui.set_reader_mode_choice(choice.selection());
                ui.set_reading_mode(mode_index(mode));
                ui.set_current_page(index as i32);
                app.restore_position(ui);
                app.render(ui);
                app.reflow(ui, ui.get_reader_width(), ui.get_reader_viewport_height());
                app.step_page(ui, 0);
            },
        );
    }
    fn mount_pages(&self, ui: &AppWindow) {
        let view = self.view();
        let Some(reader) = &view.reader else {
            return;
        };
        let mut position = 0.0;
        let rows = reader
            .ratios
            .iter()
            .enumerate()
            .map(|(i, ratio)| {
                let row = PageRow {
                    page_id: reader.page_id(i).into(),
                    image: slint::Image::default(),
                    tiles: ModelRc::default(),
                    ratio: *ratio,
                    position,
                    loaded: false,
                    error: "".into(),
                };
                position += ratio;
                row
            })
            .collect::<Vec<_>>();
        ui.set_total_ratio(position);
        ui.set_pages(ModelRc::new(VecModel::from(rows)));
    }
    fn restore_position(&self, ui: &AppWindow) {
        let view = self.view();
        let Some(reader) = &view.reader else {
            return;
        };
        let index = reader
            .anchor
            .page_index
            .min(reader.pages.len().saturating_sub(1));
        let offset = reader.ratios[..index].iter().sum::<f32>()
            + reader.ratios[index] * reader.anchor.offset() as f32;
        ui.set_scroll_y(-offset * ui.get_reader_width());
    }
    fn choose_reader_mode(
        &self,
        ui: &AppWindow,
        index: i32,
        write: impl FnOnce(&reader_domain::SeriesId, ModeChoice) -> Result<()>,
    ) {
        let (series, previous) = {
            let view = self.view();
            let Some(reader) = view.reader.as_ref() else {
                return;
            };
            (reader.chapter.series.clone(), reader.mode_choice)
        };
        let Some(choice) = ModeChoice::from_selection(index) else {
            ui.set_reader_mode_choice(previous.selection());
            return;
        };
        if let Err(message) = write(&series, choice) {
            ui.set_reader_mode_choice(previous.selection());
            ui.set_status(format!("{}: {message}", text::tr("Could not save reading mode")).into());
            return;
        }
        let mode = {
            let mut view = self.view();
            let Some(reader) = view.reader.as_mut() else {
                return;
            };
            if reader.chapter.series != series {
                return;
            }
            reader.mode_choice = choice;
            reader.mode = choice.resolve(reader.automatic_mode);
            reader.mode_switching = true;
            reader.budget_blocked.clear();
            reader.mode
        };
        ui.set_reader_mode_choice(choice.selection());
        ui.set_reading_mode(mode_index(mode));
        self.restore_position(ui);
        if let Some(reader) = self.view().reader.as_mut() {
            reader.mode_switching = false;
        }
        self.step_page(ui, 0);
    }
    fn reflow(&self, ui: &AppWindow, width: f32, height: f32) {
        if !width.is_finite() || width <= 0. || !height.is_finite() || height <= 0. {
            return;
        }
        let Ok(mut view) = self.view.try_lock() else {
            return;
        };
        if let Some(reader) = &mut view.reader {
            reader.viewport_width = width;
            reader.viewport_height = height;
        }
        drop(view);
        self.restore_position(ui);
        let index = { self.view().reader.as_ref().map(|r| r.anchor.page_index) };
        if let Some(index) = index {
            self.request_images(ui, index);
        }
    }
    fn position(self: &Arc<Self>, ui: &AppWindow, offset: f32, width: f32, viewport: f32) {
        if width <= 0.0 || !offset.is_finite() || !width.is_finite() {
            return;
        }
        let Ok(mut view) = self.view.try_lock() else {
            return;
        };
        let Some(reader) = &mut view.reader else {
            return;
        };
        if reader.mode != ReadingMode::Webtoon || reader.mode_switching {
            return;
        }
        // A changed layout restores the logical anchor before interpreting pixels.
        if reader.viewport_width > 0. && (reader.viewport_width - width).abs() > 0.5 {
            return;
        }
        reader.viewport_width = width;
        reader.viewport_height = viewport;

        let mut remaining = offset.max(0.0) / width;
        let mut index = 0;
        while index + 1 < reader.pages.len() && remaining >= reader.ratios[index] {
            remaining -= reader.ratios[index];
            index += 1;
        }
        let fraction = (remaining / reader.ratios[index]).clamp(0.0, 1.0);
        reader.anchor = ResumeAnchor::new(reader.page_id(index), index, f64::from(fraction))
            .unwrap_or_default();
        let last = reader.pages.len() - 1;
        let complete = (index..=last).all(|i| reader.has_visible_pixels(i))
            && offset + viewport >= reader.ratios.iter().sum::<f32>() * width - 2.;
        reader.completed |= complete;
        let progress = ReadingProgress {
            chapter: reader.chapter.clone(),
            anchor: reader.anchor.clone(),
            mode: reader.mode,
            last_read_at: now(),
            completed: reader.completed,
        };
        view.pending = Some(progress);
        drop(view);
        ui.set_current_page(index as i32);
        self.request_images(ui, index);
    }
    fn request_images(&self, ui: &AppWindow, index: usize) {
        let generation = self.generation.load(Ordering::Acquire);
        let mut view = self.view();
        if view.section != 6 {
            return;
        }
        let chapter_navigation = view
            .reader
            .as_ref()
            .filter(|reader| reader.desired_anchor != index)
            .map(|reader| {
                let position = view
                    .chapters
                    .iter()
                    .position(|chapter| chapter.id == reader.chapter);
                (
                    position.is_some_and(|i| i > 0),
                    position.is_some_and(|i| i + 1 < view.chapters.len()),
                )
            });
        let Some(reader) = &mut view.reader else {
            return;
        };
        if reader.source.is_some() {
            self.chapter_cache.activate(generation, &reader.chapter);
        }
        if reader.viewport_width <= 0. {
            reader.viewport_width = ui.get_reader_width();
        }
        if reader.viewport_height <= 0. {
            reader.viewport_height = ui.get_reader_viewport_height();
        }
        let index = index.min(reader.pages.len().saturating_sub(1));
        let target_width = Reader::decode_width(
            reader.viewport_width.max(ui.get_reader_width()),
            ui.window().scale_factor(),
        );
        let anchor_changed = reader.desired_anchor != index;
        let width_changed = reader.target_width != target_width;
        if anchor_changed || width_changed {
            reader.budget_blocked.clear();
            reader.desired_anchor = index;
            reader.target_width = target_width;
        }
        let end = reader.visible_end();
        let first_desired = index.saturating_sub(1);
        let last_desired = (end + 1).min(reader.pages.len() - 1);
        // Scroll ticks within already covered strips do no model, navigation,
        // set-allocation or task work. Visible coverage still gets checked on
        // every tick, including the advance margin before a strip runs out.
        if !anchor_changed
            && !width_changed
            && reader.desired.first() == Some(&first_desired)
            && reader.desired.last() == Some(&last_desired)
            && reader.desired.iter().all(|target| {
                reader.defers_prefetch(*target)
                    || reader.requested.contains(target)
                    || reader.failed.contains(target)
                    || (reader.budget_blocked.contains(target) && !(index..=end).contains(target))
                    || (reader.retained.contains(target)
                        && reader.decoded_widths.get(target).copied().unwrap_or(0)
                            >= reader.quality_width(*target)
                        && !reader.needs_window(*target))
            })
        {
            return;
        }
        // A blocked prefetch becomes eligible as soon as it is visible. Failed
        // network/codec pages remain governed by the explicit Retry action.
        reader
            .budget_blocked
            .retain(|target| !(index..=end).contains(target));
        let keep = (first_desired..=last_desired).collect::<BTreeSet<_>>();
        reader.desired = keep.clone();
        if let Some((active, cancel)) = self
            .active_image
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            && !keep.contains(active)
        {
            cancel.cancel();
        }
        let model = ui.get_pages();
        for old in reader
            .retained
            .difference(&keep)
            .copied()
            .collect::<Vec<_>>()
        {
            if let Some(mut row) = model.row_data(old) {
                row.image = slint::Image::default();
                row.tiles = ModelRc::default();
                row.loaded = false;
                model.set_row_data(old, row);
            }
            reader.decoded_bytes.remove(&old);
            reader.decoded_widths.remove(&old);
            reader.decoded_windows.remove(&old);
            reader.decoded_responses.remove(&old);
        }
        reader.retained.retain(|i| keep.contains(i));
        // Visible images precede the one-page prefetch window.
        for target in (index..=end).chain(keep.iter().copied().filter(|i| *i < index || *i > end)) {
            if reader.defers_prefetch(target) {
                continue;
            }
            if !reader.requested.contains(&target)
                && (!reader.retained.contains(&target)
                    || reader.decoded_widths.get(&target).copied().unwrap_or(0)
                        < reader.quality_width(target)
                    || reader.needs_window(target))
                && !reader.failed.contains(&target)
                && !reader.budget_blocked.contains(&target)
            {
                reader.requested.insert(target);
                let _ = self.images.send(ImageTask {
                    generation,
                    index: target,
                });
            }
        }
        if anchor_changed {
            ui.set_position_label(text::page_position(index, reader.pages.len()).into());
            if let Some((previous, next)) = chapter_navigation {
                ui.set_can_previous(previous);
                ui.set_can_next(next);
            }
        }
    }
    fn image_worker(
        app: Weak<Self>,
        receiver: mpsc::Receiver<ImageTask>,
        ui: slint::Weak<AppWindow>,
    ) {
        let mut encoded = EncodedPages::default();
        let mut decoded = reader_engine::DisplayDecoder::default();
        let mut epoch = u64::MAX;
        loop {
            let task = match receiver.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(task) => task,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let Some(owner) = app.upgrade() else {
                        break;
                    };
                    let generation = owner.generation.load(Ordering::Acquire);
                    if !owner.image_worker_live.load(Ordering::Acquire) {
                        break;
                    }
                    let reading = {
                        let view = owner.view();
                        view.section == 6
                            && view.reader.as_ref().is_some_and(|reader| {
                                reader
                                    .source
                                    .as_ref()
                                    .is_none_or(|source| source.availability().is_available())
                            })
                    };
                    if generation != epoch || !reading {
                        decoded.clear();
                        encoded = EncodedPages::default();
                        epoch = generation;
                    }
                    continue;
                }
            };
            if task.generation != epoch {
                decoded.clear();
                encoded = EncodedPages::default();
                epoch = task.generation;
            }
            let Some(app) = app.upgrade() else {
                break;
            };
            if !app.image_worker_live.load(Ordering::Acquire) {
                break;
            }
            if app.generation.load(Ordering::Acquire) != task.generation {
                continue;
            }
            let input = {
                let mut view = app.view();
                if app.generation.load(Ordering::Acquire) != task.generation {
                    continue;
                }
                view.reader.as_mut().and_then(|r| {
                    // A task may have been queued before the visible page's
                    // actual dimensions/window were published. Recheck here,
                    // before fetching or replacing the worker's original.
                    if !r.start_image(task.index) {
                        return None;
                    }
                    r.pages
                        .get(task.index)
                        .cloned()
                        .map(|p| (p, r.source.clone(), r.chapter.clone()))
                })
            };
            let Some((input, source, chapter)) = input else {
                continue;
            };
            let cancel = CancelToken::default();
            *app.active_image.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((task.index, cancel.clone()));
            if app.generation.load(Ordering::Acquire) != task.generation {
                cancel.cancel();
            }
            let result: Result<_> = (|| {
                let mut pending_write = None;
                let mut cache_request = None;
                let bytes = match &input {
                    PageInput::Local { .. } => encoded.read(task.generation, task.index, || {
                        input.read(source.as_ref(), &cancel)
                    })?,
                    PageInput::Remote(page) => {
                        let source = source
                            .as_ref()
                            .ok_or_else(|| text::tr("Source unavailable"))?;
                        let weak = Arc::downgrade(&app);
                        let request = app.chapter_cache.prepare(
                            &chapter,
                            page,
                            source,
                            cancel.clone(),
                            Arc::new(move || {
                                weak.upgrade().is_some_and(|owner| {
                                    owner.generation.load(Ordering::Acquire) == task.generation
                                })
                            }),
                        )?;
                        let mut fetched = false;
                        let mut reused = false;
                        let mut load = || {
                            fetched = true;
                            let read = app.chapter_cache.read(&request)?;
                            reused = read.from_cache;
                            pending_write = read.pending;
                            Ok(read.bytes)
                        };
                        let mut bytes = if let Some(key) = request.memory_key() {
                            encoded.read_scoped(task.generation, task.index, &key, &mut load)?
                        } else {
                            Arc::new(load()?)
                        };
                        if !fetched {
                            reused = true;
                        }
                        if !fetched && request.validate_scope().is_err() {
                            let read = app.chapter_cache.read(&request)?;
                            reused = read.from_cache;
                            pending_write = read.pending;
                            bytes = Arc::new(read.bytes);
                        }
                        request.validate_active()?;
                        app.chapter_cache.touch(&request);
                        cache_request = Some((request, reused));
                        bytes
                    }
                };
                cancel.check().map_err(error)?;
                let dimensions =
                    reader_engine::inspect_image(&bytes, reader_engine::DecodeLimits::default())
                        .map_err(error)?;
                let (display_width, (start, end), budget) = {
                    let view = app.view();
                    if app.generation.load(Ordering::Acquire) != task.generation
                        || cancel.is_cancelled()
                    {
                        return Err(text::tr("Operation cancelled"));
                    }
                    let reader = view
                        .reader
                        .as_ref()
                        .ok_or_else(|| text::tr("Source unavailable"))?;
                    task_window_request(
                        reader,
                        app.generation.load(Ordering::Acquire),
                        &task,
                        dimensions,
                    )?
                };
                let native_bytes = u64::from(dimensions.width) * u64::from(dimensions.height) * 4;
                let copy_budget = (reader_engine::DecodeLimits::default()
                    .max_decoded_bytes
                    .saturating_sub(native_bytes)
                    / 2) as usize;
                let window = decoded
                    .decode_window(
                        &bytes,
                        reader_engine::DecodeLimits::default(),
                        display_width,
                        start,
                        end,
                        budget.min(copy_budget),
                    )
                    .map_err(error)?;
                cancel.check().map_err(error)?;
                let response = Arc::downgrade(&bytes);
                let reuse = {
                    let view = app.view();
                    view.reader.as_ref().and_then(|reader| {
                        same_response(reader.decoded_responses.get(&task.index), &response)
                            .then(|| reader.decoded_windows.get(&task.index).copied())
                            .flatten()
                    })
                };
                let prepared = prepare_display_tiles(
                    window.image.as_raw(),
                    window.image.width(),
                    window.image.height(),
                    window.plan.first_row,
                    window.plan.full_height,
                    reuse,
                );
                let plan = window.plan;
                drop(window);
                let write = pending_write.map(|pending| (pending, bytes));
                Ok((
                    dimensions,
                    plan,
                    prepared,
                    display_width,
                    write,
                    cache_request,
                    reuse,
                    response,
                ))
            })();
            let pending = Arc::new(Mutex::new(Some(result)));
            let handed = pending.clone();
            let (finished, ack) = mpsc::sync_channel(1);
            let completion = ImageCompletion(finished);
            let waiting = Arc::downgrade(&app);
            let availability = source.as_ref().map(Source::availability);
            let _ = ui.upgrade_in_event_loop(move |ui| {
                let _completion = completion;
                let Some(result) = handed.lock().unwrap_or_else(|e| e.into_inner()).take() else {
                    return;
                };

                if app.generation.load(Ordering::Acquire) != task.generation {
                    return;
                }
                let model = ui.get_pages();
                let Some(mut row) = model.row_data(task.index) else {
                    return;
                };
                let mut view = app.view();
                let Some(reader) = &mut view.reader else {
                    return;
                };
                reader.requested.remove(&task.index);
                if !reader.desired.contains(&task.index) {
                    return;
                }
                let mut loaded = false;
                let mut geometry_updated = false;
                match result {
                    Ok((
                        dimensions,
                        plan,
                        prepared,
                        display_width,
                        write,
                        cache_request,
                        reuse,
                        response,
                    )) => {
                        if cache_request.as_ref().is_some_and(|(request, reused)| {
                            request.validate_for_publication(*reused).is_err()
                        }) {
                            let current = reader.anchor.page_index;
                            drop(view);
                            app.request_images(&ui, current);
                            return;
                        }
                        let actual_ratio = dimensions.height as f32 / dimensions.width as f32;
                        let (start, end) = reader.span_with_ratio(task.index, actual_ratio);
                        let current_budget = reader
                            .window_request(task.index, dimensions)
                            .map(|r| r.2)
                            .unwrap_or(0);
                        if !plan.covers(start, end) || plan.byte_len() > current_budget {
                            let current = reader.anchor.page_index;
                            drop(view);
                            app.request_images(&ui, current);
                            return;
                        }
                        // A scroll/source transition may have evicted the old
                        // strips while this completion was being prepared.
                        // Retry from current state without blanking visible art.
                        if reuse.is_some_and(|previous| {
                            reader.decoded_windows.get(&task.index) != Some(&previous)
                                || !same_response(
                                    reader.decoded_responses.get(&task.index),
                                    &response,
                                )
                        }) {
                            reader.decoded_responses.remove(&task.index);
                            let current = reader.anchor.page_index;
                            drop(view);
                            app.request_images(&ui, current);
                            return;
                        }
                        let mut used = reader.retained_bytes_without(task.index);
                        let visible = reader.anchor.page_index..=reader.visible_end();
                        if visible.contains(&task.index) {
                            let mut victims = reader
                                .retained
                                .iter()
                                .copied()
                                .filter(|i| *i != task.index && !visible.contains(i))
                                .collect::<Vec<_>>();
                            victims.sort_by_key(|i| std::cmp::Reverse(i.abs_diff(task.index)));
                            for victim in victims {
                                if used.saturating_add(plan.byte_len()) <= READER_MODEL_BYTES {
                                    break;
                                }
                                used = used.saturating_sub(
                                    reader.decoded_bytes.remove(&victim).unwrap_or(0),
                                );
                                reader.retained.remove(&victim);
                                reader.decoded_widths.remove(&victim);
                                reader.decoded_windows.remove(&victim);
                                reader.decoded_responses.remove(&victim);
                                reader.budget_blocked.insert(victim);
                                if let Some(mut old) = model.row_data(victim) {
                                    old.tiles = ModelRc::default();
                                    old.image = slint::Image::default();
                                    old.loaded = false;
                                    model.set_row_data(victim, old);
                                }
                            }
                        }
                        if used.saturating_add(plan.byte_len()) > READER_MODEL_BYTES {
                            reader.budget_blocked.insert(task.index);
                            return;
                        }
                        let previous = reader.decoded_windows.get(&task.index).copied();
                        let Some(tiles) = reconcile_display_tiles(&row.tiles, previous, prepared)
                        else {
                            reader.decoded_responses.remove(&task.index);
                            let current = reader.anchor.page_index;
                            drop(view);
                            app.request_images(&ui, current);
                            return;
                        };
                        loaded = true;
                        let geometry_changed =
                            reader.native_dimensions.get(&task.index) != Some(&dimensions);
                        let parent_changed =
                            !row.loaded || !row.error.is_empty() || row.tiles != tiles;
                        reader.decoded_bytes.insert(task.index, plan.byte_len());
                        reader.decoded_widths.insert(task.index, display_width);
                        reader.decoded_windows.insert(task.index, plan);
                        reader.decoded_responses.insert(task.index, response);
                        reader.native_dimensions.insert(task.index, dimensions);
                        reader.budget_blocked.remove(&task.index);
                        if geometry_changed {
                            reader.budget_blocked.clear();
                        }
                        row.tiles = tiles;
                        row.loaded = true;
                        row.error = "".into();
                        reader.retained.insert(task.index);
                        if parent_changed {
                            model.set_row_data(task.index, row);
                        }
                        if geometry_changed {
                            reader.ratios[task.index] =
                                dimensions.height as f32 / dimensions.width as f32;
                            let mut position = 0.0;
                            for (i, ratio) in reader.ratios.iter().enumerate() {
                                if let Some(mut row) = model.row_data(i)
                                    && (row.position != position || row.ratio != *ratio)
                                {
                                    row.position = position;
                                    row.ratio = *ratio;
                                    model.set_row_data(i, row);
                                }
                                position += ratio;
                            }
                            ui.set_total_ratio(position);
                            geometry_updated = true;
                        }
                        if let Some((pending, bytes)) = write {
                            app.chapter_cache.enqueue(pending, bytes);
                        }
                    }
                    Err(error) => {
                        if cancel.is_cancelled() {
                            let current = reader.anchor.page_index;
                            drop(view);
                            app.request_images(&ui, current);
                            return;
                        }
                        reader.failed.insert(task.index);
                        row.error = error.into();
                        model.set_row_data(task.index, row);
                    }
                }
                drop(view);
                if geometry_updated {
                    app.restore_position(&ui);
                }
                if loaded {
                    let mode = { app.view().reader.as_ref().map(|r| r.mode) };
                    if mode == Some(ReadingMode::Webtoon) {
                        app.position(
                            &ui,
                            -ui.get_scroll_y(),
                            ui.get_reader_width(),
                            ui.get_reader_viewport_height(),
                        );
                    } else {
                        let mut view = app.view();
                        if let Some(reader) = &mut view.reader {
                            let index = reader.anchor.page_index;
                            reader.completed |=
                                index + 1 == reader.pages.len() && reader.has_visible_pixels(index);
                            let progress = ReadingProgress {
                                chapter: reader.chapter.clone(),
                                anchor: reader.anchor.clone(),
                                mode: reader.mode,
                                last_read_at: now(),
                                completed: reader.completed,
                            };
                            view.pending = Some(progress);
                        }
                    }
                    // Geometry can grow while a fetch/decode is in flight.
                    // Keep its preview and immediately queue the new quality
                    // in paged modes as well as Webtoon, without a scroll.
                    let index = { app.view().reader.as_ref().map(|r| r.anchor.page_index) };
                    if let Some(index) = index {
                        app.request_images(&ui, index);
                    }
                }
            });
            if !wait_image_completion(&pending, &ack, || {
                waiting.upgrade().is_some_and(|owner| {
                    owner.image_worker_live.load(Ordering::Acquire)
                        && owner.generation.load(Ordering::Acquire) == task.generation
                }) && availability
                    .as_ref()
                    .is_none_or(|guard| guard.is_available())
            }) {
                decoded.clear();
                encoded = EncodedPages::default();
            }
        }
    }
    fn step_page(&self, ui: &AppWindow, step: i32) {
        let mut view = self.view();
        let Some(reader) = &mut view.reader else {
            return;
        };
        let index = (reader.anchor.page_index as i32 + step).clamp(0, reader.pages.len() as i32 - 1)
            as usize;
        if step == 0 {
            reader.failed.remove(&index);
            reader.budget_blocked.remove(&index);
        }
        let fraction = if step == 0 {
            reader.anchor.offset()
        } else {
            0.0
        };
        reader.anchor =
            ResumeAnchor::new(reader.page_id(index), index, fraction).unwrap_or_default();
        reader.completed |= reader.has_visible_pixels(index)
            && index + 1 == reader.pages.len()
            && reader.mode != ReadingMode::Webtoon;
        let progress = ReadingProgress {
            chapter: reader.chapter.clone(),
            anchor: reader.anchor.clone(),
            mode: reader.mode,
            last_read_at: now(),
            completed: reader.completed,
        };
        view.pending = Some(progress);
        drop(view);
        ui.set_current_page(index as i32);
        self.restore_position(ui);
        self.request_images(ui, index);
    }
    fn step_chapter(self: &Arc<Self>, ui: &AppWindow, step: i32) {
        let chapter = {
            let view = self.view();
            if step == 0 {
                let state = self.services.domain.snapshot().unwrap_or_default();
                resume_chapter(&view.chapters, &state.progress).map(|c| c.id.clone())
            } else {
                view.reader
                    .as_ref()
                    .and_then(|r| view.chapters.iter().position(|c| c.id == r.chapter))
                    .and_then(|i| usize::try_from(i as i64 + i64::from(step)).ok())
                    .and_then(|i| view.chapters.get(i))
                    .map(|c| c.id.clone())
            }
        };
        if let Some(chapter) = chapter {
            self.open_chapter(ui, chapter);
        }
    }
    fn download_list(self: &Arc<Self>, ui: &AppWindow) {
        self.task(
            ui,
            |app, _| app.services.downloads.jobs().map_err(error),
            |app, ui, jobs| {
                let state = app.services.domain.snapshot().unwrap_or_default();
                let rows = jobs
                    .iter()
                    .map(|job| ItemRow {
                        title: state
                            .chapters
                            .iter()
                            .find(|c| c.id == job.chapter)
                            .map(|c| c.title.clone())
                            .unwrap_or_else(|| job.chapter.local_id.clone())
                            .into(),
                        subtitle: job.error.clone().unwrap_or_default().into(),
                        badge: format!(
                            "{:?} · {}/{}",
                            job.status,
                            job.completed_pages(),
                            job.pages.len()
                        )
                        .into(),
                        bookmarked: job.status == reader_downloads::JobStatus::Complete,
                        downloaded: job.chapter.series.source_id != "local"
                            && job.status == reader_downloads::JobStatus::Complete,
                        has_download: job.chapter.series.source_id != "local",
                        local_import: job.chapter.series.source_id == "local",
                        ..ItemRow::default()
                    })
                    .collect::<Vec<_>>();
                let mut view = app.view();
                view.download_states = jobs
                    .iter()
                    .map(|job| (job.chapter.clone(), job.status.clone()))
                    .collect();
                view.rows = jobs
                    .into_iter()
                    .map(|job| Row::Download(job.chapter))
                    .collect();
                drop(view);
                app.render(ui);
                ui.set_items(ModelRc::new(VecModel::from(rows)));
            },
        );
    }
    fn download(self: &Arc<Self>, ui: &AppWindow, chapter: ChapterId) {
        // Downloads outlive navigation; pausing/cancellation is persisted in the
        // chapter job and checked before each page is published.
        ui.set_status(
            text::tr("Chapter download started. Open Downloads to pause or retry.").into(),
        );
        let app = self.clone();
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let result = (|| {
                let cancel = CancelToken::default();
                app.services.prepare_download(&chapter, &cancel)?;
                let queued_app = app.clone();
                let series = chapter.series.clone();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    let view = queued_app.view();
                    let refresh =
                        view.section == 5 && view.series.as_ref().is_some_and(|s| s.id == series);
                    drop(view);
                    if refresh {
                        queued_app.refresh_download_states(&ui);
                    }
                });
                let _serial = app
                    .download_serial
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                app.services.run_queued_download(&chapter, &cancel)
            })();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_status(
                    match result {
                        Ok(()) => text::tr("Download finished or paused."),
                        Err(e) => e,
                    }
                    .into(),
                );
                if app.view().section == 7 {
                    app.download_list(&ui);
                }
                app.refresh_download_states(&ui);
            });
        });
    }
    fn entry_action(self: &Arc<Self>, ui: &AppWindow, key: &str, action: &str) {
        if action == "change-status" {
            return;
        }
        if action == "categories" {
            ui.set_entry_categories(ModelRc::new(VecModel::from(Vec::<EntryCategoryRow>::new())));
        }
        let result = (|| -> Result<()> {
            let id: reader_domain::SeriesId = serde_json::from_str(key).map_err(error)?;
            let snapshot = self.services.domain.snapshot().map_err(error)?;
            let series = snapshot
                .series
                .iter()
                .find(|series| series.id == id)
                .cloned()
                .ok_or_else(|| text::tr("Title is no longer available."))?;
            if action == "refresh" {
                self.task(
                    ui,
                    move |app, cancel| app.services.details(&series, cancel),
                    |app, ui, (series, chapters)| {
                        let mut view = app.view();
                        if view
                            .series
                            .as_ref()
                            .is_some_and(|current| current.id == series.id)
                        {
                            view.series = Some(series);
                            view.chapters = chapters;
                        }
                        drop(view);
                        app.render(ui);
                    },
                );
                return Ok(());
            }
            if action != "categories" {
                self.services
                    .domain
                    .update(|state| update_entry_membership(state, &id, action))
                    .map_err(error)?;
                self.render(ui);
            }
            if action == "categories" || action.starts_with("category:") {
                let snapshot = self.services.domain.snapshot().map_err(error)?;
                let entry = snapshot
                    .library
                    .iter()
                    .find(|entry| entry.series == id)
                    .ok_or_else(|| text::tr("Title is no longer in your library."))?;
                ui.set_entry_categories(ModelRc::new(VecModel::from(
                    snapshot
                        .categories
                        .iter()
                        .map(|category| EntryCategoryRow {
                            id: category.id.clone().into(),
                            label: category.name.clone().into(),
                            checked: entry.categories.contains(&category.id),
                        })
                        .collect::<Vec<_>>(),
                )));
            }
            Ok(())
        })();
        if let Err(error) = result {
            ui.set_status(error.into());
        }
    }
    fn item_action(self: &Arc<Self>, ui: &AppWindow, index: usize, action: &str) {
        if action == "clear-history" {
            if let Err(e) = self.services.domain.update(|state| {
                state.history.clear();
                Ok(())
            }) {
                ui.set_status(error(e).into());
            }
            self.render(ui);
            return;
        }
        let row = self.view().rows.get(index).cloned();
        if action == "remove-history" {
            if let Some(Row::Chapter(chapter)) = row {
                if let Err(e) = self.services.domain.update(|state| {
                    state.history.retain(|h| h.chapter != chapter.id);
                    Ok(())
                }) {
                    ui.set_status(error(e).into());
                }
                self.render(ui);
            }
            return;
        }
        let chapter = match row {
            Some(Row::Chapter(c)) => c.id,
            Some(Row::Download(c)) => c,
            _ => return,
        };
        let result = match action {
            "delete-download" => {
                // Capture the stable identity before asynchronous work; row
                // ordering can change while a chapter bundle is removed.
                self.task(
                    ui,
                    move |app, _| {
                        app.remove_download(&chapter)?;
                        Ok(chapter)
                    },
                    |app, ui, chapter| {
                        app.view().download_states.remove(&chapter);
                        if app.view().section == 7 {
                            app.download_list(ui);
                        } else {
                            app.render(ui);
                        }
                        ui.set_status(text::tr("Download deleted.").into());
                        app.refresh_download_states(ui);
                    },
                );
                return;
            }
            "download" | "resume" => {
                self.download(ui, chapter);
                return;
            }
            "pause" => self.services.downloads.pause(&chapter).map_err(error),
            "cancel" => self.services.downloads.cancel(&chapter).map_err(error),
            "bookmark" => self
                .services
                .domain
                .update(|state| {
                    if state.bookmarks.iter().any(|b| b.chapter == chapter) {
                        state.bookmarks.retain(|b| b.chapter != chapter);
                    } else {
                        state.bookmarks.push(Bookmark {
                            chapter,
                            anchor: ResumeAnchor::new("0", 0, 0.0)?,
                            note: String::new(),
                        });
                    }
                    Ok(())
                })
                .map_err(error),
            "read" => self
                .services
                .domain
                .update(|state| {
                    let completed = !state
                        .progress
                        .iter()
                        .any(|p| p.chapter == chapter && p.completed);
                    let anchor = state
                        .progress
                        .iter()
                        .find(|p| p.chapter == chapter)
                        .map(|p| p.anchor.clone())
                        .unwrap_or(ResumeAnchor::new("0", 0, 0.0)?);
                    state.set_progress(ReadingProgress {
                        chapter,
                        anchor,
                        mode: state.preferences.mode,
                        last_read_at: now(),
                        completed,
                    });
                    Ok(())
                })
                .map_err(error),
            _ => return,
        };
        if let Err(e) = result {
            ui.set_status(e.into());
        } else if self.view().section == 7 {
            self.download_list(ui);
        } else {
            self.render(ui);
        }
    }
    fn refresh_download_states(self: &Arc<Self>, ui: &AppWindow) {
        let revision = {
            let mut view = self.view();
            view.download_revision = view.download_revision.wrapping_add(1);
            if view.download_refreshing {
                view.download_refresh_again = true;
                return;
            }
            view.download_refreshing = true;
            view.download_revision
        };
        let app = self.clone();
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let states = app.services.downloads.statuses().map_err(error);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let mut view = app.view();
                view.download_refreshing = false;
                let again = std::mem::take(&mut view.download_refresh_again);
                let current = revision == view.download_revision;
                let refresh = current && view.section == 5;
                if current {
                    match states {
                        Ok(states) => view.download_states = states,
                        Err(message) => ui.set_status(message.into()),
                    }
                }
                drop(view);
                if refresh {
                    app.render(&ui);
                }
                if again {
                    app.refresh_download_states(&ui);
                }
            });
        });
    }
    fn remove_download(&self, chapter: &ChapterId) -> Result<()> {
        if chapter.series.source_id == "local" {
            return Err(text::tr(
                "Imported local chapters cannot be deleted as downloads.",
            ));
        }
        self.services
            .downloads
            .remove(chapter)
            .map_err(|message| match message {
                reader_downloads::Error::Invalid(reason) if reason == "chapter already running" => {
                    text::tr(
                        "Pause or cancel this download and wait for it to stop before deleting it.",
                    )
                }
                message => error(message),
            })
    }
    fn browse_sources(&self, adult: bool) -> Result<BrowseSources> {
        let extensions = self.services.sources.list().map_err(error)?;
        let icons = crate::addon_icons::prepare(&self.services.sources, &extensions);
        Ok(BrowseSources {
            choices: self.sources(adult)?,
            extensions,
            icons,
        })
    }
    fn publish_sources(&self, ui: &AppWindow, sources: BrowseSources) {
        crate::addon_icons::publish(sources.icons);
        ui.set_extensions(ModelRc::new(VecModel::from(
            sources
                .extensions
                .into_iter()
                .map(|e| ExtensionRow {
                    icon: crate::addon_icons::image(&e.id),
                    id: e.id.into(),
                    name: e.name.into(),
                    version: e.version.into(),
                    language: e.language.into(),
                    enabled: e.enabled,
                })
                .collect::<Vec<_>>(),
        )));
        {
            let mut view = self.view();
            view.choices = sources.choices;
            view.rows.clear();
            view.selected = 0;
        }
        self.render(ui);
    }
    fn install_sources(
        &self,
        path: PathBuf,
        adult: bool,
    ) -> Result<(SourceDescriptor, BrowseSources)> {
        let metadata = std::fs::metadata(&path).map_err(error)?;
        if metadata.len() > 8 * 1024 * 1024 {
            return Err("Addon ZIP exceeds 8 MiB".into());
        }
        let id = self
            .services
            .install(&std::fs::read(path).map_err(error)?)?;
        let sources = self.browse_sources(adult)?;
        let descriptor = sources
            .extensions
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| "Installed source is unavailable".to_string())?;
        Ok((descriptor, sources))
    }
    fn install(self: &Arc<Self>, ui: &AppWindow, path: PathBuf) {
        let adult = ui.get_show_adult();
        self.task(
            ui,
            move |app, _| app.install_sources(path, adult),
            |app, ui, (descriptor, sources)| {
                app.view().section = 3;
                ui.set_source_catalog_open(false);
                app.publish_sources(ui, sources);
                ui.set_status(
                    text::tr(if descriptor.nsfw && !ui.get_show_adult() {
                        "Addon installed. Turn on Show adult sources to select it."
                    } else if !descriptor.enabled {
                        "Updated addon needs explicit enabling before use."
                    } else {
                        "Addon installed. Choose Popular or Latest to browse."
                    })
                    .into(),
                );
            },
        );
    }
}
// Texture dimensions stay bounded while logical progress continues to refer
// to the original page. Full decode admission remains the engine's responsibility.
#[cfg(test)]
fn display_tiles(pixels: &[u8], width: u32, height: u32) -> Vec<TileRow> {
    display_window_tiles(pixels, width, height, 0, height)
}
fn same_response(previous: Option<&Weak<Vec<u8>>>, current: &Weak<Vec<u8>>) -> bool {
    previous.is_some_and(|previous| previous.strong_count() > 0 && Weak::ptr_eq(previous, current))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TileRect {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    full_width: u32,
    full_height: u32,
}
struct PreparedTile {
    rect: TileRect,
    // None retains the existing strip's image AND its Slint delegate.
    pixels: Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>,
}
fn tile_rects(plan: reader_engine::DisplayWindowPlan) -> Vec<TileRect> {
    let mut result = Vec::new();
    let mut top = plan.first_row;
    let end = plan.first_row + plan.rows;
    while top < end {
        let bottom =
            (top / reader_engine::DISPLAY_TILE_ROWS + 1) * reader_engine::DISPLAY_TILE_ROWS;
        let height = (bottom.min(end) - top).min(2048);
        for left in (0..plan.width).step_by(2048) {
            result.push(TileRect {
                left,
                top,
                width: 2048.min(plan.width - left),
                height,
                full_width: plan.width,
                full_height: plan.full_height,
            });
        }
        top += height;
    }
    result
}
fn mount_tile(tile: PreparedTile) -> TileRow {
    let rect = tile.rect;
    TileRow {
        image: slint::Image::from_rgba8(tile.pixels.expect("new tile has pixels")),
        offset: rect.top as f32 / rect.full_height as f32,
        fraction: rect.height as f32 / rect.full_height as f32,
        left: rect.left as f32 / rect.full_width as f32,
        right: (rect.left + rect.width) as f32 / rect.full_width as f32,
        bottom: (rect.top + rect.height) as f32 / rect.full_height as f32,
    }
}
/// Preserve the model and surviving repeated items. Slint's renderer can cache
/// a dynamic image on its item, so a new model defeats image-handle reuse.
fn reconcile_display_tiles(
    current: &ModelRc<TileRow>,
    previous: Option<reader_engine::DisplayWindowPlan>,
    prepared: Vec<PreparedTile>,
) -> Option<ModelRc<TileRow>> {
    let mut keys = previous.map(tile_rects).unwrap_or_default();
    let has_reuse = prepared.iter().any(|tile| tile.pixels.is_none());
    if has_reuse
        && (keys.len() != current.row_count()
            || prepared
                .iter()
                .any(|tile| tile.pixels.is_none() && !keys.contains(&tile.rect)))
    {
        return None;
    }
    let model = current.as_any().downcast_ref::<VecModel<TileRow>>();
    let owned = model
        .is_none()
        .then(|| ModelRc::new(VecModel::<TileRow>::default()));
    let model = match model {
        Some(model) => model,
        None if has_reuse => return None,
        None => owned
            .as_ref()
            .unwrap()
            .as_any()
            .downcast_ref::<VecModel<TileRow>>()
            .unwrap(),
    };
    if keys.len() != model.row_count() {
        // This is an initial mount or a fully new response, never a reused
        // completion with missing backing rows (validated above).
        while model.row_count() > 0 {
            model.remove(model.row_count() - 1);
        }
        keys.clear();
    }
    // Drop outgoing model references before allocating entering image handles.
    // In particular, reverse scrolling and disjoint jumps must not temporarily
    // append a second window alongside the complete old one.
    let desired = prepared
        .iter()
        .map(|tile| tile.rect)
        .collect::<BTreeSet<_>>();
    for index in (0..keys.len()).rev() {
        if !desired.contains(&keys[index]) {
            model.remove(index);
            keys.remove(index);
        }
    }
    let count = prepared.len();
    for (index, tile) in prepared.into_iter().enumerate() {
        if let Some(found) = keys[index..].iter().position(|key| *key == tile.rect) {
            for _ in 0..found {
                model.remove(index);
                keys.remove(index);
            }
            if tile.pixels.is_some() {
                model.set_row_data(index, mount_tile(tile));
            }
        } else {
            let key = tile.rect;
            model.insert(index, mount_tile(tile));
            keys.insert(index, key);
        }
    }
    while keys.len() > count {
        model.remove(count);
        keys.remove(count);
    }
    Some(owned.unwrap_or_else(|| current.clone()))
}
#[cfg(test)]
fn display_window_tiles(
    pixels: &[u8],
    width: u32,
    height: u32,
    first_row: u32,
    full_height: u32,
) -> Vec<TileRow> {
    prepare_display_tiles(pixels, width, height, first_row, full_height, None)
        .into_iter()
        .map(mount_tile)
        .collect()
}
fn prepare_display_tiles(
    pixels: &[u8],
    width: u32,
    height: u32,
    first_row: u32,
    full_height: u32,
    previous: Option<reader_engine::DisplayWindowPlan>,
) -> Vec<PreparedTile> {
    let stride = width as usize * 4;
    let old = previous.map(tile_rects).unwrap_or_default();
    tile_rects(reader_engine::DisplayWindowPlan {
        width,
        full_height,
        first_row,
        rows: height,
    })
    .into_iter()
    .map(|rect| {
        let pixels = if old.contains(&rect) {
            None
        } else {
            let mut buffer =
                slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(rect.width, rect.height);
            for row in 0..rect.height as usize {
                let source = (rect.top as usize - first_row as usize + row) * stride
                    + rect.left as usize * 4;
                let target = row * rect.width as usize * 4;
                buffer.make_mut_bytes()[target..target + rect.width as usize * 4]
                    .copy_from_slice(&pixels[source..source + rect.width as usize * 4]);
            }
            Some(buffer)
        };
        PreparedTile { rect, pixels }
    })
    .collect()
}

fn mode_index(mode: ReadingMode) -> i32 {
    match mode {
        ReadingMode::Webtoon => 0,
        ReadingMode::LeftToRight => 1,
        ReadingMode::RightToLeft => 2,
        ReadingMode::Vertical => 3,
    }
}
struct ReaderScreencap {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}
enum FrozenScreencap {
    Device(ReaderScreencap),
    Chapter(Box<ChapterScreencap>),
}
struct ChapterScreencap {
    viewport: crate::chapter_capture::Rect,
    reference: usize,
    pages: Vec<crate::chapter_capture::PageRect>,
    inputs: BTreeMap<usize, PageInput>,
    source: Option<Source>,
    background: [u8; 4],
}
fn capture_chapter_request(
    app: &Controller,
    ui: &AppWindow,
    reference: i32,
) -> Result<ChapterScreencap> {
    use crate::chapter_capture::{PageRect, Rect};
    let frame = ui.get_reader_capture_viewport();
    let viewport = Rect {
        x: f64::from(frame.x),
        y: f64::from(frame.y),
        width: f64::from(frame.width),
        height: f64::from(frame.height),
    };
    let reference = usize::try_from(reference).map_err(error)?;
    let view = app.view();
    let reader = view
        .reader
        .as_ref()
        .ok_or_else(|| text::tr("Chapter image is unavailable"))?;
    let width = f64::from(ui.get_reader_width());
    let model = ui.get_pages();
    let mut pages = Vec::new();
    let mut inputs = BTreeMap::new();
    for index in 0..model.row_count() {
        let Some(row) = model.row_data(index).filter(|row| row.loaded) else {
            continue;
        };
        if ui.get_reading_mode() != 0 && index != ui.get_current_page() as usize {
            continue;
        }
        let rect = chapter_capture_rect(&frame, width, ui.get_reading_mode(), &row);
        if rect.x < viewport.x + viewport.width
            && rect.x + rect.width > viewport.x
            && rect.y < viewport.y + viewport.height
            && rect.y + rect.height > viewport.y
            && let Some(input) = reader.pages.get(index)
        {
            pages.push(PageRect { index, rect });
            inputs.insert(index, input.clone());
        }
    }
    let color = ui.get_reader_capture_background();
    Ok(ChapterScreencap {
        viewport,
        reference,
        pages,
        inputs,
        source: reader.source.clone(),
        background: [color.red(), color.green(), color.blue(), color.alpha()],
    })
}
// Match the reader's actual fit/centering geometry. The viewport and pan are
// frozen by Slint immediately before opening the menu, rather than guessed
// from a saved logical reading anchor or the device's physical resolution.
fn chapter_capture_rect(
    viewport: &crate::ReaderCaptureViewport,
    width: f64,
    mode: i32,
    row: &PageRow,
) -> crate::chapter_capture::Rect {
    let x = f64::from(viewport.x + viewport.content_x)
        + (f64::from(viewport.width).max(width) - width) / 2.;
    let y = f64::from(viewport.y + viewport.content_y);
    if mode == 0 {
        crate::chapter_capture::Rect {
            x,
            y: y + f64::from(row.position) * width,
            width,
            height: width * f64::from(row.ratio),
        }
    } else {
        let height = f64::from(viewport.height);
        let image_width = width.min(height / f64::from(row.ratio));
        let image_height = height.min(width * f64::from(row.ratio));
        crate::chapter_capture::Rect {
            x: x + (width - image_width) / 2.,
            y: y + (height - image_height) / 2.,
            width: image_width,
            height: image_height,
        }
    }
}
fn capture_reader(ui: &AppWindow) -> Result<ReaderScreencap> {
    let size = ui.window().size();
    if u64::from(size.width) * u64::from(size.height) > 8_000_000 {
        return Err(text::tr("Reader screencap exceeds the image size limit"));
    }
    // Called before the context menu becomes visible. FemtoVG renders the
    // current scene offscreen, so neither the menu nor hidden OSD is captured.
    let image = ui.window().take_snapshot().map_err(error)?;
    Ok(ReaderScreencap {
        width: image.width(),
        height: image.height(),
        pixels: image.as_bytes().to_vec(),
    })
}
#[derive(Clone)]
struct FrozenExportName {
    chapter: ChapterId,
    page: usize,
    source_y: Option<u32>,
}
fn export_source_row(page_y: f64, page_height: f64, viewport_y: f64, native_height: u32) -> u32 {
    let fraction = ((viewport_y - page_y) / page_height).clamp(0., 1.);
    (fraction * f64::from(native_height))
        .round()
        .min(f64::from(native_height.saturating_sub(1))) as u32
}
fn export_name_position(
    app: &Controller,
    ui: &AppWindow,
    index: i32,
    screen: bool,
) -> Result<FrozenExportName> {
    let view = app.view();
    let reader = view
        .reader
        .as_ref()
        .ok_or_else(|| text::tr("Chapter image is unavailable"))?;
    let index = usize::try_from(index).map_err(error)?;
    if !screen {
        if index >= reader.pages.len() {
            return Err(text::tr("Chapter image is unavailable"));
        }
        return Ok(FrozenExportName {
            chapter: reader.chapter.clone(),
            page: index + 1,
            source_y: None,
        });
    }
    let viewport = ui.get_reader_capture_viewport();
    let model = ui.get_pages();
    for page in 0..model.row_count() {
        if ui.get_reading_mode() != 0 && page != ui.get_current_page() as usize {
            continue;
        }
        let Some(row) = model.row_data(page).filter(|row| row.loaded) else {
            continue;
        };
        let rect = chapter_capture_rect(
            &viewport,
            f64::from(ui.get_reader_width()),
            ui.get_reading_mode(),
            &row,
        );
        if rect.y >= f64::from(viewport.y + viewport.height)
            || rect.y + rect.height <= f64::from(viewport.y)
            || rect.x >= f64::from(viewport.x + viewport.width)
            || rect.x + rect.width <= f64::from(viewport.x)
        {
            continue;
        }
        let height = reader
            .native_dimensions
            .get(&page)
            .map(|dimensions| dimensions.height)
            .ok_or_else(|| text::tr("Chapter image dimensions are unavailable"))?;
        let source_y = export_source_row(rect.y, rect.height, f64::from(viewport.y), height);
        return Ok(FrozenExportName {
            chapter: reader.chapter.clone(),
            page: page + 1,
            source_y: Some(source_y),
        });
    }
    Err(text::tr("Reader screencap is unavailable"))
}

fn wire_reader_context(app: &Arc<Controller>, ui: &AppWindow) {
    use std::{cell::RefCell, rc::Rc};
    let screencap = Rc::new(RefCell::new(None));
    let exporting = Arc::new(AtomicBool::new(false));
    type PendingConflict = Option<(crate::reader_export::ExportCurrent, ReplyOnDrop)>;
    let conflict = Arc::new(Mutex::new(PendingConflict::None));
    let timer = Rc::new(slint::Timer::default());
    struct ReplyOnDrop(Option<crate::reader_export::ConflictReply>);
    impl Drop for ReplyOnDrop {
        fn drop(&mut self) {
            if let Some(reply) = self.0.take() {
                reply(crate::reader_export::ConflictChoice::Cancel);
            }
        }
    }
    let resolver: crate::reader_export::ConflictResolver = {
        let weak = ui.as_weak();
        let conflict = Arc::downgrade(&conflict);
        let owner = Arc::downgrade(app);
        Arc::new(move |name, current, reply| {
            let conflict = conflict.clone();
            let owner = owner.clone();
            let pending_reply = ReplyOnDrop(Some(reply));
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let Some(conflict) = conflict.upgrade() else {
                    return;
                };
                if owner
                    .upgrade()
                    .is_none_or(|app| !app.image_worker_live.load(Ordering::Acquire))
                {
                    return;
                };
                if ui.get_section() != 6 || !current() {
                    return;
                }
                let old = conflict
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .replace((current, pending_reply));
                drop(old);
                ui.set_export_conflict_name(name.into());
                ui.set_export_conflict_open(true);
            });
        })
    };
    ui.on_export_conflict_choice({
        let weak = ui.as_weak();
        let conflict = conflict.clone();
        move |choice| {
            if let Some(ui) = weak.upgrade() {
                ui.set_export_conflict_open(false);
            }
            let pending = conflict.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some((current, mut reply)) = pending {
                let choice = if current() { choice } else { 2 };
                reply.0.take().expect("one export conflict reply")(match choice {
                    0 => crate::reader_export::ConflictChoice::Overwrite,
                    1 => crate::reader_export::ConflictChoice::AddNumber,
                    _ => crate::reader_export::ConflictChoice::Cancel,
                });
            }
        }
    });
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(100),
        {
            let weak = ui.as_weak();
            let owner = Arc::downgrade(app);
            let conflict = conflict.clone();
            move || {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                let mut pending = conflict.lock().unwrap_or_else(|e| e.into_inner());
                let stale = pending.as_ref().is_some_and(|(current, _)| {
                    !current()
                        || owner
                            .upgrade()
                            .is_none_or(|app| !app.image_worker_live.load(Ordering::Acquire))
                        || ui.get_section() != 6
                });
                if stale {
                    let reply = pending.take();
                    drop(pending);
                    ui.set_export_conflict_open(false);
                    drop(reply);
                }
            }
        },
    );
    ui.on_reader_context_opening({
        let app = app.clone();
        let weak = ui.as_weak();
        let screencap = screencap.clone();
        move |index| {
            if let Some(ui) = weak.upgrade() {
                let capture = if crate::capture_settings::preferences(&ui).resolution
                    == crate::capture_settings::CaptureResolution::Chapter
                {
                    capture_chapter_request(&app, &ui, index)
                        .map(|capture| FrozenScreencap::Chapter(Box::new(capture)))
                } else {
                    capture_reader(&ui).map(FrozenScreencap::Device)
                };
                let naming = export_name_position(&app, &ui, index, true);
                *screencap.borrow_mut() =
                    Some((app.generation.load(Ordering::Acquire), capture, naming));
            }
        }
    });
    ui.on_reader_context_closed({
        let screencap = screencap.clone();
        move || {
            screencap.borrow_mut().take();
        }
    });
    ui.on_reader_context_action({
        let app = app.clone();
        let weak = ui.as_weak();
        move |index, action| {
            let _timer = &timer;
            let Some(ui) = weak.upgrade() else { return };
            let (screen, clipboard) = match action.as_str() {
                "save-image" => (false, false),
                "save-screencap" => (true, false),
                "copy-image" => (false, true),
                "copy-screencap" => (true, true),
                _ => return,
            };
            if exporting.swap(true, Ordering::AcqRel) {
                ui.set_reader_context_status(text::tr("Another image action is active").into());
                return;
            }
            ui.set_reader_context_busy(true);
            ui.set_reader_context_status(text::tr("Preparing image…").into());
            let snapshot = screencap.borrow_mut().take();
            let preferences = crate::capture_settings::preferences(&ui);
            let naming = if screen {
                None
            } else {
                Some(export_name_position(&app, &ui, index, false))
            };
            let domain = app.services.domain.clone();
            let resolver = resolver.clone();
            let owner = Arc::downgrade(&app);
            let generation = app.generation.load(Ordering::Acquire);
            let cancel = app.cancel.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let input = {
                let view = app.view();
                view.reader.as_ref().and_then(|reader| {
                    usize::try_from(index)
                        .ok()
                        .and_then(|i| reader.pages.get(i))
                        .cloned()
                        .map(|page| (page, reader.source.clone()))
                })
            };
            let done = {
                let weak = weak.clone();
                let exporting = exporting.clone();
                let app = app.clone();
                move |result: Result<()>| {
                    exporting.store(false, Ordering::Release);
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        ui.set_reader_context_busy(false);
                        if app.generation.load(Ordering::Acquire) == generation
                            && ui.get_section() == 6
                        {
                            ui.set_reader_context_status(
                                match result {
                                    Ok(()) => text::tr(if clipboard {
                                        "Image copied to clipboard."
                                    } else {
                                        "Image saved."
                                    }),
                                    Err(message) => text::tr(&message),
                                }
                                .into(),
                            );
                        }
                    });
                }
            };
            std::thread::spawn(move || {
                let mut final_naming = naming;
                let image = (|| {
                    cancel.check().map_err(error)?;
                    if screen {
                        let (captured_generation, capture, naming) =
                            snapshot.ok_or_else(|| text::tr("Reader screencap is unavailable"))?;
                        final_naming = Some(naming);
                        if captured_generation != generation {
                            return Err(text::tr("Image action cancelled"));
                        }
                        let capture = match capture? {
                            FrozenScreencap::Device(capture) => capture,
                            FrozenScreencap::Chapter(capture) => {
                                let rendered = crate::chapter_capture::capture(
                                    capture.viewport,
                                    capture.reference,
                                    &capture.pages,
                                    capture.background,
                                    |index| {
                                        cancel.check().map_err(error)?;
                                        capture
                                            .inputs
                                            .get(&index)
                                            .ok_or_else(|| {
                                                text::tr("Chapter image is unavailable")
                                            })?
                                            .read(capture.source.as_ref(), &cancel)
                                    },
                                )?;
                                ReaderScreencap {
                                    width: rendered.width,
                                    height: rendered.height,
                                    pixels: rendered.pixels,
                                }
                            }
                        };
                        crate::reader_export::ExportImage::from_rgba_with_preferences(
                            capture.width,
                            capture.height,
                            &capture.pixels,
                            &preferences,
                        )
                    } else {
                        let (page, source) =
                            input.ok_or_else(|| text::tr("Chapter image is unavailable"))?;
                        let bytes = page.read(source.as_ref(), &cancel)?;
                        crate::reader_export::ExportImage::original(
                            bytes,
                            &format!("nova-reader-page-{}", index + 1),
                        )
                    }
                })();
                match image {
                    Ok(image) if !cancel.is_cancelled() => {
                        let current: crate::reader_export::ExportCurrent = Arc::new(move || {
                            !cancel.is_cancelled()
                                && owner.upgrade().is_some_and(|app| {
                                    app.image_worker_live.load(Ordering::Acquire)
                                        && app.generation.load(Ordering::Acquire) == generation
                                })
                        });
                        let named = (|| {
                            if clipboard {
                                return Ok(crate::reader_export::SaveContext::new(
                                    image.name.clone(),
                                    "",
                                    &preferences,
                                    resolver,
                                    current,
                                ));
                            }
                            let name = final_naming
                                .ok_or_else(|| text::tr("Chapter image is unavailable"))??;
                            let state = domain.snapshot().map_err(error)?;
                            let title = state
                                .series
                                .iter()
                                .find(|series| series.id == name.chapter.series)
                                .map(|series| series.title.as_str())
                                .unwrap_or("Series");
                            let chapter = state
                                .chapters
                                .iter()
                                .find(|chapter| chapter.id == name.chapter);
                            let label = chapter
                                .and_then(|chapter| chapter.number.as_deref())
                                .or_else(|| chapter.map(|chapter| chapter.label.as_str()))
                                .unwrap_or("Unknown");
                            let extension = std::path::Path::new(&image.name)
                                .extension()
                                .and_then(|ext| ext.to_str())
                                .ok_or_else(|| text::tr("Unsupported image export format"))?;
                            let filename = crate::export_names::filename(
                                title,
                                label,
                                name.page,
                                name.source_y,
                                extension,
                            );
                            Ok(crate::reader_export::SaveContext::new(
                                filename,
                                title,
                                &preferences,
                                resolver,
                                current,
                            ))
                        })();
                        match named {
                            Ok(context) => crate::reader_export::deliver_with_context(
                                image, clipboard, context, done,
                            ),
                            Err(message) => done(Err(message)),
                        }
                    }
                    Ok(_) => done(Err(text::tr("Image action cancelled"))),
                    Err(message) => done(Err(message)),
                }
            });
        }
    });
}

pub fn run(services: Services) -> Result<()> {
    let ui = AppWindow::new().map_err(error)?;
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_touch_menus(cfg!(target_os = "android"));
    ui.set_about_update_checks_enabled(cfg!(feature = "update-check"));
    crate::about::prepare_license_text(&ui);
    ui.set_license_sources(ModelRc::new(VecModel::from(
        crate::about::open_source_license_sources(),
    )));
    // Nova's About source-link guard and platform browser dispatch.
    ui.on_license_source_requested(move |url| {
        if !url.starts_with("https://") {
            eprintln!("Nova Reader: refusing non-HTTPS license source URL");
            return;
        }
        if let Err(error) = crate::browser::open_browser(&url) {
            eprintln!("Nova Reader: could not open license source page {url}: {error}");
        }
    });
    crate::appearance::init(&ui)?;
    crate::capture_settings::init(&ui)?;
    crate::image_storage::init(&ui)?;
    crate::reading_preferences::init(&ui)?;
    let (sender, receiver) = mpsc::channel();
    let artwork = crate::artwork::ArtworkLoader::new(
        services.clone(),
        ui.as_weak(),
        crate::paths::cache_dir()?.join("posters"),
    );
    let app = Arc::new(Controller {
        progress_checkpoint: crate::progress_checkpoint::ProgressWriter::new(
            services.domain.clone(),
        )
        .map_err(error)?,
        services,
        artwork,
        chapter_cache: Arc::new(crate::chapter_cache::ChapterCache::new(
            crate::paths::chapter_cache_dir()?,
        )),
        image_worker_live: AtomicBool::new(true),
        view: Mutex::new(View {
            source_browser: crate::source_browser::SourceBrowser::load(),
            ..View::default()
        }),
        generation: AtomicU64::new(0),
        cancel: Mutex::new(CancelToken::default()),
        images: sender,
        download_serial: Mutex::new(()),
        active_image: Mutex::new(None),
        metadata_refresh: Arc::default(),
    });
    let _ = LIFECYCLE.set(Arc::downgrade(&app));
    let weak = ui.as_weak();
    app.chapter_cache.on_gc(Arc::new(move || {
        let _ = weak.upgrade_in_event_loop(|ui| {
            ui.global::<crate::ImageStorageSettings>()
                .invoke_chapter_cache_refresh()
        });
    }));
    wire_reader_context(&app, &ui);
    let worker = Arc::downgrade(&app);
    let weak = ui.as_weak();
    std::thread::spawn(move || Controller::image_worker(worker, receiver, weak));
    macro_rules! wire { ($method:ident, |$this:ident,$window:ident $(,$arg:ident)*| $body:expr) => {{let $this=app.clone();let weak=ui.as_weak();ui.$method(move |$($arg),*|{if let Some($window)=weak.upgrade(){$body}});}}; }
    #[cfg(target_os = "android")]
    {
        ui.on_immersive_changed(|immersive| {
            if let Err(e) = crate::android_documents::set_reader_immersive(immersive) {
                eprintln!("Nova Reader: {e}");
            }
        });
        // Java's Activity state can survive recreation while this new UI
        // starts on Library, whose default section emits no change callback.
        if let Err(e) = crate::android_documents::set_reader_immersive(false) {
            eprintln!("Nova Reader: {e}");
        }
    }
    wire!(on_request_artwork, |app, ui, first, last| app
        .request_artwork(&ui, first, last));
    wire!(on_image_cache_refresh, |app, ui| {
        let _ = ui;
        app.artwork.refresh_usage();
    });
    wire!(on_image_cache_clear, |app, ui| {
        app.artwork.clear();
        let model = ui.get_items();
        for i in 0..model.row_count() {
            if let Some(mut row) = model.row_data(i) {
                row.poster = slint::Image::default();
                model.set_row_data(i, row);
            }
        }
        ui.set_series_poster(slint::Image::default());
    });
    {
        let owner = app.clone();
        let weak = ui.as_weak();
        ui.global::<crate::ImageStorageSettings>()
            .on_chapter_cache_refresh(move || {
                if let Some(ui) = weak.upgrade() {
                    owner.chapter_cache_maintenance(&ui, false);
                }
            });
        let owner = app.clone();
        let weak = ui.as_weak();
        ui.global::<crate::ImageStorageSettings>()
            .on_chapter_cache_clear(move || {
                if let Some(ui) = weak.upgrade() {
                    owner.chapter_cache_maintenance(&ui, true);
                }
            });
        let owner = app.clone();
        ui.global::<crate::ImageStorageSettings>()
            .on_policy_changed(move || {
                owner.artwork.apply_policy();
                owner.chapter_cache.wake();
            });
    }
    wire!(on_library_sort_changed, |app, ui| app.render(&ui));
    wire!(on_chapter_order_changed, |app, ui, descending| {
        let series = app.view().series.as_ref().map(|series| series.id.clone());
        if let Some(series) = series {
            match crate::reading_preferences::save_chapter_order(&ui, &series, descending) {
                Ok(()) => {
                    app.render(&ui);
                }
                Err(error) => ui.set_status(
                    format!("{}: {error}", text::tr("Could not save chapter order")).into(),
                ),
            }
        }
    });
    wire!(on_navigate, |app, ui, section| app.navigate(&ui, section));
    wire!(on_return_to_catalogue, |app, ui| app
        .return_to_catalogue(&ui));
    wire!(on_open_item, |app, ui, index| {
        let row = { app.view().rows.get(index as usize).cloned() };
        match row {
            Some(Row::Series(series)) => app.open_series(&ui, series),
            Some(Row::Chapter(chapter)) => app.open_chapter(&ui, chapter.id),
            Some(Row::Download(chapter)) => app.open_chapter(&ui, chapter),
            None => {}
        }
    });
    wire!(on_open_series, |app, ui, index| {
        let series = {
            app.view()
                .artwork_rows
                .get(index as usize)
                .cloned()
                .flatten()
        };
        if let Some(series) = series {
            app.open_series(&ui, series);
        }
    });
    wire!(on_item_action, |app, ui, index, action| app.item_action(
        &ui,
        index as usize,
        &action
    ));
    wire!(on_source_selected, |app, ui, index| app
        .select_source(&ui, index, "popular"));
    wire!(on_source_latest_selected, |app, ui, index| app
        .select_source(&ui, index, "latest"));
    wire!(on_source_pin_picked, |app, ui, id| {
        let result = app.view().source_browser.toggle_pin(&id);
        if let Err(e) = result {
            ui.set_status(e.into());
        } else {
            app.render(&ui);
        }
    });
    wire!(on_source_filters_requested, |app, ui| {
        if ui.get_source_filters_open() {
            ui.set_source_filters_open(false);
            return;
        }
        if app.view().source_filters_loaded {
            ui.set_source_filters(ModelRc::new(VecModel::from(
                app.view().source_filters.rows(),
            )));
            ui.set_source_filters_open(true);
            return;
        }
        let choice = {
            let view = app.view();
            view.choices.get(view.selected).cloned()
        };
        if let Some(choice) = choice.filter(|c| c.filters && c.enabled) {
            app.task(
                &ui,
                move |app, cancel| {
                    let source = app.services.source(&choice.id)?;
                    let response = source
                        .call_cancelled("filters", &json!({"sourceId":choice.id}), cancel)
                        .map_err(error)?;
                    crate::source_filters::FilterSet::parse(response)
                },
                |app, ui, filters| {
                    ui.set_source_filter_notice(filters.notice().into());
                    ui.set_source_filters(ModelRc::new(VecModel::from(filters.rows())));
                    let mut view = app.view();
                    view.source_filters = filters;
                    view.source_filters_loaded = true;
                    drop(view);
                    ui.set_source_filters_open(true);
                },
            );
        }
    });
    wire!(on_source_filter_changed, |app, ui, key, value| {
        let _ = ui;
        let mut view = app.view();
        // Keep the native editor mounted while typing. Reopening the panel
        // publishes its validated draft from the provider-value model.
        view.source_filters.change(&key, &value);
    });
    wire!(on_source_filter_selected, |app, ui, key, index| {
        let _ = ui;
        let mut view = app.view();
        view.source_filters.select(&key, index);
    });
    wire!(on_source_filters_applied, |app, ui| {
        ui.set_source_filters_open(false);
        app.browse(&ui, "filtered", Some(String::new()), false);
    });
    wire!(on_search, |app, ui, query| {
        if app.view().section == 0 || app.view().section == 2 {
            app.view().query = query.to_string();
            app.render(&ui);
        } else {
            app.browse(&ui, "", Some(query.to_string()), false);
        }
    });
    wire!(on_browse, |app, ui, kind| {
        let section = app.view().section;
        if kind == "refresh" {
            app.task(
                &ui,
                |app, cancel| {
                    let state = app.services.domain.snapshot().map_err(error)?;
                    let mut failed = 0;
                    for library in state.library {
                        cancel.check().map_err(error)?;
                        if let Some(series) = state.series.iter().find(|s| s.id == library.series)
                            && app.services.details(series, cancel).is_err()
                        {
                            failed += 1;
                        }
                    }
                    Ok(failed)
                },
                |app, ui, failed| {
                    nova_storage::set_str("reader:last-update", &now().to_string());
                    app.render(ui);
                    if failed > 0 {
                        ui.set_status(
                            format!(
                                "{failed} source refreshes failed. Saved chapters remain available."
                            )
                            .into(),
                        );
                    }
                },
            );
        } else if section == 5 {
            let series = { app.view().series.clone() };
            if let Some(series) = series {
                app.task(
                    &ui,
                    move |app, cancel| app.services.details(&series, cancel),
                    |app, ui, (series, chapters)| {
                        let mut view = app.view();
                        view.series = Some(series);
                        view.chapters = chapters;
                        drop(view);
                        app.render(ui);
                    },
                );
            }
        } else {
            app.browse(&ui, &kind, Some(String::new()), false);
        }
    });
    wire!(on_load_more, |app, ui| app.browse(&ui, "", None, true));
    wire!(on_toggle_library, |app, ui| {
        let series = { app.view().series.clone() };
        if let Some(series) = series {
            let result = app.services.domain.update(|state| {
                if state.library.iter().any(|l| l.series == series.id) {
                    state.library.retain(|l| l.series != series.id);
                } else {
                    state.library.push(reader_domain::LibraryEntry {
                        reading_status: Default::default(),
                        series: series.id,
                        categories: BTreeSet::new(),
                        preferences: None,
                        bookmarked: false,
                        added_at: now(),
                    });
                }
                Ok(())
            });
            if let Err(e) = result {
                ui.set_status(error(e).into());
            }
            app.render(&ui);
        }
    });
    wire!(on_reader_reflow, |app, ui, width, height| app
        .reflow(&ui, width, height));
    wire!(on_reader_position, |app, ui, offset, width, height| app
        .position(&ui, offset, width, height));
    wire!(on_page_step, |app, ui, step| app.step_page(&ui, step));
    wire!(on_chapter_step, |app, ui, step| app.step_chapter(&ui, step));
    wire!(on_mode_changed, |app, ui, index| app.choose_reader_mode(
        &ui,
        index,
        reader_modes::save
    ));
    wire!(on_download_chapter, |app, ui| {
        let chapter = app.view().reader.as_ref().map(|r| r.chapter.clone());
        if let Some(chapter) = chapter {
            app.download(&ui, chapter);
        }
    });
    wire!(on_install_path, |app, ui, path| app
        .install(&ui, PathBuf::from(path.as_str())));
    wire!(on_install_addon, |app, ui| {
        #[cfg(not(target_os = "android"))]
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Reader addon", &["zip"])
            .pick_file()
        {
            app.install(&ui, path);
        }
        #[cfg(target_os = "android")]
        {
            let weak = ui.as_weak();
            let app = app.clone();
            crate::android_documents::pick_import(
                crate::android_documents::ImportKind::AddonZip,
                move |result| {
                    let _ = weak.upgrade_in_event_loop(move |ui| match result {
                        Ok(path) => app.install(&ui, path),
                        Err(e) => ui.set_status(e.into()),
                    });
                },
            );
        }
    });
    wire!(on_local_import, |app, ui, folder| {
        #[cfg(not(target_os = "android"))]
        {
            let path = if folder {
                rfd::FileDialog::new().pick_folder()
            } else {
                rfd::FileDialog::new()
                    .add_filter("Local chapter", &["cbz", "zip"])
                    .pick_file()
            };
            if let Some(path) = path {
                app.task(
                    &ui,
                    move |app, _| app.services.import_local(&path, folder),
                    |app, ui, _| {
                        app.view().section = 0;
                        app.render(ui);
                    },
                );
            }
        }
        #[cfg(target_os = "android")]
        {
            let weak = ui.as_weak();
            let app = app.clone();
            let kind = if folder {
                crate::android_documents::ImportKind::ImageFolder
            } else {
                crate::android_documents::ImportKind::ComicArchive
            };
            crate::android_documents::pick_import(kind, move |result| {
                let _ = weak.upgrade_in_event_loop(move |ui| match result {
                    Ok(path) => app.task(
                        &ui,
                        move |app, _| {
                            let result = app.services.import_local(&path, folder);
                            if folder {
                                let _ = std::fs::remove_dir_all(&path);
                            } else {
                                let _ = std::fs::remove_file(&path);
                            }
                            result
                        },
                        |app, ui, _| {
                            app.view().section = 0;
                            app.render(ui);
                        },
                    ),
                    Err(e) => ui.set_status(e.into()),
                });
            });
        }
    });
    wire!(on_backup, |app, ui, restore| {
        #[cfg(not(target_os = "android"))]
        let path = if restore {
            rfd::FileDialog::new()
                .add_filter("Metadata backup", &["json"])
                .pick_file()
        } else {
            rfd::FileDialog::new()
                .set_file_name("nova-reader-backup.json")
                .save_file()
        };
        #[cfg(not(target_os = "android"))]
        if let Some(path) = path {
            app.task(
                &ui,
                move |app, _| {
                    if restore {
                        let file = std::fs::File::open(path).map_err(error)?;
                        if file.metadata().map_err(error)?.len() > 32 * 1024 * 1024 {
                            return Err("Backup exceeds 32 MiB".into());
                        }
                        use std::io::Read;
                        let mut bytes = Vec::new();
                        file.take(32 * 1024 * 1024 + 1)
                            .read_to_end(&mut bytes)
                            .map_err(error)?;
                        app.services.domain.restore_json(&bytes).map_err(error)?;
                    } else {
                        std::fs::write(path, app.services.domain.backup_json().map_err(error)?)
                            .map_err(error)?;
                    }
                    Ok(())
                },
                move |app, ui, _| {
                    app.render(ui);
                    ui.set_status(
                        text::tr(if restore {
                            "Metadata restored. Missing sources can be installed in Browse."
                        } else {
                            "Metadata backup saved."
                        })
                        .into(),
                    );
                },
            );
        }
        #[cfg(target_os = "android")]
        {
            if restore {
                let weak = ui.as_weak();
                let app = app.clone();
                crate::android_documents::pick_import(
                    crate::android_documents::ImportKind::BackupJson,
                    move |result| {
                        let _ = weak.upgrade_in_event_loop(move |ui| match result {
                            Ok(path) => app.task(
                                &ui,
                                move |app, _| {
                                    let bytes = std::fs::read(&path).map_err(error)?;
                                    let result =
                                        app.services.domain.restore_json(&bytes).map_err(error);
                                    let _ = std::fs::remove_file(path);
                                    result
                                },
                                |app, ui, _| {
                                    app.render(ui);
                                    ui.set_status(text::tr("Metadata restored.").into());
                                },
                            ),
                            Err(e) => ui.set_status(e.into()),
                        });
                    },
                );
            } else {
                app.task(
                    &ui,
                    |app, _| {
                        let path = crate::android_documents::cache_dir()?
                            .join("nova-reader-backup-staged.json");
                        std::fs::write(&path, app.services.domain.backup_json().map_err(error)?)
                            .map_err(error)?;
                        Ok(path)
                    },
                    |_, ui, path| {
                        let weak = ui.as_weak();
                        let cleanup = path.clone();
                        crate::android_documents::export_backup(path, move |result| {
                            let _ = std::fs::remove_file(cleanup);
                            let _ = weak.upgrade_in_event_loop(move |ui| {
                                ui.set_status(
                                    match result {
                                        Ok(()) => text::tr("Metadata backup saved."),
                                        Err(e) => e,
                                    }
                                    .into(),
                                )
                            });
                        });
                    },
                );
            }
        }
    });
    wire!(on_category_selected, |app, ui, index| {
        app.view().category = index.max(0) as usize;
        app.render(&ui);
    });
    wire!(on_entry_action, |app, ui, key, action| app
        .entry_action(&ui, &key, &action));
    wire!(on_catalog_enabled_changed, |app, ui, id, enabled| {
        let package = { app.view().settings_package.clone() };
        if let Some(package) = package {
            let result = crate::source_catalogs::set_enabled(&package, &id, enabled);
            let saved = result.is_ok();
            match result {
                Ok(()) => {
                    app.view()
                        .choices
                        .retain(|choice| choice.id != id.as_str() || enabled);
                    ui.set_status(text::tr("Catalogue settings saved.").into());
                }
                Err(error) => ui.set_status(error.into()),
            }
            let rows = ui
                .get_source_catalog_settings()
                .iter()
                .map(|mut row| {
                    if saved && row.key == id {
                        row.checked = enabled;
                    }
                    row
                })
                .collect::<Vec<_>>();
            ui.set_source_catalog_settings(ModelRc::new(VecModel::from(rows)));
        }
    });
    wire!(on_setting_changed, |app, ui, key, value| {
        let package = { app.view().settings_package.clone() };
        if let Some(package) = package {
            let kind = app
                .services
                .sources
                .settings_schema(&package)
                .ok()
                .and_then(|fields| {
                    fields
                        .into_iter()
                        .find(|f| f.key == key.as_str())
                        .map(|f| f.kind)
                });
            let value = if kind == Some(reader_sources::SettingKind::Bool) {
                serde_json::Value::Bool(value == "true")
            } else {
                serde_json::Value::String(value.to_string())
            };
            match app
                .services
                .sources
                .set_settings(&package, json!({key.as_str():value}))
            {
                Ok(()) => ui.set_status(text::tr("Source settings saved.").into()),
                Err(e) => ui.set_status(error(e).into()),
            }
        }
    });
    wire!(on_add_category, |app, ui, name| {
        if !name.trim().is_empty() {
            if let Err(e) = app.services.domain.update(|state| {
                state.categories.push(reader_domain::Category {
                    id: format!("category-{}", now()),
                    name: name.trim().into(),
                });
                Ok(())
            }) {
                ui.set_status(error(e).into());
            } else {
                ui.set_category_text("".into());
                ui.set_status(text::tr("Category added.").into());
            }
        }
    });
    wire!(on_source_action, |app, ui, action| {
        let choice = {
            let view = app.view();
            view.choices.get(view.selected).cloned()
        };
        if let Some(choice) = choice {
            app.extension_action(&ui, &choice.package, &action);
        }
    });
    wire!(on_extension_action, |app, ui, package, action| app
        .extension_action(&ui, &package, &action));
    wire!(on_check_update, |app, ui| app.task(
        &ui,
        |_, _| crate::updates::check(),
        |_, ui, message| ui.set_status(message.into())
    ));
    {
        let app = app.clone();
        ui.on_flush_progress(move || app.flush());
    }
    let timer = slint::Timer::default();
    let checkpoint = app.clone();
    let refresh_ui = ui.as_weak();
    let mut inventory_check = std::time::Instant::now();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(600),
        move || {
            checkpoint.checkpoint_progress();
            if let Some(ui) = refresh_ui.upgrade()
                && !checkpoint.metadata_refresh_allowed(&ui)
            {
                checkpoint.metadata_refresh.pause();
            }
            if inventory_check.elapsed() >= std::time::Duration::from_secs(60) {
                inventory_check = std::time::Instant::now();
                if let Some(ui) = refresh_ui.upgrade() {
                    checkpoint.refresh_metadata(&ui);
                }
            }
        },
    );
    let initial = Arc::downgrade(&app);
    let initial_ui = ui.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_secs(1), move || {
        if let (Some(app), Some(ui)) = (initial.upgrade(), initial_ui.upgrade()) {
            app.refresh_metadata(&ui);
        }
    });
    let close = app.clone();
    ui.window().on_close_requested(move || {
        close.metadata_refresh.pause();
        close.flush();
        slint::CloseRequestResponse::HideWindow
    });
    app.render(&ui);
    app.refresh_download_states(&ui);
    // Recovery verifies persisted payloads; keep that potentially large scan
    // off the event loop as well as the lightweight list-status refresh.
    let startup = app.clone();
    let startup_ui = ui.as_weak();
    std::thread::spawn(move || {
        let queued = startup.services.downloads.jobs().map(|jobs| {
            jobs.into_iter()
                .filter(|job| job.status == reader_downloads::JobStatus::Queued)
                .map(|job| job.chapter)
                .collect::<Vec<_>>()
        });
        if let Ok(queued) = queued {
            let _ = startup_ui.upgrade_in_event_loop(move |ui| {
                for chapter in queued {
                    startup.download(&ui, chapter);
                }
                startup.refresh_download_states(&ui);
            });
        }
    });
    let result = ui.run().map_err(error);
    app.metadata_refresh.pause();
    app.image_worker_live.store(false, Ordering::Release);
    app.invalidate();
    crate::reader_export::shutdown();
    app.flush();
    app.invalidate();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installed_adult_source_is_visible_without_toggling() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "app::tests::installed_adult_source_child",
            ])
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    #[ignore = "isolated process for Nova's single application directory"]
    fn installed_adult_source_child() {
        use std::io::{Cursor, Write};
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let services = Services::open(&dir.path().join("data")).unwrap();
        let (images, _receiver) = mpsc::channel();
        let app = Controller {
            progress_checkpoint: crate::progress_checkpoint::ProgressWriter::new(
                services.domain.clone(),
            )
            .unwrap(),
            artwork: crate::artwork::ArtworkLoader::new(
                services.clone(),
                ui.as_weak(),
                dir.path().join("posters"),
            ),
            services,
            chapter_cache: Arc::new(crate::chapter_cache::ChapterCache::new(
                dir.path().join("chapters"),
            )),
            image_worker_live: AtomicBool::new(true),
            view: Mutex::new(View {
                section: 3,
                ..View::default()
            }),
            generation: AtomicU64::new(0),
            cancel: Mutex::new(CancelToken::default()),
            images,
            download_serial: Mutex::new(()),
            active_image: Mutex::new(None),
            metadata_refresh: Arc::default(),
        };
        let mut manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../examples/addons/neutral-fixture/manifest.json"
        ))
        .unwrap();
        manifest["nsfw"] = json!(true);
        manifest["assets"] = json!(["icon.png"]);
        manifest["icon"] = json!("icon.png");
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            (
                "main.js",
                include_bytes!("../examples/addons/neutral-fixture/main.js").to_vec(),
            ),
            (
                "icon.png",
                include_bytes!("../examples/addons/mangafire/icon.png").to_vec(),
            ),
        ] {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(&bytes).unwrap();
        }
        let path = dir.path().join("addon.zip");
        std::fs::write(&path, archive.finish().unwrap().into_inner()).unwrap();
        ui.set_show_adult(true);
        let (descriptor, sources) = app
            .install_sources(path.clone(), ui.get_show_adult())
            .unwrap();
        assert!(descriptor.nsfw && descriptor.enabled);
        app.publish_sources(&ui, sources);
        assert_eq!(ui.get_source_rows().row_count(), 1);
        assert_eq!(
            ui.get_source_rows().row_data(0).unwrap().id,
            "neutral-fixture"
        );
        assert_eq!(ui.get_extensions().row_count(), 1);
        assert_eq!(
            ui.get_extensions().row_data(0).unwrap().icon.size().width,
            64
        );
        assert_eq!(
            ui.get_source_rows().row_data(0).unwrap().icon.size().width,
            64
        );

        crate::source_catalogs::set_enabled("neutral-fixture", "neutral-fixture", false).unwrap();
        assert!(app.sources(true).unwrap().is_empty());
        let settings = catalogue_rows(app.catalogue_choices("neutral-fixture").unwrap());
        assert_eq!(settings.len(), 1);
        assert!(!settings[0].checked);
        assert_eq!(app.services.sources.list().unwrap().len(), 1);
        assert!(app.services.sources.source("neutral-fixture").is_ok());
        assert!(
            crate::source_catalogs::set_enabled("other-package", "neutral-fixture", false).is_err()
        );
        crate::source_catalogs::set_enabled("neutral-fixture", "neutral-fixture", true).unwrap();
        assert_eq!(app.sources(true).unwrap().len(), 1);
        assert!(catalogue_rows(app.catalogue_choices("neutral-fixture").unwrap())[0].checked);

        // Reinstalling with visibility off still updates Extensions, without
        // exposing an adult source in the filtered list.
        ui.set_show_adult(false);
        let (_, sources) = app.install_sources(path, ui.get_show_adult()).unwrap();
        app.publish_sources(&ui, sources);
        assert_eq!(ui.get_source_rows().row_count(), 0);
        assert_eq!(ui.get_extensions().row_count(), 1);
        assert_eq!(
            ui.get_extensions().row_data(0).unwrap().icon.size().width,
            64
        );
    }

    #[test]
    fn export_capture_source_rows_stay_native_across_density_zoom_and_capture_resolution() {
        for density in [1., 1.25, 1.5, 2.75, 3.] {
            for zoom in [0.75, 1., 1.4, 2.] {
                let scale = 392. / 720. * density * zoom;
                let page_height = (25000. * scale) as f32;
                let viewport_y = 24. * density;
                let page_y = (viewport_y - 12480. * scale) as f32;
                assert_eq!(
                    export_source_row(f64::from(page_y), f64::from(page_height), viewport_y, 25000),
                    12480
                );
            }
        }
        assert_eq!(export_source_row(20., 100., 0., 25000), 0);
        assert_eq!(export_source_row(-200., 100., 0., 25000), 24999);
    }
    #[test]
    fn image_handoff_cancellation_releases_pending_pixels_without_waiting_for_ui() {
        struct Pixels(Arc<AtomicU64>);
        impl Drop for Pixels {
            fn drop(&mut self) {
                self.0.store(0, Ordering::Release);
            }
        }
        let bytes = Arc::new(AtomicU64::new(32 * 1024 * 1024));
        let pending = Arc::new(Mutex::new(Some(Pixels(bytes.clone()))));
        let (sent, ack) = mpsc::sync_channel(1);
        let late_ui = ImageCompletion(sent);
        assert!(!wait_image_completion(&pending, &ack, || false));
        assert_eq!(bytes.load(Ordering::Acquire), 0);
        assert!(pending.lock().unwrap().is_none());
        drop(late_ui); // An abandoned UI callback still acknowledges on Drop.
        assert!(
            ack.recv_timeout(std::time::Duration::from_millis(50))
                .is_ok()
        );
    }
    #[test]
    fn encoded_ram_hits_are_partitioned_by_live_source_context() {
        let mut cache = EncodedPages::default();
        let mut calls = 0;
        let first = cache
            .read_scoped(1, 0, "scope-a", || {
                calls += 1;
                Ok(vec![1])
            })
            .unwrap();
        let again = cache
            .read_scoped(1, 0, "scope-a", || {
                panic!("same authorized context should reuse")
            })
            .unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        let changed = cache
            .read_scoped(1, 0, "scope-b", || {
                calls += 1;
                Ok(vec![2])
            })
            .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(changed.as_slice(), [2]);
        assert!(!Arc::ptr_eq(&first, &changed));
    }
    fn tall_reader() -> Reader {
        Reader {
            chapter: ChapterId::new(reader_domain::SeriesId::new("fixture", "sharp"), "chapter"),
            source: None,
            pages: (0..2)
                .map(|i| PageInput::Local {
                    path: PathBuf::new(),
                    id: format!("page-{i}"),
                })
                .collect(),
            ratios: vec![20000. / 720.; 2],
            requested: BTreeSet::new(),
            retained: BTreeSet::new(),
            desired: BTreeSet::new(),
            failed: BTreeSet::new(),
            budget_blocked: BTreeSet::new(),
            desired_anchor: usize::MAX,
            decoded_bytes: BTreeMap::new(),
            decoded_widths: BTreeMap::new(),
            decoded_windows: BTreeMap::new(),
            decoded_responses: BTreeMap::new(),
            native_dimensions: BTreeMap::new(),
            target_width: 1280,
            viewport_width: 390.,
            viewport_height: 844.,
            anchor: ResumeAnchor::new("page-0", 0, 0.).unwrap(),
            mode: ReadingMode::Webtoon,
            mode_choice: ModeChoice::Automatic,
            automatic_mode: ReadingMode::Webtoon,
            mode_switching: false,
            completed: false,
        }
    }
    #[test]
    fn native_width_windows_follow_each_mode_density_and_zoom() {
        let dimensions = reader_engine::PageDimensions {
            width: 720,
            height: 20000,
        };
        let mut reader = tall_reader();
        for mode in [
            ReadingMode::Webtoon,
            ReadingMode::LeftToRight,
            ReadingMode::RightToLeft,
            ReadingMode::Vertical,
        ] {
            reader.mode = mode;
            for dpr in [1., 1.25, 1.5, 2.75, 3.] {
                for zoom in [0.75, 1., 1.5, 3.] {
                    reader.viewport_width = (390_f32 * zoom).min(1100.);
                    reader.target_width = Reader::decode_width(reader.viewport_width, dpr);
                    let before = reader.anchor.clone();
                    let (width, (start, end), budget) =
                        reader.window_request(0, dimensions).unwrap();
                    let plan =
                        reader_engine::plan_display_window(dimensions, width, start, end, budget)
                            .unwrap();
                    assert_eq!(plan.width, width.min(720));
                    if mode == ReadingMode::Webtoon && reader.viewport_width * dpr >= 720. {
                        assert_eq!(plan.width, 720);
                    }
                    if mode != ReadingMode::Webtoon {
                        assert!(
                            width
                                <= Reader::decode_width(
                                    reader.viewport_height / (20000. / 720.) * dpr,
                                    1.
                                )
                        );
                    }
                    assert!(plan.covers(start, end));
                    assert!(plan.byte_len() <= reader.window_budget());
                    assert!(
                        READER_MODEL_BYTES + plan.byte_len() <= 64 * 1024 * 1024,
                        "replacement headroom must include old+new buffers"
                    );
                    assert_eq!(reader.anchor, before);
                }
            }
        }
        // Unknown source dimensions must not request a whole tall page on the
        // first load: actual probed ratio determines the visible row span.
        reader = tall_reader();
        reader.ratios = vec![1.5; 2];
        let (width, (start, end), budget) = reader.window_request(0, dimensions).unwrap();
        assert!(end < 0.1);
        assert_eq!(width, 1280);
        assert_eq!(
            reader_engine::plan_display_window(dimensions, width, start, end, budget)
                .unwrap()
                .width,
            720
        );
    }
    #[test]
    fn windows_refresh_before_exhaustion_and_partial_pages_cannot_complete() {
        let dimensions = reader_engine::PageDimensions {
            width: 720,
            height: 20000,
        };
        let mut reader = tall_reader();
        let (width, (start, end), budget) = reader.window_request(0, dimensions).unwrap();
        let first =
            reader_engine::plan_display_window(dimensions, width, start, end, budget).unwrap();
        reader.retained.insert(0);
        reader.decoded_windows.insert(0, first);
        reader.decoded_bytes.insert(0, first.byte_len());
        assert!(reader.has_visible_pixels(0));
        assert!(!reader.needs_window(0));
        let visible_rows = (end * 20000.).ceil() as u32;
        reader.anchor = ResumeAnchor::new(
            "page-0",
            0,
            f64::from(first.first_row + first.rows - visible_rows - 256) / 20000.,
        )
        .unwrap();
        assert!(reader.has_visible_pixels(0));
        assert!(
            reader.needs_window(0),
            "request ahead while old pixels still cover viewport"
        );
        for offset in [0.8, 0.1, 0.95, 0.0] {
            reader.anchor = ResumeAnchor::new("page-0", 0, offset).unwrap();
            let (width, (start, end), budget) = reader.window_request(0, dimensions).unwrap();
            let next =
                reader_engine::plan_display_window(dimensions, width, start, end, budget).unwrap();
            reader.decoded_windows.insert(0, next);
            reader.decoded_bytes.insert(0, next.byte_len());
            assert!(reader.has_visible_pixels(0));
            assert!(!reader.needs_window(0));
        }
        reader.retained.insert(1);
        reader.decoded_windows.insert(1, first);
        reader.anchor = ResumeAnchor::new("page-1", 1, 0.99).unwrap();
        assert!(
            !reader.has_visible_pixels(1),
            "old top window is not a read bottom window"
        );
        let (width, (start, end), budget) = reader.window_request(1, dimensions).unwrap();
        let bottom =
            reader_engine::plan_display_window(dimensions, width, start, end, budget).unwrap();
        reader.decoded_windows.insert(1, bottom);
        assert!(reader.has_visible_pixels(1));
        reader.anchor = ResumeAnchor::new("page-0", 0, 0.98).unwrap();
        assert_eq!(reader.visible_end(), 1);
        let mut total = 0;
        for index in 0..2 {
            let (w, (a, b), budget) = reader.window_request(index, dimensions).unwrap();
            total += reader_engine::plan_display_window(dimensions, w, a, b, budget)
                .unwrap()
                .byte_len();
        }
        assert!(
            total <= READER_MODEL_BYTES,
            "both visible pages must fit together"
        );
    }
    #[test]
    fn long_visible_window_defers_already_queued_prefetch_until_final_window() {
        let mut reader = tall_reader();
        reader.ratios[0] = 25000. / 720.;
        reader.desired.extend([0, 1]);
        reader.requested.extend([0, 1]);
        // Both tasks are initially queued before dimensions are known.
        assert!(!reader.defers_prefetch(1));
        let dimensions = reader_engine::PageDimensions {
            width: 720,
            height: 25000,
        };
        let (width, (start, end), budget) = reader.window_request(0, dimensions).unwrap();
        let plan =
            reader_engine::plan_display_window(dimensions, width, start, end, budget).unwrap();
        assert!(plan.first_row + plan.rows < plan.full_height);
        reader.decoded_windows.insert(0, plan);
        reader.retained.insert(0);
        reader.requested.remove(&0);
        assert!(reader.defers_prefetch(1));
        assert!(!reader.defers_prefetch(0), "visible work is never delayed");
        assert!(
            !reader.start_image(1),
            "the queued worker task must recheck after publication"
        );
        assert!(
            !reader.requested.contains(&1),
            "a suppressed task must not leave a stuck request marker"
        );
        assert!(
            reader.defers_prefetch(1),
            "ordinary scroll ticks intentionally skip this target"
        );
        // Its last admitted window enables normal prefetch before the next page
        // becomes visible, without replacing the long page's display strips.
        reader.anchor = ResumeAnchor::new("page-0", 0, 0.9).unwrap();
        let (width, (start, end), budget) = reader.window_request(0, dimensions).unwrap();
        let last =
            reader_engine::plan_display_window(dimensions, width, start, end, budget).unwrap();
        assert_eq!(reader.visible_end(), 0);
        assert_eq!(last.first_row + last.rows, last.full_height);
        reader.decoded_windows.insert(0, last);
        assert!(!reader.defers_prefetch(1));
        reader.requested.insert(1);
        assert!(reader.start_image(1));
        // The reported 719×3500 page fits its admitted window, so its usual
        // next-page prefetch stays active from the start.
        reader.anchor = ResumeAnchor::new("page-0", 0, 0.).unwrap();
        reader.ratios[0] = 3500. / 719.;
        let short = reader_engine::PageDimensions {
            width: 719,
            height: 3500,
        };
        let (width, (start, end), budget) = reader.window_request(0, short).unwrap();
        let plan = reader_engine::plan_display_window(short, width, start, end, budget).unwrap();
        assert_eq!(plan.rows, plan.full_height);
        reader.decoded_windows.insert(0, plan);
        assert!(!reader.defers_prefetch(1));
        assert!(reader.start_image(1));
    }

    #[test]
    fn stale_generation_and_shorter_reader_cannot_plan_old_page() {
        let mut reader = tall_reader();
        reader.pages.truncate(1);
        reader.ratios.truncate(1);
        let dimensions = reader_engine::PageDimensions {
            width: 720,
            height: 20000,
        };
        assert!(
            task_window_request(
                &reader,
                2,
                &ImageTask {
                    generation: 1,
                    index: 0
                },
                dimensions
            )
            .is_err()
        );
        assert!(
            task_window_request(
                &reader,
                2,
                &ImageTask {
                    generation: 2,
                    index: 19
                },
                dimensions
            )
            .is_err()
        );
        assert!(
            task_window_request(
                &reader,
                2,
                &ImageTask {
                    generation: 2,
                    index: 0
                },
                dimensions
            )
            .is_ok()
        );
    }
    #[test]
    fn moving_windows_reuse_encoded_source_bytes_and_generation_invalidates_cache() {
        let mut cache = EncodedPages::default();
        let mut requests = 0;
        for _ in [0.0, 0.4, 0.8, 0.1, 0.0] {
            let bytes = cache
                .read(1, 0, || {
                    requests += 1;
                    Ok(vec![7; 1000])
                })
                .unwrap();
            assert_eq!(bytes[0], 7);
        }
        assert_eq!(requests, 1, "window movement must not refetch source bytes");
        cache
            .read(1, 1, || {
                requests += 1;
                Ok(vec![9; 1000])
            })
            .unwrap();
        cache
            .read(1, 0, || {
                requests += 1;
                Ok(vec![7; 1000])
            })
            .unwrap();
        assert_eq!(requests, 2);
        cache
            .read(2, 0, || {
                requests += 1;
                Ok(vec![8; 1000])
            })
            .unwrap();
        assert_eq!(requests, 3);
        assert!(cache.bytes <= 32 * 1024 * 1024);
    }
    #[test]
    fn slow_scrolling_720_by_25000_retains_overlapping_tiles_and_model_delegates() {
        use i_slint_core::model::{ModelChangeListener, ModelChangeListenerContainer};
        use std::{cell::Cell, pin::Pin};
        #[derive(Default)]
        struct Changes {
            changed: Cell<usize>,
            added: Cell<usize>,
            removed: Cell<usize>,
            reset: Cell<usize>,
            current_count: Cell<usize>,
            high_water: Cell<usize>,
        }
        impl ModelChangeListener for Changes {
            fn row_changed(self: Pin<&Self>, _: usize) {
                self.changed.set(self.changed.get() + 1);
            }
            fn row_added(self: Pin<&Self>, _: usize, count: usize) {
                self.added.set(self.added.get() + count);
                self.current_count.set(self.current_count.get() + count);
                self.high_water
                    .set(self.high_water.get().max(self.current_count.get()));
            }
            fn row_removed(self: Pin<&Self>, _: usize, count: usize) {
                self.removed.set(self.removed.get() + count);
                self.current_count.set(self.current_count.get() - count);
            }
            fn reset(self: Pin<&Self>) {
                self.reset.set(self.reset.get() + 1);
            }
        }
        let changes = Box::pin(ModelChangeListenerContainer::<Changes>::default());
        let model = ModelRc::new(VecModel::<TileRow>::default());
        model
            .model_tracker()
            .attach_peer(changes.as_ref().model_peer());
        let dimensions = reader_engine::PageDimensions {
            width: 720,
            height: 25000,
        };
        let budget = READER_MODEL_BYTES.min(
            (reader_engine::DecodeLimits::default().max_decoded_bytes as usize - 720 * 25000 * 4)
                / 2,
        );
        let mut previous: Option<reader_engine::DisplayWindowPlan> = None;
        let mut refreshed = 0;
        let tight_budget = 1602 * 720 * 4;
        let cases = (0usize..23400)
            .step_by(64)
            .map(|first| (first, budget, true))
            .chain(
                (0usize..23400)
                    .step_by(64)
                    .rev()
                    .map(|first| (first, budget, true)),
            )
            .chain([20000, 1000, 23000, 3000].map(|first| (first, budget, false)))
            .chain([3010, 3074, 3138, 4010, 4074, 3010].map(|first| (first, tight_budget, false)));
        for (first, current_budget, incremental) in cases {
            let start = first as f64 / 25000.;
            let end = (first + 1600) as f64 / 25000.;
            if previous.is_some_and(|plan| {
                plan.covers_with_margin(start, end) && plan.byte_len() <= current_budget
            }) {
                continue;
            }
            let plan =
                reader_engine::plan_display_window(dimensions, 1280, start, end, current_budget)
                    .unwrap();
            assert_eq!(plan.width, 720);
            assert!(plan.covers(start, end));
            assert!(plan.byte_len() <= current_budget);
            if current_budget == tight_budget {
                assert!(
                    plan.first_row % reader_engine::DISPLAY_TILE_ROWS != 0
                        || plan.rows < reader_engine::DISPLAY_TILE_ROWS,
                    "exercise budget-limited partial strip edges"
                );
            }
            let mut pixels = vec![255; plan.byte_len()];
            for row in 0..plan.rows {
                let index = row as usize * 720 * 4;
                pixels[index] = ((plan.first_row + row) % 251) as u8;
            }
            let old = previous
                .map(tile_rects)
                .unwrap_or_default()
                .into_iter()
                .zip(model.iter().map(|row| row.image))
                .collect::<BTreeMap<_, _>>();
            let prepared = prepare_display_tiles(
                &pixels,
                720,
                plan.rows,
                plan.first_row,
                plan.full_height,
                previous,
            );
            let entering = prepared.iter().filter(|tile| tile.pixels.is_some()).count();
            if incremental && previous.is_some() {
                assert!(
                    entering <= 1,
                    "slow scroll must upload only its entering strip"
                );
            }
            let keys = tile_rects(plan);
            let leaving = old.keys().filter(|key| !keys.contains(key)).count();
            let added_before = changes.added.get();
            let removed_before = changes.removed.get();
            let row_limit = model.row_count().max(keys.len());
            changes.high_water.set(model.row_count());
            let updated = reconcile_display_tiles(&model, previous, prepared).unwrap();
            assert!(
                changes.high_water.get() <= row_limit,
                "drop outgoing strips before adding new ones, including reverse scroll and jumps"
            );
            assert_eq!(changes.current_count.get(), keys.len());
            assert_eq!(updated, model, "keep the repeated model identity");
            assert_eq!(changes.added.get() - added_before, entering);
            assert_eq!(changes.removed.get() - removed_before, leaving);
            assert_eq!(
                changes.changed.get(),
                0,
                "overlap delegates must not be rebound"
            );
            assert_eq!(
                changes.reset.get(),
                0,
                "never reset the repeated tile model"
            );
            for (key, row) in keys.iter().zip(model.iter()) {
                let buffer = row.image.to_rgba8().unwrap();
                assert_eq!(buffer.as_bytes()[0], (key.top % 251) as u8);
                if let Some(old) = old.get(key) {
                    assert_eq!(row.image, *old);
                    assert_eq!(
                        buffer.as_bytes().as_ptr(),
                        old.to_rgba8().unwrap().as_bytes().as_ptr(),
                        "keep the exact pixel allocation for overlap"
                    );
                }
            }
            assert_eq!(
                model
                    .iter()
                    .map(|row| row.image.size().width as usize
                        * row.image.size().height as usize
                        * 4)
                    .sum::<usize>(),
                plan.byte_len()
            );
            previous = Some(plan);
            refreshed += 1;
        }
        assert!(
            refreshed >= 8,
            "exercise repeated slow-scroll window transitions"
        );
        let source = Arc::new(vec![1]);
        let identity = Arc::downgrade(&source);
        assert!(same_response(Some(&identity), &identity));
        let replacement = Arc::new(vec![1]);
        assert!(
            !same_response(Some(&identity), &Arc::downgrade(&replacement)),
            "same bytes in a new response must not reuse old tiles"
        );
        let plan = previous.unwrap();
        let prepared = prepare_display_tiles(
            &vec![255; plan.byte_len()],
            plan.width,
            plan.rows,
            plan.first_row,
            plan.full_height,
            previous,
        );
        assert!(
            reconcile_display_tiles(&ModelRc::default(), previous, prepared).is_none(),
            "reject evicted backing tiles before changing the model"
        );
        assert!(
            model.row_count() > 0,
            "a rejected completion must leave existing art intact"
        );
    }

    #[test]
    fn horizontal_tiles_preserve_source_pixels_and_partial_row_coordinates() {
        let image = image::RgbaImage::from_fn(5000, 8, |x, y| {
            image::Rgba([(x % 251) as u8, (x / 251) as u8, y as u8, 255])
        });
        let tiles = display_window_tiles(image.as_raw(), 5000, 8, 5000, 20000);
        assert_eq!(tiles.len(), 3);
        for (index, tile) in tiles.iter().enumerate() {
            assert!(tile.image.size().width <= 2048 && tile.image.size().height <= 2048);
            assert_eq!(tile.offset, 0.25);
            assert_eq!(tile.bottom, 5008. / 20000.);
            let pixels = tile.image.to_rgba8().unwrap();
            for y in 0..8 {
                for x in [0, pixels.width() - 1] {
                    let pixel = pixels.as_slice()[(y * pixels.width() + x) as usize];
                    let expected = image.get_pixel(index as u32 * 2048 + x, y);
                    assert_eq!([pixel.r, pixel.g, pixel.b, pixel.a], expected.0);
                }
            }
        }
        assert_eq!(tiles[0].left, 0.);
        assert_eq!(tiles.last().unwrap().right, 1.);
        for pair in tiles.windows(2) {
            assert_eq!(pair[0].right, pair[1].left);
        }
    }

    #[test]
    fn entry_category_membership_persists_without_changing_other_sources_or_progress() {
        const MODE: &str = "READER_ENTRY_MEMBERSHIP_MODE";
        const DIRECTORY: &str = "READER_ENTRY_MEMBERSHIP_DIRECTORY";
        if let Ok(mode) = std::env::var(MODE) {
            let store = reader_domain::DomainStore::open(std::path::Path::new(
                &std::env::var(DIRECTORY).unwrap(),
            ))
            .unwrap();
            let first = reader_domain::SeriesId::new("source-a", "same");
            let second = reader_domain::SeriesId::new("source-b", "same");
            if mode == "write" {
                store
                    .update(|state| {
                        state.categories = vec![
                            reader_domain::Category {
                                id: "favorite-id".into(),
                                name: "Favorites".into(),
                            },
                            reader_domain::Category {
                                id: "other-id".into(),
                                name: "Other".into(),
                            },
                        ];
                        for id in [&first, &second] {
                            state.series.push(Series {
                                publication_status: Default::default(),
                                id: id.clone(),
                                title: "Shared title".into(),
                                description: String::new(),
                                authors: vec![],
                                genres: vec![],
                                cover_url: None,
                                canonical_url: None,
                            });
                            update_entry_membership(state, id, "add")?;
                        }
                        state.library[1].categories.insert("other-id".into());
                        state.library[1].bookmarked = true;
                        state.library[1].preferences = Some(reader_domain::ReaderPreferences {
                            gap_pixels: 17,
                            ..Default::default()
                        });
                        state.chapters.push(Chapter {
                            id: ChapterId::new(first.clone(), "chapter"),
                            title: "Chapter".into(),
                            label: "1".into(),
                            number: Some("1".into()),
                            volume: None,
                            source_order: 0,
                            language: None,
                            scanlator: None,
                            uploaded_at: None,
                        });
                        state.progress.push(ReadingProgress {
                            chapter: ChapterId::new(first.clone(), "chapter"),
                            anchor: ResumeAnchor::new("page", 0, 0.25)?,
                            mode: ReadingMode::Webtoon,
                            last_read_at: 7,
                            completed: true,
                        });
                        Ok(())
                    })
                    .unwrap();
                for action in [
                    "category:favorite-id",
                    "category:favorite-id",
                    "remove",
                    "add",
                    "category:favorite-id",
                ] {
                    store
                        .update(|state| update_entry_membership(state, &first, action))
                        .unwrap();
                }
                let original = store.snapshot().unwrap();
                for code in [6, 5, 7, 2, 1, 0, 7] {
                    store
                        .update(|state| {
                            update_entry_membership(state, &first, &format!("status:{code}"))
                        })
                        .unwrap();
                }
                let after = store.snapshot().unwrap();
                assert_eq!(after.progress, original.progress);
                assert_eq!(after.history, original.history);
                assert_eq!(
                    after.library.iter().find(|entry| entry.series == second),
                    original.library.iter().find(|entry| entry.series == second)
                );
                let before = store.snapshot().unwrap();
                assert!(
                    store
                        .update(|state| update_entry_membership(
                            state,
                            &first,
                            "category:missing-id"
                        ))
                        .is_err()
                );
                assert_eq!(store.snapshot().unwrap(), before);
            }
            assert_eq!(
                store
                    .snapshot()
                    .unwrap()
                    .library
                    .iter()
                    .find(|entry| entry.series == first)
                    .unwrap()
                    .reading_status,
                reader_domain::ReadingStatus::Completed
            );
            assert_eq!(
                store
                    .snapshot()
                    .unwrap()
                    .library
                    .iter()
                    .find(|entry| entry.series == second)
                    .unwrap()
                    .reading_status,
                reader_domain::ReadingStatus::Auto
            );
            let snapshot = store.snapshot().unwrap();
            let first = snapshot
                .library
                .iter()
                .find(|entry| entry.series == first)
                .unwrap();
            let second = snapshot
                .library
                .iter()
                .find(|entry| entry.series == second)
                .unwrap();
            assert_eq!(first.categories, BTreeSet::from(["favorite-id".into()]));
            assert_eq!(second.categories, BTreeSet::from(["other-id".into()]));
            assert!(second.bookmarked);
            assert_eq!(second.preferences.as_ref().unwrap().gap_pixels, 17);
            assert_eq!(snapshot.progress.len(), 1);
            assert!(snapshot.progress[0].completed);
            assert_eq!(snapshot.progress[0].last_read_at, 7);
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        for mode in ["write", "reload"] {
            let output=std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","app::tests::entry_category_membership_persists_without_changing_other_sources_or_progress","--nocapture"])
                .env(MODE,mode).env(DIRECTORY,directory.path()).output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn continue_label_and_action_share_the_actual_resume_target() {
        let series = reader_domain::SeriesId::new("fixture", "series");
        let chapters = (0..3)
            .map(|i| Chapter {
                id: ChapterId::new(series.clone(), format!("chapter-{i}")),
                title: format!("Story chapter {i}"),
                label: String::new(),
                number: Some(if i == 2 {
                    "106.1".into()
                } else {
                    i.to_string()
                }),
                volume: None,
                source_order: i,
                language: None,
                scanlator: None,
                uploaded_at: None,
            })
            .collect::<Vec<_>>();
        let mut progress = vec![];
        assert_eq!(
            resume_chapter(&chapters, &progress).unwrap().id,
            chapters[0].id
        );
        for (i, completed, last_read_at) in [(0, true, 20), (1, false, 10), (2, false, 30)] {
            progress.push(ReadingProgress {
                chapter: chapters[i].id.clone(),
                anchor: ResumeAnchor::default(),
                mode: ReadingMode::Webtoon,
                last_read_at,
                completed,
            });
        }
        let target = resume_chapter(&chapters, &progress).unwrap();
        assert_eq!(target.id, chapters[2].id);
        assert_eq!(resume_label(target), "106.1");
        progress[2].completed = true;
        assert_eq!(
            resume_chapter(&chapters, &progress).unwrap().id,
            chapters[1].id
        );
        progress[1].completed = true;
        assert_eq!(
            resume_chapter(&chapters, &progress).unwrap().id,
            chapters[0].id
        );
    }

    #[test]
    fn logical_reflow_visible_window_and_tall_textures() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(516., 700.));
        let dir = tempfile::tempdir().unwrap();
        let (images, _receiver) = mpsc::channel();
        let chapter = ChapterId::new(reader_domain::SeriesId::new("fixture", "series"), "chapter");
        let reader = Reader {
            chapter,
            source: None,
            pages: (0..20)
                .map(|i| PageInput::Local {
                    path: PathBuf::new(),
                    id: format!("stable-{i}"),
                })
                .collect(),
            ratios: vec![0.1; 20],
            requested: BTreeSet::new(),
            retained: BTreeSet::new(),
            desired: BTreeSet::new(),
            failed: BTreeSet::new(),
            budget_blocked: BTreeSet::new(),
            desired_anchor: usize::MAX,
            decoded_bytes: BTreeMap::new(),
            decoded_widths: BTreeMap::new(),
            decoded_windows: BTreeMap::new(),
            decoded_responses: BTreeMap::new(),
            native_dimensions: BTreeMap::new(),
            target_width: 0,
            viewport_width: 500.,
            viewport_height: 500.,
            anchor: ResumeAnchor::new("stable-2", 2, 0.375).unwrap(),
            mode: ReadingMode::Webtoon,
            mode_choice: ModeChoice::Automatic,
            automatic_mode: ReadingMode::Webtoon,
            mode_switching: false,
            completed: false,
        };
        let services = Services::open(&dir.path().join("data")).unwrap();
        let app = Arc::new(Controller {
            progress_checkpoint: crate::progress_checkpoint::ProgressWriter::new(
                services.domain.clone(),
            )
            .unwrap(),
            artwork: crate::artwork::ArtworkLoader::new(
                services.clone(),
                ui.as_weak(),
                dir.path().join("cache").join("posters"),
            ),
            services,
            chapter_cache: Arc::new(crate::chapter_cache::ChapterCache::new(
                dir.path().join("cache/viewed-chapters"),
            )),
            image_worker_live: AtomicBool::new(true),
            view: Mutex::new(View {
                reader: Some(reader),
                section: 6,
                ..View::default()
            }),
            generation: AtomicU64::new(1),
            cancel: Mutex::new(CancelToken::default()),
            images,
            download_serial: Mutex::new(()),
            active_image: Mutex::new(None),
            metadata_refresh: Arc::default(),
        });
        // A direct History/Updates chapter open has no mounted detail hero.
        // Its OSD and capture folder must use the active series identity.
        let active_series = app.view().reader.as_ref().unwrap().chapter.series.clone();
        app.view().series = Some(Series {
            publication_status: Default::default(),
            id: active_series,
            title: "Current reader series".into(),
            description: String::new(),
            authors: vec![],
            genres: vec![],
            cover_url: None,
            canonical_url: None,
        });
        ui.set_series_title("Stale detail title".into());
        app.render(&ui);
        assert_eq!(ui.get_series_title(), "Current reader series");
        assert_eq!(
            reader_series_title(&app.view()),
            Some("Current reader series")
        );
        app.view().series.as_mut().unwrap().id.local_id = "another-series".into();
        assert_eq!(reader_series_title(&app.view()), None);
        app.view().series.as_mut().unwrap().id.local_id = "series".into();
        app.mount_pages(&ui);
        let before = app.view().reader.as_ref().unwrap().anchor.clone();
        ui.set_zoom(2.);
        app.reflow(&ui, ui.get_reader_width(), 500.);
        assert_eq!(app.view().reader.as_ref().unwrap().anchor, before);
        let expected = (0.2 + 0.1 * 0.375) * ui.get_reader_width();
        assert!((-ui.get_scroll_y() - expected).abs() < 0.01);
        app.position(&ui, 0., 500., 500.); // stale pixels from the old layout
        assert_eq!(app.view().reader.as_ref().unwrap().anchor, before);
        ui.set_zoom(1.);
        app.reflow(&ui, 500., 500.);
        app.position(&ui, 0., 500., 500.);
        let mut view = app.view();
        let reader = view.reader.as_mut().unwrap();
        assert!(reader.desired.contains(&9));
        assert!(reader.desired.contains(&10)); // prefetch after ten visible pages
        assert!(!reader.completed); // unloaded placeholders cannot complete
        reader.failed.insert(0);
        reader.requested.clear();
        drop(view);
        app.request_images(&ui, 0);
        assert!(!app.view().reader.as_ref().unwrap().requested.contains(&0));

        assert_eq!(Reader::decode_width(500., 1.), 512);
        assert_eq!(Reader::decode_width(501., 1.), 512);
        assert_eq!(Reader::decode_width(500., 2.), 1024);
        assert_eq!(Reader::decode_width(1100., 3.), 3328);

        // A low-resolution preview must upgrade at the same anchor, in every
        // reading mode, even when the earlier decode completes after reflow.
        for mode in [
            ReadingMode::Webtoon,
            ReadingMode::LeftToRight,
            ReadingMode::RightToLeft,
            ReadingMode::Vertical,
        ] {
            let before = {
                let mut view = app.view();
                let reader = view.reader.as_mut().unwrap();
                reader.mode = mode;
                reader.failed.clear();
                reader.requested.clear();
                reader.retained.insert(0);
                reader.decoded_widths.insert(0, 128);
                reader.decoded_bytes.insert(0, 128 * 100 * 4);
                reader.anchor.clone()
            };
            app.reflow(&ui, 500., 500.);
            {
                let mut view = app.view();
                let reader = view.reader.as_mut().unwrap();
                assert_eq!(reader.anchor, before);
                assert!(reader.retained.contains(&0)); // keep the preview
                assert!(reader.requested.contains(&0));
                // Finish an in-flight request for the old width. Completion
                // rechecks the window after removing the requested marker.
                reader.requested.remove(&0);
                reader.decoded_widths.insert(0, 128);
            }
            app.request_images(&ui, 0);
            assert!(app.view().reader.as_ref().unwrap().requested.contains(&0));
            {
                let mut view = app.view();
                let reader = view.reader.as_mut().unwrap();
                reader.requested.remove(&0);
                reader.decoded_widths.insert(0, reader.target_width);
                // A source can remain 128px wide despite satisfying the full
                // requested quality; actual texture size must not retry it.
            }
            app.request_images(&ui, 0);
            app.request_images(&ui, 0);
            assert!(!app.view().reader.as_ref().unwrap().requested.contains(&0));
            app.reflow(&ui, 1000., 500.);
            assert!(app.view().reader.as_ref().unwrap().requested.contains(&0));
            {
                let mut view = app.view();
                let reader = view.reader.as_mut().unwrap();
                reader.requested.remove(&0);
                reader.decoded_widths.insert(0, reader.target_width);
            }
            app.reflow(&ui, 250., 500.);
            assert!(!app.view().reader.as_ref().unwrap().requested.contains(&0));
            assert_eq!(app.view().reader.as_ref().unwrap().anchor, before);
        }

        // A failed durable mode write cannot change either the active reader
        // or its control. Successful choices stay per-series, never global.
        let (before_mode, before_choice, before_anchor) = {
            let view = app.view();
            let reader = view.reader.as_ref().unwrap();
            (reader.mode, reader.mode_choice, reader.anchor.clone())
        };
        ui.set_reading_mode(mode_index(before_mode));
        ui.set_reader_mode_choice(2); // the selector optimistically selected RTL
        app.choose_reader_mode(&ui, 2, |_, _| Err("disk unavailable".into()));
        assert_eq!(ui.get_reader_mode_choice(), before_choice.selection());
        assert_eq!(ui.get_reading_mode(), mode_index(before_mode));
        assert_eq!(app.view().reader.as_ref().unwrap().mode, before_mode);
        assert_eq!(app.view().reader.as_ref().unwrap().anchor, before_anchor);
        let global = app.services.domain.snapshot().unwrap().preferences.mode;
        app.choose_reader_mode(&ui, 2, reader_modes::save);
        assert_eq!(ui.get_reader_mode_choice(), 2);
        assert_eq!(
            app.view().reader.as_ref().unwrap().mode,
            ReadingMode::RightToLeft
        );
        app.reflow(&ui, 500., 500.); // image/layout work cannot restore Automatic
        assert_eq!(
            app.view().reader.as_ref().unwrap().mode_choice,
            ModeChoice::Manual(ReadingMode::RightToLeft)
        );
        assert_eq!(app.view().reader.as_ref().unwrap().anchor, before_anchor);
        app.choose_reader_mode(&ui, 4, reader_modes::save);
        assert_eq!(
            app.view().reader.as_ref().unwrap().mode,
            ReadingMode::Webtoon
        );
        assert_eq!(
            app.services.domain.snapshot().unwrap().preferences.mode,
            global
        );
        assert_eq!(app.view().reader.as_ref().unwrap().anchor, before_anchor);

        {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            reader.decoded_bytes = BTreeMap::from([(0, 12 * 1024 * 1024), (1, 16 * 1024 * 1024)]);
            assert_eq!(reader.retained_bytes_without(0), 16 * 1024 * 1024);
            // Two 16MiB visible windows fit the steady model allowance. The
            // 12MiB old preview can survive until the next frame and still
            // remains inside the separate replacement headroom.
            assert_eq!(
                reader.retained_bytes_without(0) + 16 * 1024 * 1024,
                READER_MODEL_BYTES
            );
            assert!(
                reader.decoded_bytes.values().sum::<usize>() + 16 * 1024 * 1024 <= 64 * 1024 * 1024
            );
        }

        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/addons/neutral-fixture/pages/tall.png"
        ))
        .unwrap();
        let image = reader_engine::decode_display_image(
            &bytes,
            reader_engine::DecodeLimits::default(),
            400,
            65536,
        )
        .unwrap();
        assert_eq!((image.width(), image.height()), (128, 12000));
        let tiles = display_tiles(image.as_raw(), image.width(), image.height());
        assert_eq!(tiles.len(), 6);
        assert!(
            tiles
                .iter()
                .all(|t| t.image.size().width == 128 && t.image.size().height <= 2048)
        );
        assert!((tiles.iter().map(|t| t.fraction).sum::<f32>() - 1.).abs() < 0.00001);
        assert!(
            (tiles.last().unwrap().offset + tiles.last().unwrap().fraction - 1.).abs() < 0.00001
        );
        // Chapter flags and deletion share the store's verified job state.
        // Running transfers must be stopped first; imported chapters are the
        // user's only local copy and are never treated as removable downloads.
        let series = app.view().series.clone().unwrap();
        let chapter = Chapter {
            id: ChapterId::new(series.id.clone(), "download-delete"),
            title: "Download deletion fixture".into(),
            label: "1".into(),
            number: Some("1".into()),
            volume: None,
            source_order: 0,
            language: None,
            scanlator: None,
            uploaded_at: None,
        };
        app.services
            .save_details(series.clone(), vec![chapter.clone()])
            .unwrap();
        let resource = reader_downloads::PageResource {
            id: "first".into(),
            url: "https://example.invalid/page".into(),
            headers: BTreeMap::new(),
        };
        app.services
            .downloads
            .enqueue(chapter.id.clone(), vec![resource.clone()])
            .unwrap();
        app.view().section = 5;
        app.view().download_states = app.services.downloads.statuses().unwrap();
        app.render(&ui);
        let row = ui.get_items().row_data(0).unwrap();
        assert!(row.has_download && !row.downloaded && !row.local_import);
        app.services
            .downloads
            .run(&chapter.id, |_| {
                assert!(
                    app.remove_download(&chapter.id)
                        .unwrap_err()
                        .contains("Pause or cancel")
                );
                Ok(bytes.clone())
            })
            .unwrap();
        app.view().download_states = app.services.downloads.statuses().unwrap();
        app.render(&ui);
        let row = ui.get_items().row_data(0).unwrap();
        assert!(row.has_download && row.downloaded);
        app.remove_download(&chapter.id).unwrap();
        app.view().download_states = app.services.downloads.statuses().unwrap();
        app.render(&ui);
        let row = ui.get_items().row_data(0).unwrap();
        assert!(!row.has_download && !row.downloaded);

        let local = ChapterId::new(reader_domain::SeriesId::new("local", "import"), "chapter");
        app.services
            .downloads
            .enqueue(local.clone(), vec![resource])
            .unwrap();
        app.services
            .downloads
            .run(&local, |_| Ok(bytes.clone()))
            .unwrap();
        let local_before = app.services.downloads.offline(&local).unwrap().pages;
        assert!(
            app.remove_download(&local)
                .unwrap_err()
                .contains("Imported local")
        );
        assert_eq!(
            app.services.downloads.offline(&local).unwrap().pages,
            local_before
        );

        // Each entry restores its own durable order before publishing chapter
        // rows, including entries that have never been added to the library.
        let mut second_chapter = chapter.clone();
        second_chapter.id.local_id = "second-ordering-chapter".into();
        second_chapter.title = "Second ordering chapter".into();
        second_chapter.source_order = 2;
        app.services
            .save_details(
                series.clone(),
                vec![chapter.clone(), second_chapter.clone()],
            )
            .unwrap();
        crate::reading_preferences::set_chapter_descending(&series.id, false).unwrap();
        ui.set_detail_series_key("".into());
        app.view().section = 5;
        app.render(&ui);
        assert!(!ui.get_chapters_descending());
        assert_eq!(ui.get_items().row_data(0).unwrap().title, chapter.title);
        crate::reading_preferences::set_chapter_descending(&series.id, true).unwrap();
        ui.set_detail_series_key("".into());
        app.render(&ui);
        assert!(ui.get_chapters_descending());
        assert_eq!(
            ui.get_items().row_data(0).unwrap().title,
            second_chapter.title
        );

        // Returning from a result restores its source, search/pagination and
        // catalogue rows instead of routing to the Browse management screen.
        {
            let mut view = app.view();
            view.choices = (0..2)
                .map(|i| Choice {
                    package: "fixture".into(),
                    id: format!("fixture.{i}"),
                    name: format!("Source {i}"),
                    enabled: true,
                    filters: false,
                    language: "en".into(),
                    latest: true,
                })
                .collect();
            view.saved_catalogue = Some(CatalogueState {
                source_id: "fixture.1".into(),
                rows: vec![Series {
                    publication_status: Default::default(),
                    id: reader_domain::SeriesId::new("fixture.1", "result"),
                    title: "Saved result".into(),
                    description: String::new(),
                    authors: vec![],
                    genres: vec![],
                    cover_url: None,
                    canonical_url: None,
                }],
                query: "saved query".into(),
                catalogue: "latest".into(),
                page: 3,
                has_more: true,
                page_error: "Failed saved page".into(),
                selected: 1,
                filters_active: true,
            });
            view.section = 5;
            view.query.clear();
            view.selected = 0;
            view.rows.clear();
        }
        app.return_to_catalogue(&ui);
        assert_eq!(ui.get_section(), 3);
        assert!(ui.get_source_catalog_open());
        assert_eq!(ui.get_catalogue_page_error(), "Failed saved page");
        assert_eq!(ui.get_source_index(), 1);
        assert_eq!(ui.get_items().row_data(0).unwrap().title, "Saved result");
        let view = app.view();
        assert_eq!(view.query, "saved query");
        assert_eq!(view.page, 3);
        assert!(view.has_more && view.filters_active);
        drop(view);
        // A failed continuation keeps both the committed page and source-owned
        // rows; Retry requests page + 1 again. A non-advancing source must also
        // stop automatic filling instead of repeatedly returning duplicates.
        let existing = app.view().saved_catalogue.as_ref().unwrap().rows[0].clone();
        for result in [
            Err("Page temporarily unavailable".into()),
            Ok((Vec::new(), true)),
            Ok((vec![existing.clone()], true)),
        ] {
            app.complete_browse(&ui, "fixture.1".into(), 4, true, result);
            let view = app.view();
            assert_eq!(view.page, 3);
            assert_eq!(view.rows.len(), 1);
            assert!(view.has_more);
            assert!(!ui.get_catalogue_page_error().is_empty());
            assert_eq!(view.saved_catalogue.as_ref().unwrap().page, 3);
            assert!(!view.saved_catalogue.as_ref().unwrap().page_error.is_empty());
        }
        let mut new_series = existing.clone();
        new_series.id.local_id = "next-result".into();
        app.complete_browse(
            &ui,
            "fixture.1".into(),
            4,
            true,
            Ok((vec![existing, new_series], false)),
        );
        assert_eq!(app.view().page, 4);
        assert_eq!(app.view().rows.len(), 2);
        assert!(!ui.get_has_more());
        assert!(ui.get_catalogue_page_error().is_empty());
        let generation = app.generation.load(Ordering::Acquire);
        app.browse(&ui, "", None, true);
        assert_eq!(app.generation.load(Ordering::Acquire), generation);
        // Switching to a disabled source still cancels old work and clears
        // result ownership/pagination before the disabled-source early return.
        let old_cancel = app.cancel.lock().unwrap().clone();
        {
            let mut view = app.view();
            view.choices[1].enabled = false;
            view.page = 8;
            view.has_more = true;
        }
        ui.set_has_more(true);
        ui.set_busy(true);
        app.select_source(&ui, 1, "popular");
        assert!(old_cancel.is_cancelled());
        assert!(!ui.get_busy() && !ui.get_has_more());
        {
            let view = app.view();
            assert!(view.saved_catalogue.is_none());
            assert_eq!(view.page, 0);
            assert!(!view.has_more && view.rows.is_empty());
        }
        // A failed page-one request after prior pagination cannot expose an
        // old Load more state or advance the next request past page one.
        {
            let mut view = app.view();
            view.selected = 0;
            view.page = 9;
            view.has_more = true;
        }
        ui.set_has_more(true);
        app.browse(&ui, "latest", Some("new search".into()), false);
        assert!(!ui.get_has_more());
        let view = app.view();
        assert_eq!(view.page, 0);
        assert!(!view.has_more);
        drop(view);
        app.invalidate();
        ui.set_busy(false);
        app.complete_browse(
            &ui,
            "fixture.0".into(),
            1,
            false,
            Err("First page failed".into()),
        );
        let generation = app.generation.load(Ordering::Acquire);
        app.browse(&ui, "", None, true);
        assert!(app.generation.load(Ordering::Acquire) > generation);
        assert_eq!(app.view().page, 0, "Retry does not precommit page one");
        assert!(ui.get_catalogue_page_error().is_empty());
        app.invalidate();

        // Opening an uncached catalogue title publishes its existing metadata
        // synchronously. Provider completion is deliberately not pumped here:
        // even a missing/slow source must not delay the detail route or expose
        // the prior entry's chapter rows and Continue action.
        let mut uncached = series;
        uncached.id = reader_domain::SeriesId::new("unavailable", "optimistic-detail");
        uncached.title = "Immediate catalogue title".into();
        app.open_series(&ui, uncached.clone());
        assert_eq!(ui.get_section(), 5);
        assert_eq!(ui.get_series_title(), uncached.title);
        assert_eq!(ui.get_detail_origin(), 3);
        assert_eq!(ui.get_items().row_count(), 0);
        assert!(ui.get_resume_chapter().is_empty());
        assert!(ui.get_busy());
        assert!(ui.get_status().is_empty());
        app.invalidate();
    }

    #[test]
    fn chapter_screencap_geometry_keeps_scroll_pan_and_contain_letterboxing() {
        let viewport = crate::ReaderCaptureViewport {
            x: 0.,
            y: 24.,
            width: 400.,
            height: 800.,
            content_x: -100.,
            content_y: -500.,
        };
        let row = PageRow {
            ratio: 2.,
            position: 1.5,
            ..Default::default()
        };
        let webtoon = chapter_capture_rect(&viewport, 600., 0, &row);
        assert_eq!(
            (webtoon.x, webtoon.y, webtoon.width, webtoon.height),
            (-100., 424., 600., 1200.)
        );
        for mode in 1..=3 {
            let contained = chapter_capture_rect(&viewport, 600., mode, &row);
            assert_eq!(
                (contained.x, contained.y, contained.width, contained.height),
                (0., -476., 400., 800.)
            );
        }
        let viewport = crate::ReaderCaptureViewport {
            content_x: 0.,
            content_y: 0.,
            ..viewport
        };
        let row = PageRow {
            ratio: 0.5,
            ..Default::default()
        };
        let contained = chapter_capture_rect(&viewport, 300., 1, &row);
        assert_eq!(
            (contained.x, contained.y, contained.width, contained.height),
            (50., 349., 300., 150.)
        );
    }
}
