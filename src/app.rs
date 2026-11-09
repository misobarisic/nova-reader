use crate::reader_modes::{self, ModeChoice};
use crate::{
    AppWindow, Appearance, EntryCategoryRow, ExtensionRow, ItemRow, PageRow, SettingRow, TileRow,
    page_store::{PageImage, WeakPage},
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
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
};

#[path = "chapter_batch.rs"]
mod chapter_batch;
#[path = "chapter_selection.rs"]
mod chapter_selection;
#[path = "downloads.rs"]
mod downloads;

#[path = "browse_page.rs"]
mod browse_page;
#[path = "library_navigation.rs"]
mod library_navigation;
#[path = "library_view.rs"]
mod library_view;
pub(crate) use browse_page::set_global_posters;
#[path = "timeline_groups.rs"]
mod timeline_groups;
#[path = "timeline_page.rs"]
pub(crate) mod timeline_page;

static LIFECYCLE: OnceLock<Weak<Controller>> = OnceLock::new();
#[path = "backup_actions.rs"]
mod backup_actions;
#[path = "gallery_presentation.rs"]
mod gallery_presentation;
#[path = "page_overview.rs"]
mod page_overview;
#[path = "profile_actions.rs"]
mod profile_actions;
#[path = "reader_band_read_ahead.rs"]
mod reader_band_read_ahead;
#[path = "reader_capture_editor.rs"]
mod reader_capture_editor;
#[path = "reader_capture_sources.rs"]
mod reader_capture_sources;
#[path = "reader_chapter_navigation.rs"]
mod reader_chapter_navigation;
#[path = "reader_continuity.rs"]
mod reader_continuity;
#[path = "reader_controls.rs"]
mod reader_controls;
#[cfg(test)]
#[path = "reader_handoff_tests.rs"]
mod reader_handoff_tests;
#[path = "reader_open.rs"]
mod reader_open;
#[cfg(test)]
#[path = "reader_preview.rs"]
mod reader_preview;
#[path = "reader_progressive.rs"]
mod reader_progressive;
#[cfg(test)]
#[path = "reader_read_ahead_tests.rs"]
mod reader_read_ahead_tests;
#[path = "reader_save_image.rs"]
mod reader_save_image;
#[path = "reader_scroll_admission.rs"]
mod reader_scroll_admission;
#[path = "thumbnail_cache_settings.rs"]
mod thumbnail_cache_settings;
#[cfg(target_os = "android")]
pub fn flush_lifecycle() {
    if let Some(app) = LIFECYCLE.get().and_then(Weak::upgrade) {
        app.metadata_refresh.pause();
        app.flush();
    }
}

#[derive(Clone, Default)]
struct Choice {
    package: String,
    id: String,
    name: String,
    enabled: bool,
    filters: bool,
    language: String,
    latest: bool,
    /// The package is marked adult.
    adult: bool,
    /// The package's display name ("MangaDex"), for source rows.
    extension: String,
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
}
/// Counts scroll frames that reached the reader, so a frame deferred by view
/// contention is replayed only if nothing newer has been applied since.
static POSITION_FRAME: AtomicU64 = AtomicU64::new(0);
#[derive(Clone)]
enum PageInput {
    Verified(Box<crate::page_inventory::Resource>),
    #[cfg(test)]
    Local {
        path: PathBuf,
        id: String,
    },
    Remote(reader_sources::Page),
}
impl PageInput {
    fn read(&self, source: Option<&Source>, cancel: &CancelToken) -> Result<PageImage> {
        cancel.check().map_err(error)?;
        let cap = reader_engine::current_limits().max_encoded_bytes;
        let page = match self {
            Self::Verified(resource) => resource.read(cancel)?,
            #[cfg(test)]
            Self::Local { path, id } => {
                use std::io::Read;
                reader_handoff_tests::PAGE_READS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(id.clone());
                let file = std::fs::File::open(path).map_err(error)?;
                if file.metadata().map_err(error)?.len() > cap as u64 {
                    return Err(text::tr("Image exceeds byte limit"));
                }
                let mut bytes = Vec::new();
                file.take(cap as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(error)?;
                PageImage::from_stored(bytes)?
            }
            Self::Remote(page) => PageImage::from_encoded(
                source
                    .ok_or_else(|| text::tr("Source unavailable"))?
                    .fetch_page_limited(&page.url, &page.headers, cancel, cap)
                    .map_err(error)?,
            ),
        };
        cancel.check().map_err(error)?;
        if page.encoded_len() > cap {
            return Err(text::tr("Image exceeds byte limit"));
        }
        Ok(page)
    }
}
// Leave half of the 64MiB display allowance for replacements: renderer/model
// references can survive until the next frame even after a model update.
const READER_MODEL_BYTES: usize = 32 * 1024 * 1024;
// Reader images come from flaky sources; a transient 5xx/network blip should
// not force the reader to leave and reopen the chapter. Attempts are bounded
// so a genuinely broken page settles into the visible, manual-retry state.
const IMAGE_RETRY_DELAYS_MS: [u64; 4] = [800, 2_500, 6_000, 15_000];
// 429 is a rate limit, not a blip: wait considerably longer before retrying so
// the reader does not amplify the source's throttling.
const RATE_LIMITED_RETRY_FACTOR: u64 = 5;
fn http_status(message: &str) -> Option<u16> {
    let start = message.find("http ")? + "http ".len();
    message[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}
/// Auth, not-found, unsupported/corrupt images and admission limits need a
/// different action than waiting; only transport-level failures are retried.
fn transient_image_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    // Checked before the permanent markers: "rate limited" also contains
    // "limit", but throttling is exactly the case bounded backoff should handle.
    if message.contains("rate limited") {
        return true;
    }
    for permanent in [
        "not found",
        "authentication required",
        "challenge required",
        "unavailable",
        "cancelled",
        "unsupported",
        "exceeds",
        "limit",
        "corrupt",
        "invalid image",
        "could not decode",
    ] {
        if message.contains(permanent) {
            return false;
        }
    }
    if let Some(status) = http_status(&message) {
        return status == 408 || status == 429 || (500..600).contains(&status);
    }
    [
        "network request failed",
        "timed out",
        "timeout",
        "connection reset",
        "connection refused",
        "connection closed",
        "broken pipe",
        "unexpected eof",
        "transfer failed",
    ]
    .iter()
    .any(|transient| message.contains(transient))
}
const DOWNLOAD_RETRY_DELAYS_MS: [u64; 3] = [2_000, 6_000, 15_000];
fn rate_limited_image_error(message: &str) -> bool {
    message.to_ascii_lowercase().contains("rate limited")
}
/// Next backoff for `attempt` (0 = first retry). `None` exhausts the budget or
/// marks the failure permanent.
fn image_retry_delay(message: &str, attempt: usize) -> Option<std::time::Duration> {
    if !transient_image_error(message) {
        return None;
    }
    let base = *IMAGE_RETRY_DELAYS_MS.get(attempt)?;
    let millis = if rate_limited_image_error(message) {
        base.saturating_mul(RATE_LIMITED_RETRY_FACTOR)
    } else {
        base
    };
    Some(std::time::Duration::from_millis(millis))
}
/// Downloads see the same source failures as reader images but run on a shared
/// queue thread; a short in-place backoff keeps a 5xx/network blip from failing
/// a chapter that would otherwise need a manual Retry.
fn download_retry_delay(message: &str, attempt: usize) -> Option<std::time::Duration> {
    if !transient_image_error(message) {
        return None;
    }
    let base = *DOWNLOAD_RETRY_DELAYS_MS.get(attempt)?;
    let millis = if rate_limited_image_error(message) {
        base.saturating_mul(RATE_LIMITED_RETRY_FACTOR)
    } else {
        base
    };
    Some(std::time::Duration::from_millis(millis))
}
/// Sleep in bounded slices so pause/cancel/shutdown release the queue quickly.
fn sleep_cancellable(cancel: &CancelToken, delay: std::time::Duration) -> bool {
    let step = std::time::Duration::from_millis(100);
    let mut remaining = delay;
    while !remaining.is_zero() {
        if cancel.is_cancelled() {
            return false;
        }
        let slice = remaining.min(step);
        std::thread::sleep(slice);
        remaining -= slice;
    }
    !cancel.is_cancelled()
}
struct Reader {
    chapter: ChapterId,
    source: Option<Source>,
    pages: Vec<PageInput>,
    ratios: Vec<f32>,
    requested: BTreeSet<usize>,
    retained: BTreeSet<usize>,
    desired: BTreeSet<usize>,
    failed: BTreeSet<usize>,
    /// Automatic transient-failure retries already spent, per page.
    image_retries: BTreeMap<usize, u8>,
    /// Stable page retry deadlines survive retained-reader generation changes.
    image_retry_due: BTreeMap<String, std::time::Instant>,
    budget_blocked: BTreeSet<usize>,
    desired_anchor: usize,
    /// Stable identity of a previous page blocked by scroll admission.
    reverse_demand: Option<String>,
    window_demand: Option<reader_scroll_admission::WindowDemand>,
    chapter_navigation: Option<reader_chapter_navigation::ChapterNavigation>,
    decoded_bytes: BTreeMap<usize, usize>,
    // Track the requested quality, since source dimensions and the display
    // pixel limit can produce a texture narrower than that request.
    decoded_widths: BTreeMap<usize, u32>,
    decoded_windows: BTreeMap<usize, reader_engine::DisplayWindowPlan>,
    // Weak identity never extends the encoded-response memory cache budget.
    decoded_responses: BTreeMap<usize, WeakPage>,
    native_dimensions: BTreeMap<usize, reader_engine::PageDimensions>,
    progressive_job: Option<(usize, u64)>,
    presentations: BTreeMap<usize, reader_engine::PagePresentation>,
    display_options: reader_engine::DisplayOptions,
    gap_pixels: f32,
    chapter_boundaries: bool,
    preload_epoch: u64,
    segments: Vec<reader_continuity::ChapterSegment>,
    preloading: Option<ChapterId>,
    preload_nonce: u64,
    continuity_backwards: bool,
    continuity_failures: BTreeMap<ChapterId, String>,
    continuity_error: Option<(ChapterId, String)>,
    bookmarked_chapters: BTreeSet<ChapterId>,
    target_width: u32,
    viewport_width: f32,
    viewport_height: f32,
    anchor: ResumeAnchor,
    mode: ReadingMode,
    mode_choice: ModeChoice,
    automatic_mode: ReadingMode,
    mode_switching: bool,
    collection: bool,
    completed: bool,
    /// Only a real reader selection may infer completion or replace progress.
    /// Neutral Pages settlement keeps this false across checkpoints and exit.
    completion_eligible: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PositionOrigin {
    User,
    Restoration,
    ZoomSettlement,
    OverviewSettlement,
}

impl Reader {
    fn gap_after(&self, index: usize) -> f32 {
        if self.mode != ReadingMode::Webtoon {
            return 0.;
        }
        if index + 1 < self.pages.len()
            && self.owner(index) != self.owner(index + 1)
            && self.chapter_boundaries
        {
            96.
        } else {
            self.gap_pixels
        }
    }
    fn page_position(&self, index: usize) -> f32 {
        self.ratios[..index].iter().sum::<f32>()
            + (0..index)
                .map(|i| self.gap_after(i) / self.viewport_width.max(1.))
                .sum::<f32>()
    }
    /// `page_position(i)` for every page in one pass. The ratio and gap sums
    /// accumulate left to right exactly like `page_position`'s two iterator
    /// sums (which start at -0.0), so every value is bit-identical, but the
    /// whole list costs O(n) instead of O(n^2).
    fn page_positions(&self) -> Vec<f32> {
        let positions = self.page_positions_unchecked();
        debug_assert!(
            positions
                .iter()
                .enumerate()
                .all(|(i, p)| p.to_bits() == self.page_position(i).to_bits()),
            "batched positions must equal page_position"
        );
        positions
    }
    fn page_positions_unchecked(&self) -> Vec<f32> {
        let width = self.viewport_width.max(1.);
        let (mut ratios, mut gaps) = (-0_f32, -0_f32);
        (0..self.ratios.len())
            .map(|i| {
                if i > 0 {
                    ratios += self.ratios[i - 1];
                    gaps += self.gap_after(i - 1) / width;
                }
                ratios + gaps
            })
            .collect()
    }
    fn total_extent(&self) -> f32 {
        self.page_position(self.ratios.len().saturating_sub(1))
            + self.ratios.last().copied().unwrap_or(0.)
    }
    fn progress_record(&self) -> ReadingProgress {
        let index = self.anchor.page_index;
        ReadingProgress {
            chapter: self.owner(index).clone(),
            // A completed chapter keeps its read flag but no resume page, so the
            // next open starts at image one instead of the last page finished.
            // The exception is a place the viewer is actively reading in a
            // chapter already read (e.g. scrolled back up into it).
            anchor: if self.completed && !self.keeps_place_in_read_chapter() {
                ResumeAnchor::cleared()
            } else {
                ResumeAnchor::new(
                    self.page_id(index),
                    self.local_index(index),
                    self.anchor.offset(),
                )
                .unwrap_or_default()
            },
            mode: self.mode,
            last_read_at: now(),
            completed: self.completed,
        }
    }
    /// True when a chapter already marked read is being read partway: live
    /// reading (not a neutral overview), past the first image's top and short
    /// of its end. The end of a read chapter restarts, like finishing it.
    fn keeps_place_in_read_chapter(&self) -> bool {
        let index = self.anchor.page_index;
        let last = self
            .chapter_segment(index)
            .map_or(self.pages.len(), |segment| segment.end)
            .saturating_sub(1);
        self.completion_eligible
            && (self.local_index(index) > 0 || self.anchor.offset() > 0.)
            && index < last
            && !self.chapter_finished()
    }
    fn activate_page(&mut self, index: usize) -> Option<ReadingProgress> {
        let owner = self.owner(index).clone();
        if self.chapter == owner {
            return None;
        }
        let mut old = self.progress_record();
        if index > self.anchor.page_index {
            // Finishing a chapter keeps its read flag but drops the resume page.
            old.completed = true;
            old.anchor = ResumeAnchor::cleared();
        }
        self.source = self.source_for(index);
        self.chapter = owner;
        self.completed = self.continuity_completion(index, old.completed);
        Some(old)
    }
    fn is_split(&self, index: usize) -> bool {
        self.presentations.get(&index).is_some_and(|p| p.split)
    }
    /// Mihon marks a chapter read when its final source image is selected;
    /// selecting the first half of a split image is enough. Loading is allowed,
    /// but an explicitly failed image cannot newly finish a chapter.
    fn selected_image_finishes_chapter(&self, index: usize) -> bool {
        self.completion_eligible
            && !self.collection
            && index < self.pages.len()
            && self
                .chapter_segment(index)
                .map_or(self.pages.len(), |s| s.end)
                == index + 1
            && !self.failed.contains(&index)
    }
    fn paged_chapter_finished(&self) -> bool {
        self.mode != ReadingMode::Webtoon
            && self.selected_image_finishes_chapter(self.anchor.page_index)
    }
    /// Webtoon tracks the last image whose bottom is visible or whose top has
    /// passed the viewport top (Mihon's WebtoonLayoutManager rule). The anchor
    /// stays at our top visible source point, so a short final image can be
    /// selected below it. Adjacent retained chapters never extend this chapter.
    fn chapter_finished(&self) -> bool {
        if self.collection
            || self.pages.is_empty()
            || self.chapter_navigation.is_some()
            || self.mode_switching
        {
            return false;
        }
        if self.mode != ReadingMode::Webtoon {
            return self.paged_chapter_finished();
        }
        if self.viewport_width <= 0. || self.viewport_height <= 0. {
            return false;
        }
        let index = self.anchor.page_index;
        let last = self
            .chapter_segment(index)
            .map_or(self.pages.len(), |segment| segment.end)
            - 1;
        let viewport_start = (self.page_position(index)
            + self.ratios[index] * self.anchor.offset() as f32)
            * self.viewport_width;
        let viewport_end = viewport_start + self.viewport_height;
        let image_start = self.page_position(last) * self.viewport_width;
        let image_end = image_start + self.ratios[last] * self.viewport_width;
        self.selected_image_finishes_chapter(last)
            && image_start < viewport_end
            && image_end > viewport_start
            && (image_end <= viewport_end || image_start < viewport_start)
    }
    fn decode_width(width: f32, scale_factor: f32) -> u32 {
        // Small layout changes share a decode, while high-density screens get
        // physical-pixel detail within a bounded display allocation.
        let physical = (width * scale_factor).clamp(128., 16384.);
        ((physical / 128.).ceil() as u32 * 128).clamp(128, 16384)
    }

    /// Do not let provisional page heights become a reading anchor. A small
    /// loading edge may enter the viewport, but unread artwork stays ahead of
    /// the reader until its real geometry/pixels have arrived. Explicit seeks
    /// and restored anchors remain valid starting points.
    fn loaded_scroll_offset(
        &self,
        requested: f32,
        width: f32,
        viewport: f32,
        ready: impl Fn(usize) -> bool,
    ) -> f32 {
        let current = (self.page_position(self.anchor.page_index)
            + self.ratios[self.anchor.page_index] * self.anchor.offset() as f32)
            * width;
        let requested = requested.max(0.);
        let loading_edge = 48_f32.min(viewport.max(0.));
        if requested > current {
            for index in self.anchor.page_index..self.pages.len() {
                if !ready(index) {
                    let limit =
                        (self.page_position(index) * width - viewport + loading_edge).max(current);
                    return requested.min(limit);
                }
                if self.page_position(index) * width > requested + viewport {
                    break;
                }
            }
        } else if requested < current {
            for index in (0..=self.anchor.page_index).rev() {
                if !ready(index) {
                    let limit = (self.page_position(index + 1) * width).min(current);
                    return requested.max(limit);
                }
                if (self.page_position(index) + self.ratios[index]) * width < requested {
                    break;
                }
            }
        }
        requested
    }

    fn visible_end(&self) -> usize {
        let index = self.anchor.page_index.min(self.pages.len() - 1);
        let mut end = index;
        if self.mode == ReadingMode::Webtoon && self.viewport_width > 0. {
            let mut covered =
                self.ratios[index] * self.viewport_width * (1. - self.anchor.offset() as f32);
            while end + 1 < self.pages.len() && covered < self.viewport_height {
                end += 1;
                covered +=
                    self.ratios[end] * self.viewport_width + self.gap_after(end.saturating_sub(1));
            }
        }
        end
    }
    fn visible_span(&self, index: usize) -> (f64, f64) {
        self.span_with_ratio(index, self.ratios[index])
    }
    fn span_with_ratio(&self, index: usize, actual_ratio: f32) -> (f64, f64) {
        if self.mode != ReadingMode::Webtoon {
            return if index == self.anchor.page_index && self.is_split(index) {
                let half = if self.anchor.offset() >= 0.5 { 0.5 } else { 0. };
                (half, half + 0.5)
            } else {
                // Adjacent pages can be visible during a gallery drag. A
                // loaded preview must cover its entire page, not a top strip.
                (0., 1.)
            };
        }
        let page_start = f64::from(self.page_position(index));
        let anchor = self.anchor.page_index.min(self.pages.len() - 1);
        let correction = if index < anchor {
            f64::from(actual_ratio - self.ratios[index])
        } else {
            0.
        };
        let viewport_start = f64::from(self.page_position(anchor))
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
        let anchor = self.anchor.page_index.min(self.pages.len() - 1);
        let mut end = anchor;
        if self.mode == ReadingMode::Webtoon {
            let ratio_at = |i| if i == index { ratio } else { self.ratios[i] };
            let mut covered =
                ratio_at(anchor) * self.viewport_width * (1. - self.anchor.offset() as f32);
            while end + 1 < self.pages.len() && covered < self.viewport_height {
                end += 1;
                covered +=
                    ratio_at(end) * self.viewport_width + self.gap_after(end.saturating_sub(1));
            }
        }
        let handoff = self.handoff_budgets(Some((index, dimensions)));
        let budget = if let Some(budget) = handoff.as_ref().and_then(|b| b.get(&index)) {
            *budget
        } else if self.reverse_target() == Some(index) {
            // Visible geometry is not yet trustworthy. Its completion explicitly
            // restarts scheduling; do not decode an ordinary full-size neighbor.
            return None;
        } else {
            (READER_MODEL_BYTES / (end - anchor + 1)).min(reader_engine::DISPLAY_WINDOW_BYTES)
        };
        let width = if self.mode == ReadingMode::Webtoon {
            self.target_width
        } else {
            self.paged_detail_width_with_budget(index, dimensions, budget)
        };
        let span = self.navigation_span(index, dimensions).unwrap_or_else(|| {
            self.demand_window_span(index, dimensions, self.span_with_ratio(index, ratio))
        });
        Some((width, span, budget))
    }
    fn reverse_target(&self) -> Option<usize> {
        // Explicit chapter entry works in every mode and in both directions.
        // It shares the same old-visible-first bounded handoff as scroll demand.
        if let Some(index) = self.navigation_pixel_target() {
            return Some(index);
        }
        if self.mode != ReadingMode::Webtoon {
            return None;
        }
        if let Some(index) = self.window_demand_target() {
            return Some(index);
        }
        let identity = self.reverse_demand.as_ref()?;
        (0..self.anchor.page_index).find(|index| self.row_page_id(*index) == *identity)
    }

    /// Reserve a bounded previous tail only after accounting for every visible
    /// row at the existing physical detail. The caller may supply newly probed
    /// geometry before it has been published to the UI model.
    fn handoff_budgets(
        &self,
        probed: Option<(usize, reader_engine::PageDimensions)>,
    ) -> Option<BTreeMap<usize, usize>> {
        let target = self.reverse_target()?;
        let mut floors = BTreeMap::new();
        let mut required = 0usize;
        for index in self.anchor.page_index..=self.visible_end() {
            if self.failed.contains(&index) && !self.retained.contains(&index) {
                floors.insert(index, 0);
                continue;
            }
            let dimensions = probed
                .filter(|(i, _)| *i == index)
                .map(|(_, d)| d)
                .or_else(|| {
                    self.presentations
                        .get(&index)
                        .map(|presentation| presentation.effective)
                })
                .or_else(|| {
                    self.failed
                        .contains(&index)
                        .then(|| self.native_dimensions.get(&index).copied())
                        .flatten()
                })?;
            let width = if self.mode == ReadingMode::Webtoon {
                self.target_width.min(dimensions.width).max(1)
            } else {
                self.decoded_windows
                    .get(&index)
                    .map(|plan| plan.width)
                    .unwrap_or_else(|| self.paged_detail_width(index, dimensions))
                    .min(dimensions.width)
                    .max(1)
            };
            let height = (u64::from(dimensions.height) * u64::from(width)
                / u64::from(dimensions.width))
            .max(1) as u32;
            let (mut start, mut end) =
                self.span_with_ratio(index, dimensions.height as f32 / dimensions.width as f32);
            if let Some(span) = self.navigation_span(index, dimensions) {
                start = start.min(span.0);
                end = end.max(span.1);
            }
            // Match display::visible_rows exactly, including its one-row floor.
            let first = ((start * f64::from(height)).floor() as u32).min(height - 1);
            let last = ((end * f64::from(height)).ceil() as u32)
                .max(first + 1)
                .min(height);
            let mut bytes = ((last - first) as usize).checked_mul(width as usize * 4)?;
            if self.failed.contains(&index) {
                // A terminal replacement failure pins its surviving pixels.
                // Reclaiming that overscan is no longer runnable work; reserve
                // the previous tail from the bytes that can actually be freed.
                bytes = bytes.max(self.decoded_bytes.get(&index).copied().unwrap_or(0));
            }
            required = required.saturating_add(bytes);
            floors.insert(index, bytes);
        }
        let reserve = READER_MODEL_BYTES
            .saturating_sub(required)
            .min(READER_MODEL_BYTES / 2);
        let extra = READER_MODEL_BYTES
            .saturating_sub(required)
            .saturating_sub(reserve)
            / floors.len();
        for bytes in floors.values_mut() {
            *bytes += extra;
        }
        let mut used_destination = 0usize;
        for index in self.navigation_pixel_targets() {
            if index != target && !floors.contains_key(&index) {
                let bytes = self
                    .decoded_bytes
                    .get(&index)
                    .copied()
                    .unwrap_or(0)
                    .min(reserve.saturating_sub(used_destination));
                used_destination = used_destination.saturating_add(bytes);
                floors.insert(index, bytes);
            }
        }
        let target_floor = floors.get(&target).copied().unwrap_or(0);
        floors.insert(
            target,
            target_floor.saturating_add(reserve.saturating_sub(used_destination)),
        );
        Some(floors)
    }

    fn budget_for(&self, index: usize) -> usize {
        self.handoff_budgets(None)
            .and_then(|b| b.get(&index).copied())
            .unwrap_or_else(|| self.window_budget())
    }

    fn handoff_waits_for_visible(&self, target: usize) -> bool {
        if self.reverse_target() != Some(target) {
            return false;
        }
        let Some(budgets) = self.handoff_budgets(None) else {
            if (self.anchor.page_index..=self.visible_end()).contains(&target) {
                // This target is itself an unresolved visible page. Permit its
                // worker probe; the post-probe phase guard will then schedule
                // any other visible shrink before target decoding/publication.
                return (self.anchor.page_index..=self.visible_end())
                    .filter(|i| *i != target)
                    .any(|i| {
                        !self.failed.contains(&i)
                            && (!self.has_visible_pixels(i) || !self.presentations.contains_key(&i))
                    });
            }
            return true;
        };
        // Even a zero-byte tail is runnable: probing its actual output width
        // lets ordinary display admission produce an honest terminal limit
        // error. Deferring it forever would recreate the boundary deadlock.
        (self.anchor.page_index..=self.visible_end()).any(|index| {
            if index == target {
                return false;
            }
            (!self.failed.contains(&index) && !self.has_visible_pixels(index))
                || self.decoded_bytes.get(&index).copied().unwrap_or(0) > budgets[&index]
        })
    }

    fn observe_reverse_scroll(
        &mut self,
        attempted: f32,
        width: f32,
        ready: impl Fn(usize) -> bool,
    ) {
        let current = (self.page_position(self.anchor.page_index)
            + self.ratios[self.anchor.page_index] * self.anchor.offset() as f32)
            * width;
        if attempted > current + 0.5 {
            self.reverse_demand = None;
        } else if attempted < self.page_position(self.anchor.page_index) * width
            && let Some(target) = (0..self.anchor.page_index)
                .rev()
                .find(|i| !ready(*i) && (self.page_position(*i + 1) * width) > attempted)
        {
            let identity = self.row_page_id(target);
            if self.reverse_demand.as_ref() != Some(&identity) {
                self.reverse_demand = Some(identity);
                // A changed allocation is a new admission opportunity.
                self.budget_blocked.remove(&target);
            }
        }
    }

    /// Revalidate queued/decoded work against the live viewport and allocation.
    /// A demand may appear while a speculative decode is already running.
    fn accepts_window(
        &self,
        index: usize,
        dimensions: reader_engine::PageDimensions,
        plan: reader_engine::DisplayWindowPlan,
    ) -> bool {
        let Some((_, (start, end), budget)) = self.window_request(index, dimensions) else {
            return false;
        };
        !self.defers_prefetch(index) && plan.covers(start, end) && plan.byte_len() <= budget
    }

    fn has_visible_pixels(&self, index: usize) -> bool {
        self.retained.contains(&index)
            && self.decoded_windows.get(&index).is_some_and(|plan| {
                let (start, end) = self.visible_span(index);
                plan.covers(start, end)
            })
    }
    /// Preserve the same source-column detail as Webtoon before the renderer
    /// fits a paged image. A second CPU fit-downsample loses thin strokes and
    /// makes the renderer interpolate an already reduced image again.
    fn paged_detail_width(&self, index: usize, dimensions: reader_engine::PageDimensions) -> u32 {
        self.paged_detail_width_with_budget(index, dimensions, self.window_budget())
    }
    fn paged_detail_width_with_budget(
        &self,
        index: usize,
        dimensions: reader_engine::PageDimensions,
        budget: usize,
    ) -> u32 {
        let (start, end) = self.navigation_span(index, dimensions).unwrap_or_else(|| {
            self.span_with_ratio(index, dimensions.height as f32 / dimensions.width as f32)
        });
        let source = self
            .presentations
            .get(&index)
            .map(|presentation| presentation.source)
            .unwrap_or(dimensions);
        let source_bytes = u64::from(source.width) * u64::from(source.height) * 4;
        let copy_budget = (reader_engine::current_limits()
            .max_decoded_bytes
            .saturating_sub(source_bytes)
            / 2) as usize;
        let budget = budget.min(copy_budget);
        let mut low = 1;
        let mut high = self.target_width.min(dimensions.width).max(1);
        while low < high {
            let candidate = low + (high - low).div_ceil(2);
            if reader_engine::plan_display_window(dimensions, candidate, start, end, budget).is_ok()
            {
                low = candidate;
            } else {
                high = candidate - 1;
            }
        }
        low
    }
    fn quality_width(&self, index: usize) -> u32 {
        if self.mode == ReadingMode::Webtoon {
            return self.target_width;
        }
        self.native_dimensions
            .get(&index)
            .map(|dimensions| {
                self.paged_detail_width_with_budget(index, *dimensions, self.budget_for(index))
            })
            .unwrap_or(self.target_width)
    }
    fn window_budget(&self) -> usize {
        let visible = self.visible_end() + 1 - self.anchor.page_index.min(self.pages.len() - 1);
        (READER_MODEL_BYTES / visible).min(reader_engine::DISPLAY_WINDOW_BYTES)
    }
    fn needs_window(&self, index: usize) -> bool {
        self.decoded_windows.get(&index).is_some_and(|plan| {
            let (start, end) = self
                .native_dimensions
                .get(&index)
                .and_then(|dimensions| self.window_request(index, *dimensions).map(|r| r.1))
                .unwrap_or_else(|| self.visible_span(index));
            !plan.covers_with_margin(start, end) || plan.byte_len() > self.budget_for(index)
        })
    }

    // The worker retains one decoded original. Decoding an offscreen page
    // before a long visible page's final window would evict that original and
    // make its next scroll window decompress the entire source again.
    fn defers_prefetch(&self, target: usize) -> bool {
        if self.pages.is_empty() {
            return false;
        }
        if self.reverse_target() == Some(target) {
            return self.handoff_waits_for_visible(target);
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

    fn retry_index(&self, identity: &str, due: std::time::Instant) -> Option<usize> {
        (self.image_retry_due.get(identity) == Some(&due))
            .then(|| (0..self.pages.len()).find(|i| self.row_page_id(*i) == identity))
            .flatten()
    }

    fn prune_retry_deadlines(&mut self) {
        let live = self
            .image_retry_due
            .keys()
            .filter(|id| (0..self.pages.len()).any(|i| self.row_page_id(i) == **id))
            .cloned()
            .collect::<BTreeSet<_>>();
        self.image_retry_due.retain(|id, _| live.contains(id));
    }

    fn page_id(&self, index: usize) -> String {
        match &self.pages[index] {
            #[cfg(test)]
            PageInput::Local { id, .. } => id.clone(),
            PageInput::Verified(resource) => resource.id.clone(),
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
    if reader.defers_prefetch(task.index) {
        return Err(text::tr("Operation cancelled"));
    }
    reader
        .window_request(task.index, dimensions)
        .ok_or_else(|| text::tr("Operation cancelled"))
}

fn record_task_presentation(
    reader: &mut Reader,
    generation: u64,
    task: &ImageTask,
    identity: (&ChapterId, &str),
    options: reader_engine::DisplayOptions,
    presentation: reader_engine::PagePresentation,
) -> Result<()> {
    if generation != task.generation
        || reader.pages.get(task.index).is_none()
        || reader.owner(task.index) != identity.0
        || reader.page_id(task.index) != identity.1
        || reader.display_options != options
    {
        return Err(text::tr("Operation cancelled"));
    }
    reader.presentations.insert(task.index, presentation);
    Ok(())
}

/// Encoded chapter bytes are lossless, generation-scoped and memory-only.
/// Re-windowing a long page must not repeat a potentially slow source request.
#[derive(Default)]
struct EncodedPages {
    generation: u64,
    pages: BTreeMap<usize, PageImage>,
    order: std::collections::VecDeque<usize>,
    bytes: usize,
    contexts: BTreeMap<usize, String>,
    /// What idle fetch-ahead learned about a page it fetched before any task
    /// asked: the deferred disk-cache write and whether the bytes came from
    /// the disk cache. The page's own task takes it, as if it had fetched.
    ahead: BTreeMap<usize, FetchedAhead>,
    /// Pages whose idle fetch failed: only a task fetches them (and reports).
    skipped: BTreeSet<usize>,
}
struct FetchedAhead {
    pending: Option<crate::chapter_cache::PendingWrite>,
    reused: bool,
}
impl EncodedPages {
    fn holds(&self, generation: u64, index: usize) -> bool {
        self.generation == generation && self.pages.contains_key(&index)
    }
    fn skipped(&self, generation: u64, index: usize) -> bool {
        self.generation == generation && self.skipped.contains(&index)
    }
    fn skip(&mut self, generation: u64, index: usize) {
        if self.generation == generation {
            self.skipped.insert(index);
        }
    }
    /// Bytes held for `indices`, for bounding what fetch-ahead keeps.
    fn bytes_of(&self, indices: impl Iterator<Item = usize>) -> usize {
        indices
            .filter_map(|index| self.pages.get(&index))
            .map(PageImage::encoded_len)
            .sum()
    }
    fn insert_ahead(
        &mut self,
        generation: u64,
        index: usize,
        context: &str,
        page: PageImage,
        fetched: Option<FetchedAhead>,
    ) {
        let _ = self.read_scoped(generation, index, context, || Ok(page));
        match fetched {
            Some(fetched) if self.pages.contains_key(&index) => {
                self.ahead.insert(index, fetched);
            }
            _ => {
                self.ahead.remove(&index);
            }
        }
    }
    fn take_ahead(&mut self, generation: u64, index: usize) -> Option<FetchedAhead> {
        (self.generation == generation)
            .then(|| self.ahead.remove(&index))
            .flatten()
    }
    fn read(
        &mut self,
        generation: u64,
        index: usize,
        fetch: impl FnOnce() -> Result<PageImage>,
    ) -> Result<PageImage> {
        self.read_scoped(generation, index, "", fetch)
    }
    fn discard(&mut self, generation: u64, index: usize) {
        if self.generation != generation {
            return;
        }
        self.ahead.remove(&index);
        if let Some(page) = self.pages.remove(&index) {
            self.bytes = self.bytes.saturating_sub(page.encoded_len());
        }
        self.order.retain(|i| *i != index);
        self.contexts.remove(&index);
    }
    fn read_scoped(
        &mut self,
        generation: u64,
        index: usize,
        context: &str,
        fetch: impl FnOnce() -> Result<PageImage>,
    ) -> Result<PageImage> {
        if self.generation != generation {
            self.pages.clear();
            self.order.clear();
            self.bytes = 0;
            self.generation = generation;
            self.contexts.clear();
            self.ahead.clear();
            self.skipped.clear();
        }
        if self.contexts.get(&index).is_some_and(|old| old != context) {
            self.ahead.remove(&index);
            if let Some(old) = self.pages.remove(&index) {
                self.bytes -= old.encoded_len();
            }
            self.order.retain(|i| *i != index);
            self.contexts.remove(&index);
        }
        if let Some(page) = self.pages.get(&index) {
            self.order.retain(|i| *i != index);
            self.order.push_back(index);
            return Ok(page.clone());
        }
        let page = fetch()?;
        let len = page.encoded_len();
        while self.bytes + len > 32 * 1024 * 1024 {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(old) = self.pages.remove(&old) {
                self.bytes -= old.encoded_len();
            }
            self.contexts.remove(&old);
        }
        if len <= 32 * 1024 * 1024 {
            self.bytes += len;
            self.pages.insert(index, page.clone());
            self.order.push_back(index);
            self.contexts.insert(index, context.into());
        }
        Ok(page)
    }
}
/// Library titles a manual refresh can ask a source about. Imported folders and
/// CBZs ("local") have no remote provider, and disabled addons cannot answer;
/// counting either as a failed refresh would report errors the user cannot fix.
fn refreshable_series(
    state: &reader_domain::ReaderState,
    disabled_packages: &[String],
) -> Vec<Series> {
    state
        .library
        .iter()
        .filter_map(|entry| state.series.iter().find(|s| s.id == entry.series))
        .filter(|series| {
            let source = series.id.source_id.as_str();
            source != "local"
                && !disabled_packages.iter().any(|package| {
                    source == package
                        || source
                            .strip_prefix(package.as_str())
                            .is_some_and(|rest| rest.starts_with('.'))
                })
        })
        .cloned()
        .collect()
}
/// (total, unread) chapter counts per series from one pass over the catalogue.
/// Library redraws run on every search keystroke, so scanning progress once per
/// chapter (or per sort comparison) would grow with chapters x progress records.
fn series_chapter_counts(
    chapters: &[Chapter],
    progress: &[ReadingProgress],
) -> HashMap<reader_domain::SeriesId, (usize, usize)> {
    let completed: HashSet<&ChapterId> = progress
        .iter()
        .filter(|p| p.completed)
        .map(|p| &p.chapter)
        .collect();
    let mut counts: HashMap<reader_domain::SeriesId, (usize, usize)> = HashMap::new();
    for chapter in chapters {
        let entry = counts.entry(chapter.id.series.clone()).or_default();
        entry.0 += 1;
        if !completed.contains(&chapter.id) {
            entry.1 += 1;
        }
    }
    counts
}
/// Browse choices one package contributes. A package whose capabilities claim
/// another package's source identity yields an error so the caller can skip just
/// that package instead of failing the whole source list.
fn package_choices(
    package_id: &str,
    package_name: &str,
    package_language: &str,
    enabled: bool,
    variants: &[serde_json::Value],
) -> std::result::Result<Vec<Choice>, String> {
    if variants.is_empty() {
        return Ok(vec![Choice {
            id: package_id.into(),
            package: package_id.into(),
            name: format!(
                "{package_name}{}",
                if enabled {
                    String::new()
                } else {
                    text::disabled_suffix()
                }
            ),
            enabled,
            filters: false,
            language: package_language.into(),
            latest: false,
            adult: false,
            extension: package_name.into(),
        }]);
    }
    let mut choices = Vec::new();
    for variant in variants {
        let id = variant
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or(package_id);
        // A package cannot claim another package's source identity.
        if id != package_id && !id.starts_with(&format!("{package_id}.")) {
            return Err(text::tr("Source advertised an invalid identity"));
        }
        let language = variant
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or(package_language);
        let operations = variant.get("operations").and_then(|v| v.as_array());
        choices.push(Choice {
            id: id.into(),
            package: package_id.into(),
            name: format!("{package_name} · {language}"),
            enabled,
            language: language.to_owned(),
            latest: operations.is_some_and(|v| v.iter().any(|op| op.as_str() == Some("browse"))),
            filters: operations.is_some_and(|v| v.iter().any(|op| op.as_str() == Some("filters"))),
            adult: false,
            extension: package_name.into(),
        });
    }
    Ok(choices)
}
/// Filters sent with a catalogue request: the snapshot taken on Apply, never
/// the live draft the filter panel is still editing.
fn applied_browse_filters(view: &View) -> serde_json::Value {
    if view.filters_active && !view.applied_filters.is_null() {
        view.applied_filters.clone()
    } else {
        json!({})
    }
}
/// Remembers what the user applied; later draft edits stay local to the panel.
fn apply_source_filters(view: &mut View) {
    view.applied_filters = view.source_filters.values();
}
/// Removes a hidden catalogue from the picker and keeps `selected` on the same
/// source (indices shift after `retain`). Returns false when the hidden source
/// was the one being browsed, so the caller can leave the catalogue.
fn hide_choice(view: &mut View, id: &str) -> bool {
    let browsed = view.choices.get(view.selected).map(|c| c.id.clone());
    view.choices.retain(|choice| choice.id != id);
    match browsed {
        Some(current) if current == id => {
            view.saved_catalogue = None;
            view.rows.clear();
            view.selected = 0;
            false
        }
        Some(current) => {
            view.selected = view
                .choices
                .iter()
                .position(|c| c.id == current)
                .unwrap_or(0);
            true
        }
        None => true,
    }
}
/// Settings page rows. Secret values never leave the registry, so a secret row
/// only reports through `checked` whether one is stored.
fn setting_rows(
    schema: Vec<reader_sources::SettingField>,
    values: &serde_json::Value,
    secrets: &BTreeMap<String, bool>,
) -> Vec<SettingRow> {
    schema
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
            let secret = f.kind == reader_sources::SettingKind::Secret;
            SettingRow {
                checked: if secret {
                    secrets.get(&f.key).copied().unwrap_or(false)
                } else {
                    value.as_bool().unwrap_or(false)
                },
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
                options: ModelRc::new(VecModel::from(
                    f.options.into_iter().map(Into::into).collect::<Vec<_>>(),
                )),
                selected,
            }
        })
        .collect()
}
/// Saves one addon setting. Submitting an empty secret field is ignored (and
/// reported as not saved): Enter on an untouched field would otherwise erase the
/// stored token without the user ever seeing it.
fn save_setting(
    sources: &reader_sources::SourceRegistry,
    package: &str,
    key: &str,
    value: &str,
) -> Result<bool> {
    let kind = sources
        .settings_schema(package)
        .ok()
        .and_then(|fields| fields.into_iter().find(|f| f.key == key).map(|f| f.kind));
    if kind == Some(reader_sources::SettingKind::Secret) && value.is_empty() {
        return Ok(false);
    }
    let value = if kind == Some(reader_sources::SettingKind::Bool) {
        serde_json::Value::Bool(value == "true")
    } else {
        serde_json::Value::String(value.to_string())
    };
    sources
        .set_settings(package, json!({key: value}))
        .map_err(error)?;
    Ok(true)
}
/// Rebuilds the addon settings model from stored values, discarding any
/// control state the user changed but that was not (or could not be) saved.
fn republish_settings(app: &Controller, ui: &AppWindow, package: &str) {
    let schema = app.services.sources.settings_schema(package);
    let values = app.services.sources.settings(package);
    let secrets = app.services.sources.settings_secrets(package);
    if let (Ok(schema), Ok(values), Ok(secrets)) = (schema, values, secrets) {
        ui.set_settings(ModelRc::new(VecModel::from(setting_rows(
            schema, &values, &secrets,
        ))));
    }
}
/// Detail's Unread/Downloaded filters hide rows in Slint, which cannot count
/// them; these totals let it show an empty state instead of a blank list.
pub(crate) fn publish_chapter_filter_counts(ui: &AppWindow) {
    let model = ui.get_items();
    let rows = model.iter().collect::<Vec<_>>();
    ui.set_detail_unread_count(rows.iter().filter(|row| !row.read).count() as i32);
    ui.set_detail_downloaded_count(rows.iter().filter(|row| row.downloaded).count() as i32);
    ui.set_detail_bookmarked_count(rows.iter().filter(|row| row.bookmarked).count() as i32);
    ui.set_detail_new_count(rows.iter().filter(|row| row.is_new).count() as i32);
    // Each row's place among the rows a filter shows, so wide screens can
    // split the shown chapters into two even columns.
    let ranks = |shown: fn(&ItemRow) -> bool| {
        let mut next = 0;
        let ranks = rows
            .iter()
            .map(|row| {
                let rank = next;
                if shown(row) {
                    next += 1;
                }
                rank
            })
            .collect::<Vec<i32>>();
        ModelRc::new(VecModel::from(ranks))
    };
    ui.set_detail_rank_unread(ranks(|row| !row.read));
    ui.set_detail_rank_downloaded(ranks(|row| row.downloaded));
    ui.set_detail_rank_bookmarked(ranks(|row| row.bookmarked));
}
fn has_saved_reading_position(progress: &ReadingProgress) -> bool {
    !progress.completed && !progress.anchor.is_cleared() && !progress.anchor.page_id.is_empty()
}
fn resume_chapter<'a>(
    chapters: &'a [Chapter],
    progress: &[ReadingProgress],
) -> Option<&'a Chapter> {
    progress
        .iter()
        .filter(|p| has_saved_reading_position(p))
        .filter_map(|p| {
            chapters
                .iter()
                .find(|c| c.id == p.chapter)
                .map(|c| (p.last_read_at, c))
        })
        .max_by_key(|(read_at, _)| *read_at)
        .map(|(_, c)| c)
        .or_else(|| {
            // Pick by natural reading order, not slice position: newest-first
            // sources list the latest chapter first.
            chapters
                .iter()
                .filter(|c| !progress.iter().any(|p| p.chapter == c.id && p.completed))
                .min_by(|a, b| reader_open::reading_order(a, b))
        })
        .or_else(|| {
            chapters
                .iter()
                .min_by(|a, b| reader_open::reading_order(a, b))
        })
}
/// A cleared or completed chapter has no reading position, so the detail
/// list shows no page marker even when History records a visit.
fn chapter_progress_label(progress: &ReadingProgress, gallery: bool) -> Option<String> {
    (!progress.completed && !progress.anchor.is_cleared()).then(|| {
        format!(
            "{}: {}",
            text::tr(if gallery { "Image" } else { "Page" }),
            progress.anchor.page_index.saturating_add(1)
        )
    })
}
fn resume_action_label(
    chapters: &[Chapter],
    progress: &[ReadingProgress],
    gallery: bool,
) -> String {
    let target = resume_chapter(chapters, progress);
    // Keep the collection viewer's established terminology and behavior.
    if gallery {
        return if chapters.len() == 1 {
            text::tr("Continue")
        } else {
            target.map_or_else(
                || text::tr("Continue"),
                |chapter| format!("{} {}", text::tr("Continue chapter"), resume_label(chapter)),
            )
        };
    }
    let Some(chapter) = target else {
        return text::tr("Start reading");
    };
    if chapters.iter().all(|chapter| {
        progress
            .iter()
            .any(|p| p.chapter == chapter.id && p.completed)
    }) {
        text::tr("Read again")
    } else if progress.iter().any(|p| {
        (p.completed || has_saved_reading_position(p))
            && chapters.iter().any(|chapter| chapter.id == p.chapter)
    }) {
        format!("{} {}", text::tr("Continue chapter"), resume_label(chapter))
    } else {
        format!("{} {}", text::tr("Start chapter"), resume_label(chapter))
    }
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
    /// The current visit to a title's detail screen, kept through the reader
    /// and page overview so its "new" chapters stay marked until it is left.
    chapter_news: Option<crate::chapter_news::Visit>,
    /// Chapters selected on the detail screen; `Some` while selecting.
    chapter_selection: Option<BTreeSet<ChapterId>>,
    rows: Vec<Row>,
    downloads: downloads::DownloadView,
    download_states: BTreeMap<ChapterId, reader_downloads::JobStatus>,
    download_refreshing: bool,
    download_refresh_again: bool,
    download_revision: u64,
    choices: Vec<Choice>,
    selected: usize,
    series: Option<Series>,
    chapters: Vec<Chapter>,
    reader: Option<Reader>,
    overview: Option<page_overview::Overview>,
    reader_open: Option<ChapterId>,
    /// Detail's "Read again": the next prepare for this chapter ignores its
    /// stored place and starts at image one.
    reread: Option<ChapterId>,
    catalogue: String,
    query: String,
    page: usize,
    has_more: bool,
    catalogue_page_error: String,
    pending: Option<ReadingProgress>,
    library_navigation: library_navigation::State,
    /// The state the last render drew. Switching Library category changes no
    /// data, so it re-filters this instead of re-reading the database.
    rendered: Option<Arc<reader_domain::ReaderState>>,
    settings_package: Option<String>,
    source_filters: crate::source_filters::FilterSet,
    source_browser: crate::source_browser::SourceBrowser,
    source_filters_loaded: bool,
    source_filters_loading: bool,
    /// Filter values captured when the user pressed Apply. Pagination must keep
    /// sending these even while the draft in `source_filters` is being edited.
    applied_filters: serde_json::Value,
    filters_active: bool,
    saved_catalogue: Option<CatalogueState>,
    artwork_rows: Vec<Option<Series>>,
    artwork_seen: BTreeMap<usize, std::time::Instant>,
    artwork_pruned: Option<std::time::Instant>,
    /// Updates and History: grouping state and the published lines.
    timeline: timeline_page::State,
    /// Browse: search across sources.
    global_search: browse_page::GlobalSearch,
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
            .ok_or_else(|| reader_domain::Error::Invalid(text::tr("Unknown reading status")))?;
        let entry = state
            .library
            .iter_mut()
            .find(|entry| &entry.series == series)
            .ok_or_else(|| {
                reader_domain::Error::Invalid(text::tr("Title is no longer in your library."))
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
    capture_originals: Mutex<reader_capture_sources::Originals>,
    thumbnail_service: OnceLock<Arc<crate::page_thumbnails::PageThumbnails>>,
    image_worker_live: AtomicBool,
    view: Mutex<View>,
    generation: AtomicU64,
    cancel: Mutex<CancelToken>,
    images: mpsc::Sender<ImageTask>,
    download_serial: Mutex<()>,
    active_image: Mutex<Option<(usize, CancelToken)>>,
    continuation_work: reader_continuity::ContinuationWork,
    metadata_refresh: Arc<crate::metadata_refresh::Scheduler>,
}
impl Controller {
    fn refresh_synced(self: &Arc<Self>, ui: &AppWindow) {
        self.reconcile_profiles(ui);
        let settings_package = { self.view().settings_package.clone() };
        if ui.get_section() == 8
            && let Some(package) = settings_package
        {
            crate::source_compression::select(ui, &package);
        }
        self.render(ui);
        self.refresh_download_states(ui);
        if self.view().section != 3 || ui.get_source_catalog_open() {
            return;
        }
        let generation = self.generation.load(Ordering::Acquire);
        let adult = ui.get_show_adult();
        let app = self.clone();
        let weak = ui.as_weak();
        crate::profiles::spawn(move || {
            let sources = app.browse_sources(adult);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if app.generation.load(Ordering::Acquire) == generation
                    && app.view().section == 3
                    && !ui.get_source_catalog_open()
                    && ui.get_show_adult() == adult
                {
                    match sources {
                        Ok(sources) => app.publish_sources(&ui, sources),
                        Err(error) => ui.set_status(error.into()),
                    }
                }
            });
        });
    }
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
        if let Some(service) = self.thumbnail_service.get() {
            service.cancel();
        }
        self.artwork.invalidate();
        self.chapter_cache.deactivate();
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let mut current = self.cancel.lock().unwrap_or_else(|e| e.into_inner());
        current.cancel();
        self.continuation_work.cancel();
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
        crate::profiles::spawn(move || {
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
                    // Page entry requests both counters. The shared maintenance
                    // guard may defer the thumbnail query until this one ends.
                    settings.invoke_thumbnail_cache_refresh();
                })
                .is_err()
            {
                failed.maintenance_busy.store(false, Ordering::Release);
            }
        });
    }
    fn flush(&self) {
        self.checkpoint_library();
        if let Err(error) = library_navigation::flush() {
            eprintln!("Nova Reader: could not save Library location: {error}");
        }
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
        // Selection provenance belongs to the reader, not pending progress or
        // the overview lifetime: both can disappear before a later exit flush.
        if view.section == 6
            && let Some(reader) = view.reader.as_mut()
            && !reader.completed
            && reader.chapter_finished()
        {
            reader.completed = true;
            view.pending = Some(reader.progress_record());
        }
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
        crate::profiles::spawn(move || {
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
            // One malformed addon must not hide every other source in Browse.
            match package_choices(
                &package.id,
                &package.name,
                &package.language,
                package.enabled,
                &variants,
            ) {
                Ok(found) => choices.extend(found.into_iter().map(|choice| Choice {
                    adult: package.nsfw,
                    ..choice
                })),
                Err(reason) => eprintln!(
                    "Nova Reader: skipping source package {}: {reason}",
                    package.id
                ),
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
        match self.services.domain.snapshot() {
            Ok(s) => self.render_snapshot(ui, Arc::new(s)),
            Err(e) => ui.set_status(error(e).into()),
        }
    }
    fn render_snapshot(&self, ui: &AppWindow, snapshot: Arc<reader_domain::ReaderState>) {
        ui.set_detail_gallery_single(false);
        let mut view = self.view();
        view.rendered = Some(snapshot.clone());
        if !crate::metadata_refresh::surface_allowed(view.section) {
            self.metadata_refresh.pause();
        }
        ui.set_section(view.section);
        if view.section == 6 {
            ui.set_series_title(
                reader_series_title(&view)
                    .unwrap_or(text::tr("Series").as_str())
                    .into(),
            );
        }
        let builtins = reader_domain::ReadingStatus::BUILTINS;
        view.library_navigation
            .reconcile_categories(&snapshot.categories);
        let category = view.library_navigation.category_index(&snapshot.categories);
        ui.set_category_index(category as i32);
        let category_id = category
            .checked_sub(builtins.len() + 1)
            .and_then(|index| snapshot.categories.get(index))
            .map(|category| category.id.clone());
        let status_filter = category
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
        // Indexes built once: a category switch re-renders everything, so
        // per-title scans of the chapter/progress lists would be quadratic.
        let counts = series_chapter_counts(&snapshot.chapters, &snapshot.progress);
        let buckets = library_view::buckets(&snapshot, &counts);
        let series_by_id: HashMap<&reader_domain::SeriesId, &Series> =
            snapshot.series.iter().map(|s| (&s.id, s)).collect();
        let library_by_id: HashMap<&reader_domain::SeriesId, &reader_domain::LibraryEntry> =
            snapshot.library.iter().map(|l| (&l.series, l)).collect();
        ui.set_category_counts(ModelRc::new(VecModel::from(library_view::category_counts(
            &snapshot, &buckets,
        ))));
        ui.set_library_count_label(
            text::titles_label(
                snapshot
                    .library
                    .iter()
                    .filter(|entry| series_by_id.contains_key(&entry.series))
                    .count(),
            )
            .into(),
        );
        let (heading, caption) = match view.section {
            0 => ("Library", ""),
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
                            && status_filter
                                .is_none_or(|status| buckets.get(&l.series) == Some(&status))
                    })
                    .filter_map(|l| series_by_id.get(&l.series).copied())
                    .filter(|s| query.is_empty() || s.title.to_lowercase().contains(&query))
                    .cloned()
                    .collect();
                library_view::sort(
                    &mut series,
                    ui.global::<Appearance>().get_library_sort(),
                    ui.global::<Appearance>().get_library_sort_reversed(),
                    &snapshot,
                    &counts,
                );
                view.rows = series.into_iter().map(Row::Series).collect();
            }
            1 => {
                let in_library: HashSet<&reader_domain::SeriesId> =
                    snapshot.library.iter().map(|l| &l.series).collect();
                let completed: HashSet<&ChapterId> = snapshot
                    .progress
                    .iter()
                    .filter(|p| p.completed)
                    .map(|p| &p.chapter)
                    .collect();
                // Unread chapters, plus recent read ones so the All and
                // Unread chips differ and read rows show muted.
                let recent = now() - timeline_page::RECENT_READ_SECONDS;
                view.rows = snapshot
                    .chapters
                    .iter()
                    .filter(|c| {
                        in_library.contains(&c.id.series)
                            && (!completed.contains(&c.id)
                                || c.uploaded_at.is_some_and(|at| at >= recent))
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
                    let gallery = crate::content_presentation::Settings::load(&series.id)
                        .unwrap_or_default()
                        .effective()
                        == crate::content_presentation::Presentation::Gallery;
                    ui.set_detail_gallery_single(
                        gallery
                            && snapshot
                                .chapters
                                .iter()
                                .filter(|chapter| chapter.id.series == series.id)
                                .count()
                                == 1,
                    );
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
                    crate::artwork::set_series_poster(
                        ui,
                        self.artwork.cached_image(series).unwrap_or_default(),
                    );
                    ui.set_in_library(snapshot.library.iter().any(|l| l.series == series.id));
                    view.chapters = snapshot
                        .chapters
                        .iter()
                        .filter(|c| c.id.series == series.id)
                        .cloned()
                        .collect();
                    view.chapters.sort_by(reader_open::reading_order);
                    ui.set_resume_chapter(
                        if ui.get_detail_gallery_single() {
                            String::new()
                        } else {
                            resume_chapter(&view.chapters, &snapshot.progress)
                                .map(resume_label)
                                .unwrap_or_default()
                        }
                        .into(),
                    );
                    ui.set_resume_action_label(
                        resume_action_label(&view.chapters, &snapshot.progress, gallery).into(),
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
            let label = reader_domain::profile::kv::get_str("reader:last-update")
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
        if view.section == 5 {
            let series = view.series.as_ref().map(|s| s.id.clone());
            if let Some(series) = series {
                if view
                    .chapter_news
                    .as_ref()
                    .is_none_or(|visit| visit.series() != &series)
                {
                    view.chapter_news = Some(crate::chapter_news::Visit::open(series));
                }
                let shown: Vec<String> = view
                    .rows
                    .iter()
                    .filter_map(|row| match row {
                        Row::Chapter(c) => Some(c.id.local_id.clone()),
                        _ => None,
                    })
                    .collect();
                if let Some(visit) = view.chapter_news.as_mut() {
                    visit.record(shown.iter().map(String::as_str));
                }
            }
        } else if !matches!(view.section, 6 | 23) {
            view.chapter_news = None;
        }
        // Selection belongs to one visit of the chapter list.
        if view.section != 5 {
            view.chapter_selection = None;
        }
        chapter_selection::publish(&view, ui);
        downloads::publish_detail_queue(&view, ui);
        if view.section == 5
            && let Some(series) = view.series.as_ref()
        {
            publish_title_menu(&view, &snapshot, &series.id, ui);
        }
        let clock = crate::timeline::TimelineClock::now();
        view.artwork_rows = view
            .rows
            .iter()
            .map(|row| match row {
                Row::Series(s) => Some(s.clone()),
                Row::Chapter(c) => series_by_id.get(&c.id.series).map(|s| (*s).clone()),
            })
            .collect();
        view.artwork_seen.clear();
        let mut downloaded_by_series: HashMap<&reader_domain::SeriesId, usize> = HashMap::new();
        for (chapter, status) in &view.download_states {
            if *status == reader_downloads::JobStatus::Complete {
                *downloaded_by_series.entry(&chapter.series).or_default() += 1;
            }
        }
        let mut chapter_ids_by_series: HashMap<&reader_domain::SeriesId, Vec<&str>> =
            HashMap::new();
        if view.section == 0 {
            for c in &snapshot.chapters {
                chapter_ids_by_series
                    .entry(&c.id.series)
                    .or_default()
                    .push(c.id.local_id.as_str());
            }
        }
        let progress_by_chapter: HashMap<&ChapterId, &ReadingProgress> =
            snapshot.progress.iter().map(|p| (&p.chapter, p)).collect();
        let read_at_by_chapter: HashMap<&ChapterId, i64> = if view.section == 2 {
            let mut m = HashMap::new();
            for h in &snapshot.history {
                m.entry(&h.chapter).or_insert(h.read_at);
            }
            m
        } else {
            HashMap::new()
        };
        let jobs_by_chapter: HashMap<&ChapterId, &reader_downloads::ChapterDownload> = view
            .downloads
            .jobs
            .iter()
            .map(|j| (&j.chapter, j))
            .collect();
        let series_context = library_view::SeriesContext {
            section: view.section,
            counts: &counts,
            buckets: &buckets,
            library: &library_by_id,
            downloaded: &downloaded_by_series,
            chapter_ids: &chapter_ids_by_series,
        };
        let items: Vec<_> = view
            .rows
            .iter()
            .map(|row| match row {
                Row::Series(s) => library_view::series_fields(&series_context, s)
                    .into_row(self.artwork.cached_image(s).unwrap_or_default()),
                Row::Chapter(c) => {
                    let local_import = c.id.series.source_id == "local";
                    let download = if local_import {
                        None
                    } else {
                        view.download_states.get(&c.id)
                    };
                    let downloaded = download == Some(&reader_downloads::JobStatus::Complete);
                    let series = series_by_id.get(&c.id.series).copied();
                    let key = series.map(crate::artwork::key).unwrap_or_default();
                    let progress = progress_by_chapter.get(&c.id).copied();
                    let gallery = crate::content_presentation::Settings::load(&c.id.series)
                        .unwrap_or_default()
                        .effective()
                        == crate::content_presentation::Presentation::Gallery;
                    let read = progress.is_some_and(|p| p.completed);
                    let stamp = if view.section == 2 {
                        clock.history(read_at_by_chapter.get(&c.id).copied().unwrap_or(0))
                    } else {
                        clock.release(c.uploaded_at)
                    };
                    let job = jobs_by_chapter.get(&c.id).copied();
                    let download_progress = job
                        .filter(|j| !j.pages.is_empty())
                        .map(|j| j.completed_pages() as f32 / j.pages.len() as f32)
                        .unwrap_or(0.);
                    let download_state = download
                        .map(|s| match s {
                            reader_downloads::JobStatus::Complete => 5,
                            reader_downloads::JobStatus::Queued => 1,
                            reader_downloads::JobStatus::Running => 2,
                            reader_downloads::JobStatus::Paused => 3,
                            reader_downloads::JobStatus::Failed => 4,
                            reader_downloads::JobStatus::Cancelled => 6,
                        })
                        .unwrap_or(0);
                    // The page reached in a partly read chapter. Its length
                    // is known only from a download's page list.
                    let page = progress
                        .filter(|p| !p.completed && !p.anchor.is_cleared())
                        .map(|p| p.anchor.page_index);
                    let page_total = job.map(|j| j.pages.len()).filter(|n| *n > 0);
                    let status = if view.section == 5 {
                        crate::chapter_list::status(
                            download_state,
                            download_progress,
                            page,
                            gallery,
                        )
                    } else {
                        None
                    };
                    let mut meta = Vec::new();
                    if c.uploaded_at.is_some() {
                        meta.push(clock.release_date(c.uploaded_at));
                    }
                    if view.section != 5
                        && let Some(progress) = progress
                        && let Some(label) = chapter_progress_label(progress, gallery)
                    {
                        meta.push(label);
                    }
                    if let Some(scanlator) = &c.scanlator {
                        meta.push(scanlator.clone());
                    }
                    if let Some(language) = &c.language {
                        meta.push(language.clone());
                    }
                    if downloaded && view.section != 5 {
                        meta.push(text::tr("Downloaded"));
                    }
                    // Size on disk, from the download manifest already loaded.
                    if downloaded
                        && view.section == 5
                        && let Some(job) = job
                        && job.downloaded_bytes() > 0
                    {
                        meta.push(text::file_size(job.downloaded_bytes() as usize));
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
                            if gallery {
                                progress
                                    .filter(|p| !p.anchor.is_cleared())
                                    .map(|p| {
                                        format!(
                                            "{} · {} {}",
                                            c.title,
                                            text::tr("Image"),
                                            p.anchor.page_index.saturating_add(1)
                                        )
                                    })
                                    .unwrap_or_else(|| c.title.clone())
                            } else {
                                c.title.clone()
                            }
                        } else {
                            String::new()
                        }
                        .into(),
                        badge: text::tr(if read { "Read" } else { "Unread" }).into(),
                        read,
                        downloaded,
                        download_state,
                        download_progress,
                        number: c.number.clone().unwrap_or_default().into(),
                        short_title: crate::chapter_list::short_title(
                            &c.title,
                            c.number.as_deref(),
                        )
                        .into(),
                        status_danger: status.as_ref().is_some_and(|(_, danger)| *danger),
                        status: status.map(|(status, _)| status).unwrap_or_default().into(),
                        reading_status: if view.section == 5 {
                            crate::chapter_list::status(0, 0., page, gallery)
                                .map(|(status, _)| status)
                                .unwrap_or_default()
                        } else {
                            String::new()
                        }
                        .into(),
                        read_fraction: crate::chapter_list::read_fraction(page, page_total),
                        selected: view
                            .chapter_selection
                            .as_ref()
                            .is_some_and(|set| set.contains(&c.id)),
                        is_new: view.section == 5
                            && view
                                .chapter_news
                                .as_ref()
                                .is_some_and(|visit| visit.is_new(&c.id.local_id)),
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
            })
            .collect();
        if matches!(view.section, 1 | 2) {
            timeline_page::publish(&mut view, &snapshot, ui, &items);
        }
        if view.section != 7 {
            ui.set_items(ModelRc::new(VecModel::from(items)));
            publish_chapter_filter_counts(ui);
        }
        if view.section == 0 {
            let position = view.library_navigation.position(&snapshot.categories);
            let index = view.rows.iter().position(|row| {
                matches!(row, Row::Series(series) if !position.series_key.is_empty() && serde_json::to_string(&series.id).ok().as_deref() == Some(position.series_key.as_str()))
            });
            ui.set_library_scroll_y(position.scroll_y);
            ui.set_library_restore_index(
                index
                    .and_then(|index| i32::try_from(index).ok())
                    .unwrap_or(-1),
            );
            ui.set_library_restore_fraction(position.fraction);
            ui.set_library_restore_serial(ui.get_library_restore_serial().wrapping_add(1));
        }
        ui.set_has_more(view.section == 3 && view.has_more);
        ui.set_catalogue_page_error(view.catalogue_page_error.clone().into());
        ui.set_sources(ModelRc::new(VecModel::from(
            view.choices
                .iter()
                .map(|c| c.name.clone().into())
                .collect::<Vec<_>>(),
        )));
        browse_page::publish_sources(&view, ui);
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
                    let secrets = app
                        .services
                        .sources
                        .settings_secrets(&package)
                        .map_err(error)?;
                    let catalogues = app.catalogue_choices(&package)?;
                    Ok((package, schema, values, secrets, catalogues))
                },
                |app, ui, (package, schema, values, secrets, catalogues)| {
                    {
                        let mut view = app.view();
                        view.section = 8;
                        view.settings_package = Some(package.clone());
                        crate::source_compression::select(ui, &package);
                    }
                    let rows = setting_rows(schema, &values, &secrets);
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
        let result = crate::sync_addons::local_change(&self.services, || {
            match action {
                "remove" => self.services.sources.remove(package),
                "toggle" => self.services.sources.set_enabled(package, !current.enabled),
                _ => return Err(text::tr("Unknown source action")),
            }
            .map_err(error)
        });
        if let Err(e) = result {
            ui.set_status(error(e).into());
        } else {
            self.navigate(ui, 3);
        }
    }
    fn select_source(self: &Arc<Self>, ui: &AppWindow, index: i32, kind: &str) {
        self.select_source_with(ui, index, kind, String::new());
    }
    /// Open a source's catalogue; a non-empty `query` opens it searching.
    fn select_source_with(self: &Arc<Self>, ui: &AppWindow, index: i32, kind: &str, query: String) {
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
            view.source_filters_loading = false;
            view.applied_filters = serde_json::Value::Null;
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
        ui.set_source_query(query.clone().into());
        self.browse(ui, kind, Some(query), false);
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
        // The startup picker cannot be left without choosing a profile.
        if section != 24 && self.profile_selection_required(ui) {
            return;
        }
        if section == 24 {
            self.publish_profiles(ui);
        } else {
            ui.global::<crate::ProfilesBridge>().set_error("".into());
        }
        ui.invoke_remember_library_position();
        if section == 23 && self.gallery_return(ui) {
            return;
        }
        self.clear_overview_images(ui);
        self.view().overview = None;
        if section == 22 {
            crate::image_limits_ui::refresh(ui);
        }
        if section == 21
            && let Ok(state) = self.services.domain.snapshot()
        {
            crate::download_cleanup::refresh(ui, &state);
        }
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
            {
                let mut view = self.view();
                view.reader = None;
                view.reader_open = None;
            }
            ui.set_reader_opening(false);
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
    fn return_to_catalogue(self: &Arc<Self>, ui: &AppWindow) {
        ui.invoke_prepare_catalogue_return();
        self.flush();
        self.invalidate();
        let vanished;
        {
            let mut view = self.view();
            view.rows.clear();
            view.has_more = false;
            view.page = 0;
            view.catalogue_page_error.clear();
            let saved = view.saved_catalogue.take();
            // The saved catalogue may have been hidden or its addon removed while
            // the user was away; reopening it would show some other source.
            vanished = saved
                .as_ref()
                .is_some_and(|saved| !view.choices.iter().any(|c| c.id == saved.source_id));
            if let Some(mut saved) = saved
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
        if vanished {
            ui.set_source_catalog_open(false);
            self.navigate(ui, 3);
            return;
        }
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
                applied_browse_filters(&view),
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
                        .map(|s| app.services.series_listing(&choice.id, s))
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
        ui.invoke_remember_library_position();
        if crate::content_presentation::Settings::load(&series.id)
            .unwrap_or_default()
            .effective()
            == crate::content_presentation::Presentation::Gallery
        {
            self.open_gallery(ui, series);
            return;
        }
        self.clear_overview_images(ui);
        self.view().overview = None;
        ui.set_detail_origin(self.view().section);
        if self.view().series.as_ref().map(|current| &current.id) != Some(&series.id) {
            ui.set_chapter_filter(0);
        }
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
    fn mount_pages(&self, ui: &AppWindow) {
        let view = self.view();
        let Some(reader) = &view.reader else {
            return;
        };
        let positions = reader.page_positions();
        let rows = reader
            .ratios
            .iter()
            .enumerate()
            .map(|(i, ratio)| PageRow {
                page_id: reader.row_page_id(i).into(),
                image: slint::Image::default(),
                tiles: ModelRc::default(),
                ratio: *ratio,
                position: positions[i],
                loaded: false,
                preview: false,
                error: "".into(),
            })
            .collect::<Vec<_>>();
        ui.set_total_ratio(reader.total_extent());
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
        if reader.mode != ReadingMode::Webtoon {
            // Paged frames have their own local pan. Chapter-stacked resume
            // coordinates belong only to the scrolling Webtoon canvas.
            return;
        }
        let offset =
            reader.page_position(index) + reader.ratios[index] * reader.anchor.offset() as f32;
        ui.set_scroll_y(-offset * ui.get_reader_width());
    }
    fn choose_reader_mode(
        self: &Arc<Self>,
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
            // Leave `reader.mode` for apply_reader_preferences to resolve: it
            // compares the old and new mode to decide whether the 96px chapter
            // gaps change row positions.
            reader.mode_choice = choice;
            reader.mode_switching = true;
            reader.budget_blocked.clear();
            choice.resolve(reader.automatic_mode)
        };
        ui.set_reader_mode_choice(choice.selection());
        ui.set_reading_mode(mode_index(mode));
        self.apply_reader_preferences(ui);
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
        if view.section != 6 {
            return;
        }
        let mut width_changed = false;
        let mut viewport_changed = false;
        if let Some(reader) = &mut view.reader {
            width_changed = (reader.viewport_width - width).abs() > f32::EPSILON;
            viewport_changed =
                width_changed || (reader.viewport_height - height).abs() > f32::EPSILON;
            reader.viewport_width = width;
            reader.viewport_height = height;
        }
        drop(view);
        if width_changed {
            // Row positions include fixed-pixel gaps normalized by column
            // width. Reproject settled geometry without replacing tile handles.
            self.update_reader_geometry(ui);
            self.publish_reader_controls(ui);
        }
        if viewport_changed {
            self.restore_position(ui);
        }
        let index = { self.view().reader.as_ref().map(|r| r.anchor.page_index) };
        if let Some(index) = index {
            self.request_images(ui, index);
        }
    }
    /// A new press on the artwork means the user took over again.
    fn reader_pressed(&self) {
        if let Some(reader) = self.view().reader.as_mut() {
            reader.chapter_navigation = None;
        }
    }
    fn position(self: &Arc<Self>, ui: &AppWindow, offset: f32, width: f32, viewport: f32) {
        self.position_internal(ui, offset, width, viewport, PositionOrigin::User);
    }
    fn position_internal(
        self: &Arc<Self>,
        ui: &AppWindow,
        offset: f32,
        width: f32,
        viewport: f32,
        origin: PositionOrigin,
    ) {
        if width <= 0.0 || !offset.is_finite() || !width.is_finite() {
            return;
        }
        let Ok(mut view) = self.view.try_lock() else {
            // Another thread (the sync persistence barrier) owns the view. Do
            // not lose this frame, which may be the last one of a fling:
            // replay it on the UI thread unless a newer frame lands first.
            let frame = POSITION_FRAME.load(Ordering::Acquire);
            let (app, weak) = (self.clone(), ui.as_weak());
            let _ = slint::invoke_from_event_loop(move || {
                if POSITION_FRAME.load(Ordering::Acquire) == frame
                    && let Some(ui) = weak.upgrade()
                {
                    app.position_internal(&ui, offset, width, viewport, origin);
                }
            });
            return;
        };
        POSITION_FRAME.fetch_add(1, Ordering::AcqRel);
        if view.section != 6 {
            return;
        }
        let Some(reader) = &mut view.reader else {
            return;
        };
        if origin == PositionOrigin::OverviewSettlement {
            reader.completion_eligible = false;
        }
        if reader.mode != ReadingMode::Webtoon || reader.mode_switching {
            return;
        }
        // A changed layout restores the logical anchor before interpreting pixels.
        if reader.viewport_width > 0. && (reader.viewport_width - width).abs() > 0.5 {
            return;
        }
        reader.viewport_width = width;
        reader.viewport_height = viewport;
        if origin == PositionOrigin::Restoration && reader.chapter_navigation.is_some() {
            return;
        }

        let rows = ui.get_pages();
        let requested_offset = offset;
        let current = (reader.page_position(reader.anchor.page_index)
            + reader.ratios[reader.anchor.page_index] * reader.anchor.offset() as f32)
            * width;
        if origin == PositionOrigin::User {
            if (offset - current).abs() > 0.5 {
                reader.continuity_backwards = offset < current;
                reader.retain_window_direction(offset > current);
                // A pending explicit chapter jump survives scroll frames (a
                // running fling keeps delivering them); a new press cancels it.
            }
            reader.observe_reverse_scroll(offset, width, |index| {
                rows.row_data(index)
                    .is_some_and(|row| row.loaded || !row.error.is_empty())
            });
        }
        let mut offset = reader.loaded_scroll_offset(offset, width, viewport, |index| {
            // A terminal failure remains skippable; only in-flight images block.
            rows.row_data(index)
                .is_some_and(|row| row.loaded || !row.error.is_empty())
        });
        if origin == PositionOrigin::User {
            offset = reader.decoded_scroll_offset(offset, width, viewport);
        }
        if origin == PositionOrigin::User && (offset - current).abs() > 0.5 {
            // Restoring scroll-y can emit this same callback. Only an admitted
            // new source position starts reading again after neutral Pages.
            reader.completion_eligible = true;
        }
        // Keep Flickable itself at the admitted position, not just the durable
        // anchor; otherwise loading would reveal content above a blank scroll.
        if (offset - requested_offset).abs() > 0.5 {
            ui.set_scroll_y(-offset);
        }
        let mut remaining = offset / width;
        let mut index = 0;
        while index + 1 < reader.pages.len()
            && remaining >= reader.ratios[index] + reader.gap_after(index) / width
        {
            remaining -= reader.ratios[index] + reader.gap_after(index) / width;
            index += 1;
        }
        if matches!(
            origin,
            PositionOrigin::ZoomSettlement | PositionOrigin::OverviewSettlement
        ) && remaining > reader.ratios[index]
        {
            // ResumeAnchor cannot encode a gap. Admit its preceding real-image
            // edge and keep the actual canvas at that same source coordinate.
            remaining = reader.ratios[index];
            offset = (reader.page_position(index) + remaining) * width;
            ui.set_scroll_y(-offset);
        }
        // A viewport top inside the 96px chapter gap anchors at the preceding
        // image's end (ResumeAnchor cannot encode a gap). Remember how far into
        // the gap the canvas really is so a chapter-change rebase does not snap
        // it to the image edge and shove the content below by the whole gap.
        let into_gap = (remaining - reader.ratios[index]).max(0.);
        let fraction = (remaining / reader.ratios[index]).clamp(0.0, 1.0);
        let settlement_progress = origin != PositionOrigin::OverviewSettlement
            && (origin != PositionOrigin::ZoomSettlement
                || (reader.has_visible_pixels(reader.anchor.page_index)
                    && reader
                        .navigation_target_chapter()
                        .is_none_or(|target| target != &reader.chapter)));
        let (departing, changed_chapter) =
            if origin == PositionOrigin::OverviewSettlement || !reader.completion_eligible {
                let owner = reader.owner(index).clone();
                let changed = owner != reader.chapter;
                if changed {
                    let completed = reader.completed;
                    // Carry existing manual/visit flags between live owners; a
                    // neutral overview snapshot does not finish the chapter left.
                    let source = reader.source_for(index);
                    reader.completed = reader.continuity_completion(index, completed);
                    reader.chapter = owner;
                    reader.source = source;
                }
                (None, changed)
            } else {
                let departing = reader.activate_page(index);
                let changed = departing.is_some();
                (departing, changed)
            };
        if let Some(previous) = departing
            && settlement_progress
        {
            let _ = self.progress_checkpoint.enqueue(previous);
        }
        reader.anchor = ResumeAnchor::new(reader.page_id(index), index, f64::from(fraction))
            .unwrap_or_default();
        if reader.reverse_demand.is_some() && reader.reverse_target().is_none() {
            reader.reverse_demand = None;
        }
        if origin != PositionOrigin::OverviewSettlement {
            reader.completed |= reader.chapter_finished();
        }
        // Chapter preparation reconciles the live viewport for pixel admission,
        // but only the eventual covered entry commits destination progress.
        let record = reader.completion_eligible
            && (!reader.collection || reader.has_visible_pixels(reader.anchor.page_index))
            && !matches!(
                origin,
                PositionOrigin::ZoomSettlement | PositionOrigin::OverviewSettlement
            );
        if record && reader.chapter_navigation.is_none() {
            let progress = reader.progress_record();
            view.pending = Some(progress);
        }
        drop(view);
        if changed_chapter
            && !matches!(
                origin,
                PositionOrigin::ZoomSettlement | PositionOrigin::OverviewSettlement
            )
        {
            ui.invoke_cancel_reader_zoom();
            ui.set_zoom(1.);
            ui.set_scroll_x(0.);
            self.restore_position(ui);
            if into_gap > 0. {
                ui.set_scroll_y(ui.get_scroll_y() - into_gap * ui.get_reader_width());
            }
        }
        ui.set_current_page(index as i32);
        self.publish_reader_controls(ui);
        self.request_images(ui, index);
        self.maybe_preload_next(ui);
    }
    /// Re-queue one page after a transient failure. Only the page that actually
    /// failed is cleared; a page already scrolled out of the request window
    /// stays failed until the reader returns to it.
    fn retry_failed_image(self: &Arc<Self>, ui: &AppWindow, index: usize, generation: u64) {
        if self.generation.load(Ordering::Acquire) != generation || ui.get_section() != 6 {
            return;
        }
        let anchor = {
            let mut view = self.view();
            let Some(reader) = view.reader.as_mut() else {
                return;
            };
            if !reader.failed.remove(&index) {
                return;
            }
            reader.image_retry_due.remove(&reader.row_page_id(index));
            reader.requested.remove(&index);
            reader.budget_blocked.remove(&index);
            reader.anchor.page_index
        };
        self.request_images(ui, anchor);
    }

    fn retry_image_identity(
        self: &Arc<Self>,
        ui: &AppWindow,
        identity: &str,
        due: std::time::Instant,
    ) {
        if ui.get_section() != 6 {
            // Pages overview retains this reader; return explicitly rebinds
            // any deadline that elapsed while the reader was suspended.
            return;
        }
        let index = self
            .view()
            .reader
            .as_ref()
            .and_then(|r| r.retry_index(identity, due));
        if let Some(index) = index {
            self.retry_failed_image(ui, index, self.generation.load(Ordering::Acquire));
        }
    }

    fn schedule_image_retry(
        self: &Arc<Self>,
        ui: &AppWindow,
        identity: String,
        due: std::time::Instant,
    ) {
        let app = Arc::downgrade(self);
        let weak = ui.as_weak();
        crate::profiles::spawn(move || {
            std::thread::sleep(due.saturating_duration_since(std::time::Instant::now()));
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if let Some(app) = app.upgrade() {
                    app.retry_image_identity(&ui, &identity, due);
                }
            });
        });
    }

    fn resume_image_retries(self: &Arc<Self>, ui: &AppWindow) {
        let pending = self
            .view()
            .reader
            .as_ref()
            .map(|r| r.image_retry_due.clone())
            .unwrap_or_default();
        for (identity, due) in pending {
            if due <= std::time::Instant::now() {
                self.retry_image_identity(ui, &identity, due);
            }
        }
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
        let demand = reader.reverse_target();
        // One page behind, and read-ahead in the scroll direction (about two
        // viewport heights, within the model budget enforced at publication).
        let backwards = reader.continuity_backwards;
        let ahead = reader.read_ahead_pages(if backwards { index } else { end }, backwards);
        let first_desired = demand
            .map(|target| target.min(index))
            .unwrap_or_else(|| index.saturating_sub(if backwards { ahead } else { 1 }));
        let last_desired = if let Some(target) = demand {
            target.max(end)
        } else {
            (end + if backwards { 1 } else { ahead }).min(reader.pages.len() - 1)
        };
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
        let mut keep = if let Some(target) = demand {
            (index..=end)
                .chain(std::iter::once(target))
                .chain(reader.navigation_pixel_targets())
                .collect::<BTreeSet<_>>()
        } else {
            (first_desired..=last_desired).collect::<BTreeSet<_>>()
        };
        keep.extend(reader.navigation_pixel_targets());
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
        for old in 0..model.row_count() {
            if old != index
                && let Some(mut row) = model.row_data(old)
                && row.preview
            {
                reader_progressive::clear_preview(&mut row);
                model.set_row_data(old, row);
            }
        }
        // Pages that left the read-ahead window stay retained, farthest first to
        // go when a completion needs the model budget. Pages decoded for a
        // different width or display recipe are useless and dropped at once, as
        // is everything outside a reverse handoff, whose budgets assume it.
        for old in reader
            .retained
            .difference(&keep)
            .copied()
            .filter(|old| {
                demand.is_some()
                    || reader.decoded_widths.get(old).copied().unwrap_or(0)
                        < reader.quality_width(*old)
            })
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
            reader.retained.remove(&old);
        }
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
                // Visible work wins over an idle fetch that would delay it.
                if let Some((slot, cancel)) = self
                    .active_image
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    && *slot == reader_band_read_ahead::FETCH_AHEAD_SLOT
                {
                    cancel.cancel();
                }
                let _ = self.images.send(ImageTask {
                    generation,
                    index: target,
                });
            }
        }
        if anchor_changed && let Some((previous, next)) = chapter_navigation {
            ui.set_can_previous(previous);
            ui.set_can_next(next);
        }
    }
    fn image_worker(
        app: Weak<Self>,
        receiver: mpsc::Receiver<ImageTask>,
        ui: slint::Weak<AppWindow>,
    ) {
        let mut encoded = EncodedPages::default();
        let mut decoded = reader_engine::DisplayDecoder::default();
        let mut decode_permit = None;
        let mut epoch = u64::MAX;
        let mut limits_epoch = reader_engine::limits_revision();
        // The page the decoder last rendered a window of: the one whose bands
        // are read ahead while idle.
        let mut ahead_page: Option<usize> = None;
        loop {
            if limits_epoch != reader_engine::limits_revision() {
                decoded.clear();
                decode_permit.take();
                encoded = EncodedPages::default();
                limits_epoch = reader_engine::limits_revision();
            }
            // Queued work always goes first. With none, the viewer is idle and
            // the worker spends the time decoding bands the next window shift
            // will need, one band at a time so a new task is never kept waiting.
            let queued = match receiver.try_recv() {
                Ok(task) => Some(task),
                Err(mpsc::TryRecvError::Disconnected) => break,
                Err(mpsc::TryRecvError::Empty) => None,
            };
            let task = match queued.map_or_else(
                || {
                    if Self::read_ahead_step(
                        &app,
                        epoch,
                        &mut ahead_page,
                        &mut encoded,
                        &mut decoded,
                        &mut decode_permit,
                    ) {
                        return Err(None);
                    }
                    #[cfg(test)]
                    reader_handoff_tests::WORKER_IDLE.fetch_add(1, Ordering::AcqRel);
                    receiver
                        .recv_timeout(std::time::Duration::from_millis(250))
                        .map_err(Some)
                },
                Ok,
            ) {
                Ok(task) => task,
                Err(None) => continue,
                Err(Some(mpsc::RecvTimeoutError::Disconnected)) => break,
                Err(Some(mpsc::RecvTimeoutError::Timeout)) => {
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
                        decode_permit.take();
                        encoded = EncodedPages::default();
                        epoch = generation;
                    }
                    continue;
                }
            };
            if task.generation != epoch {
                decoded.clear();
                decode_permit.take();
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
                    r.pages.get(task.index).cloned().map(|p| {
                        (
                            p,
                            r.source_for(task.index),
                            r.owner(task.index).clone(),
                            r.page_id(task.index),
                        )
                    })
                })
            };
            let Some((input, source, chapter, input_id)) = input else {
                continue;
            };
            let cancel = CancelToken::default();
            *app.active_image.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((task.index, cancel.clone()));
            if app.generation.load(Ordering::Acquire) != task.generation {
                cancel.cancel();
            }
            let mut progressive = None;
            let mut limits_revision = reader_engine::limits_revision();
            #[cfg(test)]
            let bands_before = decoded.band_stats();
            let result: Result<_> = reader_engine::with_current_limits(|| {
                limits_revision = reader_engine::current_limits_revision();
                let mut pending_write = None;
                let mut cache_request = None;
                let before_scope = reader_capture_sources::scope(&input, source.as_ref())?;
                let mut fresh_original = false;
                let page = match &input {
                    PageInput::Verified(_) => encoded.read(task.generation, task.index, || {
                        decoded.clear();
                        decode_permit.take();
                        fresh_original = true;
                        input.read(source.as_ref(), &cancel)
                    })?,
                    #[cfg(test)]
                    PageInput::Local { .. } => encoded.read(task.generation, task.index, || {
                        decoded.clear();
                        decode_permit.take();
                        fresh_original = true;
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
                        progressive = reader_progressive::Stream::new(
                            &app,
                            ui.clone(),
                            &task,
                            cancel.clone(),
                            request.clone(),
                        );
                        let mut fetched = false;
                        let mut reused = false;
                        let mut load = || {
                            decoded.clear();
                            decode_permit.take();
                            fetched = true;
                            let read =
                                app.chapter_cache.read_with_chunks(&request, &mut |chunk| {
                                    if let Some(stream) = progressive.as_mut() {
                                        stream.feed(chunk, || {
                                            decoded.clear();
                                            decode_permit.take();
                                        });
                                    }
                                })?;
                            // Validate streaming EOF before EncodedPages can
                            // retain a complete-looking but truncated response.
                            if let Some(stream) = progressive.as_mut() {
                                stream.finish()?;
                            }
                            reused = read.from_cache;
                            pending_write = read.pending;
                            Ok(read.page)
                        };
                        let mut page = if let Some(key) = request.memory_key() {
                            encoded.read_scoped(task.generation, task.index, &key, &mut load)?
                        } else {
                            load()?
                        };
                        // Idle fetch-ahead may have fetched this page already; it
                        // is then handled as if this task had fetched it.
                        if !fetched
                            && let Some(ahead) = encoded.take_ahead(task.generation, task.index)
                        {
                            fetched = true;
                            reused = ahead.reused;
                            pending_write = ahead.pending;
                        }
                        if !fetched {
                            reused = true;
                        }
                        if !fetched && request.validate_scope().is_err() {
                            decoded.clear();
                            decode_permit.take();
                            if let Some(stream) = progressive.as_mut() {
                                stream.finish()?;
                            }
                            progressive = reader_progressive::Stream::new(
                                &app,
                                ui.clone(),
                                &task,
                                cancel.clone(),
                                request.clone(),
                            );
                            let read =
                                app.chapter_cache.read_with_chunks(&request, &mut |chunk| {
                                    if let Some(stream) = progressive.as_mut() {
                                        stream.feed(chunk, || {
                                            decoded.clear();
                                            decode_permit.take();
                                        });
                                    }
                                })?;
                            reused = read.from_cache;
                            pending_write = read.pending;
                            page = read.page;
                        }
                        request.validate_active()?;
                        fresh_original = fetched && !reused;
                        app.chapter_cache.touch(&request);
                        cache_request = Some((request, reused));
                        page
                    }
                };
                if let Some(stream) = progressive.as_mut() {
                    stream.finish()?;
                }
                cancel.check().map_err(error)?;
                let after_scope = reader_capture_sources::scope(&input, source.as_ref())?;
                app.capture_originals
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .record(
                        task.generation,
                        task.index,
                        &page,
                        if before_scope == after_scope {
                            before_scope
                        } else {
                            None
                        },
                        fresh_original,
                    );
                let source_dimensions = page
                    .dimensions(reader_engine::current_limits())
                    .map_err(error)?;
                let options = {
                    let view = app.view();
                    view.reader
                        .as_ref()
                        .ok_or_else(|| text::tr("Source unavailable"))?
                        .display_options
                };
                if decode_permit.is_none() {
                    decode_permit = crate::decode_admission::SHARED.acquire(
                        crate::decode_admission::Priority::Viewer,
                        || {
                            !cancel.is_cancelled()
                                && app.generation.load(Ordering::Acquire) == task.generation
                        },
                    );
                    if decode_permit.is_none() {
                        return Err(text::tr("Operation cancelled"));
                    }
                }
                let presentation = decoded
                    .presentation(&page, reader_engine::current_limits(), options)
                    .map_err(error)?;
                let dimensions = presentation.effective;
                {
                    let mut view = app.view();
                    if app.generation.load(Ordering::Acquire) != task.generation
                        || cancel.is_cancelled()
                    {
                        return Err(text::tr("Operation cancelled"));
                    }
                    if let Some(reader) = view.reader.as_mut() {
                        record_task_presentation(
                            reader,
                            app.generation.load(Ordering::Acquire),
                            &task,
                            (&chapter, &input_id),
                            options,
                            presentation,
                        )?;
                    }
                }
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
                let native_bytes =
                    u64::from(source_dimensions.width) * u64::from(source_dimensions.height) * 4;
                let copy_budget = (reader_engine::current_limits()
                    .max_decoded_bytes
                    .saturating_sub(native_bytes)
                    / 2) as usize;
                let response = page.downgrade();
                let reuse = {
                    let view = app.view();
                    view.reader.as_ref().and_then(|reader| {
                        same_response(reader.decoded_responses.get(&task.index), &response)
                            .then(|| reader.decoded_windows.get(&task.index).copied())
                            .flatten()
                    })
                };
                #[cfg(test)]
                reader_handoff_tests::DECODE_LOG
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((Arc::as_ptr(&app) as usize, task.index));
                // Surviving strips keep their images, so render only the strips
                // entering the window, straight into their upload buffers.
                let (plan, prepared) = decoded
                    .render_window_tiles(
                        &page,
                        reader_engine::current_limits(),
                        display_width,
                        start,
                        end,
                        budget.min(copy_budget),
                        options,
                        |renderer| {
                            prepare_display_tiles(renderer.plan(), reuse, |rect, out| {
                                renderer.render_rect(
                                    rect.top,
                                    rect.height,
                                    rect.left,
                                    rect.width,
                                    out,
                                )
                            })
                        },
                    )
                    .map_err(error)?;
                cancel.check().map_err(error)?;
                let write = pending_write.map(|pending| (pending, page));
                Ok((
                    dimensions,
                    plan,
                    prepared,
                    display_width,
                    write,
                    cache_request,
                    reuse,
                    response,
                    options,
                    presentation,
                ))
            });
            #[cfg(test)]
            reader_handoff_tests::log_bands(&app, task.index, bands_before, &decoded);
            let preview_cleanup = progressive
                .as_ref()
                .map(reader_progressive::Stream::cleanup);
            drop(progressive);
            ahead_page = result.is_ok().then_some(task.index);
            if result.is_err() {
                encoded.discard(task.generation, task.index);
                decoded.clear();
                decode_permit.take();
            }
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

                if app.generation.load(Ordering::Acquire) != task.generation
                    || reader_engine::limits_revision() != limits_revision
                {
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
                if reader.pages.get(task.index).is_none()
                    || reader.owner(task.index) != &chapter
                    || reader.page_id(task.index) != input_id
                {
                    return;
                }
                reader.requested.remove(&task.index);
                if !reader.desired.contains(&task.index) {
                    return;
                }
                let mut loaded = false;
                let mut geometry_updated = false;
                let mut image_retries = Vec::new();
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
                        options,
                        presentation,
                    )) => {
                        if reader.display_options != options {
                            let current = reader.anchor.page_index;
                            drop(view);
                            app.request_images(&ui, current);
                            return;
                        }
                        if cache_request.as_ref().is_some_and(|(request, reused)| {
                            request.validate_for_publication(*reused).is_err()
                        }) {
                            let current = reader.anchor.page_index;
                            drop(view);
                            app.request_images(&ui, current);
                            return;
                        }
                        if !reader.accepts_window(task.index, dimensions, plan) {
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
                        {
                            // Visible work may reclaim any non-visible page.
                            // Read-ahead only reclaims pages outside the
                            // read-ahead window, never nearer prefetch.
                            let urgent = visible.contains(&task.index);
                            let mut victims = reader
                                .retained
                                .iter()
                                .copied()
                                .filter(|i| {
                                    *i != task.index
                                        && !visible.contains(i)
                                        && (urgent || !reader.desired.contains(i))
                                })
                                .collect::<Vec<_>>();
                            victims.sort_by_key(|i| {
                                (
                                    reader.desired.contains(i),
                                    std::cmp::Reverse(i.abs_diff(task.index)),
                                )
                            });
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
                                if reader.desired.contains(&victim) {
                                    reader.budget_blocked.insert(victim);
                                }
                                if let Some(mut old) = model.row_data(victim) {
                                    old.tiles = ModelRc::default();
                                    old.image = slint::Image::default();
                                    old.loaded = false;
                                    model.set_row_data(victim, old);
                                }
                            }
                        }
                        if used.saturating_add(plan.byte_len()) > READER_MODEL_BYTES {
                            if reader.reverse_target() != Some(task.index) {
                                reader.budget_blocked.insert(task.index);
                            } else {
                                let current = reader.anchor.page_index;
                                drop(view);
                                app.request_images(&ui, current);
                            }
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
                        reader.presentations.insert(task.index, presentation);
                        reader.budget_blocked.remove(&task.index);
                        if geometry_changed {
                            reader.budget_blocked.clear();
                        }
                        row.tiles = tiles;
                        row.loaded = true;
                        row.preview = false;
                        reader.image_retries.remove(&task.index);
                        reader
                            .image_retry_due
                            .remove(&reader.row_page_id(task.index));
                        if reader
                            .progressive_job
                            .is_some_and(|(index, _)| index == task.index)
                        {
                            reader.progressive_job = None;
                        }
                        row.error = "".into();
                        reader.retained.insert(task.index);
                        if parent_changed {
                            model.set_row_data(task.index, row);
                        }
                        if geometry_changed {
                            reader.ratios[task.index] =
                                dimensions.height as f32 / dimensions.width as f32;
                            let positions = reader.page_positions();
                            for (i, ratio) in reader.ratios.iter().enumerate() {
                                let position = positions[i];
                                if let Some(mut row) = model.row_data(i)
                                    && (row.position != position || row.ratio != *ratio)
                                {
                                    row.position = position;
                                    row.ratio = *ratio;
                                    model.set_row_data(i, row);
                                }
                            }
                            ui.set_total_ratio(reader.total_extent());
                            geometry_updated = true;
                        }
                        if let Some((pending, page)) = write {
                            app.chapter_cache.enqueue(pending, page);
                        }
                    }
                    Err(error) => {
                        reader_progressive::clear_preview(&mut row);
                        model.set_row_data(task.index, row.clone());
                        if cancel.is_cancelled() || error == text::tr("Operation cancelled") {
                            let current = reader.anchor.page_index;
                            drop(view);
                            app.request_images(&ui, current);
                            return;
                        }
                        reader.failed.insert(task.index);
                        row.error = error.into();
                        // Transient source failures retry themselves with
                        // backoff instead of requiring the chapter to reopen.
                        let attempt = usize::from(
                            reader.image_retries.get(&task.index).copied().unwrap_or(0),
                        );
                        if let Some(delay) = image_retry_delay(row.error.as_str(), attempt) {
                            *reader.image_retries.entry(task.index).or_insert(0) += 1;
                            let identity = reader.row_page_id(task.index);
                            let due = std::time::Instant::now() + delay;
                            reader.image_retry_due.insert(identity.clone(), due);
                            image_retries.push((identity, due));
                        }
                        model.set_row_data(task.index, row);
                    }
                }
                drop(view);
                for (identity, due) in image_retries {
                    app.schedule_image_retry(&ui, identity, due);
                }
                if geometry_updated {
                    app.restore_position(&ui);
                }
                let navigating = app
                    .view()
                    .reader
                    .as_ref()
                    .is_some_and(|r| r.chapter_navigation.is_some());
                app.apply_chapter_navigation(&ui);
                let navigation_finished = navigating
                    && app
                        .view()
                        .reader
                        .as_ref()
                        .is_some_and(|r| r.chapter_navigation.is_none());
                if loaded && !navigation_finished {
                    let mode = { app.view().reader.as_ref().map(|r| r.mode) };
                    if mode == Some(ReadingMode::Webtoon) {
                        app.position_internal(
                            &ui,
                            -ui.get_scroll_y(),
                            ui.get_reader_width(),
                            ui.get_reader_viewport_height(),
                            PositionOrigin::Restoration,
                        );
                    } else {
                        let mut view = app.view();
                        if let Some(reader) = &mut view.reader
                            && reader.chapter_navigation.is_none()
                            && reader.completion_eligible
                        {
                            if !reader.collection {
                                reader.completed |= reader.paged_chapter_finished();
                            }
                            let progress = reader.progress_record();
                            view.pending = Some(progress);
                        }
                    }
                }
                // Both new geometry and a terminal visible error can release
                // a waiting reverse handoff. Neither requires another gesture.
                let index = { app.view().reader.as_ref().map(|r| r.anchor.page_index) };
                if let Some(index) = index {
                    app.publish_reader_controls(&ui);
                    app.request_images(&ui, index);
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
                decode_permit.take();
                encoded = EncodedPages::default();
            }
            if let Some(cleanup) = preview_cleanup {
                cleanup.clear();
            }
        }
    }
    fn step_page(self: &Arc<Self>, ui: &AppWindow, step: i32) {
        let mut view = self.view();
        let Some(reader) = &mut view.reader else {
            return;
        };
        reader.completion_eligible = true;
        reader.reverse_demand = None;
        reader.window_demand = None;
        reader.chapter_navigation = None;
        if step != 0 {
            reader.continuity_backwards = step < 0;
        }
        let current = reader.anchor.page_index;
        let split = reader.mode != ReadingMode::Webtoon && reader.is_split(current);
        let second = reader.anchor.offset() >= 0.5;
        let (index, fraction) = if step == 0 {
            (current, reader.anchor.offset())
        } else if step > 0 && split && !second {
            (current, 0.5)
        } else if step < 0 && split && second {
            (current, 0.)
        } else {
            let i = (current as i32 + step).clamp(0, reader.pages.len() as i32 - 1) as usize;
            (
                i,
                if step < 0 && reader.mode != ReadingMode::Webtoon && reader.is_split(i) {
                    0.5
                } else {
                    0.
                },
            )
        };
        if step == 0 {
            reader.failed.remove(&index);
            reader.image_retries.remove(&index);
            reader.image_retry_due.remove(&reader.row_page_id(index));
            reader.budget_blocked.remove(&index);
        }
        let moved = step_moves(current, second, index, fraction);
        let departing = reader.activate_page(index);
        let changed_chapter = departing.is_some();
        if let Some(previous) = departing {
            let _ = self.progress_checkpoint.enqueue(previous);
        }
        reader.anchor =
            ResumeAnchor::new(reader.page_id(index), index, fraction).unwrap_or_default();
        if !reader.collection {
            reader.completed |= reader.chapter_finished()
                || reader.selected_image_finishes_chapter(reader.anchor.page_index);
        }
        // The selected image is what the OSD and gallery highlight show, so it
        // is what progress stores even if its pixels have not decoded yet.
        let progress = reader.progress_record();
        view.pending = Some(progress);
        drop(view);
        if changed_chapter {
            ui.invoke_cancel_reader_zoom();
            ui.set_zoom(1.);
            ui.set_scroll_x(0.);
            self.restore_position(ui);
        }
        ui.set_current_page(index as i32);
        if ui.get_reading_mode() != 0 {
            ui.set_scroll_x(0.);
            ui.set_scroll_y(0.);
        }
        self.restore_position(ui);
        self.publish_reader_controls(ui);
        // Stepping past either end clamps in place: nothing changed on screen,
        // so a flash or page animation would only flicker.
        if step != 0 && moved {
            ui.invoke_flash_reader_page();
            ui.invoke_animate_reader_page();
        }
        self.request_images(ui, index);
        self.maybe_preload_next(ui);
    }
    fn step_chapter(self: &Arc<Self>, ui: &AppWindow, step: i32) {
        if step != 0 {
            if let Some(reader) = self.view().reader.as_mut() {
                reader.window_demand = None;
                reader.continuity_backwards = step < 0;
            }
            self.navigate_chapter(ui, step < 0);
            return;
        }
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
            if step == 0 {
                let completed = self.services.domain.snapshot().is_ok_and(|s| {
                    s.progress
                        .iter()
                        .any(|p| p.chapter == chapter && p.completed)
                });
                self.view().reread = completed.then(|| chapter.clone());
            }
            self.open_chapter(ui, chapter);
        }
    }
    fn download_list(self: &Arc<Self>, ui: &AppWindow) {
        ui.set_section(7);
        ui.set_heading(text::tr("Downloads").into());
        ui.set_download_busy(self.view().downloads.catalogue.is_none());
        self.publish_downloads(ui);
        self.refresh_download_states(ui);
    }
    fn download(self: &Arc<Self>, ui: &AppWindow, chapter: ChapterId) {
        self.download_many(ui, vec![chapter]);
    }
    fn download_many(self: &Arc<Self>, ui: &AppWindow, chapters: Vec<ChapterId>) {
        let chapters = {
            let mut view = self.view();
            chapters
                .into_iter()
                .filter(|chapter| {
                    if chapter.series.source_id == "local"
                        || view.downloads.pending.contains_key(chapter)
                    {
                        return false;
                    }
                    view.downloads.pending.insert(chapter.clone(), None);
                    view.downloads.prepare_errors.remove(chapter);
                    true
                })
                .collect::<Vec<_>>()
        };
        if chapters.is_empty() {
            return;
        }
        ui.set_status("".into());
        self.publish_downloads(ui);
        let app = self.clone();
        let weak = ui.as_weak();
        let lease = crate::download_background::begin(&self.services.downloads);
        crate::profiles::spawn(move || {
            let _lease = lease;
            // One worker per explicit batch; metadata and transfers remain
            // bounded even when Resume all contains hundreds of chapters.
            let _serial = app
                .download_serial
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for chapter in chapters {
                let cancel = CancelToken::default();
                let prepared = app.services.prepare_download(&chapter, &cancel);
                {
                    let mut view = app.view();
                    if let Err(failure) = &prepared {
                        view.downloads
                            .prepare_errors
                            .insert(chapter.clone(), failure.clone());
                    }
                }
                let desired = app.view().downloads.pending.remove(&chapter).flatten();
                if prepared.is_ok() {
                    match desired {
                        Some(reader_downloads::JobStatus::Paused) => {
                            let _ = app.services.downloads.pause(&chapter);
                        }
                        Some(reader_downloads::JobStatus::Cancelled) => {
                            let _ = app.services.downloads.cancel(&chapter);
                        }
                        _ => {}
                    }
                }
                let owner = app.clone();
                let _ = weak.upgrade_in_event_loop(move |ui| owner.refresh_download_states(&ui));
                let mut download_attempts: BTreeMap<ChapterId, u8> = BTreeMap::new();
                loop {
                    let before = app
                        .services
                        .downloads
                        .snapshot()
                        .ok()
                        .and_then(|jobs| {
                            jobs.into_iter()
                                .find(|j| j.status == reader_downloads::JobStatus::Queued)
                        })
                        .map(|j| j.chapter);
                    match app.services.run_next_download(&cancel) {
                        Ok(Some(_)) => {}
                        Ok(None) => break,
                        Err(failure) => {
                            eprintln!("Nova Reader download: {failure}");
                            // Transient source failures are retried in place with
                            // backoff before the queue advances; a permanently
                            // failed job still moves on instead of blocking.
                            if let Some(chapter) = before.clone()
                                && transient_image_error(&failure)
                                && let Some(delay) = download_retry_delay(
                                    &failure,
                                    usize::from(*download_attempts.get(&chapter).unwrap_or(&0)),
                                )
                            {
                                *download_attempts.entry(chapter.clone()).or_insert(0) += 1;
                                // The reader may cancel or pause during the backoff;
                                // only a job still Failed is re-queued.
                                if sleep_cancellable(&cancel, delay)
                                    && let Ok(true) = app
                                        .services
                                        .downloads
                                        .resume_if_failed(&chapter)
                                        .map_err(error)
                                    && let Ok(()) =
                                        app.services.run_queued_download(&chapter, &cancel)
                                {
                                    download_attempts.remove(&chapter);
                                    continue;
                                }
                            }
                            // A transport failure is persisted as Failed and
                            // advances the queue. A failed metadata write must
                            // not retry the same queued identity indefinitely.
                            let after = app
                                .services
                                .downloads
                                .snapshot()
                                .ok()
                                .and_then(|jobs| {
                                    jobs.into_iter()
                                        .find(|j| j.status == reader_downloads::JobStatus::Queued)
                                })
                                .map(|j| j.chapter);
                            if before == after {
                                break;
                            }
                        }
                    }
                }
            }
            drop(_serial);
            let _ = weak.upgrade_in_event_loop(move |ui| app.refresh_download_states(&ui));
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
            if let Some(user) = match action {
                "presentation-default" => {
                    Some(crate::content_presentation::UserPreference::Default)
                }
                "presentation-reader" => Some(crate::content_presentation::UserPreference::Reader),
                "presentation-gallery" => {
                    Some(crate::content_presentation::UserPreference::Gallery)
                }
                _ => None,
            } {
                let saved = crate::content_presentation::Settings::set_user(&id, user);
                // Always project durable state after a failure, rather than
                // showing a selection that could not be saved.
                let settings = crate::content_presentation::Settings::load(&id).unwrap_or_default();
                ui.set_entry_menu_presentation(match settings.user {
                    crate::content_presentation::UserPreference::Default => 0,
                    crate::content_presentation::UserPreference::Reader => 1,
                    crate::content_presentation::UserPreference::Gallery => 2,
                });
                saved?;
                self.render(ui);
                return Ok(());
            }
            // Title menu → Download: chosen in reading order, skipping chapters
            // already on disk or waiting.
            let pick = match action {
                "download-unread" => Some(chapter_batch::Pick::Unread),
                "download-all" => Some(chapter_batch::Pick::All),
                _ => action
                    .strip_prefix("download-next:")
                    .and_then(|n| n.parse::<usize>().ok())
                    .map(chapter_batch::Pick::NextUnread),
            };
            if let Some(pick) = pick {
                let targets = chapter_batch::to_download(
                    &snapshot,
                    &id,
                    None,
                    pick,
                    &self.view().download_states,
                );
                if targets.is_empty() {
                    ui.set_status(text::tr("Nothing left to download.").into());
                } else {
                    self.download_many(ui, targets);
                }
                return Ok(());
            }
            if action == "delete-read-downloads" {
                // Read, fully downloaded chapters; bookmarked ones keep theirs.
                let targets: Vec<ChapterId> = {
                    let view = self.view();
                    snapshot
                        .chapters
                        .iter()
                        .filter(|c| c.id.series == id)
                        .map(|c| c.id.clone())
                        .filter(|c| {
                            view.download_states.get(c)
                                == Some(&reader_downloads::JobStatus::Complete)
                        })
                        .filter(|c| {
                            snapshot
                                .progress
                                .iter()
                                .any(|p| &p.chapter == c && p.completed)
                        })
                        .filter(|c| !snapshot.bookmarks.iter().any(|b| &b.chapter == c))
                        .collect()
                };
                self.task(
                    ui,
                    move |app, _| {
                        for chapter in &targets {
                            app.remove_download(chapter)?;
                        }
                        Ok(targets)
                    },
                    |app, ui, targets| {
                        {
                            let mut view = app.view();
                            for chapter in &targets {
                                view.download_states.remove(chapter);
                            }
                        }
                        app.render(ui);
                        ui.set_status(text::tr("Read downloads deleted.").into());
                        app.refresh_download_states(ui);
                    },
                );
                return Ok(());
            }
            if matches!(
                action,
                "mark-all-read" | "mark-all-unread" | "clear-all-positions"
            ) {
                self.checkpoint_progress();
                self.progress_checkpoint.flush(None)?;
                if action == "clear-all-positions" {
                    // Clear every saved page in one durable edit; read state,
                    // History and bookmarks stay untouched.
                    self.services
                        .domain
                        .update(|state| reset_series_positions(state, &id, now()).map(|_| ()))
                        .map_err(error)?;
                    self.render(ui);
                    ui.set_status(text::tr("Chapter positions cleared.").into());
                    return Ok(());
                }
                if !snapshot
                    .chapters
                    .iter()
                    .any(|chapter| chapter.id.series == id)
                {
                    let target = key.to_owned();
                    let action = action.to_owned();
                    self.task(
                        ui,
                        move |app, cancel| app.services.details(&series, cancel),
                        move |app, ui, (_, chapters)| {
                            if chapters.is_empty() {
                                ui.set_status(
                                    text::tr("No chapters are available for this title.").into(),
                                );
                            } else {
                                app.entry_action(ui, &target, &action);
                            }
                        },
                    );
                    return Ok(());
                }
                let mut newly_read = Vec::new();
                self.services
                    .domain
                    .update_manual_progress(|state| {
                        newly_read = set_series_read(state, &id, action == "mark-all-read", now())?;
                        Ok(())
                    })
                    .map_err(error)?;
                if !newly_read.is_empty() {
                    let services = self.services.clone();
                    crate::profiles::spawn(move || {
                        crate::download_cleanup::after_read(
                            &services.domain,
                            &services.downloads,
                            &newly_read,
                        )
                    });
                }
                self.render(ui);
                return Ok(());
            }
            if action == "details" {
                self.gallery_details(ui, series);
                return Ok(());
            }
            if action == "download" {
                let chapter = self
                    .view()
                    .overview
                    .as_ref()
                    .filter(|o| o.collection && o.series.id == id && !o.inventory.is_empty())
                    .map(|o| o.chapter.clone());
                if let Some(chapter) = chapter {
                    self.download(ui, chapter);
                }
                return Ok(());
            }
            if action == "refresh" {
                let gallery = self
                    .view()
                    .overview
                    .as_ref()
                    .is_some_and(|o| o.collection && o.series.id == id);
                if gallery {
                    self.gallery_refresh(ui);
                    return Ok(());
                }
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
            _ => return,
        };
        if action == "download-next-5" {
            // This chapter and the next ones in reading order that are not on
            // disk or already waiting, five in all.
            let targets = self.services.domain.snapshot().map(|state| {
                chapter_batch::to_download(
                    &state,
                    &chapter.series,
                    Some(&chapter),
                    chapter_batch::Pick::From(5),
                    &self.view().download_states,
                )
            });
            match targets {
                Ok(targets) => self.download_many(ui, targets),
                Err(e) => ui.set_status(error(e).into()),
            }
            return;
        }
        if action == "read-previous" {
            let mut newly_read = Vec::new();
            let result = self
                .services
                .domain
                .update_manual_progress(|state| {
                    let earlier = chapter_batch::before(state, &chapter);
                    newly_read = chapter_batch::mark_read(state, &earlier, now());
                    Ok(())
                })
                .map_err(error);
            match result {
                Err(e) => ui.set_status(e.into()),
                Ok(()) => {
                    if !newly_read.is_empty() {
                        let services = self.services.clone();
                        crate::profiles::spawn(move || {
                            crate::download_cleanup::after_read(
                                &services.domain,
                                &services.downloads,
                                &newly_read,
                            )
                        });
                    }
                    self.render(ui);
                }
            }
            return;
        }
        if matches!(action, "pause" | "cancel") {
            self.downloads_action(
                ui,
                &serde_json::to_string(&chapter).unwrap_or_default(),
                action,
            );
            return;
        }
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
            "download" | "resume" | "retry" => {
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
            "clear-progress" => {
                self.checkpoint_progress();
                self.progress_checkpoint.flush(None).and_then(|_| {
                    self.services
                        .domain
                        .update(|state| {
                            if let Some(progress) =
                                state.progress.iter_mut().find(|p| p.chapter == chapter)
                            {
                                // No saved page: reopening starts at image one without
                                // read/unread or History changing.
                                progress.anchor = ResumeAnchor::cleared();
                                progress.last_read_at = now();
                            }
                            Ok(())
                        })
                        .map_err(error)
                })
            }
            "read" => {
                let mut newly_read = false;
                let result = self
                    .services
                    .domain
                    .update_manual_progress(|state| {
                        newly_read = !state
                            .progress
                            .iter()
                            .any(|p| p.chapter == chapter && p.completed);
                        toggle_chapter_read(state, chapter.clone(), now())
                    })
                    .map_err(error);
                if result.is_ok() && newly_read {
                    let services = self.services.clone();
                    crate::profiles::spawn(move || {
                        crate::download_cleanup::after_read(
                            &services.domain,
                            &services.downloads,
                            &[chapter],
                        )
                    });
                }
                result
            }
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
            if view.download_refreshing {
                view.download_refresh_again = true;
                return;
            }
            view.download_revision = view.download_revision.wrapping_add(1);
            view.download_refreshing = true;
            view.download_revision
        };
        let app = self.clone();
        let weak = ui.as_weak();
        crate::profiles::spawn(move || {
            let states = app
                .services
                .downloads
                .snapshot()
                .map_err(error)
                .and_then(|jobs| {
                    app.services
                        .domain
                        .snapshot()
                        .map(|state| (jobs, state))
                        .map_err(error)
                });
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let mut view = app.view();
                view.download_refreshing = false;
                let again = std::mem::take(&mut view.download_refresh_again);
                let current = revision == view.download_revision;
                let refresh = current && view.section == 5;
                if current {
                    match states {
                        Ok((jobs, catalogue)) => {
                            view.download_states = jobs
                                .iter()
                                .map(|job| (job.chapter.clone(), job.status.clone()))
                                .collect();
                            view.downloads.jobs = jobs;
                            view.downloads.catalogue = Some(catalogue);
                        }
                        Err(message) => ui.set_status(message.into()),
                    }
                }
                drop(view);
                if refresh {
                    app.patch_chapter_downloads(&ui);
                } else if app.view().section == 7 {
                    app.publish_downloads(&ui);
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
        browse_page::publish_attention(
            ui,
            sources.extensions.iter().filter(|e| !e.enabled).count(),
        );
        ui.set_extensions(ModelRc::new(VecModel::from(
            sources
                .extensions
                .into_iter()
                .map(|e| ExtensionRow {
                    icon: crate::addon_icons::image(&e.id),
                    sources: sources.choices.iter().filter(|c| c.package == e.id).count() as i32,
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
        self.install_source_bytes(&std::fs::read(path).map_err(error)?, adult)
    }
    fn install_source_bytes(
        &self,
        bytes: &[u8],
        adult: bool,
    ) -> Result<(SourceDescriptor, BrowseSources)> {
        let id = self.services.install(bytes)?;
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
        self.install_task(ui, move |app, adult| app.install_sources(path, adult));
    }
    #[cfg(not(target_os = "android"))]
    fn install_bytes(self: &Arc<Self>, ui: &AppWindow, bytes: Vec<u8>) {
        self.install_task(ui, move |app, adult| {
            app.install_source_bytes(&bytes, adult)
        });
    }
    fn install_task(
        self: &Arc<Self>,
        ui: &AppWindow,
        work: impl FnOnce(&Arc<Self>, bool) -> Result<(SourceDescriptor, BrowseSources)>
        + Send
        + 'static,
    ) {
        let adult = ui.get_show_adult();
        self.task(
            ui,
            move |app, _| work(app, adult),
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
/// Batch explicit series completion without fabricating visits or moving any
/// saved page. Full source-local identities isolate equally named titles.
fn set_series_read(
    state: &mut reader_domain::ReaderState,
    series: &reader_domain::SeriesId,
    completed: bool,
    at: i64,
) -> reader_domain::Result<Vec<ChapterId>> {
    let chapters = state
        .chapters
        .iter()
        .filter(|chapter| &chapter.id.series == series)
        .map(|chapter| chapter.id.clone())
        .collect::<Vec<_>>();
    let mut newly_read = Vec::new();
    for chapter in chapters {
        let prior = state
            .progress
            .iter()
            .find(|progress| progress.chapter == chapter)
            .cloned();
        if completed && !prior.as_ref().is_some_and(|p| p.completed) {
            newly_read.push(chapter.clone());
        }
        let mut progress = prior.unwrap_or(ReadingProgress {
            chapter: chapter.clone(),
            anchor: ResumeAnchor::cleared(),
            mode: state.preferences.mode,
            last_read_at: at,
            completed,
        });
        // Mark-all is an explicit read-state edit, not a reading position:
        // completed and unread chapters both lose any saved page.
        progress.anchor = ResumeAnchor::cleared();
        progress.completed = completed;
        progress.last_read_at = at;
        state.progress.retain(|p| p.chapter != chapter);
        state.progress.push(progress);
    }
    Ok(newly_read)
}
/// Reset saved resume positions for one series while preserving read/unread
/// state, History and bookmarks. Chapters with no saved position need no entry.
fn reset_series_positions(
    state: &mut reader_domain::ReaderState,
    series: &reader_domain::SeriesId,
    at: i64,
) -> reader_domain::Result<usize> {
    let mut cleared = 0;
    for progress in state
        .progress
        .iter_mut()
        .filter(|progress| &progress.chapter.series == series)
    {
        if progress.anchor.is_cleared() {
            continue;
        }
        progress.anchor = ResumeAnchor::cleared();
        progress.last_read_at = at;
        cleared += 1;
    }
    Ok(cleared)
}

/// Explicit completion is separate from reading activity. Only real reader
/// checkpoints call ReaderState::set_progress, which also records History.
pub(crate) fn toggle_chapter_read(
    state: &mut reader_domain::ReaderState,
    chapter: ChapterId,
    at: i64,
) -> reader_domain::Result<()> {
    let completed = !state
        .progress
        .iter()
        .find(|p| p.chapter == chapter)
        .is_some_and(|p| p.completed);
    // Read state is not a reading position: whichever direction this toggles,
    // the chapter has no saved page to resume from.
    let anchor = ResumeAnchor::cleared();
    state.progress.retain(|p| p.chapter != chapter);
    state.progress.push(ReadingProgress {
        chapter,
        anchor,
        mode: state.preferences.mode,
        last_read_at: at,
        completed,
    });
    Ok(())
}
// Texture dimensions stay bounded while logical progress continues to refer
// to the original page. Full decode admission remains the engine's responsibility.
#[cfg(test)]
fn display_tiles(pixels: &[u8], width: u32, height: u32) -> Vec<TileRow> {
    display_window_tiles(pixels, width, height, 0, height)
}
fn same_response(previous: Option<&WeakPage>, current: &WeakPage) -> bool {
    previous.is_some_and(|previous| previous.strong_count() > 0 && previous.ptr_eq(current))
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
        // Sub-rects stay on the global upload grid (inside the 2,048-row strip
        // alignment), so overlapping windows share identical rects.
        let bottom =
            (top / reader_engine::DISPLAY_UPLOAD_ROWS + 1) * reader_engine::DISPLAY_UPLOAD_ROWS;
        let height = bottom.min(end) - top;
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
    prepare_display_tiles_from_pixels(pixels, width, height, first_row, full_height, None)
        .into_iter()
        .map(mount_tile)
        .collect()
}
/// Copy tiles out of an already rendered window (progressive previews, fixtures).
fn prepare_display_tiles_from_pixels(
    pixels: &[u8],
    width: u32,
    height: u32,
    first_row: u32,
    full_height: u32,
    previous: Option<reader_engine::DisplayWindowPlan>,
) -> Vec<PreparedTile> {
    let stride = width as usize * 4;
    prepare_display_tiles(
        reader_engine::DisplayWindowPlan {
            width,
            full_height,
            first_row,
            rows: height,
        },
        previous,
        |rect, out| {
            let row_bytes = rect.width as usize * 4;
            for (row, target) in out.chunks_exact_mut(row_bytes).enumerate() {
                let source = (rect.top as usize - first_row as usize + row) * stride
                    + rect.left as usize * 4;
                target.copy_from_slice(&pixels[source..source + row_bytes]);
            }
            Ok(())
        },
    )
    .unwrap()
}
/// Allocate upload buffers for the strips that are not already mounted and let
/// `render` fill each one. Strips present in `previous` stay untouched.
fn prepare_display_tiles(
    plan: reader_engine::DisplayWindowPlan,
    previous: Option<reader_engine::DisplayWindowPlan>,
    mut render: impl FnMut(&TileRect, &mut [u8]) -> reader_engine::Result<()>,
) -> reader_engine::Result<Vec<PreparedTile>> {
    let old = previous.map(tile_rects).unwrap_or_default();
    tile_rects(plan)
        .into_iter()
        .map(|rect| {
            let pixels = if old.contains(&rect) {
                None
            } else {
                let mut buffer =
                    slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(rect.width, rect.height);
                render(&rect, buffer.make_mut_bytes())?;
                Some(buffer)
            };
            Ok(PreparedTile { rect, pixels })
        })
        .collect()
}

/// Whether a page step lands on a different page or split half.
fn step_moves(current: usize, second_half: bool, index: usize, fraction: f64) -> bool {
    index != current || (fraction >= 0.5) != second_half
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
    /// Rendering the scene is the expensive part of opening the context menu, so
    /// a Device capture is taken only when an action needs it. The stamp proves
    /// the scene still has the geometry that was frozen when the menu opened.
    Deferred(ScreencapStamp),
    Chapter(Box<ChapterScreencap>),
}
#[derive(Clone)]
struct ChapterScreencap {
    viewport: crate::chapter_capture::Rect,
    reference: usize,
    pages: Vec<crate::chapter_capture::PageRect>,
    inputs: BTreeMap<usize, (PageInput, Option<Source>)>,
    originals: reader_capture_sources::Session,
    options: reader_engine::DisplayOptions,
    brightness: u8,
    device_scale: f64,
    output_scale: Option<f64>,
    known_dimensions: BTreeSet<usize>,
    layout_anchor: usize,
    layout_gaps: Vec<f64>,
    paged_fit: Option<PagedCaptureFit>,
    naming: BTreeMap<usize, (ChapterId, usize)>,
    background: [u8; 4],
}
#[derive(Clone)]
struct PagedCaptureFit {
    frame: crate::ReaderCaptureViewport,
    width: f64,
    zoom: f64,
    split_half: i32,
    split_pages: BTreeSet<usize>,
}
fn layout_paged_capture(
    pages: &mut [crate::chapter_capture::PageRect],
    anchor: usize,
    fit: &PagedCaptureFit,
) -> Result<()> {
    let anchor = pages
        .iter()
        .position(|page| page.index == anchor)
        .ok_or_else(|| text::tr("Capture geometry unavailable"))?;
    // Each source retains its own contained artwork inside a separate frame.
    // Putting neighbors flush against artwork would paint the current frame's
    // black gutters before the user explicitly extends the selection.
    for page in pages.iter_mut() {
        page.rect = paged_capture_rect(
            &fit.frame,
            fit.width,
            page.rect.height / page.rect.width,
            fit.split_pages
                .contains(&page.index)
                .then_some(fit.split_half),
            fit.zoom,
        );
    }
    let top = f64::from(fit.frame.y);
    let bottom = f64::from(fit.frame.y + fit.frame.height);
    let extents = pages
        .iter()
        .map(|page| (top.min(page.rect.y), bottom.max(page.rect.bottom())))
        .collect::<Vec<_>>();
    let mut offsets = vec![0.; pages.len()];
    for index in (0..anchor).rev() {
        offsets[index] = extents[index + 1].0 + offsets[index + 1] - extents[index].1;
    }
    for index in anchor + 1..pages.len() {
        offsets[index] = extents[index - 1].1 + offsets[index - 1] - extents[index].0;
    }
    for (page, offset) in pages.iter_mut().zip(offsets) {
        page.rect.y += offset;
    }
    Ok(())
}
fn capture_chapter_request(
    app: &Controller,
    ui: &AppWindow,
    reference: i32,
    advanced: bool,
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
    let mut naming = BTreeMap::new();
    if reference >= reader.pages.len() {
        return Err(text::tr("Chapter image is unavailable"));
    }
    let owner = reader.owner(reference).clone();
    for index in 0..model.row_count() {
        if index >= reader.pages.len() {
            continue;
        }
        let Some(row) = model.row_data(index).filter(|row| advanced || row.loaded) else {
            continue;
        };
        if (advanced && reader.owner(index) != &owner)
            || (!advanced && ui.get_reading_mode() != 0 && index != ui.get_current_page() as usize)
        {
            continue;
        }
        let rect = chapter_capture_rect_with_split(
            &frame,
            width,
            ui.get_reading_mode(),
            &row,
            (reader.mode != ReadingMode::Webtoon && reader.is_split(index))
                .then_some(ui.get_reader_split_half()),
            f64::from(ui.get_zoom()),
        );
        if (advanced
            || (rect.x < viewport.x + viewport.width
                && rect.x + rect.width > viewport.x
                && rect.y < viewport.y + viewport.height
                && rect.y + rect.height > viewport.y))
            && let Some(input) = reader.pages.get(index)
        {
            pages.push(PageRect { index, rect });
            inputs.insert(index, (input.clone(), reader.source_for(index)));
            naming.insert(
                index,
                (reader.owner(index).clone(), reader.local_index(index) + 1),
            );
        }
    }
    let paged_fit = (ui.get_reading_mode() != 0).then(|| PagedCaptureFit {
        frame: frame.clone(),
        width,
        zoom: f64::from(ui.get_zoom()),
        split_half: ui.get_reader_split_half(),
        split_pages: pages
            .iter()
            .filter(|page| reader.is_split(page.index))
            .map(|page| page.index)
            .collect(),
    });
    if let Some(fit) = &paged_fit {
        layout_paged_capture(&mut pages, reference, fit)?;
    }
    let layout_gaps = pages
        .windows(2)
        .map(|pair| pair[1].rect.y - pair[0].rect.bottom())
        .collect();
    let known_dimensions = reader
        .native_dimensions
        .keys()
        .chain(reader.presentations.keys())
        .copied()
        .collect();
    // Keep the page geometry tied to the original frame, but export only the
    // visible loaded artwork on desktop (not its centered letterbox gutters).
    let viewport = media_capture_viewport(
        viewport,
        pages
            .iter()
            .filter(|page| model.row_data(page.index).is_some_and(|row| row.loaded))
            .map(|page| page.rect),
        ui.get_reader_platform_android(),
    )?;
    let color = ui.get_reader_capture_background();
    let originals = app
        .capture_originals
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .session(
            app.generation.load(Ordering::Acquire),
            inputs.keys().copied(),
        );
    Ok(ChapterScreencap {
        originals,
        viewport,
        reference,
        pages,
        inputs,
        options: reader.display_options,
        device_scale: f64::from(ui.window().scale_factor()),
        output_scale: None,
        known_dimensions,
        layout_anchor: reference,
        layout_gaps,
        paged_fit,
        naming,
        brightness: if crate::reader_settings::snapshot().custom_brightness {
            crate::reader_settings::snapshot().brightness
        } else {
            100
        },
        background: [color.red(), color.green(), color.blue(), color.alpha()],
    })
}
// Match the reader's actual fit/centering geometry. The viewport and pan are
// frozen by Slint immediately before opening the menu, rather than guessed
// from a saved logical reading anchor or the device's physical resolution.
#[cfg(test)]
fn chapter_capture_rect(
    viewport: &crate::ReaderCaptureViewport,
    width: f64,
    mode: i32,
    row: &PageRow,
) -> crate::chapter_capture::Rect {
    chapter_capture_rect_with_split(viewport, width, mode, row, None, 1.)
}
fn chapter_capture_rect_with_split(
    viewport: &crate::ReaderCaptureViewport,
    width: f64,
    mode: i32,
    row: &PageRow,
    split: Option<i32>,
    zoom: f64,
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
        paged_capture_rect(viewport, width, f64::from(row.ratio), split, zoom)
    }
}
fn paged_capture_rect(
    viewport: &crate::ReaderCaptureViewport,
    width: f64,
    ratio: f64,
    split: Option<i32>,
    zoom: f64,
) -> crate::chapter_capture::Rect {
    let height = f64::from(viewport.height);
    let zoom = zoom.max(0.01);
    let image_width =
        (width / zoom).min(height / (ratio / if split.is_some() { 2. } else { 1. })) * zoom;
    let image_height = image_width * ratio;
    let visible_height = image_height / if split.is_some() { 2. } else { 1. };
    crate::chapter_capture::Rect {
        x: f64::from(viewport.x + viewport.content_x)
            + (f64::from(viewport.width).max(image_width) - image_width) / 2.,
        y: f64::from(viewport.y + viewport.content_y)
            + split
                .map(|half| -f64::from(half) * image_height / 2.)
                .unwrap_or((height.max(visible_height) - image_height) / 2.),
        width: image_width,
        height: image_height,
    }
}
/// Clip only exterior viewport space, never trim black pixels from the source
/// itself. Internal gaps between visible pages remain part of their composition.
fn media_capture_viewport(
    viewport: crate::chapter_capture::Rect,
    media: impl Iterator<Item = crate::chapter_capture::Rect>,
    preserve_frame: bool,
) -> Result<crate::chapter_capture::Rect> {
    if preserve_frame {
        return Ok(viewport);
    }
    media
        .filter_map(|rect| rect.intersection(viewport))
        .reduce(|a, b| {
            let x = a.x.min(b.x);
            let y = a.y.min(b.y);
            crate::chapter_capture::Rect {
                x,
                y,
                width: a.right().max(b.right()) - x,
                height: a.bottom().max(b.bottom()) - y,
            }
        })
        .ok_or_else(|| text::tr("No artwork in the selected area."))
}
/// Everything that places reader media inside the window. If any of it moves
/// while the menu is open, a late render would no longer match the frozen
/// capture viewport, so the action fails instead of exporting the wrong pixels.
#[derive(Clone, PartialEq)]
struct ScreencapStamp {
    size: (u32, u32),
    scale: f32,
    scroll: (f32, f32),
    zoom: f32,
    page: i32,
    mode: i32,
    reader_width: f32,
    split: (bool, i32),
}
impl ScreencapStamp {
    fn of(ui: &AppWindow) -> Self {
        let size = ui.window().size();
        Self {
            size: (size.width, size.height),
            scale: ui.window().scale_factor(),
            scroll: (ui.get_scroll_x(), ui.get_scroll_y()),
            zoom: ui.get_zoom(),
            page: ui.get_current_page(),
            mode: ui.get_reading_mode(),
            reader_width: ui.get_reader_width(),
            split: (ui.get_reader_split_page(), ui.get_reader_split_half()),
        }
    }
}
/// Render the deferred Device capture now, with the menu already hidden and
/// before busy feedback is shown, as the eager capture used to see the scene.
fn resolve_deferred_screencap(ui: &AppWindow, stamp: &ScreencapStamp) -> Result<FrozenScreencap> {
    if ScreencapStamp::of(ui) != *stamp {
        return Err(text::tr("Reader screencap is unavailable"));
    }
    capture_reader(ui).map(FrozenScreencap::Device)
}
#[cfg(test)]
pub(crate) static DEVICE_SNAPSHOTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
fn capture_reader(ui: &AppWindow) -> Result<ReaderScreencap> {
    #[cfg(test)]
    DEVICE_SNAPSHOTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let size = ui.window().size();
    if u64::from(size.width) * u64::from(size.height) > 8_000_000 {
        return Err(text::tr("Reader screencap exceeds the image size limit"));
    }
    // Called before the context menu becomes visible. FemtoVG renders the
    // current scene offscreen, so neither the menu nor hidden OSD is captured.
    let image = ui.window().take_snapshot().map_err(error)?;
    let frame = ui.get_reader_capture_viewport();
    let viewport = crate::chapter_capture::Rect {
        x: f64::from(frame.x),
        y: f64::from(frame.y),
        width: f64::from(frame.width),
        height: f64::from(frame.height),
    };
    let model = ui.get_pages();
    let media = (0..model.row_count()).filter_map(|index| {
        let row = model.row_data(index).filter(|row| row.loaded)?;
        if ui.get_reading_mode() != 0 && index != ui.get_current_page() as usize {
            return None;
        }
        Some(chapter_capture_rect_with_split(
            &frame,
            f64::from(ui.get_reader_width()),
            ui.get_reading_mode(),
            &row,
            (ui.get_reading_mode() != 0 && ui.get_reader_split_page())
                .then_some(ui.get_reader_split_half()),
            f64::from(ui.get_zoom()),
        ))
    });
    let bounds = media_capture_viewport(viewport, media, ui.get_reader_platform_android())?;
    let scale = f64::from(ui.window().scale_factor());
    let left = (bounds.x * scale)
        .floor()
        .clamp(0., f64::from(image.width())) as u32;
    let top = (bounds.y * scale)
        .floor()
        .clamp(0., f64::from(image.height())) as u32;
    let right = (bounds.right() * scale)
        .ceil()
        .clamp(f64::from(left), f64::from(image.width())) as u32;
    let bottom = (bounds.bottom() * scale)
        .ceil()
        .clamp(f64::from(top), f64::from(image.height())) as u32;
    let width = right - left;
    let height = bottom - top;
    if width == 0 || height == 0 {
        return Err(text::tr("Reader screencap is unavailable"));
    }
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for row in top..bottom {
        let start = (u64::from(row) * u64::from(image.width()) + u64::from(left)) as usize * 4;
        pixels.extend_from_slice(&image.as_bytes()[start..start + width as usize * 4]);
    }
    Ok(ReaderScreencap {
        width,
        height,
        pixels,
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
            chapter: reader.owner(index).clone(),
            page: reader.local_index(index) + 1,
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
        let rect = chapter_capture_rect_with_split(
            &viewport,
            f64::from(ui.get_reader_width()),
            ui.get_reading_mode(),
            &row,
            (reader.mode != ReadingMode::Webtoon && reader.is_split(page))
                .then_some(ui.get_reader_split_half()),
            f64::from(ui.get_zoom()),
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
        let source_y = if let Some(presentation) = reader.presentations.get(&page) {
            let x = ((f64::from(viewport.x).max(rect.x) - rect.x) / rect.width
                * f64::from(presentation.effective.width))
            .clamp(0., f64::from(presentation.effective.width - 1));
            let y = ((f64::from(viewport.y).max(rect.y) - rect.y) / rect.height
                * f64::from(presentation.effective.height))
            .clamp(0., f64::from(presentation.effective.height - 1));
            presentation
                .source_coordinate(x, y)
                .1
                .round()
                .clamp(0., f64::from(height.saturating_sub(1))) as u32
        } else {
            export_source_row(rect.y, rect.height, f64::from(viewport.y), height)
        };
        return Ok(FrozenExportName {
            chapter: reader.owner(page).clone(),
            page: reader.local_index(page) + 1,
            source_y: Some(source_y),
        });
    }
    Err(text::tr("Reader screencap is unavailable"))
}

fn wire_reader_context(app: &Arc<Controller>, ui: &AppWindow) {
    use std::{cell::RefCell, rc::Rc};
    let screencap = Rc::new(RefCell::new(None));
    let editor = reader_capture_editor::wire(app, ui);
    let save_sheet = reader_save_image::SheetState::default();
    // The sheet's confirmed choice, handed to the action that runs it.
    let sheet_plan = Rc::new(RefCell::new(None::<(String, reader_save_image::Plan)>));
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
            let editor = editor.clone();
            let save_sheet = save_sheet.clone();
            move || {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                if let Some(app) = owner.upgrade() {
                    reader_capture_editor::stale(&editor, &app, &ui);
                }
                reader_save_image::stale(&save_sheet, &ui);
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
        let editor = editor.clone();
        move |index| {
            if let Some(ui) = weak.upgrade() {
                reader_capture_editor::freeze(&editor, &app, &ui, index);
                let capture = if crate::capture_settings::preferences(&ui).resolution
                    == crate::capture_settings::CaptureResolution::Chapter
                {
                    capture_chapter_request(&app, &ui, index, false)
                        .map(|capture| FrozenScreencap::Chapter(Box::new(capture)))
                } else {
                    Ok(FrozenScreencap::Deferred(ScreencapStamp::of(&ui)))
                };
                let naming = export_name_position(&app, &ui, index, true);
                *screencap.borrow_mut() =
                    Some((app.generation.load(Ordering::Acquire), capture, naming));
                reader_save_image::resolve_simple_detail(&app, &ui, index);
            }
        }
    });
    ui.on_reader_context_closed({
        let screencap = screencap.clone();
        let editor = editor.clone();
        move || {
            reader_capture_editor::clear_frozen(&editor);
            screencap.borrow_mut().take();
        }
    });
    ui.on_reader_context_action({
        let app = app.clone();
        let weak = ui.as_weak();
        let save_sheet = save_sheet.clone();
        let sheet_plan = sheet_plan.clone();
        move |index, action| {
            let _timer = &timer;
            let Some(ui) = weak.upgrade() else { return };
            let (screen, clipboard) = match action.as_str() {
                "save-image" | "save-image-simple" | "save-image-plan" => (false, false),
                "save-screencap" | "save-screencap-simple" | "save-screencap-selection" => {
                    (true, false)
                }
                "copy-image" => (false, true),
                "copy-screencap" | "copy-screencap-simple" | "copy-screencap-selection" => {
                    (true, true)
                }
                _ => return,
            };
            if exporting.load(Ordering::Acquire) {
                ui.set_reader_context_status(text::tr("Another image action is active").into());
                return;
            }
            if action == "save-image" {
                reader_save_image::open_sheet(&app, &ui, &save_sheet, index);
                return;
            }
            // Save image (simple) follows the settings; the sheet's Save runs
            // exactly the source and output chosen there. Copy image keeps the
            // page as the reader has it.
            let plan = match action.as_str() {
                "save-image-simple" => Some(reader_save_image::Plan::simple(
                    crate::capture_settings::preferences(&ui),
                )),
                "save-image-plan" => {
                    let Some((page_id, plan)) = sheet_plan.borrow_mut().take() else {
                        return;
                    };
                    let same_page = app.view().reader.as_ref().is_some_and(|reader| {
                        usize::try_from(index)
                            .ok()
                            .filter(|i| *i < reader.pages.len())
                            .is_some_and(|i| reader.page_id(i) == page_id)
                    });
                    if !same_page {
                        ui.set_reader_context_status(text::tr("Image action cancelled").into());
                        return;
                    }
                    Some(plan)
                }
                _ => None,
            };
            let target = match plan {
                Some(_) => match reader_save_image::Target::of(&app, index) {
                    Some(target) => Some(target),
                    None => {
                        ui.set_reader_context_status(
                            text::tr("Chapter image is unavailable").into(),
                        );
                        return;
                    }
                },
                None => None,
            };
            if action == "save-screencap" || action == "copy-screencap" {
                reader_capture_editor::open(&editor, &app, &ui, clipboard);
                return;
            }
            let selection = if action.ends_with("-selection") {
                let selected = reader_capture_editor::export(&editor, &ui);
                if selected.is_none() {
                    return;
                }
                selected
            } else {
                None
            };
            let selection_ticket = selection.as_ref().map(|(_, _, ticket)| ticket.clone());
            if exporting.swap(true, Ordering::AcqRel) {
                ui.set_reader_context_status(text::tr("Another image action is active").into());
                return;
            }
            // The menu is hidden by now and no busy feedback exists yet, so a
            // deferred Device capture sees the same scene the eager one did.
            let snapshot = screencap
                .borrow_mut()
                .take()
                .map(|(generation, capture, naming)| {
                    let capture = match capture {
                        Ok(FrozenScreencap::Deferred(stamp)) if screen && selection.is_none() => {
                            resolve_deferred_screencap(&ui, &stamp)
                        }
                        other => other,
                    };
                    (generation, capture, naming)
                });
            if selection.is_some() {
                ui.set_capture_editor_busy(true);
            }
            ui.set_reader_context_busy(true);
            ui.set_reader_context_status(text::tr("Preparing image…").into());
            let mut preferences = crate::capture_settings::preferences(&ui);
            if action == "save-screencap-selection" {
                // The editor's Output panel chose this save's format.
                preferences = preferences.with_output(
                    crate::capture_settings::CaptureFormat::try_from(
                        ui.get_capture_editor_format(),
                    )
                    .unwrap_or_default(),
                    ui.get_capture_editor_quality(),
                );
            }
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
                        .map(|page| (page, reader.source_for(index as usize)))
                })
            };
            // Said after a successful delivery, e.g. a format fallback.
            let notice = Arc::new(Mutex::new(None::<String>));
            let done = {
                let weak = weak.clone();
                let exporting = exporting.clone();
                let app = app.clone();
                let selection_ticket = selection_ticket.clone();
                let notice = notice.clone();
                move |result: Result<()>| {
                    exporting.store(false, Ordering::Release);
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        ui.set_reader_context_busy(false);
                        if selection_ticket
                            .as_ref()
                            .is_some_and(|ticket| !ticket.current())
                        {
                            return;
                        }
                        if selection_ticket.is_some() {
                            ui.set_capture_editor_busy(false);
                            match &result {
                                Ok(()) => {
                                    ui.set_capture_editor_visible(false);
                                    ui.set_capture_editor_image(slint::Image::default());
                                }
                                Err(message) => {
                                    ui.set_capture_editor_error(text::tr(message).into())
                                }
                            }
                        }
                        if app.generation.load(Ordering::Acquire) == generation
                            && ui.get_section() == 6
                        {
                            ui.set_reader_context_status(
                                match result {
                                    Ok(()) => {
                                        let done = text::tr(if clipboard {
                                            "Image copied to clipboard."
                                        } else {
                                            "Image saved."
                                        });
                                        match notice
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .take()
                                        {
                                            Some(notice) => format!("{done} {notice}"),
                                            None => done,
                                        }
                                    }
                                    Err(message) => text::tr(&message),
                                }
                                .into(),
                            );
                        }
                    });
                }
            };
            crate::profiles::spawn(move || {
                let mut final_naming = naming;
                let image = (|| {
                    cancel.check().map_err(error)?;
                    if screen {
                        let (captured_generation, capture, naming) =
                            if let Some((generation, capture, _)) = selection {
                                (
                                    generation,
                                    Ok(FrozenScreencap::Chapter(Box::new(capture))),
                                    Err(text::tr("Capture naming unavailable")),
                                )
                            } else {
                                snapshot
                                    .ok_or_else(|| text::tr("Reader screencap is unavailable"))?
                            };
                        final_naming = Some(naming);
                        if captured_generation != generation {
                            return Err(text::tr("Image action cancelled"));
                        }
                        let capture = match capture? {
                            FrozenScreencap::Device(capture) => capture,
                            FrozenScreencap::Deferred(_) => {
                                return Err(text::tr("Reader screencap is unavailable"));
                            }
                            FrozenScreencap::Chapter(capture) => {
                                let (rendered, naming) = reader_capture_editor::render_capture(
                                    &capture,
                                    &cancel,
                                    selection_ticket.as_ref(),
                                )?;
                                final_naming = Some(Ok(naming));
                                rendered
                            }
                        };
                        crate::reader_export::ExportImage::from_rgba_for_delivery(
                            capture.width,
                            capture.height,
                            capture.pixels,
                            &preferences,
                            clipboard,
                        )
                    } else if let (Some(plan), Some(target)) = (plan, &target) {
                        target.export(&plan, &format!("nova-reader-page-{}", index + 1), &cancel)
                    } else {
                        let (page, source) =
                            input.ok_or_else(|| text::tr("Chapter image is unavailable"))?;
                        let page = page.read(source.as_ref(), &cancel)?;
                        crate::reader_export::ExportImage::from_page(
                            &page,
                            &format!("nova-reader-page-{}", index + 1),
                            &preferences,
                            clipboard,
                        )
                    }
                })();
                match image {
                    Ok(image) if !cancel.is_cancelled() => {
                        *notice.lock().unwrap_or_else(|e| e.into_inner()) = image.notice.clone();
                        let current: crate::reader_export::ExportCurrent = Arc::new(move || {
                            !cancel.is_cancelled()
                                && selection_ticket
                                    .as_ref()
                                    .is_none_or(|ticket| ticket.current())
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
    ui.on_save_image_cancelled({
        let save_sheet = save_sheet.clone();
        move || reader_save_image::cancelled(&save_sheet)
    });
    ui.on_save_image_confirmed({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some((index, page_id, plan)) = reader_save_image::confirmed(&ui, &save_sheet)
            else {
                ui.set_reader_context_status(text::tr("Image action cancelled").into());
                return;
            };
            *sheet_plan.borrow_mut() = Some((page_id, plan));
            ui.invoke_reader_context_action(index, "save-image-plan".into());
        }
    });
}

/// The detail title menu's summary line and its download counts.
fn publish_title_menu(
    view: &View,
    snapshot: &reader_domain::ReaderState,
    series: &reader_domain::SeriesId,
    ui: &AppWindow,
) {
    let chapters: Vec<&Chapter> = snapshot
        .chapters
        .iter()
        .filter(|c| &c.id.series == series)
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
    let downloaded = chapters
        .iter()
        .filter(|c| view.download_states.get(&c.id) == Some(&reader_downloads::JobStatus::Complete))
        .count();
    let bytes: u64 = view
        .downloads
        .jobs
        .iter()
        .filter(|j| {
            &j.chapter.series == series && j.status == reader_downloads::JobStatus::Complete
        })
        .map(|j| j.downloaded_bytes())
        .sum();
    let mut parts = Vec::new();
    if let Some(entry) = snapshot
        .library
        .iter()
        .find(|entry| &entry.series == series)
    {
        // The open detail title, or (from Library) the stored series.
        let status = view
            .series
            .as_ref()
            .filter(|s| &s.id == series)
            .or_else(|| snapshot.series.iter().find(|s| &s.id == series))
            .map(|s| s.publication_status)
            .unwrap_or_default();
        parts.push(text::tr(
            reader_domain::library_bucket(entry, &snapshot.chapters, &snapshot.progress, status)
                .label(),
        ));
    }
    parts.push(format!("{unread} {}", text::tr("unread")));
    parts.push(format!("{downloaded} {}", text::tr("downloaded")));
    if bytes > 0 {
        parts.push(text::file_size(bytes as usize));
    }
    ui.set_detail_menu_summary(parts.join(" · ").into());
    let count = |pick| {
        chapter_batch::to_download(snapshot, series, None, pick, &view.download_states).len() as i32
    };
    ui.set_detail_unread_to_download(count(chapter_batch::Pick::Unread));
    ui.set_detail_all_to_download(count(chapter_batch::Pick::All));
}

/// The title's layout choice (0 default, 1 reader, 2 gallery) for the title
/// menu's Layout page and the entry menu.
fn publish_presentation_choice(ui: &AppWindow, key: &str) {
    let settings = serde_json::from_str::<reader_domain::SeriesId>(key)
        .map_err(error)
        .and_then(|id| crate::content_presentation::Settings::load(&id));
    match settings {
        Ok(settings) => ui.set_entry_menu_presentation(match settings.user {
            crate::content_presentation::UserPreference::Default => 0,
            crate::content_presentation::UserPreference::Reader => 1,
            crate::content_presentation::UserPreference::Gallery => 2,
        }),
        Err(error) => {
            ui.set_entry_menu_presentation(0);
            ui.set_status(error.into());
        }
    }
}

/// Names compare case-insensitively after trimming, like the visible label.
fn category_exists(categories: &[reader_domain::Category], name: &str) -> bool {
    let wanted = name.trim().to_lowercase();
    categories
        .iter()
        .any(|category| category.name.trim().to_lowercase() == wanted)
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
    crate::download_cleanup::init(&ui, services.domain.clone());
    crate::image_storage::init(&ui)?;
    crate::source_compression::init(&ui, services.clone());
    crate::reading_preferences::init(&ui)?;
    if let Err(error) = crate::reader_settings::init() {
        ui.set_status(
            format!(
                "{}: {error}",
                text::tr("Could not read reader settings; using defaults")
            )
            .into(),
        );
    }
    let (sender, receiver) = mpsc::channel();
    let artwork = crate::artwork::ArtworkLoader::new(
        services.clone(),
        ui.as_weak(),
        crate::paths::cache_dir()?.join("posters"),
    );
    let app = Arc::new(Controller {
        progress_checkpoint: crate::progress_checkpoint::ProgressWriter::new_with_cleanup(
            services.domain.clone(),
            services.downloads.clone(),
        )
        .map_err(error)?,
        services,
        artwork,
        chapter_cache: Arc::new(crate::chapter_cache::ChapterCache::new(
            crate::paths::chapter_cache_dir()?,
        )),
        capture_originals: Mutex::default(),
        thumbnail_service: OnceLock::new(),
        image_worker_live: AtomicBool::new(true),
        view: Mutex::new(View {
            source_browser: crate::source_browser::SourceBrowser::load(),
            library_navigation: library_navigation::State::load(),
            ..View::default()
        }),
        generation: AtomicU64::new(0),
        cancel: Mutex::new(CancelToken::default()),
        images: sender,
        download_serial: Mutex::new(()),
        active_image: Mutex::new(None),
        continuation_work: Default::default(),
        metadata_refresh: Arc::default(),
    });
    let limits_app = Arc::downgrade(&app);
    crate::image_limits_ui::init(&ui, move || {
        if let Some(app) = limits_app.upgrade() {
            app.artwork.apply_image_limits();
        }
    });
    let _ = LIFECYCLE.set(Arc::downgrade(&app));
    let barrier = Arc::downgrade(&app);
    nova_sync::set_persistence_barrier(Arc::new(move || {
        if let Some(app) = barrier.upgrade() {
            app.checkpoint_progress();
            app.progress_checkpoint
                .flush(None)
                .map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }));
    let remote = Arc::downgrade(&app);
    let reapply = Arc::downgrade(&app);
    crate::sync_settings::install_reapply(move |ui| {
        if let Some(app) = reapply.upgrade() {
            app.reload_synced_reader_mode();
            app.apply_reader_preferences(ui);
        }
    });
    let sync = crate::sync_ui::SyncController::initialize(app.services.clone(), &ui, move |ui| {
        if let Some(app) = remote.upgrade() {
            app.refresh_synced(ui);
        }
    })?;
    let weak = ui.as_weak();
    app.chapter_cache.on_gc(Arc::new(move || {
        let _ = weak.upgrade_in_event_loop(|ui| {
            ui.global::<crate::ImageStorageSettings>()
                .invoke_chapter_cache_refresh();
            ui.global::<crate::ImageStorageSettings>()
                .invoke_thumbnail_cache_refresh()
        });
    }));
    wire_reader_context(&app, &ui);
    timeline_page::wire(&app, &ui);
    browse_page::wire(&app, &ui);
    profile_actions::wire(&app, &ui);
    backup_actions::wire(&app, &ui);
    reader_controls::wire(&app, &ui);
    reader_open::wire(&app, &ui);
    let _overview_timer = page_overview::wire(&app, &ui);
    thumbnail_cache_settings::wire(&app, &ui);
    {
        let app = app.clone();
        let weak = ui.as_weak();
        ui.on_retry_continuation(move || {
            if let Some(ui) = weak.upgrade() {
                app.retry_continuation(&ui);
            }
        });
    }
    let worker = Arc::downgrade(&app);
    let weak = ui.as_weak();
    std::thread::spawn(move || Controller::image_worker(worker, receiver, weak));
    macro_rules! wire { ($method:ident, |$this:ident,$window:ident $(,$arg:ident)*| $body:expr) => {{let $this=app.clone();let weak=ui.as_weak();ui.$method(move |$($arg),*|{if let Some($window)=weak.upgrade(){$body}});}}; }
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
        crate::artwork::set_series_poster(&ui, slint::Image::default());
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
    wire!(on_detail_queue_action, |app, ui, action| app
        .detail_queue_action(&ui, &action));
    wire!(on_chapter_selection, |app, ui, op, index| app
        .chapter_selection(&ui, &op, index));
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
            browse_page::publish_filters(&app.view(), &ui);
            ui.set_source_filters_open(true);
            return;
        }
        let choice = {
            let view = app.view();
            view.choices.get(view.selected).cloned()
        };
        if let Some(choice) = choice.filter(|c| c.filters && c.enabled) {
            {
                let mut view = app.view();
                if view.source_filters_loading {
                    return;
                }
                view.source_filters_loading = true;
            }
            // Deliberately not `app.task`: that invalidates the current generation
            // and would cancel the catalogue page still loading underneath,
            // leaving an empty result with no retry.
            let app = app.clone();
            let weak = ui.as_weak();
            crate::profiles::spawn(move || {
                let result = (|| {
                    let source = app.services.source(&choice.id)?;
                    let response = source
                        .call_cancelled(
                            "filters",
                            &json!({"sourceId":choice.id}),
                            &CancelToken::default(),
                        )
                        .map_err(error)?;
                    crate::source_filters::FilterSet::parse(response)
                })();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    let mut view = app.view();
                    // The user may have chosen another source while this loaded.
                    if view.choices.get(view.selected).map(|c| c.id.as_str())
                        != Some(choice.id.as_str())
                    {
                        return;
                    }
                    view.source_filters_loading = false;
                    match result {
                        Ok(filters) => {
                            ui.set_source_filter_notice(filters.notice().into());
                            ui.set_source_filters(ModelRc::new(VecModel::from(filters.rows())));
                            view.source_filters = filters;
                            view.source_filters_loaded = true;
                            browse_page::publish_filters(&view, &ui);
                            drop(view);
                            ui.set_source_filters_open(true);
                        }
                        Err(message) => {
                            drop(view);
                            ui.set_status(text::tr(&message).into());
                        }
                    }
                });
            });
        }
    });
    wire!(on_source_filter_changed, |app, ui, key, value| {
        let _ = ui;
        let mut view = app.view();
        // Keep the native editor mounted while typing. Reopening the panel
        // publishes its validated draft from the provider-value model.
        view.source_filters.change(&key, &value);
        // Pills and switches redraw at once; text keeps its editor.
        if key.starts_with(' ') || value == "true" || value == "false" {
            browse_page::publish_filters(&view, &ui);
        }
    });
    wire!(on_source_filter_selected, |app, ui, key, index| {
        let _ = ui;
        let mut view = app.view();
        view.source_filters.select(&key, index);
        browse_page::publish_filters(&view, &ui);
    });
    wire!(on_source_filters_applied, |app, ui| {
        ui.set_source_filters_open(false);
        apply_source_filters(&mut app.view());
        browse_page::publish_filters(&app.view(), &ui);
        app.browse(&ui, "filtered", Some(String::new()), false);
    });
    wire!(on_search, |app, ui, query| {
        if app.view().section == 0 || app.view().section == 2 {
            {
                let mut view = app.view();
                if view.section == 0 && view.query.as_str() != query.as_str() {
                    view.library_navigation.reset_position();
                }
                view.query = query.to_string();
            }
            app.render(&ui);
        } else {
            app.browse(&ui, "", Some(query.to_string()), false);
        }
    });
    wire!(on_browse, |app, ui, kind| {
        let section = app.view().section;
        if kind == "refresh" {
            app.view().timeline.refresh = Some((0, 0));
            timeline_page::publish_refresh(&app.view().timeline, &ui);
            let progress = ui.as_weak();
            app.task(
                &ui,
                move |app, cancel| {
                    let state = app.services.domain.snapshot().map_err(error)?;
                    let disabled = app
                        .services
                        .sources
                        .list()
                        .map_err(error)?
                        .into_iter()
                        .filter(|package| !package.enabled)
                        .map(|package| package.id)
                        .collect::<Vec<_>>();
                    let mut failed = 0;
                    let series = refreshable_series(&state, &disabled);
                    // "Checking N titles… done / N" in the Updates header.
                    let report = |done: usize| {
                        app.view().timeline.refresh = Some((done, series.len()));
                        let app = app.clone();
                        let _ = progress.upgrade_in_event_loop(move |ui| {
                            timeline_page::publish_refresh(&app.view().timeline, &ui);
                        });
                    };
                    report(0);
                    for (done, series) in series.iter().enumerate() {
                        cancel.check().map_err(error)?;
                        if app.services.details(series, cancel).is_err() {
                            failed += 1;
                        }
                        report(done + 1);
                    }
                    Ok(failed)
                },
                |app, ui, failed| {
                    app.view().timeline.refresh = None;
                    reader_domain::profile::kv::set_str("reader:last-update", &now().to_string());
                    app.render(ui);
                    if failed > 0 {
                        ui.set_status(text::library_refresh_failed(failed).into());
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
    wire!(on_reader_press, |app, _ui| app.reader_pressed());
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
        {
            let app = app.clone();
            crate::desktop_dialogs::pick_zip(&ui, move |ui, bytes| app.install_bytes(ui, bytes));
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
                        Err(e) if text::is_user_cancel(&e) => {}
                        Err(e) => ui.set_status(text::error(&e).into()),
                    });
                },
            );
        }
    });
    wire!(on_local_import, |app, ui, folder| {
        #[cfg(not(target_os = "android"))]
        {
            use crate::desktop_dialogs::PathRequest;
            let app = app.clone();
            // The native dialog blocks, so it runs on the dialog worker.
            let request = if folder {
                PathRequest::ChapterFolder
            } else {
                PathRequest::ChapterFile
            };
            crate::desktop_dialogs::pick_path(&ui, request, move |ui, path| {
                app.task(
                    ui,
                    move |app, _| app.services.import_local(&path, folder),
                    |app, ui, _| {
                        app.view().section = 0;
                        app.render(ui);
                    },
                );
            });
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
                    Err(e) if text::is_user_cancel(&e) => {}
                    Err(e) => ui.set_status(text::error(&e).into()),
                });
            });
        }
    });
    wire!(on_category_selected, |app, ui, index| app
        .select_library_category(&ui, index));
    wire!(on_library_viewport_changed, |app, ui, key, fraction, y| app
        .remember_library_position(&ui, key.to_string(), fraction, y));
    wire!(on_library_checkpoint, |app, _ui| app.checkpoint_library());
    wire!(on_entry_menu_requested, |_app, ui, key| {
        publish_presentation_choice(&ui, &key)
    });
    wire!(on_title_menu_requested, |app, ui, key| {
        let Ok(series) = serde_json::from_str::<reader_domain::SeriesId>(&key) else {
            return;
        };
        if let Ok(snapshot) = app.services.domain.snapshot() {
            publish_title_menu(&app.view(), &snapshot, &series, &ui);
            // The Status, Categories and Layout pages' current choices.
            let entry = snapshot.library.iter().find(|entry| entry.series == series);
            ui.set_title_menu_status(entry.map_or(-1, |entry| entry.reading_status.action()));
            ui.set_entry_categories(ModelRc::new(VecModel::from(
                snapshot
                    .categories
                    .iter()
                    .map(|category| EntryCategoryRow {
                        id: category.id.clone().into(),
                        label: category.name.clone().into(),
                        checked: entry.is_some_and(|entry| entry.categories.contains(&category.id)),
                    })
                    .collect::<Vec<_>>(),
            )));
        }
        publish_presentation_choice(&ui, &key);
    });
    wire!(on_entry_action, |app, ui, key, action| app
        .entry_action(&ui, &key, &action));
    wire!(on_catalog_enabled_changed, |app, ui, id, enabled| {
        let package = { app.view().settings_package.clone() };
        if let Some(package) = package {
            let result = crate::sync_addons::local_change(&app.services, || {
                crate::source_catalogs::set_enabled(&package, &id, enabled)
            });
            let saved = result.is_ok();
            match result {
                Ok(()) => {
                    if !enabled && !hide_choice(&mut app.view(), id.as_str()) {
                        // The catalogue being browsed was just hidden. Back from
                        // settings must land on Browse, not on another source.
                        ui.set_source_catalog_open(false);
                    }
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
            match crate::sync_addons::local_change(&app.services, || {
                save_setting(&app.services.sources, &package, &key, &value)
            }) {
                Ok(true) => {
                    ui.set_status(text::tr("Source settings saved.").into());
                    // Clears the typed secret and refreshes its set/not-set hint.
                    republish_settings(&app, &ui, &package);
                }
                // An empty secret submit changes nothing.
                Ok(false) => {}
                Err(e) => {
                    ui.set_status(error(e).into());
                    // A toggled switch keeps its new state in the UI even when
                    // the save failed; show what is actually stored.
                    republish_settings(&app, &ui, &package);
                }
            }
        }
    });
    wire!(on_add_category, |app, ui, name| {
        if !name.trim().is_empty() {
            let mut duplicate = false;
            if let Err(e) = app.services.domain.update(|state| {
                if category_exists(&state.categories, &name) {
                    duplicate = true;
                    return Ok(());
                }
                state.categories.push(reader_domain::Category {
                    id: crate::categories::new_id().map_err(reader_domain::Error::Invalid)?,
                    name: name.trim().into(),
                });
                Ok(())
            }) {
                ui.set_status(error(e).into());
            } else if duplicate {
                ui.set_status(text::tr("A category with that name already exists.").into());
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
        // Pin / Unpin from the catalogue's ⋯ menu.
        if action == "pin" {
            if let Some(choice) = choice {
                app.unpin(&ui, &choice.id);
            }
            return;
        }
        if let Some(choice) = choice {
            app.extension_action(&ui, &choice.package, &action);
        }
    });
    wire!(on_extension_action, |app, ui, package, action| app
        .extension_action(&ui, &package, &action));
    // Its own worker and busy flag: the check must not invalidate or report
    // through other work, and navigating away must not drop its result.
    wire!(on_check_update, |_app, ui| {
        if ui.get_update_checking() {
            return;
        }
        ui.set_update_checking(true);
        ui.set_status("".into());
        let weak = ui.as_weak();
        crate::profiles::spawn(move || {
            let message = match crate::updates::check() {
                Ok(message) | Err(message) => message,
            };
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_update_checking(false);
                ui.set_status(message.into());
            });
        });
    });
    {
        let app = app.clone();
        ui.on_flush_progress(move || app.flush());
    }
    downloads::wire(&app, &ui);
    let timer = slint::Timer::default();
    let checkpoint = app.clone();
    let refresh_ui = ui.as_weak();
    let mut inventory_check = std::time::Instant::now();
    let sync_timer = sync.clone();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(600),
        move || {
            checkpoint.checkpoint_progress();
            if let Some(ui) = refresh_ui.upgrade() {
                sync_timer.tick(&ui);
                let download_surface = matches!(checkpoint.view().section, 5 | 7);
                if download_surface {
                    checkpoint.refresh_download_states(&ui);
                }
            }
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
    if app.profile_selection_required(&ui) {
        app.navigate(&ui, 24);
    }
    // Recovery verifies persisted payloads; keep that potentially large scan
    // off the event loop as well as the lightweight list-status refresh.
    let startup = app.clone();
    let startup_ui = ui.as_weak();
    crate::profiles::spawn(move || {
        let queued = startup.services.downloads.jobs().map(|jobs| {
            jobs.into_iter()
                .filter(|job| job.status == reader_downloads::JobStatus::Queued)
                .map(|job| job.chapter)
                .collect::<Vec<_>>()
        });
        if let Ok(queued) = queued {
            let _ = startup_ui.upgrade_in_event_loop(move |ui| {
                startup.download_many(&ui, queued);
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
    sync.shutdown();
    nova_sync::set_persistence_barrier(Arc::new(|| Ok(())));
    app.invalidate();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manual_read_flags_preserve_actual_history_and_resume_without_creating_visits() {
        let series = reader_domain::SeriesId::new("fixture", "series");
        let unopened = ChapterId::new(series.clone(), "unopened");
        let visited = ChapterId::new(series, "visited");
        let mut state = reader_domain::ReaderState::default();
        toggle_chapter_read(&mut state, unopened.clone(), 100).unwrap();
        assert!(state.history.is_empty());
        assert!(state.progress[0].completed);
        toggle_chapter_read(&mut state, unopened, 101).unwrap();
        assert!(state.history.is_empty());
        assert!(!state.progress[0].completed);
        let anchor = ResumeAnchor::new("source-page-3", 2, 0.4).unwrap();
        state.set_progress(ReadingProgress {
            chapter: visited.clone(),
            anchor,
            mode: ReadingMode::Webtoon,
            last_read_at: 42,
            completed: false,
        });
        let actual_history = state.history.clone();
        for at in [102, 103] {
            toggle_chapter_read(&mut state, visited.clone(), at).unwrap();
            assert_eq!(
                state.history, actual_history,
                "manual flags cannot reorder actual visits"
            );
            assert!(
                state
                    .progress
                    .iter()
                    .find(|p| p.chapter == visited)
                    .unwrap()
                    .anchor
                    .is_cleared(),
                "read state is not a reading position"
            );
        }
    }
    #[test]
    fn series_read_flags_preserve_other_sources_history_and_resume() {
        let series = reader_domain::SeriesId::new("a", "same");
        let other = reader_domain::SeriesId::new("b", "same");
        let chapter = |series: reader_domain::SeriesId, id: &str| Chapter {
            id: ChapterId::new(series, id),
            title: id.into(),
            label: String::new(),
            number: None,
            volume: None,
            source_order: 0,
            language: None,
            scanlator: None,
            uploaded_at: None,
        };
        let mut state = reader_domain::ReaderState {
            chapters: vec![
                chapter(series.clone(), "one"),
                chapter(series.clone(), "two"),
                chapter(other.clone(), "one"),
            ],
            ..Default::default()
        };
        let anchor = ResumeAnchor::new("actual-page", 3, 0.4).unwrap();
        for source in [&series, &other] {
            state.set_progress(ReadingProgress {
                chapter: ChapterId::new(source.clone(), "one"),
                anchor: anchor.clone(),
                mode: ReadingMode::RightToLeft,
                last_read_at: 10,
                completed: false,
            });
        }
        let history = state.history.clone();
        assert_eq!(
            set_series_read(&mut state, &series, true, 20)
                .unwrap()
                .len(),
            2
        );
        for completed in [true, false] {
            set_series_read(&mut state, &series, completed, 21).unwrap();
            assert_eq!(state.history, history);
            for progress in &state.progress {
                assert_eq!(
                    progress.completed,
                    progress.chapter.series == series && completed
                );
                if progress.chapter.local_id == "one" {
                    if progress.chapter.series == series {
                        assert!(
                            progress.anchor.is_cleared(),
                            "mark-all drops the saved page"
                        );
                    } else {
                        assert_eq!(progress.anchor, anchor, "other sources keep their page");
                    }
                    assert_eq!(progress.mode, ReadingMode::RightToLeft);
                }
            }
        }
    }

    #[test]
    fn clearing_series_positions_preserves_read_state_history_and_other_sources() {
        let series = reader_domain::SeriesId::new("a", "same");
        let other = reader_domain::SeriesId::new("b", "same");
        let mut state = reader_domain::ReaderState::default();
        for (source, chapter, page, completed) in [
            (&series, "one", 4, true),
            (&series, "two", 2, false),
            (&other, "one", 7, false),
        ] {
            state.set_progress(ReadingProgress {
                chapter: ChapterId::new(source.clone(), chapter),
                anchor: ResumeAnchor::new("page", page, 0.25).unwrap(),
                mode: ReadingMode::Webtoon,
                last_read_at: 10,
                completed,
            });
        }
        let history = state.history.clone();
        assert_eq!(reset_series_positions(&mut state, &series, 20).unwrap(), 2);
        for progress in &state.progress {
            if progress.chapter.series == series {
                assert!(progress.anchor.is_cleared());
                assert_eq!(progress.mode, ReadingMode::Webtoon);
            } else {
                assert_eq!(
                    progress.anchor.page_index, 7,
                    "equally named titles from another source stay untouched"
                );
            }
        }
        assert_eq!(
            state
                .progress
                .iter()
                .map(|p| (p.chapter.local_id.as_str(), p.completed))
                .collect::<Vec<_>>(),
            vec![("one", true), ("two", false), ("one", false)]
        );
        assert_eq!(state.history, history);
        // Already-cleared anchors are a no-op rather than a redundant write.
        assert_eq!(reset_series_positions(&mut state, &series, 21).unwrap(), 0);
    }

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
    fn transient_image_failures_back_off_and_permanent_ones_do_not() {
        use std::time::Duration;
        for transient in [
            "network request failed: HTTP 500",
            "network request failed: HTTP 503",
            "network request failed: HTTP 408",
            "source rate limited the request",
            "network request failed: error sending request: operation timed out",
            "network request failed: connection reset by peer",
        ] {
            assert!(transient_image_error(transient), "{transient}");
        }
        for permanent in [
            "source resource not found",
            "authentication required",
            "interactive challenge required",
            "source is disabled or unavailable",
            "image exceeds byte limit",
            "decoded image exceeds memory limits",
            "could not decode image",
            "operation cancelled",
            "unsupported image format",
        ] {
            assert!(!transient_image_error(permanent), "{permanent}");
            assert_eq!(image_retry_delay(permanent, 0), None, "{permanent}");
        }
        // Exponential-ish backoff, then the page stays failed for a manual
        // seek/retry rather than hammering the source forever.
        assert_eq!(
            image_retry_delay("network request failed: HTTP 502", 0),
            Some(Duration::from_millis(800))
        );
        assert_eq!(
            image_retry_delay("network request failed: HTTP 502", 3),
            Some(Duration::from_millis(15_000))
        );
        assert_eq!(
            image_retry_delay("network request failed: HTTP 502", 4),
            None
        );
        // Throttling waits much longer than a one-off 5xx.
        assert_eq!(
            image_retry_delay("source rate limited the request", 0),
            Some(Duration::from_millis(4_000))
        );
        // The download queue shares the classifier but has its own budget.
        assert_eq!(
            download_retry_delay("network request failed: HTTP 500", 0),
            Some(Duration::from_millis(2_000))
        );
        assert_eq!(
            download_retry_delay("network request failed: HTTP 500", 2),
            Some(Duration::from_millis(15_000))
        );
        assert_eq!(
            download_retry_delay("network request failed: HTTP 500", 3),
            None
        );
        assert_eq!(download_retry_delay("source resource not found", 0), None);
    }

    #[test]
    fn completed_and_cleared_chapters_have_no_page_marker_or_resume() {
        let chapter = ChapterId::new(reader_domain::SeriesId::new("s", "x"), "c");
        let mut progress = ReadingProgress {
            chapter,
            anchor: ResumeAnchor::new("page-7", 6, 0.5).unwrap(),
            mode: ReadingMode::Webtoon,
            last_read_at: 1,
            completed: false,
        };
        assert_eq!(
            chapter_progress_label(&progress, false).as_deref(),
            Some("Page: 7")
        );
        progress.completed = true;
        assert_eq!(chapter_progress_label(&progress, false), None);
        progress.completed = false;
        progress.anchor = ResumeAnchor::cleared();
        assert_eq!(chapter_progress_label(&progress, false), None);
        assert_eq!(chapter_progress_label(&progress, true), None);

        let mut reader = tall_reader();
        reader.completed = true;
        let saved = reader.progress_record();
        assert!(saved.completed);
        assert!(saved.anchor.is_cleared());
        reader.completed = false;
        assert!(!reader.progress_record().anchor.is_cleared());
    }

    #[test]
    fn retry_requeues_only_the_failed_page_in_the_same_generation() {
        // nova-storage allows one application directory per process; isolate
        // this Controller fixture like the other storage-backed unit tests.
        const CHILD: &str = "READER_IMAGE_RETRY_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::tests::retry_requeues_only_the_failed_page_in_the_same_generation",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let mut reader = tall_reader();
        reader.failed = BTreeSet::from([0]);
        reader.requested = BTreeSet::from([0, 1]);
        reader.budget_blocked = BTreeSet::from([0]);
        ui.set_section(6);
        let (images, _receiver) = mpsc::channel();
        let dir = tempfile::tempdir().unwrap();
        let services = Services::open(&dir.path().join("data")).unwrap();
        let app = Arc::new(Controller {
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
                dir.path().join("cache"),
            )),
            capture_originals: Mutex::default(),
            thumbnail_service: OnceLock::new(),
            image_worker_live: AtomicBool::new(false),
            view: Mutex::new(View {
                section: 6,
                reader: Some(reader),
                ..View::default()
            }),
            generation: AtomicU64::new(7),
            cancel: Mutex::default(),
            images,
            download_serial: Mutex::default(),
            active_image: Mutex::default(),
            continuation_work: Default::default(),
            metadata_refresh: Arc::default(),
        });
        // A stale generation never retries the old failure.
        app.retry_failed_image(&ui, 0, 6);
        assert!(app.view().reader.as_ref().unwrap().failed.contains(&0));
        app.retry_failed_image(&ui, 0, 7);
        let reader = app.view();
        let reader = reader.reader.as_ref().unwrap();
        assert!(!reader.failed.contains(&0), "retry clears the hard failure");
        assert!(
            reader.requested.contains(&0),
            "the cleared page is queued again for this generation"
        );
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
            capture_originals: Mutex::default(),
            thumbnail_service: OnceLock::new(),
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
            continuation_work: Default::default(),
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
                include_bytes!("../crates/reader-sources/tests/fixtures/neutral-icon.png").to_vec(),
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
                Ok(PageImage::from_encoded(vec![1]))
            })
            .unwrap();
        let again = cache
            .read_scoped(1, 0, "scope-a", || {
                panic!("same authorized context should reuse")
            })
            .unwrap();
        assert!(first.ptr_eq(&again));
        let changed = cache
            .read_scoped(1, 0, "scope-b", || {
                calls += 1;
                Ok(PageImage::from_encoded(vec![2]))
            })
            .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(changed.stored_bytes(), [2]);
        assert!(!first.ptr_eq(&changed));
    }
    pub(super) fn tall_reader() -> Reader {
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
            image_retries: BTreeMap::new(),
            image_retry_due: BTreeMap::new(),
            budget_blocked: BTreeSet::new(),
            desired_anchor: usize::MAX,
            reverse_demand: None,
            window_demand: None,
            chapter_navigation: None,
            decoded_bytes: BTreeMap::new(),
            decoded_widths: BTreeMap::new(),
            decoded_windows: BTreeMap::new(),
            decoded_responses: BTreeMap::new(),
            native_dimensions: BTreeMap::new(),
            progressive_job: None,
            presentations: BTreeMap::new(),
            display_options: reader_engine::DisplayOptions::default(),
            gap_pixels: 0.,
            chapter_boundaries: true,
            preload_epoch: 0,
            segments: Vec::new(),
            preloading: None,
            preload_nonce: 0,
            continuity_backwards: false,
            continuity_failures: BTreeMap::new(),
            continuity_error: None,
            bookmarked_chapters: BTreeSet::new(),
            target_width: 1280,
            viewport_width: 390.,
            viewport_height: 844.,
            anchor: ResumeAnchor::new("page-0", 0, 0.).unwrap(),
            mode: ReadingMode::Webtoon,
            mode_choice: ModeChoice::Automatic,
            automatic_mode: ReadingMode::Webtoon,
            mode_switching: false,
            collection: false,
            completed: false,
            completion_eligible: true,
        }
    }
    #[test]
    fn webtoon_scroll_stops_before_unknown_geometry_and_releases_after_loading() {
        let mut reader = tall_reader();
        reader.ratios = vec![2., 1.5];
        assert_eq!(
            reader.loaded_scroll_offset(9000., 400., 600., |_| false),
            0.
        );
        assert_eq!(
            reader.loaded_scroll_offset(9000., 400., 600., |i| i == 0),
            248.
        );
        reader.ratios[1] = 50.;
        assert_eq!(
            reader.loaded_scroll_offset(9000., 400., 600., |i| i == 0),
            248.,
            "a tall incoming page cannot move the admitted anchor"
        );
        assert_eq!(
            reader.loaded_scroll_offset(9000., 400., 600., |_| true),
            9000.
        );
        // Fast reverse scrolling must not enter an unloaded previous page at a
        // guessed fractional offset either; start at its bottom once it loads.
        reader.anchor = ResumeAnchor::new("page-1", 1, 0.).unwrap();
        assert_eq!(
            reader.loaded_scroll_offset(0., 400., 600., |i| i == 1),
            800.
        );
        assert_eq!(reader.loaded_scroll_offset(0., 400., 600., |_| true), 0.);
        // An explicit saved position inside a loading image stays put.
        reader.anchor = ResumeAnchor::new("page-1", 1, 0.5).unwrap();
        let restored = (reader.page_position(1) + reader.ratios[1] * 0.5) * 400.;
        assert_eq!(
            reader.loaded_scroll_offset(90000., 400., 600., |_| false),
            restored
        );
    }

    #[test]
    fn paged_completion_uses_the_final_source_image_and_chapter_local_end() {
        let mut reader = tall_reader();
        reader.completion_eligible = true;
        reader.mode = ReadingMode::LeftToRight;
        reader.anchor = ResumeAnchor::new("page-1", 1, 0.).unwrap();
        let presentation = reader_engine::page_presentation(
            &image::RgbaImage::new(2, 1),
            reader_engine::DisplayOptions {
                split_wide: true,
                ..Default::default()
            },
        )
        .unwrap();
        reader.presentations.insert(1, presentation);
        reader.retained.insert(1);
        reader.decoded_windows.insert(
            1,
            reader_engine::DisplayWindowPlan {
                width: 1,
                full_height: 2,
                first_row: 0,
                rows: 2,
            },
        );
        assert!(
            reader.paged_chapter_finished(),
            "Mihon marks the final source image read on its first split half"
        );
        reader.anchor = ResumeAnchor::new("page-1", 1, 0.5).unwrap();
        assert!(reader.paged_chapter_finished());
        reader.anchor = ResumeAnchor::new("page-0", 0, 0.).unwrap();
        reader.retained.insert(0);
        reader.decoded_windows.insert(
            0,
            reader_engine::DisplayWindowPlan {
                width: 1,
                full_height: 1,
                first_row: 0,
                rows: 1,
            },
        );
        let next = ChapterId::new(reader.chapter.series.clone(), "next");
        reader.segments = vec![
            reader_continuity::ChapterSegment {
                chapter: reader.chapter.clone(),
                title: "First".into(),
                start: 0,
                end: 1,
                source: None,
                completed: false,
            },
            reader_continuity::ChapterSegment {
                chapter: next,
                title: "Next".into(),
                start: 1,
                end: 2,
                source: None,
                completed: false,
            },
        ];
        assert!(
            reader.paged_chapter_finished(),
            "a prefetched chapter cannot postpone completion"
        );
        reader.retained.remove(&0);
        assert!(
            reader.paged_chapter_finished(),
            "Mihon allows a selected loading image to complete a chapter"
        );
    }
    #[test]
    fn webtoon_completion_follows_mihon_visible_image_selection() {
        let mut reader = tall_reader();
        reader.completion_eligible = true;
        reader.viewport_width = 100.;
        reader.viewport_height = 200.;
        reader.ratios = vec![1., 10.];
        reader.anchor = ResumeAnchor::new("page-1", 1, 0.).unwrap();
        assert!(
            !reader.chapter_finished(),
            "a tall image entering at its top is not yet selected by scrolling"
        );
        reader.anchor = ResumeAnchor::new("page-1", 1, 0.01).unwrap();
        assert!(
            reader.chapter_finished(),
            "Mihon selects a tall image once its top passes the viewport top"
        );
        assert!(
            reader.decoded_windows.is_empty(),
            "selection allows loading images, like Mihon"
        );

        let next = ChapterId::new(reader.chapter.series.clone(), "next");
        reader.segments = vec![
            reader_continuity::ChapterSegment {
                chapter: reader.chapter.clone(),
                title: "Current".into(),
                start: 0,
                end: 2,
                source: None,
                completed: false,
            },
            reader_continuity::ChapterSegment {
                chapter: next,
                title: "Next".into(),
                start: 2,
                end: 3,
                source: None,
                completed: false,
            },
        ];
        reader.pages.push(PageInput::Local {
            path: PathBuf::new(),
            id: "next-page".into(),
        });
        reader.ratios.push(10.);
        reader.failed.insert(2);
        assert!(
            reader.chapter_finished(),
            "a retained next chapter cannot postpone completion"
        );
        reader.failed.insert(1);
        assert!(
            !reader.chapter_finished(),
            "an explicitly failed final image cannot finish a chapter"
        );
        reader.failed.remove(&1);
        reader.anchor = ResumeAnchor::new("page-0", 0, 0.).unwrap();
        assert!(
            !reader.chapter_finished(),
            "merely prefetching the final image does not select it"
        );
        reader.ratios[1] = 0.5;
        assert!(
            reader.chapter_finished(),
            "a short final image below the top anchor is selected when its bottom is visible"
        );
        reader.ratios[0] = 10.;
        assert!(
            !reader.chapter_finished(),
            "an offscreen final image does not count"
        );
    }

    #[test]
    fn reader_exit_reconciles_live_completion_before_flushing_pending_progress() {
        // nova-storage has one application directory per process.
        const CHILD: &str = "READER_EXIT_COMPLETION_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::tests::reader_exit_reconciles_live_completion_before_flushing_pending_progress",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let services = Services::open(&dir.path().join("data")).unwrap();
        let chapter = tall_reader().chapter;
        let next = ChapterId::new(chapter.series.clone(), "next");
        services
            .domain
            .update(|state| {
                state.series.push(Series {
                    id: chapter.series.clone(),
                    title: "Completion fixture".into(),
                    publication_status: Default::default(),
                    description: String::new(),
                    authors: vec![],
                    genres: vec![],
                    cover_url: None,
                    canonical_url: None,
                });
                for id in [chapter.clone(), next.clone()] {
                    state.chapters.push(Chapter {
                        id,
                        title: "Chapter".into(),
                        label: "1".into(),
                        number: None,
                        volume: None,
                        source_order: 0,
                        language: None,
                        scanlator: None,
                        uploaded_at: None,
                    });
                }
                Ok(())
            })
            .unwrap();
        let (images, _receiver) = mpsc::channel();
        let app = Arc::new(Controller {
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
                dir.path().join("cache"),
            )),
            capture_originals: Mutex::default(),
            thumbnail_service: OnceLock::new(),
            image_worker_live: AtomicBool::new(false),
            view: Mutex::default(),
            generation: AtomicU64::new(0),
            cancel: Mutex::default(),
            images,
            download_serial: Mutex::default(),
            active_image: Mutex::default(),
            continuation_work: Default::default(),
            metadata_refresh: Arc::default(),
        });
        for case in 0..25 {
            // Clearing History creates a sync tombstone that intentionally
            // rejects same-second checkpoints for that identity. Each case
            // represents a fresh chapter, not reading after a History clear.
            let chapter = ChapterId::new(chapter.series.clone(), format!("chapter-{case}"));
            let next = ChapterId::new(chapter.series.clone(), format!("next-{case}"));
            app.services
                .domain
                .update(|state| {
                    state.progress.clear();
                    state.history.clear();
                    for id in [chapter.clone(), next.clone()] {
                        let mut metadata = state.chapters[0].clone();
                        metadata.id = id;
                        state.chapters.push(metadata);
                    }
                    Ok(())
                })
                .unwrap();
            let mut reader = tall_reader();
            reader.chapter = chapter.clone();
            reader.completion_eligible = case != 14;
            reader.mode = match case {
                1 | 12 | 13 | 16 => ReadingMode::RightToLeft,
                2 | 17 => ReadingMode::Vertical,
                7 | 8 | 11 | 18..=20 | 23 | 24 => ReadingMode::Webtoon,
                _ => ReadingMode::LeftToRight,
            };
            reader.viewport_width = 100.;
            reader.viewport_height = 200.;
            reader.ratios = vec![1., 10.];
            reader.retained.insert(1);
            reader.decoded_windows.insert(
                1,
                reader_engine::DisplayWindowPlan {
                    width: 1,
                    full_height: 100,
                    first_row: 0,
                    rows: 100,
                },
            );
            let previous = reader.progress_record();
            reader.anchor = ResumeAnchor::new("page-1", 1, 0.).unwrap();
            match case {
                4 | 22 => reader.collection = true,
                5 | 21 => {
                    reader.failed.insert(1);
                }
                6 | 15 | 23 => {
                    reader.retained.clear();
                }
                8 => {
                    reader.anchor = ResumeAnchor::new("page-1", 1, 0.8).unwrap();
                    reader.segments = vec![
                        reader_continuity::ChapterSegment {
                            chapter: chapter.clone(),
                            title: "Current".into(),
                            start: 0,
                            end: 2,
                            source: None,
                            completed: false,
                        },
                        reader_continuity::ChapterSegment {
                            chapter: next.clone(),
                            title: "Next".into(),
                            start: 2,
                            end: 3,
                            source: None,
                            completed: false,
                        },
                    ];
                    reader.pages.push(PageInput::Local {
                        path: PathBuf::new(),
                        id: "next-page".into(),
                    });
                    reader.ratios.push(10.);
                    reader.failed.insert(2);
                }
                10 => {
                    reader.completed = true;
                }
                11 => {
                    reader.anchor = ResumeAnchor::new("page-1", 1, 0.8).unwrap();
                    reader.decoded_windows.get_mut(&1).unwrap().rows = 50;
                }
                12 | 13 | 16 => {
                    let presentation = reader_engine::page_presentation(
                        &image::RgbaImage::new(2, 1),
                        reader_engine::DisplayOptions {
                            split_wide: true,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    reader.presentations.insert(1, presentation);
                    if case == 13 {
                        reader.anchor = ResumeAnchor::new("page-1", 1, 0.5).unwrap();
                    }
                }
                19 => {
                    reader.ratios = vec![1., 0.5];
                    reader.anchor = ResumeAnchor::new("page-0", 0, 0.).unwrap();
                }
                20 => {
                    reader.ratios = vec![10., 1.];
                    reader.anchor = ResumeAnchor::new("page-0", 0, 0.).unwrap();
                }
                _ => {}
            }
            let pending = match case {
                3 | 9 => None,
                // Neutral settlement may move the viewport without permission
                // to replace the last actual reading position.
                14 => Some(previous),
                _ => Some(reader.progress_record()),
            };
            let section = if case == 9 { 23 } else { 6 };
            ui.set_section(section);
            {
                let mut view = app.view();
                view.section = section;
                view.reader = Some(reader);
                view.pending = pending;
            }
            if case >= 15 {
                app.mount_pages(&ui);
                if matches!(case, 18..=20) {
                    let rows = ui.get_pages();
                    for index in 0..rows.row_count() {
                        let mut row = rows.row_data(index).unwrap();
                        row.loaded = true;
                        rows.set_row_data(index, row);
                    }
                    // Actual scroll callback: tall image early, short final
                    // below the top anchor, or a merely prefetched final image.
                    app.position(&ui, if case == 18 { 110. } else { 0. }, 100., 200.);
                } else if case == 24 {
                    // Opening/resuming explicitly selects the requested image.
                    app.step_page(&ui, 0);
                } else {
                    app.reader_seek(&ui, if case == 16 { 0.1 } else { 1. });
                }
                let view = app.view();
                let expected = matches!(case, 15..=19 | 23 | 24);
                assert_eq!(
                    view.reader.as_ref().unwrap().completed,
                    expected,
                    "case {case}: update on selection, before exit"
                );
                assert_eq!(
                    view.pending.as_ref().unwrap().completed,
                    expected,
                    "case {case}: current pending progress"
                );
            }
            // This is the same durable exit path used by Back.
            app.navigate(&ui, 4);
            assert!(app.view().reader.is_none());
            let state = app.services.domain.snapshot().unwrap();
            if case == 9 {
                assert!(state.progress.is_empty(), "Pages must not infer completion");
                assert!(state.history.is_empty(), "Pages must not create history");
                continue;
            }
            assert_eq!(state.progress.len(), 1, "case {case}");
            let saved = &state.progress[0];
            let completed = matches!(case, 0..=3 | 6 | 8 | 10..=13 | 15..=19 | 23 | 24);
            assert_eq!(saved.chapter, chapter, "case {case}: canonical owner");
            assert_eq!(saved.completed, completed, "case {case}");
            assert_eq!(saved.anchor.is_cleared(), completed, "case {case}");
            assert_eq!(state.history.len(), 1, "case {case}");
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
                    reader.viewport_width = if mode == ReadingMode::Webtoon {
                        (390_f32 * zoom).min(1100.)
                    } else {
                        390_f32 * zoom
                    };
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
                        assert!(width <= reader.target_width.min(dimensions.width));
                        let desired = reader.target_width.min(dimensions.width);
                        if reader_engine::plan_display_window(
                            dimensions, desired, start, end, budget,
                        )
                        .is_ok()
                        {
                            assert_eq!(width, desired);
                        }
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
    fn paged_initial_detail_keeps_webtoon_columns_when_height_limited_and_caps_memory() {
        let mut reader = tall_reader();
        for mode in [
            ReadingMode::LeftToRight,
            ReadingMode::RightToLeft,
            ReadingMode::Vertical,
        ] {
            reader.mode = mode;
            for (logical_width, height, density, dimensions) in [
                (
                    390.,
                    844.,
                    2.75,
                    reader_engine::PageDimensions {
                        width: 1920,
                        height: 5760,
                    },
                ),
                (
                    1100.,
                    780.,
                    1.,
                    reader_engine::PageDimensions {
                        width: 1920,
                        height: 2880,
                    },
                ),
            ] {
                reader.viewport_width = logical_width;
                reader.viewport_height = height;
                reader.target_width = Reader::decode_width(logical_width, density);
                reader.ratios[0] = dimensions.height as f32 / dimensions.width as f32;
                let (width, (start, end), budget) = reader.window_request(0, dimensions).unwrap();
                assert_eq!(width, reader.target_width.min(dimensions.width));
                // Chapter metadata can queue its first image before the
                // conditional Flickable publishes a nonzero height. That
                // startup ordering must not install a final 128px texture.
                reader.viewport_height = 0.;
                assert_eq!(reader.window_request(0, dimensions).unwrap().0, width);
                reader.viewport_height = height;
                let fitted_width = height / (dimensions.height as f32 / dimensions.width as f32);
                assert!(
                    fitted_width < logical_width,
                    "fixture must exercise the previously reduced paged target"
                );
                assert!(width > Reader::decode_width(fitted_width, density));
                let plan =
                    reader_engine::plan_display_window(dimensions, width, start, end, budget)
                        .unwrap();
                assert!(plan.byte_len() <= READER_MODEL_BYTES);
                reader.native_dimensions.insert(0, dimensions);
                assert_eq!(
                    reader.quality_width(0),
                    width,
                    "retained detail must not retry the same bounded target"
                );
            }
            let dimensions = reader_engine::PageDimensions {
                width: 1920,
                height: 5760,
            };
            reader.target_width = 4096;
            let (width, (start, end), budget) = reader.window_request(0, dimensions).unwrap();
            assert!(width < 1920);
            assert!(
                reader_engine::plan_display_window(dimensions, width, start, end, budget).is_ok()
            );
            assert!(
                reader_engine::plan_display_window(dimensions, width + 1, start, end, budget)
                    .is_err()
            );
        }
    }

    #[test]
    fn paged_neighbor_previews_cover_every_row_within_the_existing_model_budget() {
        let dimensions = reader_engine::PageDimensions {
            width: 720,
            height: 20000,
        };
        let mut reader = tall_reader();
        for mode in [
            ReadingMode::LeftToRight,
            ReadingMode::RightToLeft,
            ReadingMode::Vertical,
        ] {
            reader.mode = mode;
            let (width, (start, end), budget) = reader.window_request(1, dimensions).unwrap();
            assert_eq!((start, end), (0., 1.));
            let plan =
                reader_engine::plan_display_window(dimensions, width, start, end, budget).unwrap();
            assert_eq!(plan.first_row, 0);
            assert_eq!(
                plan.rows, plan.full_height,
                "a loaded neighbor cannot hide an uncaptured lower strip"
            );
            assert!(plan.byte_len() <= READER_MODEL_BYTES);
            reader.native_dimensions.insert(1, dimensions);
            assert_eq!(reader.quality_width(1), width);
            reader.decoded_windows.insert(1, plan);
            assert!(!reader.needs_window(1));
        }
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
                    Ok(PageImage::from_encoded(vec![7; 1000]))
                })
                .unwrap();
            assert_eq!(bytes.stored_bytes()[0], 7);
        }
        assert_eq!(requests, 1, "window movement must not refetch source bytes");
        cache
            .read(1, 1, || {
                requests += 1;
                Ok(PageImage::from_encoded(vec![9; 1000]))
            })
            .unwrap();
        cache
            .read(1, 0, || {
                requests += 1;
                Ok(PageImage::from_encoded(vec![7; 1000]))
            })
            .unwrap();
        assert_eq!(requests, 2);
        cache
            .read(2, 0, || {
                requests += 1;
                Ok(PageImage::from_encoded(vec![8; 1000]))
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
            let prepared = prepare_display_tiles_from_pixels(
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
                    entering
                        <= (reader_engine::DISPLAY_TILE_ROWS / reader_engine::DISPLAY_UPLOAD_ROWS)
                            as usize,
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
        let source = PageImage::from_encoded(vec![1]);
        let identity = source.downgrade();
        assert!(same_response(Some(&identity), &identity));
        let replacement = PageImage::from_encoded(vec![1]);
        assert!(
            !same_response(Some(&identity), &replacement.downgrade()),
            "same bytes in a new response must not reuse old tiles"
        );
        let plan = previous.unwrap();
        let prepared = prepare_display_tiles_from_pixels(
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
    fn upload_rects_partition_windows_on_a_grid_shared_by_overlapping_windows() {
        let plan = |first_row, rows| reader_engine::DisplayWindowPlan {
            width: 1080,
            full_height: 40_000,
            first_row,
            rows,
        };
        let upload = reader_engine::DISPLAY_UPLOAD_ROWS;
        let mut seen = BTreeMap::<u32, TileRect>::new();
        // Aligned strips, an unaligned tight-budget window and the page tail.
        for (first, rows) in [
            (0, 6144),
            (2048, 6144),
            (4096, 6144),
            (1000, 1602),
            (37_952, 2048),
            (39_999, 1),
        ] {
            let window = plan(first, rows);
            let rects = tile_rects(window);
            let mut next = first;
            for rect in &rects {
                assert_eq!(rect.top, next, "rects must be contiguous");
                assert!(rect.height >= 1 && rect.height <= upload);
                assert_eq!((rect.left, rect.width), (0, 1080));
                // Only the first/last rect of an unaligned window may be partial.
                if rect.top % upload == 0 && rect.top + upload <= first + rows {
                    assert_eq!(rect.height, upload);
                }
                assert!(
                    (rect.top / upload + 1) * upload >= rect.top + rect.height,
                    "no rect crosses the upload grid"
                );
                if (first % reader_engine::DISPLAY_TILE_ROWS == 0)
                    && (rows % reader_engine::DISPLAY_TILE_ROWS == 0)
                {
                    // Aligned windows share byte-identical rects where they overlap.
                    if let Some(old) = seen.insert(rect.top, *rect) {
                        assert_eq!(old, *rect);
                    }
                }
                next += rect.height;
            }
            assert_eq!(next, first + rows, "rects must cover the window exactly");
            // The largest single upload shrank from a 2,048-row strip.
            assert!(
                rects
                    .iter()
                    .all(|rect| rect.width * rect.height * 4 <= 1080 * upload * 4)
            );
        }
        // An aligned one-strip shift enters exactly one strip's worth of rects.
        let (before, after) = (tile_rects(plan(0, 6144)), tile_rects(plan(2048, 6144)));
        let entering = after.iter().filter(|rect| !before.contains(rect)).count();
        assert_eq!(entering as u32, reader_engine::DISPLAY_TILE_ROWS / upload);
    }

    #[test]
    fn horizontal_tiles_preserve_source_pixels_and_partial_row_coordinates() {
        let image = image::RgbaImage::from_fn(5000, 8, |x, y| {
            image::Rgba([(x % 251) as u8, (x / 251) as u8, y as u8, 255])
        });
        let tiles = display_window_tiles(image.as_raw(), 5000, 8, 5000, 20000);
        assert_eq!(tiles.len(), 3);
        for (index, tile) in tiles.iter().enumerate() {
            assert!(
                tile.image.size().width <= 2048
                    && tile.image.size().height <= reader_engine::DISPLAY_UPLOAD_ROWS
            );
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
            resume_action_label(&chapters, &progress, false),
            "Start chapter 0"
        );
        assert_eq!(resume_action_label(&[], &progress, false), "Start reading");
        assert_eq!(
            resume_chapter(&chapters, &progress).unwrap().id,
            chapters[0].id
        );
        for (i, completed, last_read_at) in [(0, true, 20), (1, false, 10), (2, false, 30)] {
            progress.push(ReadingProgress {
                chapter: chapters[i].id.clone(),
                // A real saved page zero is still a valid Continue position.
                anchor: ResumeAnchor::new(format!("page-{i}"), 0, 0.).unwrap(),
                mode: ReadingMode::Webtoon,
                last_read_at,
                completed,
            });
        }
        let target = resume_chapter(&chapters, &progress).unwrap();
        assert_eq!(target.id, chapters[2].id);
        assert_eq!(resume_label(target), "106.1");
        assert_eq!(
            resume_action_label(&chapters, &progress, false),
            "Continue chapter 106.1"
        );
        // Manual unread/reset can be newer than real reading without creating
        // a saved position or stealing the Continue action.
        progress[1].anchor = ResumeAnchor::cleared();
        progress[1].last_read_at = 999;
        assert_eq!(
            resume_chapter(&chapters, &progress).unwrap().id,
            chapters[2].id
        );
        assert_eq!(
            resume_action_label(&chapters, &progress, false),
            "Continue chapter 106.1"
        );
        progress[2].completed = true;
        progress[2].anchor = ResumeAnchor::cleared();
        assert_eq!(
            resume_chapter(&chapters, &progress).unwrap().id,
            chapters[1].id
        );
        assert_eq!(
            resume_action_label(&chapters, &progress, false),
            "Continue chapter 1"
        );
        progress[1].completed = true;
        assert_eq!(
            resume_chapter(&chapters, &progress).unwrap().id,
            chapters[0].id
        );
        assert_eq!(
            resume_action_label(&chapters, &progress, false),
            "Read again"
        );
        for record in &mut progress {
            record.completed = false;
            record.anchor = ResumeAnchor::cleared();
        }
        assert_eq!(
            resume_action_label(&chapters, &progress, false),
            "Start chapter 0"
        );
        progress[0].anchor = ResumeAnchor::default();
        assert_eq!(
            resume_action_label(&chapters, &progress, false),
            "Start chapter 0"
        );
        assert_eq!(
            resume_action_label(&chapters[..1], &progress, true),
            "Continue"
        );
        assert_eq!(
            resume_action_label(&chapters, &progress, true),
            "Continue chapter 0"
        );
    }

    #[test]
    fn start_reading_opens_chapter_one_on_newest_first_sources() {
        let series = reader_domain::SeriesId::new("fixture", "series");
        // Newest-first source: chapter 3 is listed first.
        let chapters = [("3", 0), ("2", 1), ("1", 2)].map(|(number, source_order)| Chapter {
            id: ChapterId::new(series.clone(), format!("chapter-{number}")),
            title: format!("Chapter {number}"),
            label: String::new(),
            number: Some(number.into()),
            volume: None,
            source_order,
            language: None,
            scanlator: None,
            uploaded_at: None,
        });
        let target = resume_chapter(&chapters, &[]).unwrap();
        assert_eq!(target.number.as_deref(), Some("1"));
    }

    fn plain_series(source: &str, id: &str) -> Series {
        Series {
            publication_status: Default::default(),
            id: reader_domain::SeriesId::new(source, id),
            title: id.into(),
            description: String::new(),
            authors: vec![],
            genres: vec![],
            cover_url: None,
            canonical_url: None,
        }
    }
    fn library_entry(series: &Series) -> reader_domain::LibraryEntry {
        reader_domain::LibraryEntry {
            reading_status: Default::default(),
            series: series.id.clone(),
            categories: BTreeSet::new(),
            preferences: None,
            bookmarked: false,
            added_at: 1,
        }
    }
    #[test]
    fn refresh_skips_local_imports_and_disabled_addons() {
        let local = plain_series("local", "imported");
        let remote = plain_series("fixture.en", "remote");
        let off = plain_series("off.en", "hidden");
        let state = reader_domain::ReaderState {
            library: [&local, &remote, &off].map(library_entry).to_vec(),
            series: vec![local.clone(), remote.clone(), off.clone()],
            ..Default::default()
        };
        // A library holding only an imported entry has nothing to refresh, so
        // no failure can be reported for it.
        let only_local = reader_domain::ReaderState {
            library: vec![library_entry(&local)],
            series: vec![local],
            ..Default::default()
        };
        assert!(refreshable_series(&only_local, &[]).is_empty());
        let refreshed = refreshable_series(&state, &["off".into()]);
        assert_eq!(refreshed.len(), 1);
        assert_eq!(refreshed[0].id, remote.id);
    }
    #[test]
    fn chapter_counts_are_computed_per_series_in_one_pass() {
        let a = reader_domain::SeriesId::new("s", "a");
        let b = reader_domain::SeriesId::new("s", "b");
        let chapter = |series: &reader_domain::SeriesId, id: &str| Chapter {
            id: ChapterId::new(series.clone(), id),
            title: id.into(),
            label: String::new(),
            number: None,
            volume: None,
            source_order: 0,
            language: None,
            scanlator: None,
            uploaded_at: None,
        };
        let chapters = [chapter(&a, "1"), chapter(&a, "2"), chapter(&b, "1")];
        let progress = [
            ReadingProgress {
                chapter: chapters[0].id.clone(),
                anchor: ResumeAnchor::cleared(),
                mode: ReadingMode::Webtoon,
                last_read_at: 1,
                completed: true,
            },
            ReadingProgress {
                chapter: chapters[2].id.clone(),
                anchor: ResumeAnchor::cleared(),
                mode: ReadingMode::Webtoon,
                last_read_at: 1,
                completed: false,
            },
        ];
        let counts = series_chapter_counts(&chapters, &progress);
        assert_eq!(counts[&a], (2, 1));
        assert_eq!(counts[&b], (1, 1));
        assert!(!counts.contains_key(&reader_domain::SeriesId::new("s", "none")));
    }
    #[test]
    fn pagination_uses_applied_filters_not_the_unapplied_draft() {
        let mut view = View {
            source_filters: crate::source_filters::FilterSet::parse(json!({"filters":[{
                "id":"genre", "type":"select", "options":[
                    {"label":"All", "value":""}, {"label":"Adventure", "value":"adventure"},
                    {"label":"Drama", "value":"drama"}
                ]
            }]}))
            .unwrap(),
            filters_active: true,
            ..Default::default()
        };
        let key = view.source_filters.rows()[0].key.to_string();
        assert!(view.source_filters.select(&key, 1));
        apply_source_filters(&mut view);
        // Editing the open panel afterwards must not change the next page.
        assert!(view.source_filters.select(&key, 2));
        assert_eq!(
            applied_browse_filters(&view),
            json!({"genre":"adventure"}),
            "next page keeps the applied filters"
        );
        view.filters_active = false;
        assert_eq!(applied_browse_filters(&view), json!({}));
    }
    fn choice(id: &str, package: &str) -> Choice {
        Choice {
            package: package.into(),
            id: id.into(),
            name: id.into(),
            enabled: true,
            filters: false,
            language: "en".into(),
            latest: false,
            ..Default::default()
        }
    }
    #[test]
    fn hiding_a_catalogue_keeps_the_browsed_source_selected() {
        let mut view = View {
            choices: vec![choice("a.one", "a"), choice("a.two", "a"), choice("b", "b")],
            selected: 2,
            ..Default::default()
        };
        assert!(hide_choice(&mut view, "a.one"));
        assert_eq!(view.choices[view.selected].id, "b");
        // Hiding the browsed catalogue itself reports that it is gone.
        assert!(!hide_choice(&mut view, "b"));
        assert_eq!(view.selected, 0);
        assert!(view.saved_catalogue.is_none());
    }
    #[test]
    fn one_malformed_package_does_not_hide_the_other_sources() {
        let good = package_choices(
            "good",
            "Good",
            "en",
            true,
            &[json!({"id":"good.en","language":"en","operations":["browse"]})],
        )
        .unwrap();
        assert_eq!(good[0].id, "good.en");
        assert!(good[0].latest);
        assert!(
            package_choices("evil", "Evil", "en", true, &[json!({"id":"good.en"})]).is_err(),
            "an addon cannot claim another addon's identity"
        );
        let disabled = package_choices("off", "Off", "en", false, &[]).unwrap();
        assert_eq!(disabled[0].name, "Off (disabled)");
    }
    #[test]
    fn empty_secret_submit_keeps_the_stored_secret() {
        let directory = tempfile::tempdir().unwrap();
        let sources = reader_sources::SourceRegistry::open(directory.path()).unwrap();
        let manifest = json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"fixture","name":"Fixture","version":"1.0.0","language":"en","entry":"main.js","permissions":{"domains":[]},"configuration":[{"key":"token","name":"Token","kind":"secret","default":""},{"key":"label","name":"Label","kind":"text","default":"x"}],"capabilities":["browse"]});
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("main.js", b"globalThis.source = {};".to_vec()),
        ] {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut archive, &bytes).unwrap();
        }
        sources
            .install_zip(&archive.finish().unwrap().into_inner())
            .unwrap();
        assert!(save_setting(&sources, "fixture", "token", "secret-value").unwrap());
        assert_eq!(sources.settings_secrets("fixture").unwrap()["token"], true);
        // Pressing Enter in the untouched, empty secret field changes nothing.
        assert!(!save_setting(&sources, "fixture", "token", "").unwrap());
        assert_eq!(sources.settings_secrets("fixture").unwrap()["token"], true);
        // Public text fields may still be cleared.
        assert!(save_setting(&sources, "fixture", "label", "").unwrap());
    }

    #[test]
    fn clamped_page_steps_do_not_count_as_movement() {
        // Previous on page 0 and Next on the last page clamp in place.
        assert!(!step_moves(0, false, 0, 0.));
        assert!(!step_moves(9, false, 9, 0.));
        assert!(!step_moves(9, true, 9, 0.5));
        assert!(step_moves(0, false, 1, 0.));
        // Moving between the halves of one split page is movement.
        assert!(step_moves(3, false, 3, 0.5));
        assert!(step_moves(3, true, 3, 0.));
    }

    #[test]
    fn batched_page_positions_are_bit_identical_to_page_position() {
        let mut reader = tall_reader();
        let count = 300;
        reader.pages = (0..count)
            .map(|i| PageInput::Local {
                path: PathBuf::new(),
                id: format!("page-{i}"),
            })
            .collect();
        reader.ratios = (0..count)
            .map(|i| 0.37 + (i * 7919 % 113) as f32 / 17.)
            .collect();
        reader.segments = (0..3)
            .map(|n| reader_continuity::ChapterSegment {
                chapter: ChapterId::new(
                    reader_domain::SeriesId::new("fixture", "sharp"),
                    format!("chapter-{n}"),
                ),
                title: format!("Chapter {n}"),
                start: n * 100,
                end: (n + 1) * 100,
                source: None,
                completed: false,
            })
            .collect();
        for (mode, gap, boundaries, width) in [
            (ReadingMode::Webtoon, 0., true, 512.),
            (ReadingMode::Webtoon, 16., true, 387.5),
            (ReadingMode::Webtoon, 16., false, 0.),
            (ReadingMode::Webtoon, 3.3, true, 1.),
            (ReadingMode::LeftToRight, 16., true, 512.),
        ] {
            reader.mode = mode;
            reader.gap_pixels = gap;
            reader.chapter_boundaries = boundaries;
            reader.viewport_width = width;
            let batched = reader.page_positions();
            assert_eq!(batched.len(), count);
            for (i, position) in batched.iter().enumerate() {
                assert_eq!(position.to_bits(), reader.page_position(i).to_bits());
            }
        }
        reader.ratios.clear();
        assert!(reader.page_positions().is_empty());
        // The O(n) pass is what keeps a dimensions update cheap in long chapters.
        reader.ratios = vec![1.5; count];
        reader.mode = ReadingMode::Webtoon;
        reader.gap_pixels = 16.;
        reader.chapter_boundaries = true;
        reader.viewport_width = 512.;
        let started = std::time::Instant::now();
        for _ in 0..200 {
            std::hint::black_box(reader.page_positions_unchecked());
        }
        let batched = started.elapsed().as_secs_f64() * 1e6 / 200.;
        let started = std::time::Instant::now();
        for _ in 0..200 {
            std::hint::black_box(
                (0..count)
                    .map(|i| reader.page_position(i))
                    .collect::<Vec<_>>(),
            );
        }
        let each = started.elapsed().as_secs_f64() * 1e6 / 200.;
        eprintln!("page_positions_us batched={batched:.1} per_page={each:.1}");
    }

    #[test]
    fn direct_tile_rendering_matches_copying_a_full_window_and_skips_mounted_strips() {
        let source = image::RgbaImage::from_fn(300, 9000, |x, y| {
            image::Rgba([(x * 7 + y) as u8, (x + y * 3) as u8, (y / 5) as u8, 255])
        });
        let mut png = std::io::Cursor::new(Vec::new());
        source.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let bytes = Arc::new(png.into_inner());
        let limits = reader_engine::DecodeLimits::default();
        let options = reader_engine::DisplayOptions::default();
        let mut decoder = reader_engine::DisplayDecoder::default();
        let mut previous = None;
        for (width, start) in [(300, 0.1), (212, 0.1), (212, 0.3), (212, 0.5)] {
            let window = decoder
                .decode_window_with_options(
                    &bytes,
                    limits,
                    width,
                    start,
                    start + 0.05,
                    1 << 26,
                    options,
                )
                .unwrap();
            let copied = prepare_display_tiles_from_pixels(
                window.image.as_raw(),
                window.image.width(),
                window.image.height(),
                window.plan.first_row,
                window.plan.full_height,
                previous,
            );
            let mut rendered_rects = 0;
            let (plan, direct) = decoder
                .render_window_tiles(
                    &bytes,
                    limits,
                    width,
                    start,
                    start + 0.05,
                    1 << 26,
                    options,
                    |r| {
                        prepare_display_tiles(r.plan(), previous, |rect, out| {
                            rendered_rects += 1;
                            r.render_rect(rect.top, rect.height, rect.left, rect.width, out)
                        })
                    },
                )
                .unwrap();
            assert_eq!(plan, window.plan);
            assert_eq!(copied.len(), direct.len());
            let entering = direct.iter().filter(|tile| tile.pixels.is_some()).count();
            assert_eq!(
                rendered_rects, entering,
                "mounted strips must not be rendered"
            );
            for (a, b) in copied.iter().zip(&direct) {
                assert_eq!(a.rect, b.rect);
                assert_eq!(
                    a.pixels.as_ref().map(|p| p.as_bytes().to_vec()),
                    b.pixels.as_ref().map(|p| p.as_bytes().to_vec())
                );
            }
            previous = Some(plan);
        }
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
            image_retries: BTreeMap::new(),
            image_retry_due: BTreeMap::new(),
            budget_blocked: BTreeSet::new(),
            desired_anchor: usize::MAX,
            reverse_demand: None,
            window_demand: None,
            chapter_navigation: None,
            decoded_bytes: BTreeMap::new(),
            decoded_widths: BTreeMap::new(),
            decoded_windows: BTreeMap::new(),
            decoded_responses: BTreeMap::new(),
            native_dimensions: BTreeMap::new(),
            progressive_job: None,
            presentations: BTreeMap::new(),
            display_options: reader_engine::DisplayOptions::default(),
            gap_pixels: 0.,
            chapter_boundaries: true,
            preload_epoch: 0,
            segments: Vec::new(),
            preloading: None,
            preload_nonce: 0,
            continuity_backwards: false,
            continuity_failures: BTreeMap::new(),
            continuity_error: None,
            bookmarked_chapters: BTreeSet::new(),
            target_width: 0,
            viewport_width: 500.,
            viewport_height: 500.,
            anchor: ResumeAnchor::new("stable-2", 2, 0.375).unwrap(),
            mode: ReadingMode::Webtoon,
            mode_choice: ModeChoice::Automatic,
            automatic_mode: ReadingMode::Webtoon,
            mode_switching: false,
            collection: false,
            completed: false,
            completion_eligible: false,
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
            capture_originals: Mutex::default(),
            thumbnail_service: OnceLock::new(),
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
            continuation_work: Default::default(),
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
        // Reset deliberately: ordinary scrolling now stops at unloaded images.
        app.reader_seek(&ui, 0.);
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

        // Switching paged -> Webtoon with two chapters loaded must rebuild row
        // positions: only Webtoon has the 96px chapter gap.
        app.choose_reader_mode(&ui, 2, reader_modes::save);
        let boundary = {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            reader.chapter_boundaries = true;
            let next = ChapterId::new(reader.chapter.series.clone(), "next");
            let second = reader_continuity::ChapterSegment {
                chapter: next,
                title: "Next".into(),
                start: 10,
                end: 20,
                source: None,
                completed: false,
            };
            reader.segments = vec![
                reader_continuity::ChapterSegment {
                    end: 10,
                    start: 0,
                    title: "First".into(),
                    chapter: reader.chapter.clone(),
                    source: None,
                    completed: false,
                },
                second,
            ];
            10
        };
        app.update_reader_geometry(&ui);
        app.choose_reader_mode(&ui, 0, reader_modes::save);
        {
            let view = app.view();
            let reader = view.reader.as_ref().unwrap();
            assert_eq!(reader.mode, ReadingMode::Webtoon);
            let rows = ui.get_pages();
            assert_eq!(
                rows.row_data(boundary).unwrap().position,
                reader.page_position(boundary),
                "mode switch refreshes row positions with the chapter gap"
            );
            assert_eq!(ui.get_total_ratio(), reader.total_extent());
        }

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
        assert_eq!(
            tiles.len() as u32,
            12000_u32.div_ceil(reader_engine::DISPLAY_UPLOAD_ROWS)
        );
        assert!(tiles.iter().all(|t| t.image.size().width == 128
            && t.image.size().height <= reader_engine::DISPLAY_UPLOAD_ROWS));
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
                    ..Default::default()
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
        assert_eq!(ui.get_resume_action_label(), "Start reading");
        assert!(ui.get_busy());
        assert!(ui.get_status().is_empty());
        app.invalidate();
    }

    #[test]
    fn zoomed_paged_capture_geometry_matches_scaled_contain_and_local_pan() {
        let viewport = crate::ReaderCaptureViewport {
            x: 12.,
            y: 24.,
            width: 390.,
            height: 844.,
            content_x: -195.,
            content_y: -163.,
        };
        let row = PageRow {
            ratio: 1.5,
            ..Default::default()
        };
        for mode in [1, 2, 3] {
            let rect = chapter_capture_rect_with_split(&viewport, 780., mode, &row, None, 2.);
            assert_eq!(rect.width, 780.);
            assert_eq!(rect.height, 1170.);
            assert_eq!(rect.x, -183.);
            assert_eq!(rect.y, -139.);
        }
        let split = PageRow {
            ratio: 4.,
            ..Default::default()
        };
        let rect = chapter_capture_rect_with_split(&viewport, 780., 1, &split, Some(1), 2.);
        assert_eq!(rect.width, 780.);
        assert_eq!(rect.height, 3120.);
        assert_eq!(rect.y, 24. - 163. - 1560.);
    }

    #[test]
    fn desktop_screencaps_bound_visible_media_without_exterior_gutters() {
        use crate::chapter_capture::Rect;
        let viewport = Rect {
            x: 10.,
            y: 24.,
            width: 1000.,
            height: 600.,
        };
        let strip = Rect {
            x: 150.,
            y: -200.,
            width: 720.,
            height: 2000.,
        };
        let clipped = media_capture_viewport(viewport, [strip].into_iter(), false).unwrap();
        assert_eq!(
            clipped,
            Rect {
                x: 150.,
                y: 24.,
                width: 720.,
                height: 600.
            }
        );
        let page = Rect {
            x: 210.,
            y: 124.,
            width: 600.,
            height: 400.,
        };
        assert_eq!(
            media_capture_viewport(viewport, [page].into_iter(), false).unwrap(),
            page
        );
        let zoomed = Rect {
            x: -100.,
            y: -50.,
            width: 1500.,
            height: 2000.,
        };
        assert_eq!(
            media_capture_viewport(viewport, [zoomed].into_iter(), false).unwrap(),
            viewport
        );
        // Keep internal page gaps, but exclude offscreen/unloaded geometry.
        let pages = [
            Rect {
                y: -100.,
                height: 300.,
                ..strip
            },
            Rect {
                y: 216.,
                height: 100.,
                ..strip
            },
        ];
        assert_eq!(
            media_capture_viewport(viewport, pages.into_iter(), false).unwrap(),
            Rect {
                x: 150.,
                y: 24.,
                width: 720.,
                height: 292.
            }
        );
        assert_eq!(
            media_capture_viewport(viewport, [page].into_iter(), true).unwrap(),
            viewport,
            "Android retains its existing framing"
        );
        assert!(media_capture_viewport(viewport, std::iter::empty(), false).is_err());
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
                (-100., -476., 400., 800.)
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
