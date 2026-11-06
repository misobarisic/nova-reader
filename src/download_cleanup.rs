//! Device-local download retention. Only explicit local unread → read commits
//! call this policy; enabling it and incoming sync never scan old downloads.
use crate::{AppWindow, DownloadCleanupSettings, EntryCategoryRow};
use reader_domain::{ChapterId, DomainStore, ReaderState};
use reader_downloads::DownloadStore;
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::collections::BTreeSet;

pub const SETTINGS_KEY: &str = "reader:download-cleanup";
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub excluded_categories: BTreeSet<String>,
}
impl Settings {
    pub fn load() -> Result<Self, String> {
        reader_domain::profile::kv::try_get_str(SETTINGS_KEY)
            .map_err(|e| e.to_string())?
            .map(|json| serde_json::from_str(&json).map_err(|e| e.to_string()))
            .transpose()
            .map(Option::unwrap_or_default)
    }
    fn save(&self) -> Result<(), String> {
        reader_domain::profile::kv::try_set_str(
            SETTINGS_KEY,
            &serde_json::to_string(self).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
    pub fn allows(&self, state: &ReaderState, chapter: &ChapterId) -> bool {
        self.enabled
            && chapter.series.source_id != "local"
            && state
                .progress
                .iter()
                .any(|p| p.chapter == *chapter && p.completed)
            && !state.library.iter().any(|entry| {
                entry.series == chapter.series
                    && entry
                        .categories
                        .iter()
                        .any(|id| self.excluded_categories.contains(id))
            })
    }
}

pub(crate) fn refresh(ui: &AppWindow, state: &ReaderState) {
    let settings = Settings::load().unwrap_or_default();
    let global = ui.global::<DownloadCleanupSettings>();
    global.set_enabled(settings.enabled);
    global.set_categories(ModelRc::new(VecModel::from(
        state
            .categories
            .iter()
            .map(|c| EntryCategoryRow {
                id: c.id.clone().into(),
                label: c.name.clone().into(),
                checked: settings.excluded_categories.contains(&c.id),
            })
            .collect::<Vec<_>>(),
    )));
}
pub(crate) fn init(ui: &AppWindow, domain: DomainStore) {
    let weak = ui.as_weak();
    ui.global::<DownloadCleanupSettings>()
        .on_changed(move |key| {
            let Some(ui) = weak.upgrade() else { return };
            let result = (|| {
                let state = domain.snapshot().map_err(|e| e.to_string())?;
                let mut settings = Settings::load()?;
                // A deleted category has no membership; remove stale IDs on the
                // next deliberate edit. Renaming/reordering never changes identity.
                settings
                    .excluded_categories
                    .retain(|id| state.categories.iter().any(|c| c.id == *id));
                if key.is_empty() {
                    settings.enabled = !settings.enabled;
                } else if state.categories.iter().any(|c| c.id == key.as_str())
                    && !settings.excluded_categories.remove(key.as_str())
                {
                    settings.excluded_categories.insert(key.to_string());
                }
                settings.save()?;
                Ok::<_, String>(state)
            })();
            match result {
                Ok(state) => refresh(&ui, &state),
                Err(error) => {
                    ui.set_status(
                        format!(
                            "{}: {error}",
                            nova_reader_ui::backend_text::tr(
                                "Could not save download cleanup settings"
                            )
                        )
                        .into(),
                    );
                    if let Ok(state) = domain.snapshot() {
                        refresh(&ui, &state);
                    }
                }
            }
        });
}

/// Caller has already committed the read transition. Run on a worker; never
/// changes progress, History, library membership or source files.
pub(crate) fn after_read(domain: &DomainStore, downloads: &DownloadStore, chapters: &[ChapterId]) {
    let result = (|| {
        let settings = Settings::load()?;
        if !settings.enabled {
            return Ok(());
        }
        let state = domain.snapshot().map_err(|e| e.to_string())?;
        for chapter in chapters {
            if settings.allows(&state, chapter) {
                downloads
                    .remove_completed_remote(chapter)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok::<_, String>(())
    })();
    if let Err(error) = result {
        eprintln!("Nova Reader: download cleanup failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reader_domain::{
        Category, LibraryEntry, ReadingMode, ReadingProgress, ResumeAnchor, SeriesId,
    };
    #[test]
    fn committed_manual_and_reader_completion_cleanup_survives_restart() {
        const MODE: &str = "NOVA_CLEANUP_RESTART";
        const DIR: &str = "NOVA_CLEANUP_DIR";
        if let Ok(mode) = std::env::var(MODE) {
            let directory = std::path::PathBuf::from(std::env::var(DIR).unwrap());
            let domain = DomainStore::open(&directory).unwrap();
            let downloads = DownloadStore::open(directory.join("chapters"), 1_000_000).unwrap();
            let chapter = ChapterId::new(SeriesId::new("fixture", "series"), "chapter");
            if mode == "write" {
                domain
                    .update(|state| {
                        state.series.push(reader_domain::Series {
                            publication_status: Default::default(),
                            id: chapter.series.clone(),
                            title: "Fixture".into(),
                            description: String::new(),
                            authors: vec![],
                            genres: vec![],
                            cover_url: None,
                            canonical_url: None,
                        });
                        state.chapters.push(reader_domain::Chapter {
                            id: chapter.clone(),
                            title: "Chapter".into(),
                            label: "1".into(),
                            number: None,
                            volume: None,
                            source_order: 0,
                            language: None,
                            scanlator: None,
                            uploaded_at: None,
                        });
                        Ok(())
                    })
                    .unwrap();
                Settings {
                    enabled: true,
                    excluded_categories: BTreeSet::from(["stable-id".into()]),
                }
                .save()
                .unwrap();
                let mut png = std::io::Cursor::new(Vec::new());
                image::DynamicImage::new_rgba8(2, 2)
                    .write_to(&mut png, image::ImageFormat::Png)
                    .unwrap();
                let add_download = || {
                    downloads
                        .enqueue(
                            chapter.clone(),
                            vec![reader_downloads::PageResource {
                                id: "page".into(),
                                url: "https://fixture.invalid/page".into(),
                                headers: Default::default(),
                            }],
                        )
                        .unwrap();
                    downloads
                        .run(&chapter, |_| Ok(png.get_ref().clone()))
                        .unwrap();
                };
                add_download();
                domain
                    .update_manual_progress(|state| {
                        crate::app::toggle_chapter_read(state, chapter.clone(), 10)
                    })
                    .unwrap();
                assert!(domain.snapshot().unwrap().history.is_empty());
                after_read(&domain, &downloads, std::slice::from_ref(&chapter));
                assert!(downloads.job(&chapter).is_err());
                domain
                    .update_manual_progress(|state| {
                        crate::app::toggle_chapter_read(state, chapter.clone(), 11)
                    })
                    .unwrap();
                add_download();
                let writer = crate::progress_checkpoint::ProgressWriter::new_with_cleanup(
                    domain.clone(),
                    downloads.clone(),
                )
                .unwrap();
                writer
                    .flush(Some(ReadingProgress {
                        chapter: chapter.clone(),
                        anchor: ResumeAnchor::new("page", 0, 0.).unwrap(),
                        mode: ReadingMode::Webtoon,
                        last_read_at: 12,
                        completed: true,
                    }))
                    .unwrap();
                assert!(downloads.job(&chapter).is_err());
            }
            assert!(Settings::load().unwrap().enabled);
            assert!(
                Settings::load()
                    .unwrap()
                    .excluded_categories
                    .contains("stable-id")
            );
            assert!(domain.snapshot().unwrap().progress[0].completed);
            assert_eq!(domain.snapshot().unwrap().history.len(), 1);
            assert!(downloads.job(&chapter).is_err());
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let name = concat!(
            module_path!(),
            "::committed_manual_and_reader_completion_cleanup_survives_restart"
        )
        .split_once("::")
        .unwrap()
        .1;
        for mode in ["write", "read"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--nocapture"])
                .env(MODE, mode)
                .env(DIR, directory.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
    }

    #[test]
    fn default_off_local_protection_and_any_stable_category_exclusion() {
        let chapter = ChapterId::new(SeriesId::new("source", "series"), "chapter");
        let mut state = ReaderState::default();
        state.progress.push(ReadingProgress {
            chapter: chapter.clone(),
            anchor: ResumeAnchor::new("page", 0, 0.).unwrap(),
            mode: ReadingMode::Webtoon,
            last_read_at: 1,
            completed: true,
        });
        let mut settings = Settings::default();
        assert!(!settings.allows(&state, &chapter));
        settings.enabled = true;
        assert!(settings.allows(&state, &chapter));
        state.categories.push(Category {
            id: "stable".into(),
            name: "Favorites".into(),
        });
        state.library.push(LibraryEntry {
            reading_status: Default::default(),
            series: chapter.series.clone(),
            categories: BTreeSet::from(["other".into(), "stable".into()]),
            preferences: None,
            bookmarked: false,
            added_at: 0,
        });
        settings.excluded_categories.insert("stable".into());
        assert!(!settings.allows(&state, &chapter));
        state.categories[0].name = "Renamed".into();
        assert!(!settings.allows(&state, &chapter));
        state.library[0].categories.clear();
        assert!(settings.allows(&state, &chapter));
        state.progress[0].completed = false;
        assert!(!settings.allows(&state, &chapter));
        let local = ChapterId::new(SeriesId::new("local", "series"), "chapter");
        state.progress[0].chapter = local.clone();
        state.progress[0].completed = true;
        assert!(!settings.allows(&state, &local));
        assert_eq!(
            serde_json::from_str::<Settings>("{}").unwrap(),
            Settings::default()
        );
        assert_eq!(
            serde_json::from_str::<Settings>(&serde_json::to_string(&settings).unwrap()).unwrap(),
            settings
        );
    }
}
