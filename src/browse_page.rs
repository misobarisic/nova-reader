//! Browse "Refresh" (`BrowseBridge`): the source list's sections, the open
//! catalogue's header and filter sheet, and search across sources. The
//! window's existing Browse properties alias the bridge, so the catalogue's
//! paging, filters and back navigation keep their paths in `app.rs`.
use super::*;
use nova_reader_ui::{BrowseBridge, FilterGroup, PageMenuItem, SearchSection, SourceRow};
use reader_sources::CancelToken;
use std::collections::VecDeque;

/// Sources searched at the same time.
const PARALLEL: usize = 4;
/// Cards shown per source in the results.
const RESULTS: usize = 8;

#[derive(Clone)]
enum Status {
    Loading,
    Done(Vec<Series>),
    Failed(String),
}

#[derive(Clone)]
struct Section {
    choice: Choice,
    status: Status,
}

/// Search across sources. Survives opening a series and coming back.
#[derive(Default)]
pub(super) struct GlobalSearch {
    query: String,
    /// 0 pinned sources, 1 every enabled source.
    scope: i32,
    /// Bumped per search; late results of an older one are dropped.
    generation: u64,
    cancel: Option<CancelToken>,
    sections: Vec<Section>,
}

impl GlobalSearch {
    fn stop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.generation = self.generation.wrapping_add(1);
    }
}

/// The source choices the list and sections are built from.
fn source_choices(view: &View) -> Vec<crate::source_browser::SourceChoice> {
    view.choices
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
            adult: c.adult,
            extension: c.extension.clone(),
        })
        .collect()
}

fn with_icon(view: &View, mut row: SourceRow) -> SourceRow {
    if let Some(choice) = view.choices.get(row.index.max(0) as usize) {
        row.icon = crate::addon_icons::image(&choice.package);
    }
    row
}

/// The source list: All sources (under the language chip) in `source_rows`,
/// plus the Last used card, Pinned grid and chips.
pub(super) fn publish_sources(view: &View, ui: &AppWindow) {
    let bridge = ui.global::<BrowseBridge>();
    let sections = view.source_browser.sections(&source_choices(view));
    let all: Vec<SourceRow> = sections
        .all
        .into_iter()
        .map(|row| with_icon(view, row))
        .collect();
    ui.set_source_rows(ModelRc::new(VecModel::from(all)));
    bridge.set_pinned(ModelRc::new(VecModel::from(
        sections
            .pinned
            .into_iter()
            .map(|row| with_icon(view, row))
            .collect::<Vec<_>>(),
    )));
    bridge.set_pinned_count(
        view.choices
            .iter()
            .filter(|c| c.enabled && view.source_browser.is_pinned(&c.id))
            .count() as i32,
    );
    match sections.last_used {
        Some((row, at)) => {
            let row = with_icon(view, row);
            let mut detail = row.language.to_string();
            if let Some(at) = at {
                detail = format!(
                    "{detail} · {}",
                    crate::source_browser::opened_ago(at, now())
                );
            }
            bridge.set_last_used_detail(detail.into());
            bridge.set_last_used(row);
            bridge.set_has_last_used(true);
        }
        None => bridge.set_has_last_used(false),
    }
    let mut languages = vec![text::tr("All").into()];
    let mut counts = vec![sections.total as i32];
    for (name, count) in &sections.languages {
        languages.push(name.as_str().into());
        counts.push(*count as i32);
    }
    bridge.set_languages(ModelRc::new(VecModel::from(languages)));
    bridge.set_language_counts(ModelRc::new(VecModel::from(counts)));
    bridge.set_language_index(sections.language_index as i32);
    bridge.set_source_total(sections.total as i32);
    publish_catalog(view, ui);
}

/// The open catalogue's header, Change source list and filter sheet.
pub(super) fn publish_catalog(view: &View, ui: &AppWindow) {
    let bridge = ui.global::<BrowseBridge>();
    if let Some(choice) = view.choices.get(view.selected) {
        bridge.set_catalog_icon(crate::addon_icons::image(&choice.package));
        bridge.set_catalog_language(crate::source_browser::language_label(&choice.language).into());
        bridge.set_catalog_adult(choice.adult);
        bridge.set_catalog_pinned(view.source_browser.is_pinned(&choice.id));
        bridge.set_catalog_latest(choice.latest);
    }
    bridge.set_change_items(ModelRc::new(VecModel::from(
        view.choices
            .iter()
            .enumerate()
            .filter(|(_, c)| c.enabled)
            .map(|(i, c)| PageMenuItem {
                label: c.name.as_str().into(),
                kind: 1,
                checked: i == view.selected,
                id: i.to_string().into(),
            })
            .collect::<Vec<_>>(),
    )));
    publish_filters(view, ui);
}

pub(super) fn publish_filters(view: &View, ui: &AppWindow) {
    let bridge = ui.global::<BrowseBridge>();
    let groups: Vec<FilterGroup> = if view.source_filters_loaded {
        view.source_filters.groups()
    } else {
        Vec::new()
    };
    bridge.set_filter_groups(ModelRc::new(VecModel::from(groups)));
    bridge.set_filter_count(if view.source_filters_loaded {
        view.source_filters.active_count() as i32
    } else {
        0
    });
}

/// A catalogue card's muted line: status and chapter count when known,
/// otherwise nothing.
pub(super) fn catalogue_detail(
    status: reader_domain::PublicationStatus,
    chapters: usize,
) -> String {
    use reader_domain::PublicationStatus::*;
    let status = match status {
        Ongoing => text::tr("Ongoing"),
        Completed => text::tr("Completed"),
        OnHiatus => text::tr("On hiatus"),
        Cancelled => text::tr("Cancelled"),
        Unknown => String::new(),
    };
    let chapters = (chapters > 0).then(|| text::chapters_label(chapters));
    [Some(status).filter(|s| !s.is_empty()), chapters]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Disabled extensions, for the Extensions tab's badge. Nothing checks for
/// addon updates yet, so this is the only "needs attention" signal.
pub(super) fn publish_attention(ui: &AppWindow, disabled: usize) {
    ui.global::<BrowseBridge>()
        .set_attention_count(disabled as i32);
}

fn section_row(
    app: &Controller,
    series: &Series,
    library: &[reader_domain::LibraryEntry],
) -> ItemRow {
    ItemRow {
        title: series.title.clone().into(),
        series_key: serde_json::to_string(&series.id).unwrap_or_default().into(),
        artwork_key: crate::artwork::key(series).into(),
        poster: app.artwork.cached_image(series).unwrap_or_default(),
        bookmarked: library.iter().any(|l| l.series == series.id),
        ..Default::default()
    }
}

impl Controller {
    fn publish_global(&self, ui: &AppWindow) {
        let bridge = ui.global::<BrowseBridge>();
        let library = self
            .services
            .domain
            .snapshot()
            .map(|s| s.library)
            .unwrap_or_default();
        let view = self.view();
        let search = &view.global_search;
        bridge.set_global_query(search.query.clone().into());
        bridge.set_global_scope(search.scope);
        let sections: Vec<SearchSection> = search
            .sections
            .iter()
            .map(|section| {
                let (status, count, error, items) = match &section.status {
                    Status::Loading => (0, 0, String::new(), Vec::new()),
                    Status::Done(series) => (
                        1,
                        series.len() as i32,
                        String::new(),
                        series
                            .iter()
                            .take(RESULTS)
                            .map(|s| section_row(self, s, &library))
                            .collect(),
                    ),
                    Status::Failed(error) => (2, 0, error.clone(), Vec::new()),
                };
                SearchSection {
                    name: section
                        .choice
                        .name
                        .strip_suffix(&format!(" · {}", section.choice.language))
                        .unwrap_or(&section.choice.name)
                        .into(),
                    icon: crate::addon_icons::image(&section.choice.package),
                    status,
                    count,
                    error: error.into(),
                    items: ModelRc::new(VecModel::from(items)),
                }
            })
            .collect();
        bridge.set_global_sections(ModelRc::new(VecModel::from(sections)));
    }

    /// Search every pinned (or every enabled) source, at most [`PARALLEL`]
    /// at a time. A new query or closing the results cancels the old one.
    fn global_search(self: &Arc<Self>, ui: &AppWindow, query: &str) {
        let query = query.trim().to_owned();
        let (generation, cancel, targets) = {
            let mut view = self.view();
            view.global_search.stop();
            view.global_search.query = query.clone();
            if query.is_empty() {
                view.global_search.sections.clear();
                drop(view);
                self.publish_global(ui);
                return;
            }
            let scope = view.global_search.scope;
            // Pinned sources in pin order; otherwise every enabled source.
            // `choices` already leaves out adult sources while they are hidden.
            let mut targets: Vec<Choice> = view
                .choices
                .iter()
                .filter(|c| c.enabled && (scope == 1 || view.source_browser.is_pinned(&c.id)))
                .cloned()
                .collect();
            if scope == 0 {
                targets.sort_by(|a, b| a.id.cmp(&b.id));
            }
            let cancel = CancelToken::default();
            let search = &mut view.global_search;
            search.cancel = Some(cancel.clone());
            search.sections = targets
                .iter()
                .cloned()
                .map(|choice| Section {
                    choice,
                    status: Status::Loading,
                })
                .collect();
            (search.generation, cancel, targets)
        };
        self.publish_global(ui);
        self.run_global(
            ui,
            generation,
            cancel,
            targets.into_iter().enumerate().collect(),
            query,
        );
    }

    fn run_global(
        self: &Arc<Self>,
        ui: &AppWindow,
        generation: u64,
        cancel: CancelToken,
        targets: Vec<(usize, Choice)>,
        query: String,
    ) {
        let queue = Arc::new(Mutex::new(VecDeque::from(targets)));
        for _ in 0..PARALLEL {
            let queue = queue.clone();
            let app = self.clone();
            let weak = ui.as_weak();
            let cancel = cancel.clone();
            let query = query.clone();
            crate::profiles::spawn(move || {
                loop {
                    let Some((index, choice)) =
                        queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front()
                    else {
                        return;
                    };
                    if cancel.is_cancelled() {
                        return;
                    }
                    let result = app.search_source(&choice, &query, &cancel);
                    if cancel.is_cancelled() {
                        return;
                    }
                    let app = app.clone();
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        app.global_result(&ui, generation, index, result);
                    });
                }
            });
        }
    }

    /// One source's first page for `query`, through the same call and
    /// request handling as a catalogue search.
    fn search_source(
        &self,
        choice: &Choice,
        query: &str,
        cancel: &CancelToken,
    ) -> Result<Vec<Series>> {
        let source = self
            .services
            .sources
            .source(&choice.package)
            .map_err(error)?;
        let response = source
            .call_cancelled(
                "search",
                &json!({
                    "sourceId": choice.id,
                    "catalogue": "search",
                    "query": query,
                    "page": 1,
                    "filters": {},
                }),
                cancel,
            )
            .map_err(error)?;
        let listing: reader_sources::Listing = serde_json::from_value(response).map_err(error)?;
        Ok(listing
            .items
            .into_iter()
            .map(|s| self.services.series_listing(&choice.id, s))
            .collect())
    }

    fn global_result(
        &self,
        ui: &AppWindow,
        generation: u64,
        index: usize,
        result: Result<Vec<Series>>,
    ) {
        {
            let mut view = self.view();
            let search = &mut view.global_search;
            if search.generation != generation {
                return;
            }
            let Some(section) = search.sections.get_mut(index) else {
                return;
            };
            section.status = match result {
                Ok(series) => Status::Done(series),
                Err(e) => Status::Failed(text::tr(&e)),
            };
        }
        self.publish_global(ui);
    }

    fn global_retry(self: &Arc<Self>, ui: &AppWindow, index: usize) {
        let (generation, cancel, choice, query) = {
            let mut view = self.view();
            let search = &mut view.global_search;
            let Some(cancel) = search.cancel.clone() else {
                return;
            };
            let Some(section) = search.sections.get_mut(index) else {
                return;
            };
            section.status = Status::Loading;
            (
                search.generation,
                cancel,
                section.choice.clone(),
                search.query.clone(),
            )
        };
        self.publish_global(ui);
        self.run_global(ui, generation, cancel, vec![(index, choice)], query);
    }

    fn global_series(&self, section: usize, item: usize) -> Option<Series> {
        let view = self.view();
        match &view.global_search.sections.get(section)?.status {
            Status::Done(series) => series.get(item).cloned(),
            _ => None,
        }
    }

    /// The section's source opened with the query already searched.
    fn global_more(self: &Arc<Self>, ui: &AppWindow, section: usize) {
        let (index, query) = {
            let view = self.view();
            let Some(section) = view.global_search.sections.get(section) else {
                return;
            };
            let Some(index) = view.choices.iter().position(|c| c.id == section.choice.id) else {
                return;
            };
            (index, view.global_search.query.clone())
        };
        ui.set_source_index(index as i32);
        ui.set_source_catalogue_kind("search".into());
        self.select_source_with(ui, index as i32, "search", query);
    }

    fn global_artwork(&self, section: usize) {
        let series: Vec<Series> = {
            let view = self.view();
            match view.global_search.sections.get(section).map(|s| &s.status) {
                Some(Status::Done(series)) => series.iter().take(RESULTS).cloned().collect(),
                _ => Vec::new(),
            }
        };
        for series in series {
            self.artwork.request(&series);
        }
    }

    /// Pin or unpin a source, with a short confirmation.
    pub(super) fn unpin(self: &Arc<Self>, ui: &AppWindow, id: &str) {
        let name = {
            let view = self.view();
            view.choices
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.name.clone())
                .unwrap_or_default()
        };
        let result = self.view().source_browser.toggle_pin(id);
        match result {
            Err(e) => ui.set_status(e.into()),
            Ok(()) => {
                let pinned = self.view().source_browser.is_pinned(id);
                let bridge = ui.global::<BrowseBridge>();
                bridge.set_notice(
                    format!(
                        "{} {name}",
                        text::tr(if pinned { "Pinned" } else { "Unpinned" })
                    )
                    .into(),
                );
                bridge.set_notice_serial(bridge.get_notice_serial().wrapping_add(1));
                self.render(ui);
            }
        }
    }
}

/// Covers for the results across sources, which nest their own models.
pub(crate) fn set_global_posters(
    ui: &AppWindow,
    matches: impl Fn(&str) -> bool,
    image: &slint::Image,
) {
    let sections = ui.global::<BrowseBridge>().get_global_sections();
    for s in 0..sections.row_count() {
        let Some(section) = sections.row_data(s) else {
            continue;
        };
        for i in 0..section.items.row_count() {
            if let Some(mut row) = section.items.row_data(i)
                && matches(row.artwork_key.as_str())
                && row.poster != *image
            {
                row.poster = image.clone();
                section.items.set_row_data(i, row);
            }
        }
    }
}

/// The Browse page's own callbacks; the window's are relayed in the UI crate.
pub(super) fn wire(app: &Arc<Controller>, ui: &AppWindow) {
    let bridge = ui.global::<BrowseBridge>();
    macro_rules! on {
        ($method:ident, |$this:ident, $window:ident $(,$arg:ident)*| $body:expr) => {{
            let $this = app.clone();
            let weak = ui.as_weak();
            bridge.$method(move |$($arg),*| {
                if let Some($window) = weak.upgrade() {
                    $body
                }
            });
        }};
    }
    on!(on_language_picked, |app, ui, index| {
        let language = {
            let view = app.view();
            let sections = view.source_browser.sections(&source_choices(&view));
            usize::try_from(index - 1)
                .ok()
                .and_then(|i| sections.languages.get(i))
                .map(|(name, _)| name.clone())
        };
        let result = app.view().source_browser.set_language(language);
        if let Err(e) = result {
            ui.set_status(e.into());
        }
        app.render(&ui);
    });
    on!(on_filters_reset, |app, ui| {
        app.view().source_filters.reset();
        let view = app.view();
        ui.set_source_filters(ModelRc::new(VecModel::from(view.source_filters.rows())));
        publish_filters(&view, &ui);
    });
    on!(on_unpin, |app, ui, id| app.unpin(&ui, &id));
    on!(on_global_search, |app, ui, query| app
        .global_search(&ui, &query));
    on!(on_global_scope_picked, |app, ui, scope| {
        let query = {
            let mut view = app.view();
            view.global_search.scope = scope.clamp(0, 1);
            view.global_search.query.clone()
        };
        if query.is_empty() {
            app.publish_global(&ui);
        } else {
            app.global_search(&ui, &query);
        }
    });
    on!(on_global_retry, |app, ui, section| app
        .global_retry(&ui, section.max(0) as usize));
    on!(on_global_more, |app, ui, section| app
        .global_more(&ui, section.max(0) as usize));
    on!(on_global_picked, |app, ui, section, item| {
        if let Some(series) = app.global_series(section.max(0) as usize, item.max(0) as usize) {
            app.open_series(&ui, series);
        }
    });
    on!(on_global_artwork, |app, ui, section| {
        let _ = &ui;
        app.global_artwork(section.max(0) as usize);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn catalogue_detail_leaves_out_what_the_source_does_not_say() {
        use reader_domain::PublicationStatus;
        assert_eq!(
            catalogue_detail(PublicationStatus::Ongoing, 12),
            "Ongoing · 12 chapters"
        );
        assert_eq!(
            catalogue_detail(PublicationStatus::Completed, 0),
            "Completed"
        );
        assert_eq!(catalogue_detail(PublicationStatus::Unknown, 0), "");
    }

    /// An addon built from the neutral fixture under another identity.
    /// `failing` makes its search throw; `adult` marks the package.
    fn fixture(dir: &std::path::Path, id: &str, failing: bool, adult: bool) -> PathBuf {
        let mut manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../examples/addons/neutral-fixture/manifest.json"
        ))
        .unwrap();
        manifest["id"] = json!(id);
        manifest["name"] = json!(format!("Fixture {id}"));
        manifest["nsfw"] = json!(adult);
        manifest["assets"] = json!([]);
        let mut script = include_str!("../examples/addons/neutral-fixture/main.js")
            .replace("'neutral-fixture'", &format!("'{id}'"));
        if failing {
            script = script.replace(
                "if (op === 'browse' || op === 'search')",
                "if (op === 'search') throw new Error('offline'); if (op === 'browse')",
            );
        }
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("main.js", script.into_bytes()),
        ] {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(&bytes).unwrap();
        }
        let path = dir.join(format!("{id}.zip"));
        std::fs::write(&path, archive.finish().unwrap().into_inner()).unwrap();
        path
    }

    fn pump_until(done: impl Fn() -> bool) {
        let started = std::time::Instant::now();
        while !done() && started.elapsed() < std::time::Duration::from_secs(20) {
            slint::Timer::single_shot(std::time::Duration::from_millis(20), || {
                slint::quit_event_loop().unwrap()
            });
            slint::run_event_loop().unwrap();
        }
    }

    /// Search across sources through the real controller and addons: one
    /// section per pinned source, a failing source does not block the others,
    /// adult (while hidden) and disabled sources are left out, an old search's
    /// late results are dropped, and › opens the catalogue searching.
    #[test]
    fn search_across_sources_groups_isolates_and_hands_over() {
        const CHILD: &str = "NOVA_READER_GLOBAL_SEARCH_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::browse_page::tests::search_across_sources_groups_isolates_and_hands_over",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        i_slint_backend_testing::init_integration_test_with_system_time();
        let dir = tempfile::tempdir().unwrap();
        let services = Services::open(&dir.path().join("data")).unwrap();
        let ui = AppWindow::new().unwrap();
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
                dir.path().join("viewed"),
            )),
            capture_originals: Mutex::default(),
            thumbnail_service: OnceLock::new(),
            image_worker_live: AtomicBool::new(false),
            view: Mutex::new(View {
                section: 3,
                ..View::default()
            }),
            generation: AtomicU64::new(0),
            cancel: Mutex::default(),
            images,
            download_serial: Mutex::default(),
            active_image: Mutex::default(),
            continuation_work: Default::default(),
            metadata_refresh: Arc::default(),
        });
        wire(&app, &ui);
        for (id, failing, adult) in [
            ("fixture-a", false, false),
            ("fixture-b", true, false),
            ("fixture-c", false, true),
            ("fixture-d", false, false),
        ] {
            let path = fixture(dir.path(), id, failing, adult);
            app.install_sources(path, true).unwrap();
            app.view().source_browser.toggle_pin(id).unwrap();
        }
        app.services
            .sources
            .set_enabled("fixture-d", false)
            .unwrap();
        // Adult sources hidden: the visible choices leave fixture-c out.
        let sources = app.browse_sources(false).unwrap();
        app.publish_sources(&ui, sources);
        let bridge = ui.global::<BrowseBridge>();
        assert_eq!(bridge.get_pinned_count(), 2);
        bridge.invoke_global_search("neutral".into());
        let sections = || bridge.get_global_sections().iter().collect::<Vec<_>>();
        assert_eq!(
            sections()
                .iter()
                .map(|s| s.name.to_string())
                .collect::<Vec<_>>(),
            ["Fixture fixture-a", "Fixture fixture-b"]
        );
        assert!(sections().iter().all(|s| s.status == 0), "loading first");
        pump_until(|| sections().iter().all(|s| s.status != 0));
        let found = sections();
        assert_eq!(found[0].status, 1);
        assert_eq!(found[0].count, 1);
        assert_eq!(found[0].items.row_count(), 1);
        assert_eq!(found[1].status, 2, "the failing source fails on its own");
        // A late result of an older search is dropped.
        let stale = app.view().global_search.generation.wrapping_sub(1);
        app.global_result(&ui, stale, 0, Err("late".into()));
        assert_eq!(sections()[0].status, 1);
        // A new query replaces the old one.
        bridge.invoke_global_search("nothing matches".into());
        pump_until(|| sections().iter().all(|s| s.status != 0));
        assert_eq!(sections()[0].count, 0);
        bridge.invoke_global_search("neutral".into());
        pump_until(|| sections().iter().all(|s| s.status != 0));
        // Every enabled source: still no adult or disabled one.
        bridge.invoke_global_scope_picked(1);
        assert_eq!(sections().len(), 2);
        pump_until(|| sections().iter().all(|s| s.status != 0));
        // › opens that source's catalogue with the query searched.
        app.global_more(&ui, 0);
        assert!(ui.get_source_catalog_open());
        assert_eq!(ui.get_source_query(), "neutral");
        assert_eq!(app.view().query, "neutral");
        // Clearing returns to the source list.
        bridge.invoke_global_search("".into());
        assert_eq!(bridge.get_global_query(), "");
        assert!(sections().is_empty());
    }
}
