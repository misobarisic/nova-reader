use crate::{
    AppWindow, ItemRow, PageRow, SettingRow, TileRow,
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
        atomic::{AtomicU64, Ordering},
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
    viewport_width: f32,
    viewport_height: f32,
    anchor: ResumeAnchor,
    mode: ReadingMode,
    completed: bool,
}
impl Reader {
    fn page_id(&self, index: usize) -> String {
        match &self.pages[index] {
            PageInput::Local { id, .. } => id.clone(),
            PageInput::Remote(p) => p.id.clone(),
        }
    }
}
#[derive(Default)]
struct View {
    section: i32,
    rows: Vec<Row>,
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
}
struct ImageTask {
    generation: u64,
    index: usize,
}
struct Controller {
    services: Services,
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
            0 => ("Your library", "A home for your next chapter."),
            1 => (
                "Updates",
                "Unread chapters from your library. Refresh to look for new releases.",
            ),
            2 => ("History", "Pick up where you left off."),
            3 => (
                "Find your next story",
                "Choose a source and explore its collection.",
            ),
            4 => ("More", "Make this reading space your own."),
            5 => (
                "Chapters",
                "Reading position and downloads stay with you when a source goes away.",
            ),
            7 => ("Downloads", "Your chapters, ready whenever you are."),
            8 => ("Source settings", "Settings belong to this profile."),
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
                series.sort_by_key(|a| a.title.to_lowercase());
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
                    ui.set_in_library(snapshot.library.iter().any(|l| l.series == series.id));
                    view.chapters = snapshot
                        .chapters
                        .iter()
                        .filter(|c| c.id.series == series.id)
                        .cloned()
                        .collect();
                    view.chapters.sort_by_key(|c| c.source_order);
                    view.rows = view.chapters.iter().cloned().map(Row::Chapter).collect();
                }
            }
            _ => {}
        }
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
                    ItemRow {
                        title: s.title.clone().into(),
                        subtitle: text::chapter_count(unread, chapters.len()).into(),
                        badge: s.id.source_id.clone().into(),
                        bookmarked: snapshot.library.iter().any(|l| l.series == s.id),
                    }
                }
                Row::Chapter(c) => ItemRow {
                    title: c.title.clone().into(),
                    subtitle: if view.section == 5 {
                        String::new()
                    } else {
                        snapshot
                            .series
                            .iter()
                            .find(|s| s.id == c.id.series)
                            .map(|s| s.title.clone())
                            .unwrap_or_default()
                    }
                    .into(),
                    badge: if snapshot
                        .progress
                        .iter()
                        .any(|p| p.chapter == c.id && p.completed)
                    {
                        text::tr("Read")
                    } else {
                        text::tr("Unread")
                    }
                    .into(),
                    bookmarked: snapshot.bookmarks.iter().any(|b| b.chapter == c.id),
                },
                Row::Download(c) => ItemRow {
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
        ui.set_source_index(if view.choices.is_empty() {
            -1
        } else {
            view.selected.min(view.choices.len() - 1) as i32
        });
    }
    fn navigate(self: &Arc<Self>, ui: &AppWindow, section: i32) {
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
                move |app, _| app.sources(adult),
                |app, ui, choices| {
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
        } else {
            self.render(ui);
        }
    }
    fn browse(self: &Arc<Self>, ui: &AppWindow, kind: &str, query: Option<String>, next: bool) {
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
        let (op, query, page, catalogue) = {
            let mut view = self.view();
            view.section = 3;
            if let Some(query) = query {
                view.query = query;
            }
            if !next {
                view.page = 1;
                if !kind.is_empty() {
                    view.catalogue = kind.into();
                }
            } else {
                view.page += 1;
            }
            (
                if view.query.is_empty() {
                    "browse"
                } else {
                    "search"
                },
                view.query.clone(),
                view.page.max(1),
                view.catalogue.clone(),
            )
        };
        self.task(ui, move |app,cancel| {
            let source=app.services.sources.source(&choice.package).map_err(error)?;
            let response=source.call_cancelled(op,&json!({"sourceId":choice.id,"catalogue":catalogue,"query":query,"page":page}),cancel).map_err(error)?;
            let listing:reader_sources::Listing=serde_json::from_value(response).map_err(error)?;
            let series=listing.items.into_iter().map(|s|app.services.series(&choice.id,s)).collect::<Vec<_>>();
            Ok((series,listing.has_next_page))
        },move |app,ui,(series,more)| {
            let mut view=app.view();
            if !next {view.rows.clear();}
            for series in series {if !view.rows.iter().any(|r|matches!(r,Row::Series(s) if s.id == series.id)){view.rows.push(Row::Series(series));}}
            view.has_more=more;drop(view);app.render(ui);
        });
    }
    fn open_series(self: &Arc<Self>, ui: &AppWindow, series: Series) {
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
            },
        );
    }
    fn open_chapter(self: &Arc<Self>, ui: &AppWindow, chapter: ChapterId) {
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
        if reader.desired_anchor != index {
            reader.budget_blocked.clear();
            reader.desired_anchor = index;
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
        }
        reader.retained.retain(|i| keep.contains(i));
        // Visible images precede the one-page prefetch window.
        for target in (index..=end).chain(keep.iter().copied().filter(|i| *i < index || *i > end)) {
            if !reader.requested.contains(&target)
                && !reader.retained.contains(&target)
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
                    r.pages.get(task.index).cloned().map(|p| {
                        (
                            p,
                            r.source.clone(),
                            r.viewport_width.clamp(100., 1024.) as u32,
                        )
                    })
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
            let result = (|| {
                let bytes = match input {
                    PageInput::Local { path, .. } => {
                        let file = std::fs::File::open(path).map_err(error)?;
                        if file.metadata().map_err(error)?.len() > 32 * 1024 * 1024 {
                            return Err("Image exceeds byte limit".to_string());
                        }
                        use std::io::Read;
                        let mut bytes = Vec::new();
                        file.take(32 * 1024 * 1024 + 1)
                            .read_to_end(&mut bytes)
                            .map_err(error)?;
                        bytes
                    }
                    PageInput::Remote(p) => source
                        .ok_or_else(|| "Source unavailable".to_string())?
                        .fetch_page(&p.url, &p.headers, &cancel)
                        .map_err(error)?,
                };
                cancel.check().map_err(error)?;
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
                        let mut used: usize = reader.decoded_bytes.values().sum();
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
                let recent = state
                    .progress
                    .iter()
                    .filter(|p| !p.completed && view.chapters.iter().any(|c| c.id == p.chapter))
                    .max_by_key(|p| p.last_read_at)
                    .and_then(|p| view.chapters.iter().find(|c| c.id == p.chapter));
                recent
                    .or_else(|| {
                        view.chapters
                            .iter()
                            .find(|c| {
                                !state
                                    .progress
                                    .iter()
                                    .any(|p| p.chapter == c.id && p.completed)
                            })
                            .or_else(|| view.chapters.first())
                    })
                    .map(|c| c.id.clone())
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
                    })
                    .collect::<Vec<_>>();
                app.view().rows = jobs.into_iter().map(|j| Row::Download(j.chapter)).collect();
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
            });
        });
    }
    fn item_action(self: &Arc<Self>, ui: &AppWindow, index: usize, action: &str) {
        let row = self.view().rows.get(index).cloned();
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

pub fn run(services: Services) -> Result<()> {
    let ui = AppWindow::new().map_err(error)?;
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_desktop(!cfg!(target_os = "android"));
    let (sender, receiver) = mpsc::channel();
    let app = Arc::new(Controller {
        services,
        view: Mutex::new(View::default()),
        generation: AtomicU64::new(0),
        cancel: Mutex::new(CancelToken::default()),
        images: sender,
        download_serial: Mutex::new(()),
        active_image: Mutex::new(None),
    });
    let _ = LIFECYCLE.set(Arc::downgrade(&app));
    let worker = Arc::downgrade(&app);
    let weak = ui.as_weak();
    std::thread::spawn(move || Controller::image_worker(worker, receiver, weak));
    macro_rules! wire { ($method:ident, |$this:ident,$window:ident $(,$arg:ident)*| $body:expr) => {{let $this=app.clone();let weak=ui.as_weak();ui.$method(move |$($arg),*|{if let Some($window)=weak.upgrade(){$body}});}}; }
    wire!(on_navigate, |app, ui, section| app.navigate(&ui, section));
    wire!(on_open_item, |app, ui, index| {
        let row = { app.view().rows.get(index as usize).cloned() };
        match row {
            Some(Row::Series(series)) => app.open_series(&ui, series),
            Some(Row::Chapter(chapter)) => app.open_chapter(&ui, chapter.id),
            Some(Row::Download(chapter)) => app.open_chapter(&ui, chapter),
            None => {}
        }
    });
    wire!(on_item_action, |app, ui, index, action| app.item_action(
        &ui,
        index as usize,
        &action
    ));
    wire!(on_source_selected, |app, ui, index| {
        app.view().selected = index.max(0) as usize;
        app.browse(&ui, "popular", Some(String::new()), false);
    });
    wire!(on_search, |app, ui, query| {
        if app.view().section == 0 {
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
            if action == "settings" {
                let package = choice.package;
                app.task(
                    &ui,
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
                                let selected =
                                    f.options
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
            let result = if action == "remove" {
                app.services.sources.remove(&choice.package)
            } else {
                app.services
                    .sources
                    .set_enabled(&choice.package, !choice.enabled)
            };
            if let Err(e) = result {
                ui.set_status(error(e).into());
            } else {
                app.navigate(&ui, 3);
            }
        }
    });
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
    if let Ok(jobs) = app.services.downloads.jobs() {
        for job in jobs {
            if job.status == reader_downloads::JobStatus::Queued {
                app.download(&ui, job.chapter);
            }
        }
    }
    ui.run().map_err(error)?;
    app.flush();
    app.invalidate();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            viewport_width: 500.,
            viewport_height: 500.,
            anchor: ResumeAnchor::new("stable-2", 2, 0.375).unwrap(),
            mode: ReadingMode::Webtoon,
            completed: false,
        };
        let app = Arc::new(Controller {
            services: Services::open(dir.path()).unwrap(),
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
    }
}
