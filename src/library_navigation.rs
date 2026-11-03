//! Session-only Library rows and device-local last category, outside progress/sync.
use super::*;
use reader_domain::{Category, ReadingStatus};
use serde::{Deserialize, Serialize};

const KEY: &str = "reader:library-navigation:v1";
const MAX_BYTES: usize = 1024 * 1024;
const MAX_LOCATIONS: usize = 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", deny_unknown_fields)]
enum Filter {
    #[default]
    All,
    Status(ReadingStatus),
    Category(String),
}
impl Filter {
    fn key(&self) -> String {
        serde_json::to_string(self).expect("Library filter serializes")
    }
}
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Position {
    pub(super) series_key: String,
    pub(super) fraction: f32,
    pub(super) scroll_y: f32,
}
impl Position {
    fn valid(&self) -> bool {
        self.series_key.len() <= 4096
            && self.fraction.is_finite()
            && (0. ..=1.).contains(&self.fraction)
            && self.scroll_y.is_finite()
            && (-1_000_000_000. ..=0.).contains(&self.scroll_y)
    }
}
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct State {
    selected: Filter,
    positions: BTreeMap<String, Position>,
    dirty: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionRecord {
    selected: Filter,
}
impl SelectionRecord {
    fn storage_json(&self) -> Result<String> {
        let json = serde_json::to_string(self).map_err(error)?;
        if json.len() > MAX_BYTES {
            return Err("Library category exceeds storage budget".into());
        }
        Ok(json)
    }
}
impl State {
    pub(super) fn load() -> Self {
        let raw = nova_storage::try_get_str(KEY).unwrap_or_else(|error| {
            eprintln!("Nova Reader: could not load Library location: {error}");
            None
        });
        Self::from_json(raw.as_deref())
    }
    fn from_json(raw: Option<&str>) -> Self {
        let Some(raw) = raw.filter(|raw| raw.len() <= MAX_BYTES) else {
            return Self::default();
        };
        let Ok(record) = serde_json::from_str::<SelectionRecord>(raw) else {
            return Self::default();
        };
        if matches!(record.selected, Filter::Status(ReadingStatus::Auto)) {
            return Self::default();
        }
        Self {
            selected: record.selected,
            ..Self::default()
        }
    }
    fn selection_record(&self) -> SelectionRecord {
        SelectionRecord {
            selected: self.selected.clone(),
        }
    }
    pub(super) fn reconcile_categories(&mut self, categories: &[Category]) {
        if self.category_index(categories) == 0 && self.selected != Filter::All {
            self.selected = Filter::All;
            self.dirty = true;
        }
    }
    pub(super) fn category_index(&self, categories: &[Category]) -> usize {
        match &self.selected {
            Filter::All => 0,
            Filter::Status(status) => ReadingStatus::BUILTINS
                .iter()
                .position(|candidate| candidate == status)
                .map_or(0, |index| index + 1),
            Filter::Category(id) => categories
                .iter()
                .position(|category| &category.id == id)
                .map_or(0, |index| ReadingStatus::BUILTINS.len() + 1 + index),
        }
    }
    pub(super) fn select(&mut self, index: usize, categories: &[Category]) {
        let selected = if index == 0 {
            Filter::All
        } else if let Some(status) = ReadingStatus::BUILTINS.get(index - 1) {
            Filter::Status(*status)
        } else {
            categories
                .get(index.saturating_sub(ReadingStatus::BUILTINS.len() + 1))
                .map_or(Filter::All, |category| {
                    Filter::Category(category.id.clone())
                })
        };
        if self.selected != selected {
            self.selected = selected;
            self.dirty = true;
        }
    }
    pub(super) fn position(&self, categories: &[Category]) -> Position {
        // A deleted category falls back to All without borrowing its old offset.
        let key = if self.category_index(categories) == 0 {
            Filter::All.key()
        } else {
            self.selected.key()
        };
        self.positions.get(&key).cloned().unwrap_or_default()
    }
    pub(super) fn remember(&mut self, series_key: String, fraction: f32, scroll_y: f32) {
        let position = Position {
            series_key,
            fraction,
            scroll_y,
        };
        if !position.valid() {
            return;
        }
        let key = self.selected.key();
        if self.positions.get(&key) == Some(&position)
            || (position == Position::default() && !self.positions.contains_key(&key))
        {
            return;
        }
        if self.positions.len() >= MAX_LOCATIONS
            && !self.positions.contains_key(&key)
            && let Some(oldest) = self.positions.keys().next().cloned()
        {
            self.positions.remove(&oldest);
        }
        self.positions.insert(key, position);
    }
    pub(super) fn reset_position(&mut self) {
        self.remember(String::new(), 0., 0.);
    }
}

// Row changes stay in memory. Only category selection reaches this worker;
// serialization and redb transactions never run on the scroll/UI thread.
enum Command {
    Wake,
    Barrier(mpsc::Sender<Result<()>>),
}
struct Writer {
    pending: Arc<Mutex<Option<SelectionRecord>>>,
    sender: mpsc::SyncSender<Command>,
}
static WRITER: OnceLock<std::result::Result<Writer, String>> = OnceLock::new();
impl Writer {
    fn start() -> Result<Self> {
        let pending = Arc::new(Mutex::new(None::<SelectionRecord>));
        let work = pending.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("reader-library-location".into())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    let state = work
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .take();
                    let result = if let Some(state) = state {
                        let result = state
                            .storage_json()
                            .and_then(|json| nova_storage::try_set_str(KEY, &json).map_err(error));
                        if result.is_err() {
                            let mut pending =
                                work.lock().unwrap_or_else(|error| error.into_inner());
                            if pending.is_none() {
                                *pending = Some(state);
                            }
                        }
                        result
                    } else {
                        Ok(())
                    };
                    match command {
                        Command::Wake => {
                            if let Err(error) = result {
                                eprintln!("Nova Reader: could not save Library location: {error}");
                            }
                        }
                        Command::Barrier(ack) => {
                            let _ = ack.send(result);
                        }
                    }
                }
            })
            .map_err(error)?;
        Ok(Self { pending, sender })
    }
    fn enqueue(&self, state: SelectionRecord) -> Result<()> {
        *self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(state);
        match self.sender.try_send(Command::Wake) {
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                Err("Library location writer stopped".into())
            }
        }
    }
}
fn writer() -> Result<&'static Writer> {
    WRITER
        .get_or_init(Writer::start)
        .as_ref()
        .map_err(Clone::clone)
}
pub(super) fn flush() -> Result<()> {
    let Some(writer) = WRITER.get() else {
        return Ok(());
    };
    let writer = writer.as_ref().map_err(Clone::clone)?;
    let (ack, response) = mpsc::channel();
    writer.sender.send(Command::Barrier(ack)).map_err(error)?;
    response.recv().map_err(error)?
}
impl Controller {
    pub(super) fn remember_library_position(
        &self,
        ui: &AppWindow,
        key: String,
        fraction: f32,
        scroll_y: f32,
    ) {
        let Ok(mut view) = self.view.try_lock() else {
            return;
        };
        if view.section != 0 || ui.get_section() != 0 {
            return;
        }
        view.library_navigation.remember(key, fraction, scroll_y);
    }
    pub(super) fn checkpoint_library(&self) {
        let mut view = self.view();
        if !view.library_navigation.dirty {
            return;
        }
        match writer().and_then(|writer| writer.enqueue(view.library_navigation.selection_record()))
        {
            Ok(()) => view.library_navigation.dirty = false,
            Err(error) => eprintln!("Nova Reader: could not queue Library location: {error}"),
        }
    }
    pub(super) fn select_library_category(&self, ui: &AppWindow, index: i32) {
        if let Ok(snapshot) = self.services.domain.snapshot() {
            self.view()
                .library_navigation
                .select(index.max(0) as usize, &snapshot.categories);
            self.checkpoint_library();
            self.render(ui);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn categories() -> Vec<Category> {
        vec![
            Category {
                id: "favorite-id".into(),
                name: "Favorites".into(),
            },
            Category {
                id: "other-id".into(),
                name: "Reading".into(),
            },
        ]
    }
    #[test]
    fn library_filters_and_positions_follow_stable_category_ids() {
        let categories = categories();
        let mut state = State::default();
        state.remember("all-entry".into(), 0.3, -400.);
        state.select(ReadingStatus::BUILTINS.len() + 2, &categories);
        state.remember("source-b:same-local-id".into(), 0.625, -1900.);
        let mut reordered = categories.clone();
        reordered.reverse();
        reordered[0].name = "Renamed".into();
        assert_eq!(
            state.category_index(&reordered),
            ReadingStatus::BUILTINS.len() + 1
        );
        assert_eq!(
            state.position(&reordered).series_key,
            "source-b:same-local-id"
        );
        state.select(0, &categories);
        assert_eq!(state.position(&categories).scroll_y, -400.);
        state.select(1, &categories);
        assert_eq!(
            state.position(&categories),
            Position::default(),
            "status labels cannot alias user category IDs"
        );
        state.select(ReadingStatus::BUILTINS.len() + 2, &categories);
        state.reconcile_categories(&categories[..1]);
        assert_eq!(state.category_index(&categories[..1]), 0);
        assert_eq!(state.position(&categories[..1]).series_key, "all-entry");
    }
    #[test]
    fn malformed_library_locations_do_not_become_scroll_requests() {
        for raw in [
            "null",
            "{",
            r#"{"selected":{"kind":"Status","id":"Auto"}}"#,
            r#"{"positions":{"x":{"scroll_y":15}}}"#,
        ] {
            assert_eq!(State::from_json(Some(raw)), State::default());
        }
        let mut state = State::default();
        state.remember("entry".into(), 0.5, -300.);
        let before = state.clone();
        state.remember("entry".into(), f32::NAN, -10.);
        state.remember("entry".into(), 0.5, f32::INFINITY);
        assert_eq!(state, before);
    }
    #[test]
    fn valid_long_category_id_roundtrips_with_escaped_filter_keys() {
        let categories = vec![Category {
            id: "\\\"".repeat(3000),
            name: "Imported category".into(),
        }];
        let mut state = State::default();
        let index = ReadingStatus::BUILTINS.len() + 1;
        state.select(index, &categories);
        state.remember("selected-series".into(), 0.25, -700.);
        let restored = State::from_json(Some(&state.selection_record().storage_json().unwrap()));
        assert_eq!(restored.category_index(&categories), index);
        assert_eq!(restored.position(&categories), Position::default());
        assert!(
            !state.positions.is_empty(),
            "live positions remain in memory"
        );
    }
    #[test]
    fn runtime_positions_never_enter_the_persisted_category_record() {
        let categories = categories();
        let mut state = State::default();
        for index in 0..MAX_LOCATIONS {
            state.positions.insert(
                format!("entry-{index:04}"),
                Position {
                    series_key: "\\\"".repeat(1600),
                    fraction: 0.5,
                    scroll_y: -100.,
                },
            );
        }
        state.select(ReadingStatus::BUILTINS.len() + 1, &categories);
        state.dirty = false; // The category has already been checkpointed.
        state.remember("selected-series".into(), 0.75, -2048.);
        assert!(!state.dirty, "scrolling never schedules a storage write");
        let json = state.selection_record().storage_json().unwrap();
        assert!(json.len() <= MAX_BYTES);
        let restored = State::from_json(Some(&json));
        assert_eq!(
            restored.category_index(&categories),
            ReadingStatus::BUILTINS.len() + 1
        );
        assert_eq!(restored.position(&categories), Position::default());
        assert!(
            !state.positions.is_empty(),
            "live positions remain in memory"
        );
        assert!(restored.positions.is_empty());
        assert!(!json.contains("series_key") && !json.contains("positions"));
        assert!(!json.contains("selected-series"));
    }
    #[test]
    fn only_the_library_category_survives_process_restart() {
        const MODE: &str = "READER_LIBRARY_LOCATION_TEST";
        let categories = categories();
        if let Ok(mode) = std::env::var(MODE) {
            nova_storage::init_at(&PathBuf::from(
                std::env::var_os("READER_LIBRARY_LOCATION_DIR").unwrap(),
            ));
            if mode == "write" {
                let mut state = State::load();
                state.select(ReadingStatus::BUILTINS.len() + 1, &categories);
                state.remember(
                    r#"{"source":"source-b","local_id":"same"}"#.into(),
                    0.375,
                    -1234.,
                );
                writer().unwrap().enqueue(state.selection_record()).unwrap();
                flush().unwrap();
            } else {
                let state = State::load();
                assert_eq!(
                    state.category_index(&categories),
                    ReadingStatus::BUILTINS.len() + 1
                );
                assert_eq!(state.position(&categories), Position::default());
                assert!(state.positions.is_empty(), "rows reset with the process");
            }
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        for mode in ["write", "read"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::library_navigation::tests::only_the_library_category_survives_process_restart",
                    "--nocapture",
                ])
                .env(MODE, mode)
                .env("READER_LIBRARY_LOCATION_DIR", directory.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    #[test]
    fn controller_library_roundtrip_uses_full_series_and_category_ids() {
        const CHILD: &str = "READER_LIBRARY_CONTROLLER_ID_TEST";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "app::library_navigation::tests::controller_library_roundtrip_uses_full_series_and_category_ids", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use reader_domain::{LibraryEntry, SeriesId};
        fn settle() {
            for _ in 0..32 {
                i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
            }
        }
        fn resolved(ui: &AppWindow) -> String {
            ui.get_items()
                .row_data(ui.get_library_restore_index().max(0) as usize)
                .unwrap()
                .series_key
                .to_string()
        }
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        ui.global::<Appearance>().set_library_list_view(true);
        let dir = tempfile::tempdir().unwrap();
        let services = Services::open(&dir.path().join("data")).unwrap();
        let first = SeriesId::new("source-a", "same-local-id");
        let second = SeriesId::new("source-b", "same-local-id");
        let fixtures = (0..100)
            .map(|index| {
                let id = match index {
                    45 => first.clone(),
                    55 => second.clone(),
                    _ => SeriesId::new("source-a", format!("filler-{index}")),
                };
                Series {
                    id,
                    title: format!("Fixture {index:03}"),
                    publication_status: Default::default(),
                    description: String::new(),
                    authors: vec![],
                    genres: vec![],
                    cover_url: (index == 55)
                        .then(|| "https://example.invalid/old-cover.jpg".into()),
                    canonical_url: None,
                }
            })
            .collect::<Vec<_>>();
        services
            .domain
            .update(|state| {
                state.categories = categories();
                state.series = fixtures.clone();
                for (index, series) in fixtures.iter().enumerate() {
                    state.library.push(LibraryEntry {
                        reading_status: Default::default(),
                        series: series.id.clone(),
                        categories: BTreeSet::from(["favorite-id".into()]),
                        preferences: None,
                        bookmarked: false,
                        added_at: index as i64,
                    });
                    state.chapters.push(Chapter {
                        id: ChapterId::new(series.id.clone(), "cached-chapter"),
                        title: "Cached chapter".into(),
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
        ui.on_library_viewport_changed({
            let app = app.clone();
            let weak = ui.as_weak();
            move |key, fraction, y| {
                app.remember_library_position(&weak.unwrap(), key.to_string(), fraction, y)
            }
        });
        ui.on_library_checkpoint({
            let app = app.clone();
            move || app.checkpoint_library()
        });
        ui.on_navigate({
            let app = app.clone();
            let weak = ui.as_weak();
            move |section| app.navigate(&weak.unwrap(), section)
        });
        ui.show().unwrap();
        ui.window().set_size(slint::LogicalSize::new(390., 844.));
        app.render(&ui);
        settle();
        let first_key = serde_json::to_string(&first).unwrap();
        let second_key = serde_json::to_string(&second).unwrap();
        assert_ne!(first_key, second_key);
        let mut all_position = Position::default();
        for (category, id, key) in [
            (0, first.clone(), first_key.clone()),
            (
                ReadingStatus::BUILTINS.len() + 1,
                second.clone(),
                second_key.clone(),
            ),
        ] {
            app.select_library_category(&ui, category as i32);
            settle();
            let index = ui
                .get_items()
                .iter()
                .position(|row| row.series_key.as_str() == key)
                .unwrap();
            let row = ui.get_items().row_data(index).unwrap();
            if category == 0 {
                assert!(
                    row.artwork_key.is_empty(),
                    "coverless cards still have a stable navigation identity"
                );
            } else {
                assert_ne!(
                    row.artwork_key.as_str(),
                    key,
                    "cover cache keys are not navigation identities"
                );
            }
            ui.set_library_restore_index(index as i32);
            ui.set_library_restore_fraction(0.375);
            ui.set_library_restore_serial(ui.get_library_restore_serial() + 1);
            settle();
            ui.invoke_remember_library_position();
            let before = app.view().library_navigation.position(&categories());
            assert_eq!(before.series_key, key);
            assert!((before.fraction - 0.375).abs() < 0.025);
            if category == 0 {
                all_position = before.clone();
            }
            let series = fixtures
                .iter()
                .find(|series| series.id == id)
                .unwrap()
                .clone();
            app.open_series(&ui, series);
            settle();
            assert_eq!(ui.get_section(), 5);
            ui.invoke_go_back();
            settle();
            assert_eq!(ui.get_section(), 0);
            assert_eq!(resolved(&ui), key);
            assert!((ui.get_library_restore_fraction() - before.fraction).abs() < 0.025);
        }
        // The active custom category is renamed/reordered while its cover URL
        // changes. Both caches may change their indexes; logical IDs may not.
        app.services
            .domain
            .update(|state| {
                state.categories.reverse();
                state.categories[1].name = "Renamed favorites".into();
                state
                    .series
                    .iter_mut()
                    .find(|series| series.id == second)
                    .unwrap()
                    .cover_url = Some("https://example.invalid/refreshed-cover.jpg".into());
                Ok(())
            })
            .unwrap();
        app.render(&ui);
        settle();
        assert_eq!(
            ui.get_category_index(),
            (ReadingStatus::BUILTINS.len() + 2) as i32
        );
        assert_eq!(resolved(&ui), second_key);
        assert!(
            ui.get_categories()
                .iter()
                .any(|name| name == "Renamed favorites")
        );
        // Removing the selected category restores All's separate anchor rather
        // than borrowing the removed category's last row.
        app.services
            .domain
            .update(|state| {
                state
                    .categories
                    .retain(|category| category.id != "favorite-id");
                for entry in &mut state.library {
                    entry.categories.remove("favorite-id");
                }
                Ok(())
            })
            .unwrap();
        app.render(&ui);
        settle();
        assert_eq!(ui.get_category_index(), 0);
        assert_eq!(resolved(&ui), first_key);
        assert!((ui.get_library_restore_fraction() - all_position.fraction).abs() < 0.025);
        app.flush();
    }
}
