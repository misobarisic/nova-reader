//! Selecting several chapters on the detail screen. The selection is kept in
//! the view by stable chapter identity, so it survives row refreshes and
//! reordering; rows only mirror it. Batch actions reuse the per-chapter paths
//! and refresh the screen once at the end.
use super::*;

impl Controller {
    pub(super) fn chapter_selection(self: &Arc<Self>, ui: &AppWindow, op: &str, index: i32) {
        let row_chapter = |view: &View| {
            usize::try_from(index)
                .ok()
                .and_then(|i| view.rows.get(i))
                .and_then(|row| match row {
                    Row::Chapter(c) => Some(c.id.clone()),
                    _ => None,
                })
        };
        let selected: Vec<ChapterId> = {
            let mut view = self.view();
            match op {
                "enter" => {
                    let mut set = view.chapter_selection.take().unwrap_or_default();
                    if let Some(chapter) = row_chapter(&view) {
                        set.insert(chapter);
                    }
                    view.chapter_selection = Some(set);
                }
                "toggle" => {
                    let chapter = row_chapter(&view);
                    if let (Some(set), Some(chapter)) = (view.chapter_selection.as_mut(), chapter)
                        && !set.remove(&chapter)
                    {
                        set.insert(chapter);
                    }
                    // Unselecting the last row leaves selection mode.
                    if view.chapter_selection.as_ref().is_some_and(|set| set.is_empty()) {
                        view.chapter_selection = None;
                    }
                }
                "clear" => view.chapter_selection = None,
                "all" => {
                    // Everything the current filter shows.
                    let filter = ui.get_chapter_filter();
                    let model = ui.get_items();
                    let shown: BTreeSet<ChapterId> = view
                        .rows
                        .iter()
                        .enumerate()
                        .filter_map(|(i, row)| match row {
                            Row::Chapter(c) => model.row_data(i).filter(|item| match filter {
                                1 => !item.read,
                                2 => item.downloaded,
                                3 => item.bookmarked,
                                _ => true,
                            }).map(|_| c.id.clone()),
                            _ => None,
                        })
                        .collect();
                    if !shown.is_empty() {
                        view.chapter_selection = Some(shown);
                    }
                }
                _ => {}
            }
            if matches!(op, "mark-read" | "download" | "delete-downloads") {
                view.chapter_selection
                    .take()
                    .map(|set| set.into_iter().collect())
                    .unwrap_or_default()
            } else {
                Vec::new()
            }
        };
        match op {
            "mark-read" if !selected.is_empty() => {
                let mut newly_read = Vec::new();
                if let Err(e) = self.services.domain.update_manual_progress(|state| {
                    newly_read = chapter_batch::mark_read(state, &selected, now());
                    Ok(())
                }) {
                    ui.set_status(error(e).into());
                } else if !newly_read.is_empty() {
                    let services = self.services.clone();
                    crate::profiles::spawn(move || {
                        crate::download_cleanup::after_read(
                            &services.domain,
                            &services.downloads,
                            &newly_read,
                        )
                    });
                }
            }
            "download" if !selected.is_empty() => self.download_many(ui, selected),
            "delete-downloads" if !selected.is_empty() => {
                let targets: Vec<ChapterId> = {
                    let view = self.view();
                    selected
                        .into_iter()
                        .filter(|chapter| view.download_states.contains_key(chapter))
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
                        ui.set_status(text::tr("Downloads deleted.").into());
                        app.refresh_download_states(ui);
                    },
                );
                return;
            }
            _ => {}
        }
        self.render(ui);
    }
}

/// Mirror the view's selection onto the screen.
pub(super) fn publish(view: &View, ui: &AppWindow) {
    ui.set_chapter_selecting(view.chapter_selection.is_some());
    ui.set_chapter_selected_count(
        view.chapter_selection
            .as_ref()
            .map_or(0, |set| set.len() as i32),
    );
}
