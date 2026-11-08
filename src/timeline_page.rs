//! Updates and History pages (`TimelineBridge`): facts built from the rows
//! `render` already produced, the grouped lines and the History shelf, and
//! the page's actions. Grouping itself lives in `timeline_groups.rs`.
use super::timeline_groups::{self as groups, Expansion, ExpansionState, Fact, Kind, Line};
use super::*;
use nova_reader_ui::{ShelfEntry, TimelineBridge, TimelineEntry};
use reader_domain::ReaderState;

/// History's shelf plus the section title under it. `timeline-page.slint`
/// (`TimelineMetrics`) draws the same heights.
const SHELF_TOP: f32 = 268. + 44.;
/// Updates keeps read chapters this recent so All, Unread and the muted
/// rows have something to show; older read chapters drop out as before.
pub(super) const RECENT_READ_SECONDS: i64 = 30 * 24 * 60 * 60;

/// What a shelf card or a session's primary action opens.
#[derive(Clone)]
struct Resume {
    chapter: ChapterId,
    /// "Ch. 87" or "Next: Ch. 88".
    headline: String,
    /// "Page 18 of 30" or "Ready".
    detail: String,
    progress: f32,
}

#[derive(Default)]
pub(super) struct State {
    /// Open groups and sessions, by (day, series). Reset on restart, never
    /// stored or synced.
    expansion: ExpansionState,
    /// Updates chips: 0 All, 1 Unread, 2 Downloaded.
    filter: i32,
    /// Every fact of the current section, before the filter.
    all: Vec<Fact>,
    /// The facts behind the published lines (after the filter).
    facts: Vec<Fact>,
    lines: Vec<Line>,
    /// Shelf cards, by position.
    shelf: Vec<(usize, Option<Resume>)>,
    /// The last list shown: a different one scrolls back to the top.
    shown: Option<(i32, String, i32, i32)>,
    /// A running library refresh: (done, total).
    pub(super) refresh: Option<(usize, usize)>,
}

/// The page's chapter rows. Section 1 times are uploads, section 2 reads.
fn facts(view: &View, snapshot: &ReaderState, items: &[ItemRow]) -> Vec<Fact> {
    let clock = crate::timeline::TimelineClock::now();
    view.rows
        .iter()
        .zip(items)
        .enumerate()
        .filter_map(|(row, (entry, item))| {
            let Row::Chapter(chapter) = entry else {
                return None;
            };
            let at = if view.section == 2 {
                snapshot
                    .history
                    .iter()
                    .find(|h| h.chapter == chapter.id)
                    .map(|h| h.read_at)
            } else {
                chapter.uploaded_at
            };
            let day = clock.day(at);
            let progress = snapshot.progress.iter().find(|p| p.chapter == chapter.id);
            let page = progress
                .filter(|p| !p.completed && !p.anchor.is_cleared())
                .map(|p| p.anchor.page_index);
            let pages = view
                .downloads
                .jobs
                .iter()
                .find(|j| j.chapter == chapter.id)
                .map(|j| j.pages.len())
                .filter(|n| *n > 0);
            Some(Fact {
                row,
                series: serde_json::to_string(&chapter.id.series).unwrap_or_default(),
                series_title: item.title.to_string(),
                artwork_key: item.artwork_key.to_string(),
                chapter_title: chapter.title.clone(),
                number: chapter.number.clone(),
                day_key: day.as_ref().map(|(key, _)| *key),
                day: item.group.to_string(),
                date: day.map(|(_, date)| date).unwrap_or_default(),
                time: item.timestamp.to_string(),
                read: progress.is_some_and(|p| p.completed),
                download_state: item.download_state,
                download_progress: item.download_progress,
                page,
                pages,
            })
        })
        .collect()
}

/// "Ch. 87" for chapter labels in actions and the shelf.
fn short_label(chapter: &Chapter) -> String {
    match &chapter.number {
        Some(number) => format!("{} {number}", text::tr("Ch.")),
        None => chapter.title.clone(),
    }
}

/// The session's newest chapter, at its place, or the next one once it is
/// finished. `None` when it is finished and nothing follows (caught up).
fn resume(snapshot: &ReaderState, newest: &ChapterId) -> Option<Resume> {
    let chapter = snapshot.chapters.iter().find(|c| &c.id == newest)?;
    let progress = snapshot.progress.iter().find(|p| &p.chapter == newest);
    if progress.is_none_or(|p| !p.completed) {
        let page = progress
            .filter(|p| !p.anchor.is_cleared())
            .map(|p| p.anchor.page_index + 1);
        return Some(Resume {
            chapter: chapter.id.clone(),
            headline: short_label(chapter),
            detail: page
                .map(|page| format!("{} {page}", text::tr("Page")))
                .unwrap_or_else(|| text::tr("Start")),
            progress: 0.,
        });
    }
    let mut series: Vec<&Chapter> = snapshot
        .chapters
        .iter()
        .filter(|c| c.id.series == chapter.id.series)
        .collect();
    series.sort_by(|a, b| reader_open::reading_order(a, b));
    let at = series.iter().position(|c| c.id == chapter.id)?;
    let next = series.get(at + 1)?;
    Some(Resume {
        chapter: next.id.clone(),
        headline: format!("{} {}", text::tr("Next:"), short_label(next)),
        detail: text::tr("Ready"),
        progress: 0.,
    })
}

/// "Download all" skips chapters already on disk or waiting to download.
fn not_queued(
    states: &BTreeMap<ChapterId, reader_downloads::JobStatus>,
    chapters: Vec<ChapterId>,
) -> Vec<ChapterId> {
    use reader_downloads::JobStatus::{Complete, Queued, Running};
    chapters
        .into_iter()
        .filter(|c| !matches!(states.get(c), Some(Complete | Queued | Running)))
        .collect()
}

fn chapter_of(view: &View, row: usize) -> Option<ChapterId> {
    match view.rows.get(row) {
        Some(Row::Chapter(chapter)) => Some(chapter.id.clone()),
        _ => None,
    }
}

/// Publish Updates (section 1) or History (section 2). Called by `render`
/// once it has built the rows and their `ItemRow`s.
pub(super) fn publish(view: &mut View, snapshot: &ReaderState, ui: &AppWindow, items: &[ItemRow]) {
    let bridge = ui.global::<TimelineBridge>();
    let history = view.section == 2;
    let appearance = ui.global::<Appearance>();
    let all = facts(view, snapshot, items);
    let filter = if history { 0 } else { view.timeline.filter };
    let facts: Vec<Fact> = all
        .iter()
        .filter(|f| groups::passes(f, filter))
        .cloned()
        .collect();
    let timeline = history && appearance.get_history_view() == 1;
    // The shelf shows only outside a search and when it is not hidden.
    let shelf_rows = if history && view.query.is_empty() && !appearance.get_history_shelf_hidden() {
        groups::shelf(&facts)
    } else {
        Vec::new()
    };
    let shelf: Vec<(usize, Option<Resume>)> = shelf_rows
        .iter()
        .map(|&i| {
            let resume = chapter_of(view, facts[i].row).and_then(|c| resume(snapshot, &c));
            (i, resume)
        })
        .collect();
    let top = if shelf.is_empty() { 0. } else { SHELF_TOP };
    let (lines, end) = if !history {
        groups::updates(
            &facts,
            appearance.get_updates_grouping() == 0,
            &view.timeline.expansion,
            top,
        )
    } else if timeline {
        groups::history_timeline(&facts, top)
    } else {
        groups::history_cards(&facts, &view.timeline.expansion, top)
    };
    let mut entries: Vec<TimelineEntry> = lines
        .iter()
        .enumerate()
        .map(|(index, line)| entry(view, snapshot, &facts, line, index, history))
        .collect();
    // A day's section runs to the next day's heading (or the list's end).
    let day_starts: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].kind == Kind::Day)
        .collect();
    for (k, &i) in day_starts.iter().enumerate() {
        let next = day_starts.get(k + 1).map_or(end, |&j| lines[j].y);
        entries[i].span = next - lines[i].y;
    }
    let days: Vec<TimelineEntry> = day_starts.iter().map(|&i| entries[i].clone()).collect();
    let shelf_entries: Vec<ShelfEntry> = shelf
        .iter()
        .map(|(i, resume)| {
            let fact = &facts[*i];
            let progress = groups::progress(fact).unwrap_or(0.);
            match resume {
                Some(resume) if fact.page.is_some() => ShelfEntry {
                    title: fact.series_title.clone().into(),
                    headline: resume.headline.clone().into(),
                    detail: match (fact.page, fact.pages) {
                        (Some(page), Some(pages)) => {
                            format!(
                                "{} {} {} {pages}",
                                text::tr("Page"),
                                page + 1,
                                text::tr("of")
                            )
                        }
                        _ => resume.detail.clone(),
                    }
                    .into(),
                    progress: progress.max(resume.progress),
                    caught_up: false,
                    poster: poster(items, fact),
                    artwork_key: fact.artwork_key.clone().into(),
                },
                Some(resume) => ShelfEntry {
                    title: fact.series_title.clone().into(),
                    headline: resume.headline.clone().into(),
                    detail: resume.detail.clone().into(),
                    progress: 0.,
                    caught_up: false,
                    poster: poster(items, fact),
                    artwork_key: fact.artwork_key.clone().into(),
                },
                None => ShelfEntry {
                    title: fact.series_title.clone().into(),
                    headline: text::tr("Caught up").into(),
                    detail: String::new().into(),
                    progress: 0.,
                    caught_up: true,
                    poster: poster(items, fact),
                    artwork_key: fact.artwork_key.clone().into(),
                },
            }
        })
        .collect();
    let shown = (
        view.section,
        view.query.clone(),
        filter,
        appearance.get_history_view(),
    );
    if view.timeline.shown.as_ref() != Some(&shown) {
        bridge.set_reset_serial(bridge.get_reset_serial().wrapping_add(1));
        view.timeline.shown = Some(shown);
    }
    // Update in place so the lines below an expanding group can slide.
    let model = bridge.get_entries();
    match model.as_any().downcast_ref::<VecModel<TimelineEntry>>() {
        Some(existing) => existing.set_vec(entries),
        None => bridge.set_entries(ModelRc::new(VecModel::from(entries))),
    }
    bridge.set_days(ModelRc::new(VecModel::from(days)));
    bridge.set_list_top(top);
    bridge.set_content_height(end);
    bridge.set_shelf(ModelRc::new(VecModel::from(shelf_entries)));
    bridge.set_filter(filter);
    bridge.set_filter_counts(ModelRc::new(VecModel::from(
        groups::filter_counts(&all).to_vec(),
    )));
    let unread = all.iter().filter(|f| !f.read).count();
    bridge.set_count_label(if history || unread == 0 {
        "".into()
    } else {
        format!("{unread} {}", text::tr("new")).into()
    });
    bridge.set_status_line(checked_line(snapshot).into());
    bridge.set_query(view.query.clone().into());
    if !history {
        bridge.set_search_open(false);
    }
    publish_refresh(&view.timeline, ui);
    view.timeline.all = all;
    view.timeline.facts = facts;
    view.timeline.lines = lines;
    view.timeline.shelf = shelf;
}

fn poster(items: &[ItemRow], fact: &Fact) -> slint::Image {
    items
        .get(fact.row)
        .map(|item| item.poster.clone())
        .unwrap_or_default()
}

/// "Checked Today 14:02 · 46 titles".
fn checked_line(snapshot: &ReaderState) -> String {
    let titles = text::titles_label(snapshot.library.len());
    reader_domain::profile::kv::get_str("reader:last-update")
        .and_then(|s| s.parse::<i64>().ok())
        .map(|t| {
            let stamp = crate::timeline::TimelineClock::now().history(t);
            format!(
                "{} {} {} · {titles}",
                text::tr("Checked"),
                stamp.group,
                stamp.timestamp
            )
        })
        .unwrap_or_else(|| text::tr("Refresh your library to check for new chapters."))
}

pub(super) fn publish_refresh(state: &State, ui: &AppWindow) {
    let bridge = ui.global::<TimelineBridge>();
    bridge.set_refreshing(state.refresh.is_some());
    let (done, total) = state.refresh.unwrap_or_default();
    bridge.set_refresh_done(done as i32);
    bridge.set_refresh_total(total as i32);
}

fn entry(
    view: &View,
    snapshot: &ReaderState,
    facts: &[Fact],
    line: &Line,
    index: usize,
    history: bool,
) -> TimelineEntry {
    let first = &facts[line.facts[0]];
    let mut out = TimelineEntry {
        kind: line.kind as i32,
        index: index as i32,
        y: line.y,
        height: line.height,
        expanded: line.expanded,
        last: line.last,
        progress: -1.,
        count: line.facts.len() as i32,
        poster: Default::default(),
        artwork_key: first.artwork_key.clone().into(),
        ..Default::default()
    };
    let unread = line.facts.iter().filter(|&&i| !facts[i].read).count();
    match line.kind {
        Kind::Day => {
            out.title = first.day.clone().into();
            out.subtitle = if history && view.section == 2 && line.height < 40. {
                first.date.clone()
            } else {
                groups::day_subtitle(facts, &line.facts)
            }
            .into();
            out.has_unread = unread > 0;
            out.artwork_key = "".into();
        }
        Kind::GroupHeader => {
            out.title = first.series_title.clone().into();
            let (emphasis, rest) = if history {
                groups::session_summary(facts, &line.facts)
            } else {
                groups::updates_summary(facts, &line.facts)
            };
            out.emphasis = emphasis.into();
            out.subtitle = rest.into();
            out.unread = unread as i32;
            if history && let Some(progress) = groups::progress(first) {
                out.progress = progress;
            }
        }
        Kind::Chapter => {
            out.subtitle = first.chapter_title.clone().into();
            out.read = first.read;
            out.download_state = first.download_state;
            out.download_progress = first.download_progress;
            if history {
                let page = match (first.page, first.pages) {
                    (Some(page), Some(pages)) => format!(
                        " · {} {} {} {pages}",
                        text::tr("page"),
                        page + 1,
                        text::tr("of")
                    ),
                    (Some(page), None) => format!(" · {} {}", text::tr("page"), page + 1),
                    _ => String::new(),
                };
                out.time = format!("{}{page}", first.time).into();
            }
        }
        Kind::ShowMore => {
            out.count = line.hidden as i32;
        }
        Kind::GroupActions => {
            out.action = if history {
                chapter_of(view, first.row)
                    .and_then(|newest| {
                        let resume = resume(snapshot, &newest)?;
                        // "Resume Ch. 87" at its place, or "Next: Ch. 88".
                        Some(if resume.chapter == newest {
                            format!("{} {}", text::tr("Resume"), resume.headline)
                        } else {
                            resume.headline
                        })
                    })
                    .unwrap_or_default()
            } else {
                groups::first_unread(facts, &line.facts)
                    .map(|i| {
                        let fact = &facts[i];
                        match &fact.number {
                            Some(number) => format!("{} {number}", text::tr("Read")),
                            None => text::tr("Read first new"),
                        }
                    })
                    .unwrap_or_default()
            }
            .into();
        }
        Kind::Single => {
            out.title = first.series_title.clone().into();
            out.subtitle = first.chapter_title.clone().into();
            out.read = first.read;
            out.download_state = first.download_state;
            out.download_progress = first.download_progress;
            if history {
                out.time = first.time.clone().into();
                if let Some(progress) = groups::progress(first) {
                    out.progress = progress;
                }
            }
        }
        Kind::TimelineItem => {
            out.title = first.series_title.clone().into();
            out.time = first.time.clone().into();
            if line.facts.len() > 1 {
                out.emphasis = format!("{} {}", text::tr("Read"), line.facts.len()).into();
                out.subtitle = groups::range_label(facts, &line.facts)
                    .unwrap_or_default()
                    .into();
            } else {
                let page = match (first.page, first.pages) {
                    (Some(page), Some(pages)) => {
                        format!(" · {} {}/{pages}", text::tr("page"), page + 1)
                    }
                    (Some(page), None) => format!(" · {} {}", text::tr("page"), page + 1),
                    _ => String::new(),
                };
                out.subtitle = format!("{}{page}", first.chapter_title).into();
            }
        }
    }
    out
}

impl Controller {
    /// The chapters behind a line, in display order.
    fn timeline_chapters(&self, index: usize) -> Vec<ChapterId> {
        let view = self.view();
        let state = &view.timeline;
        state
            .lines
            .get(index)
            .map(|line| {
                line.facts
                    .iter()
                    .filter_map(|&i| state.facts.get(i))
                    .filter_map(|fact| chapter_of(&view, fact.row))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn timeline_row(&self, index: usize) -> Option<usize> {
        let view = self.view();
        let line = view.timeline.lines.get(index)?;
        view.timeline.facts.get(*line.facts.first()?).map(|f| f.row)
    }

    /// Mark chapters read through the same path as the per-chapter action, so
    /// history, downloads cleanup and sync stay correct.
    fn timeline_mark_read(self: &Arc<Self>, ui: &AppWindow, chapters: Vec<ChapterId>) {
        if chapters.is_empty() {
            return;
        }
        let mut newly_read = Vec::new();
        let result = self
            .services
            .domain
            .update_manual_progress(|state| {
                newly_read = chapter_batch::mark_read(state, &chapters, now());
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
    }

    /// Queue chapters that are not downloaded or waiting yet.
    fn timeline_download(self: &Arc<Self>, ui: &AppWindow, chapters: Vec<ChapterId>) {
        let targets = not_queued(&self.view().download_states, chapters);
        if !targets.is_empty() {
            self.download_many(ui, targets);
        }
    }

    fn timeline_entry_action(self: &Arc<Self>, ui: &AppWindow, index: usize, action: &str) {
        let (key, size, history) = {
            let view = self.view();
            let Some(line) = view.timeline.lines.get(index) else {
                return;
            };
            (line.key.clone(), line.facts.len(), view.section == 2)
        };
        match action {
            "toggle" => {
                // A header covers its whole group.
                groups::toggle(&mut self.view().timeline.expansion, &key, size, history);
                self.render(ui);
            }
            "more" => {
                self.view().timeline.expansion.insert(key, Expansion::Full);
                self.render(ui);
            }
            "mark-read" | "mark-day-read" => {
                let chapters = self.timeline_chapters(index);
                self.timeline_mark_read(ui, chapters);
            }
            "download-all" => {
                let chapters = self.timeline_chapters(index);
                self.timeline_download(ui, chapters);
            }
            "read-first" => {
                let target = {
                    let view = self.view();
                    let state = &view.timeline;
                    state.lines.get(index).and_then(|line| {
                        groups::first_unread(&state.facts, &line.facts)
                            .and_then(|i| chapter_of(&view, state.facts[i].row))
                    })
                };
                if let Some(chapter) = target {
                    self.open_chapter(ui, chapter);
                }
            }
            "resume" => {
                let target = self.timeline_row(index).and_then(|row| {
                    let chapter = chapter_of(&self.view(), row)?;
                    let snapshot = self.services.domain.snapshot().ok()?;
                    resume(&snapshot, &chapter).map(|r| r.chapter)
                });
                if let Some(chapter) = target {
                    self.open_chapter(ui, chapter);
                }
            }
            "remove-session" => {
                // One domain update removes every read of the session.
                let chapters = self.timeline_chapters(index);
                if let Err(e) = self.services.domain.update(|state| {
                    state.history.retain(|h| !chapters.contains(&h.chapter));
                    Ok(())
                }) {
                    ui.set_status(error(e).into());
                }
                self.render(ui);
            }
            _ => {
                // Per-chapter actions keep their existing paths.
                if let Some(row) = self.timeline_row(index) {
                    self.item_action(ui, row, action);
                }
            }
        }
    }

    fn timeline_menu_action(self: &Arc<Self>, ui: &AppWindow, action: &str) {
        match action {
            "layout" => self.render(ui),
            "clear-history" => ui.set_clear_history_confirm_open(true),
            "mark-all-read" | "download-all-unread" => {
                let chapters: Vec<ChapterId> = {
                    let view = self.view();
                    view.timeline
                        .all
                        .iter()
                        .filter(|f| !f.read)
                        .filter_map(|f| chapter_of(&view, f.row))
                        .collect()
                };
                if action == "mark-all-read" {
                    self.timeline_mark_read(ui, chapters);
                } else {
                    self.timeline_download(ui, chapters);
                }
            }
            _ => {}
        }
    }

    fn timeline_shelf_picked(self: &Arc<Self>, ui: &AppWindow, index: usize) {
        let target = {
            let view = self.view();
            view.timeline
                .shelf
                .get(index)
                .and_then(|(_, resume)| resume.as_ref().map(|r| r.chapter.clone()))
        };
        if let Some(chapter) = target {
            self.open_chapter(ui, chapter);
        }
    }

    /// Artwork for the lines in [first, last): their rows' series.
    fn timeline_artwork(&self, ui: &AppWindow, first: usize, last: usize) {
        let rows: BTreeSet<usize> = {
            let view = self.view();
            let state = &view.timeline;
            (first..last.min(state.lines.len()))
                .filter_map(|i| state.lines[i].facts.first())
                .filter_map(|&f| state.facts.get(f).map(|fact| fact.row))
                .chain(
                    // The shelf's covers come along with the first lines.
                    state
                        .shelf
                        .iter()
                        .filter(|_| first == 0)
                        .filter_map(|(f, _)| state.facts.get(*f).map(|fact| fact.row)),
                )
                .collect()
        };
        for row in rows {
            self.request_artwork(ui, row as i32, row as i32 + 1);
        }
    }
}

/// The Updates and History page callbacks.
pub(super) fn wire(app: &Arc<Controller>, ui: &AppWindow) {
    let bridge = ui.global::<TimelineBridge>();
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
    on!(on_entry_picked, |app, ui, index| {
        if let Some(row) = app.timeline_row(index as usize) {
            ui.invoke_open_item(row as i32);
        }
    });
    on!(on_series_picked, |app, ui, index| {
        if let Some(row) = app.timeline_row(index as usize) {
            ui.invoke_open_series(row as i32);
        }
    });
    on!(on_entry_action, |app, ui, index, action| app
        .timeline_entry_action(&ui, index as usize, &action));
    on!(on_filter_picked, |app, ui, filter| {
        app.view().timeline.filter = filter.clamp(0, 2);
        app.render(&ui);
    });
    on!(on_refresh, |_app, ui| ui.invoke_browse("refresh".into()));
    on!(on_menu_action, |app, ui, action| app
        .timeline_menu_action(&ui, &action));
    on!(on_search, |app, ui, query| {
        app.view().query = query.to_string();
        app.render(&ui);
    });
    on!(on_shelf_picked, |app, ui, index| app
        .timeline_shelf_picked(&ui, index as usize));
    on!(on_request_artwork, |app, ui, first, last| app
        .timeline_artwork(
            &ui,
            first.max(0) as usize,
            last.max(0) as usize
        ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use reader_domain::{HistoryEntry, LibraryEntry, PublicationStatus, SeriesId};

    #[test]
    fn download_all_skips_downloaded_and_waiting_chapters() {
        use reader_downloads::JobStatus;
        let series = SeriesId::new("s", "a");
        let ids: Vec<_> = (0..5)
            .map(|n| ChapterId::new(series.clone(), n.to_string()))
            .collect();
        let states = BTreeMap::from([
            (ids[0].clone(), JobStatus::Complete),
            (ids[1].clone(), JobStatus::Queued),
            (ids[2].clone(), JobStatus::Running),
            (ids[3].clone(), JobStatus::Failed),
        ]);
        assert_eq!(
            not_queued(&states, ids.clone()),
            vec![ids[3].clone(), ids[4].clone()]
        );
    }

    /// Updates and History through the real controller: grouping on the
    /// page, group and day Mark read, expansion and Remove session.
    #[test]
    fn updates_and_history_group_and_act_on_the_domain() {
        const CHILD: &str = "NOVA_READER_TIMELINE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::timeline_page::tests::updates_and_history_group_and_act_on_the_domain",
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
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().unwrap();
        let services = Services::open(&dir.path().join("data")).unwrap();
        let ui = AppWindow::new().unwrap();
        crate::appearance::init(&ui).unwrap();
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
            view: Mutex::new(View::default()),
            generation: AtomicU64::new(0),
            cancel: Mutex::default(),
            images,
            download_serial: Mutex::default(),
            active_image: Mutex::default(),
            continuation_work: Default::default(),
            metadata_refresh: Arc::default(),
        });
        wire(&app, &ui);
        let today = now();
        let series = |id: &str| Series {
            publication_status: PublicationStatus::Ongoing,
            id: SeriesId::new("local-test", id),
            title: id.to_uppercase(),
            description: String::new(),
            authors: Vec::new(),
            genres: Vec::new(),
            cover_url: None,
            canonical_url: None,
        };
        let chapter = |series: &str, n: i64, at: i64| Chapter {
            id: ChapterId::new(SeriesId::new("local-test", series), n.to_string()),
            title: format!("Chapter {n}"),
            label: format!("Chapter {n}"),
            number: Some(n.to_string()),
            volume: None,
            source_order: n,
            language: None,
            scanlator: None,
            uploaded_at: Some(at),
        };
        app.services
            .domain
            .update(|state| {
                state.series = vec![series("a"), series("b")];
                // A burst of 14 in "a" and one chapter of "b", both today.
                state.chapters = (41..=54).map(|n| chapter("a", n, today - 60 + n)).collect();
                state.chapters.push(chapter("b", 7, today - 100));
                for id in ["a", "b"] {
                    state.library.push(LibraryEntry {
                        reading_status: Default::default(),
                        series: SeriesId::new("local-test", id),
                        categories: Default::default(),
                        preferences: None,
                        bookmarked: false,
                        added_at: today,
                    });
                }
                // History: three of "a" in one session, one of "b".
                for (n, at) in [(41, today - 30), (42, today - 20), (43, today - 10)] {
                    state.history.push(HistoryEntry {
                        chapter: ChapterId::new(SeriesId::new("local-test", "a"), n.to_string()),
                        read_at: at,
                    });
                }
                state.history.push(HistoryEntry {
                    chapter: ChapterId::new(SeriesId::new("local-test", "b"), "7"),
                    read_at: today - 5,
                });
                Ok(())
            })
            .unwrap();
        let kinds = |ui: &AppWindow| {
            ui.global::<TimelineBridge>()
                .get_entries()
                .iter()
                .map(|e| e.kind)
                .collect::<Vec<_>>()
        };
        app.view().section = 1;
        app.render(&ui);
        // Day, the "a" card peeking at 3 with "Show 11 more", then "b".
        assert_eq!(kinds(&ui), [0, 1, 2, 2, 2, 3, 4, 5]);
        let bridge = ui.global::<TimelineBridge>();
        let header = bridge.get_entries().row_data(1).unwrap();
        assert_eq!(header.emphasis, "14 new");
        assert_eq!(header.subtitle, "Chapters 41–54");
        assert_eq!(bridge.get_entries().row_data(5).unwrap().count, 11);
        assert_eq!(bridge.get_entries().row_data(6).unwrap().action, "Read 41");
        assert_eq!(bridge.get_count_label(), "15 new");
        assert_eq!(
            bridge.get_filter_counts().iter().collect::<Vec<_>>(),
            [15, 15, 0]
        );
        // Show more, then a re-render keeps the group open.
        bridge.invoke_entry_action(5, "more".into());
        app.render(&ui);
        assert_eq!(kinds(&ui).iter().filter(|k| **k == 2).count(), 14);
        // Header taps: back to 3, then the header alone.
        bridge.invoke_entry_action(1, "toggle".into());
        assert_eq!(kinds(&ui).iter().filter(|k| **k == 2).count(), 3);
        bridge.invoke_entry_action(1, "toggle".into());
        assert_eq!(kinds(&ui), [0, 1, 5]);
        bridge.invoke_entry_action(1, "toggle".into());
        // Mark the group read: exactly its 14 chapters.
        let actions = kinds(&ui).iter().position(|k| *k == 4).unwrap();
        bridge.invoke_entry_action(actions as i32, "mark-read".into());
        let read = |app: &Controller| {
            app.services
                .domain
                .snapshot()
                .unwrap()
                .progress
                .iter()
                .filter(|p| p.completed)
                .count()
        };
        assert_eq!(read(&app), 14);
        // Recent read chapters stay listed, muted; Unread hides the group.
        assert_eq!(bridge.get_count_label(), "1 new");
        bridge.invoke_filter_picked(1);
        assert_eq!(kinds(&ui), [0, 5]);
        // Mark the day read from its heading.
        bridge.invoke_entry_action(0, "mark-day-read".into());
        assert_eq!(read(&app), 15);
        bridge.invoke_filter_picked(0);
        // "By day only" is a flat list.
        ui.global::<Appearance>()
            .invoke_setting_changed("updates_grouping".into(), 1);
        bridge.invoke_menu_action("layout".into());
        assert_eq!(kinds(&ui).iter().filter(|k| **k == 5).count(), 15);
        // History: one session of three and a single, collapsed by default.
        app.view().section = 2;
        app.render(&ui);
        assert_eq!(bridge.get_shelf().row_count(), 2);
        let entries: Vec<_> = bridge.get_entries().iter().collect();
        assert_eq!(
            entries.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [0, 5, 1]
        );
        assert_eq!(entries[2].emphasis, "3 chapters");
        assert!(entries[2].subtitle.starts_with("Chapters 41–43"));
        bridge.invoke_entry_action(2, "toggle".into());
        assert_eq!(kinds(&ui), [0, 5, 1, 2, 2, 2, 4]);
        // Timeline: the same sessions.
        ui.global::<Appearance>()
            .invoke_setting_changed("history_view".into(), 1);
        bridge.invoke_menu_action("layout".into());
        assert_eq!(kinds(&ui), [0, 6, 6]);
        // Remove the "a" session in one update.
        bridge.invoke_entry_action(2, "remove-session".into());
        let history = app.services.domain.snapshot().unwrap().history;
        assert_eq!(history.len(), 1);
        assert_eq!(kinds(&ui), [0, 6]);
        // Searching hides the shelf.
        bridge.invoke_search("zzz".into());
        assert_eq!(bridge.get_shelf().row_count(), 0);
        assert_eq!(kinds(&ui), Vec::<i32>::new());
    }
}
