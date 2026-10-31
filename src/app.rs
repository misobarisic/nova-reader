use crate::{
    AppWindow, Appearance, ExtensionRow, ItemRow, PageRow, SettingRow, TileRow,
    services::{Result, Services, error, now},
};
use nova_reader_ui::backend_text as text;
use reader_domain::{
    Bookmark, Chapter, ChapterId, ReadingMode, ReadingProgress, ResumeAnchor, Series,
};
use reader_sources::{CancelToken, Source};
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
    target_width: u32,
    viewport_width: f32,
    viewport_height: f32,
    anchor: ResumeAnchor,
    mode: ReadingMode,
    completed: bool,
}
impl Reader {
    fn decode_width(width: f32, scale_factor: f32) -> u32 {
        // Small layout changes share a decode, while high-density screens get
        // physical-pixel detail within a bounded display allocation.
        let physical = (width * scale_factor).clamp(128., 2048.);
        ((physical / 128.).ceil() as u32 * 128).clamp(128, 2048)
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
    pending: Option<ReadingProgress>,
    category: usize,
    category_target: usize,
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
struct Controller {
    services: Services,
    artwork: crate::artwork::ArtworkLoader,
    view: Mutex<View>,
    generation: AtomicU64,
    cancel: Mutex<CancelToken>,
    images: mpsc::Sender<ImageTask>,
    download_serial: Mutex<()>,
    active_image: Mutex<Option<(usize, CancelToken)>>,
}
impl Controller {
    fn view(&self) -> std::sync::MutexGuard<'_, View> {
        self.view.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn invalidate(&self) -> (u64, CancelToken) {
        self.artwork.invalidate();
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
    fn flush(&self) {
        if let Some(progress) = self.view().pending.take()
            && let Err(error) = self.services.domain.update(|state| {
                state.set_progress(progress);
                Ok(())
            })
        {
            eprintln!("Nova Reader: could not save reading position: {error}");
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
        Ok(choices)
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
        ui.set_section(view.section);
        if view.section == 6 {
            ui.set_series_title(reader_series_title(&view).unwrap_or("Series").into());
        }
        let category_id = snapshot
            .categories
            .get(view.category.saturating_sub(1))
            .map(|c| c.id.clone())
            .filter(|_| view.category > 0);
        ui.set_categories(ModelRc::new(VecModel::from(
            std::iter::once(text::tr("All categories").into())
                .chain(snapshot.categories.iter().map(|c| c.name.clone().into()))
                .collect::<Vec<_>>(),
        )));
        let (heading, caption) = match view.section {
            0 => (
                "My Library",
                "Your saved titles, reading progress and downloads.",
            ),
            1 => (
                "Updates",
                "Unread chapters from your library. Refresh to look for new releases.",
            ),
            2 => ("History", "Pick up where you left off."),
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
                    if ui.get_series_artwork_key().as_str() != key {
                        ui.set_series_poster(slint::Image::default());
                    }
                    ui.set_series_artwork_key(key.into());
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
        let previous_posters = ui
            .get_items()
            .iter()
            .filter(|r| r.poster.size().width > 0)
            .map(|r| (r.artwork_key.to_string(), r.poster))
            .collect::<BTreeMap<_, _>>();
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
                        title: s.title.clone().into(),
                        subtitle: text::chapter_count(unread, chapters.len()).into(),
                        badge: s.id.source_id.clone().into(),
                        bookmarked: snapshot.library.iter().any(|l| l.series == s.id),
                        poster: previous_posters.get(&key).cloned().unwrap_or_default(),
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
                        poster: previous_posters.get(&key).cloned().unwrap_or_default(),
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
        ui.set_source_rows(ModelRc::new(VecModel::from(
            view.source_browser.rows(&source_choices),
        )));
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
                    Ok((package, schema, values))
                },
                |app, ui, (package, schema, values)| {
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
        let mut view = self.view();
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
            for i in 0..model.row_count() {
                if view
                    .artwork_seen
                    .get(&i)
                    .is_none_or(|t| now.duration_since(*t).as_millis() > 600)
                    && let Some(mut row) = model.row_data(i)
                    && row.poster.size().width > 0
                {
                    row.poster = slint::Image::default();
                    model.set_row_data(i, row);
                }
            }
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
            if section != 3 {
                view.query.clear();
            }
        }
        if section == 3 {
            let adult = ui.get_show_adult();
            self.task(
                ui,
                move |app, _| {
                    Ok((
                        app.sources(adult)?,
                        app.services.sources.list().map_err(error)?,
                    ))
                },
                |app, ui, (choices, extensions)| {
                    ui.set_extensions(ModelRc::new(VecModel::from(
                        extensions
                            .into_iter()
                            .map(|e| ExtensionRow {
                                id: e.id.into(),
                                name: e.name.into(),
                                version: e.version.into(),
                                language: e.language.into(),
                                enabled: e.enabled,
                            })
                            .collect::<Vec<_>>(),
                    )));
                    let mut view = app.view();
                    view.choices = choices;
                    view.rows.clear();
                    view.selected = 0;
                    drop(view);
                    app.render(ui);
                },
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
            if let Some(mut saved) = view.saved_catalogue.take()
                && let Some(selected) = view.choices.iter().position(|c| c.id == saved.source_id)
            {
                saved.selected = selected;
                view.rows = saved.rows.iter().cloned().map(Row::Series).collect();
                view.query = saved.query.clone();
                view.catalogue = saved.catalogue.clone();
                view.page = saved.page;
                view.has_more = saved.has_more;
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
            if let Some(query) = query {
                view.query = query;
            }
            if !next {
                view.page = 0;
                view.has_more = false;
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
        self.task(ui, move |app,cancel| {
            let source=app.services.sources.source(&choice.package).map_err(error)?;
            let response=source.call_cancelled(op,&json!({"sourceId":choice.id,"catalogue":catalogue,"query":query,"page":page,"filters":filters}),cancel).map_err(error)?;
            let listing:reader_sources::Listing=serde_json::from_value(response).map_err(error)?;
            let series=listing.items.into_iter().map(|s|app.services.series(&choice.id,s)).collect::<Vec<_>>();
            Ok((series,listing.has_next_page))
        },move |app,ui,(series,more)| {
            let mut view=app.view();
            if !next {view.rows.clear();}
            for series in series {if !view.rows.iter().any(|r|matches!(r,Row::Series(s) if s.id == series.id)){view.rows.push(Row::Series(series));}}
            view.has_more=more;view.page=page;
            view.saved_catalogue=Some(CatalogueState {
                source_id,
                rows:view.rows.iter().filter_map(|r|if let Row::Series(s)=r {Some(s.clone())}else{None}).collect(),
                query:view.query.clone(),catalogue:view.catalogue.clone(),page:view.page,
                has_more:more,selected:view.selected,filters_active:view.filters_active,
            });
            drop(view);app.render(ui);
        });
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
        if cached {
            self.invalidate();
            self.render(ui);
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
                let mode = progress.as_ref().map(|p| p.mode).unwrap_or_else(|| {
                    state
                        .library
                        .iter()
                        .find(|l| l.series == chapter.series)
                        .and_then(|l| l.preferences.as_ref())
                        .unwrap_or(&state.preferences)
                        .mode
                });
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
                        target_width: 0,
                        viewport_width: 0.,
                        viewport_height: 0.,
                        anchor,
                        mode,
                        completed,
                    })
                } else {
                    let (source, pages) = app.services.pages(&chapter, cancel)?;
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
                        target_width: 0,
                        viewport_width: 0.,
                        viewport_height: 0.,
                        anchor,
                        mode,
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
                view.reader = Some(reader);
                view.section = 6;
                drop(view);
                app.mount_pages(ui);
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
        if reader.mode != ReadingMode::Webtoon {
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
        let complete = (index..=last).all(|i| reader.retained.contains(&i))
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
        let chapter_ids = view
            .chapters
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>();
        let Some(reader) = &mut view.reader else {
            return;
        };
        let index = index.min(reader.pages.len().saturating_sub(1));
        let target_width = Reader::decode_width(
            reader.viewport_width.max(ui.get_reader_width()),
            ui.window().scale_factor(),
        );
        if reader.desired_anchor != index || reader.target_width != target_width {
            reader.budget_blocked.clear();
            reader.desired_anchor = index;
            reader.target_width = target_width;
        }
        let mut end = index;
        if reader.mode == ReadingMode::Webtoon && reader.viewport_width > 0. {
            let mut covered =
                reader.ratios[index] * reader.viewport_width * (1. - reader.anchor.offset() as f32);
            while end + 1 < reader.pages.len() && covered < reader.viewport_height {
                end += 1;
                covered += reader.ratios[end] * reader.viewport_width;
            }
        }
        let keep = (index.saturating_sub(1)..=(end + 1).min(reader.pages.len() - 1))
            .collect::<BTreeSet<_>>();
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
        }
        reader.retained.retain(|i| keep.contains(i));
        // Visible images precede the one-page prefetch window.
        for target in (index..=end).chain(keep.iter().copied().filter(|i| *i < index || *i > end)) {
            if !reader.requested.contains(&target)
                && (!reader.retained.contains(&target)
                    || reader.decoded_widths.get(&target).copied().unwrap_or(0) < target_width)
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
        ui.set_position_label(text::page_position(index, reader.pages.len()).into());
        let position = chapter_ids.iter().position(|c| *c == reader.chapter);
        ui.set_can_previous(position.is_some_and(|i| i > 0));
        ui.set_can_next(position.is_some_and(|i| i + 1 < chapter_ids.len()));
    }
    fn image_worker(
        app: Weak<Self>,
        receiver: mpsc::Receiver<ImageTask>,
        ui: slint::Weak<AppWindow>,
    ) {
        for task in receiver {
            let Some(app) = app.upgrade() else {
                break;
            };
            if app.generation.load(Ordering::Acquire) != task.generation {
                continue;
            }
            let input = {
                let mut view = app.view();
                view.reader.as_mut().and_then(|r| {
                    if !r.desired.contains(&task.index) {
                        r.requested.remove(&task.index);
                        return None;
                    }
                    r.pages
                        .get(task.index)
                        .cloned()
                        .map(|p| (p, r.source.clone(), r.target_width))
                })
            };
            let Some((input, source, display_width)) = input else {
                continue;
            };
            let cancel = CancelToken::default();
            *app.active_image.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((task.index, cancel.clone()));
            if app.generation.load(Ordering::Acquire) != task.generation {
                cancel.cancel();
            }
            let result: Result<_> = (|| {
                let bytes = input.read(source.as_ref(), &cancel)?;
                let dimensions =
                    reader_engine::inspect_image(&bytes, reader_engine::DecodeLimits::default())
                        .map_err(error)?;
                let image = reader_engine::decode_display_image(
                    &bytes,
                    reader_engine::DecodeLimits::default(),
                    display_width,
                    65536,
                )
                .map_err(error)?;
                Ok((dimensions, image))
            })();
            let _ = ui.upgrade_in_event_loop(move |ui| {
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
                match result {
                    Ok((dimensions, image)) => {
                        // Replacing a retained preview consumes only the new
                        // image's accounting slot, not both versions' slots.
                        let mut used = reader.retained_bytes_without(task.index);
                        if task.index == reader.anchor.page_index {
                            let mut victims = reader
                                .retained
                                .iter()
                                .copied()
                                .filter(|i| *i != task.index)
                                .collect::<Vec<_>>();
                            victims.sort_by_key(|i| std::cmp::Reverse(i.abs_diff(task.index)));
                            for victim in victims {
                                if used.saturating_add(image.as_raw().len()) <= 64 * 1024 * 1024 {
                                    break;
                                }
                                used = used.saturating_sub(
                                    reader.decoded_bytes.remove(&victim).unwrap_or(0),
                                );
                                reader.retained.remove(&victim);
                                reader.decoded_widths.remove(&victim);
                                reader.budget_blocked.insert(victim);
                                if let Some(mut old) = model.row_data(victim) {
                                    old.tiles = ModelRc::default();
                                    old.image = slint::Image::default();
                                    old.loaded = false;
                                    model.set_row_data(victim, old);
                                }
                            }
                        }
                        if used.saturating_add(image.as_raw().len()) > 64 * 1024 * 1024 {
                            reader.budget_blocked.insert(task.index);
                            return;
                        }
                        loaded = true;
                        reader
                            .decoded_bytes
                            .insert(task.index, image.as_raw().len());
                        reader.decoded_widths.insert(task.index, display_width);
                        reader.ratios[task.index] =
                            dimensions.height as f32 / dimensions.width as f32;
                        row.tiles = ModelRc::new(VecModel::from(display_tiles(
                            image.as_raw(),
                            image.width(),
                            image.height(),
                        )));
                        row.loaded = true;
                        row.error = "".into();
                        reader.retained.insert(task.index);
                        model.set_row_data(task.index, row);
                        let mut position = 0.0;
                        for (i, ratio) in reader.ratios.iter().enumerate() {
                            if let Some(mut row) = model.row_data(i) {
                                row.position = position;
                                row.ratio = *ratio;
                                model.set_row_data(i, row);
                            }
                            position += ratio;
                        }
                        ui.set_total_ratio(position);
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
                app.restore_position(&ui);
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
                                index + 1 == reader.pages.len() && reader.retained.contains(&index);
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
        reader.completed |= reader.retained.contains(&index)
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
        if action == "category" {
            if let Some(Row::Series(series)) = row {
                let category = self.view().category_target;
                let result = self.services.domain.update(|state| {
                    if category > 0
                        && let Some(id) = state.categories.get(category - 1).map(|c| c.id.clone())
                        && let Some(entry) =
                            state.library.iter_mut().find(|l| l.series == series.id)
                        && !entry.categories.remove(&id)
                    {
                        entry.categories.insert(id);
                    }
                    Ok(())
                });
                if let Err(e) = result {
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
    fn install(self: &Arc<Self>, ui: &AppWindow, path: PathBuf) {
        self.task(
            ui,
            move |app, _| {
                let metadata = std::fs::metadata(&path).map_err(error)?;
                if metadata.len() > 8 * 1024 * 1024 {
                    return Err("Addon ZIP exceeds 8 MiB".into());
                }
                let id = app.services.install(&std::fs::read(path).map_err(error)?)?;
                let descriptor = app
                    .services
                    .sources
                    .list()
                    .map_err(error)?
                    .into_iter()
                    .find(|s| s.id == id)
                    .ok_or_else(|| "Installed source is unavailable".to_string())?;
                Ok((descriptor, app.sources(false)?))
            },
            |app, ui, (descriptor, choices)| {
                {
                    let mut view = app.view();
                    view.section = 3;
                    view.rows.clear();
                    view.choices = choices;
                    view.selected = 0;
                }
                app.render(ui);
                ui.set_status(
                    text::tr(if descriptor.nsfw {
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
fn display_tiles(pixels: &[u8], width: u32, height: u32) -> Vec<TileRow> {
    let stride = width as usize * 4;
    let tile_height = 2048usize;
    pixels
        .chunks(stride * tile_height)
        .enumerate()
        .map(|(index, chunk)| {
            let rows = (chunk.len() / stride) as u32;
            let buffer =
                slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(chunk, width, rows);
            TileRow {
                image: slint::Image::from_rgba8(buffer),
                offset: (index * tile_height) as f32 / height as f32,
                fraction: rows as f32 / height as f32,
            }
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
fn reading_mode(index: i32) -> ReadingMode {
    match index {
        1 => ReadingMode::LeftToRight,
        2 => ReadingMode::RightToLeft,
        3 => ReadingMode::Vertical,
        _ => ReadingMode::Webtoon,
    }
}

struct ReaderScreencap {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
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
fn wire_reader_context(app: &Arc<Controller>, ui: &AppWindow) {
    use std::{cell::RefCell, rc::Rc};
    let screencap = Rc::new(RefCell::new(None));
    let exporting = Arc::new(AtomicBool::new(false));
    ui.on_reader_context_opening({
        let weak = ui.as_weak();
        let screencap = screencap.clone();
        move |_| {
            if let Some(ui) = weak.upgrade() {
                *screencap.borrow_mut() = Some(capture_reader(&ui));
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
            // History/Updates can open a chapter without mounting its detail
            // page first. Use the active reader's series, never stale hero UI.
            let series_title = reader_series_title(&app.view())
                .unwrap_or("Series")
                .to_owned();
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
                let image = (|| {
                    cancel.check().map_err(error)?;
                    if screen {
                        let capture = snapshot
                            .ok_or_else(|| text::tr("Reader screencap is unavailable"))??;
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
                        crate::reader_export::deliver_with_context(
                            image,
                            clipboard,
                            &series_title,
                            &preferences,
                            done,
                        )
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
    crate::appearance::init(&ui)?;
    crate::capture_settings::init(&ui)?;
    crate::image_storage::init(&ui)?;
    let (sender, receiver) = mpsc::channel();
    let artwork = crate::artwork::ArtworkLoader::new(
        services.clone(),
        ui.as_weak(),
        crate::paths::cache_dir()?.join("posters"),
    );
    let app = Arc::new(Controller {
        services,
        artwork,
        view: Mutex::new(View {
            source_browser: crate::source_browser::SourceBrowser::load(),
            ..View::default()
        }),
        generation: AtomicU64::new(0),
        cancel: Mutex::new(CancelToken::default()),
        images: sender,
        download_serial: Mutex::new(()),
        active_image: Mutex::new(None),
    });
    let _ = LIFECYCLE.set(Arc::downgrade(&app));
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
    wire!(on_library_sort_changed, |app, ui| app.render(&ui));
    wire!(on_chapter_order_changed, |app, ui, descending| {
        ui.set_chapters_descending(descending);
        app.render(&ui);
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
    wire!(on_mode_changed, |app, ui, index| {
        {
            let mut view = app.view();
            if let Some(reader) = &mut view.reader {
                reader.mode = reading_mode(index);
            }
        }
        ui.set_reading_mode(index);
        app.step_page(&ui, 0);
        let mode = reading_mode(index);
        let series = app.view().series.as_ref().map(|s| s.id.clone());
        let _ = app.services.domain.update(|state| {
            state.preferences.mode = mode;
            if let Some(library) = state
                .library
                .iter_mut()
                .find(|l| Some(&l.series) == series.as_ref())
            {
                let mut preferences = library.preferences.clone().unwrap_or_default();
                preferences.mode = mode;
                library.preferences = Some(preferences);
            }
            Ok(())
        });
    });
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
    wire!(on_category_assignment_selected, |app, _ui, index| {
        app.view().category_target = index.max(0) as usize;
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
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(600),
        move || checkpoint.flush(),
    );
    let close = app.clone();
    ui.window().on_close_requested(move || {
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
    crate::reader_export::shutdown();
    app.flush();
    app.invalidate();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

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
            target_width: 0,
            viewport_width: 500.,
            viewport_height: 500.,
            anchor: ResumeAnchor::new("stable-2", 2, 0.375).unwrap(),
            mode: ReadingMode::Webtoon,
            completed: false,
        };
        let services = Services::open(&dir.path().join("data")).unwrap();
        let app = Arc::new(Controller {
            artwork: crate::artwork::ArtworkLoader::new(
                services.clone(),
                ui.as_weak(),
                dir.path().join("cache").join("posters"),
            ),
            services,
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
        });
        // A direct History/Updates chapter open has no mounted detail hero.
        // Its OSD and capture folder must use the active series identity.
        let active_series = app.view().reader.as_ref().unwrap().chapter.series.clone();
        app.view().series = Some(Series {
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
        assert_eq!(Reader::decode_width(1100., 3.), 2048);

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

        {
            let mut view = app.view();
            let reader = view.reader.as_mut().unwrap();
            reader.decoded_bytes = BTreeMap::from([(0, 24 * 1024 * 1024), (1, 32 * 1024 * 1024)]);
            assert_eq!(reader.retained_bytes_without(0), 32 * 1024 * 1024);
            // A 32MiB replacement fits exactly; counting its 24MiB preview
            // as another retained image would incorrectly block the upgrade.
            assert_eq!(
                reader.retained_bytes_without(0) + 32 * 1024 * 1024,
                64 * 1024 * 1024
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
        assert_eq!(ui.get_source_index(), 1);
        assert_eq!(ui.get_items().row_data(0).unwrap().title, "Saved result");
        let view = app.view();
        assert_eq!(view.query, "saved query");
        assert_eq!(view.page, 3);
        assert!(view.has_more && view.filters_active);
        drop(view);
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
    }
}
