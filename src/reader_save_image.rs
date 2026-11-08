//! Save image: where a page's bytes come from and which file is written.
//!
//! A page can offer several *sources*: the original fetched again from the
//! source, the verified download, the page-cache entry of an online chapter, or
//! the imported file. Which ones exist depends on how the reader holds the
//! page. Simple Save image resolves one from the device settings; the Save
//! image sheet lists them all, probing only what is already on disk.
use super::*;
use crate::capture_settings::{CaptureFormat, CapturePreferences, SaveSourcePreference};
use crate::page_inventory::ResourceKind;
use crate::reader_export::{ExportImage, PageFacts};
use slint::SharedString;

/// Local imports are stored under this source ID; they have no remote origin.
const LOCAL_SOURCE: &str = "local";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SourceKind {
    Original,
    Download,
    Cached,
    Imported,
}

impl SourceKind {
    pub(super) fn title(self) -> String {
        text::tr(match self {
            Self::Original => "Original from source",
            Self::Download => "Downloaded copy",
            Self::Cached => "Cached copy",
            Self::Imported => "Imported file",
        })
    }
    /// The word the simple action's subtitle starts with.
    fn short(self) -> String {
        text::tr(match self {
            Self::Original => "Source",
            Self::Download => "Download",
            Self::Cached => "Cached",
            Self::Imported => "Imported",
        })
    }
}

/// Which source a save asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Want {
    /// Simple Save image: the page as the reader has it, or its cached copy
    /// first when the setting prefers one.
    Auto { prefer_cached: bool },
    /// A source picked in the sheet. It is never swapped for another one.
    Exact(SourceKind),
}

/// What a save reads and writes. Converting uses `preferences`' format and
/// quality; keeping writes the file as it is, except that a page stored in
/// another layout is encoded with those same preferences.
#[derive(Clone, Copy, Debug)]
pub(super) struct Plan {
    pub want: Want,
    pub convert: bool,
    pub preferences: CapturePreferences,
}

impl Plan {
    pub(super) fn simple(preferences: CapturePreferences) -> Self {
        Self {
            want: Want::Auto {
                prefer_cached: preferences.save_source == SaveSourcePreference::PreferCached,
            },
            convert: preferences.convert_saves,
            preferences,
        }
    }
}

/// The page a save works on, captured on the UI thread for a worker.
#[derive(Clone)]
pub(super) struct Target {
    pub input: PageInput,
    pub source: Option<Source>,
    pub chapter: ChapterId,
    pub page_id: String,
    /// The page's number within its chapter, from 1.
    pub number: usize,
    pub cache: Arc<crate::chapter_cache::ChapterCache>,
    pub services: Services,
    pub current: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl Target {
    pub(super) fn of(app: &Arc<Controller>, index: i32) -> Option<Self> {
        let generation = app.generation.load(Ordering::Acquire);
        let view = app.view();
        let reader = view.reader.as_ref()?;
        let index = usize::try_from(index).ok()?;
        let input = reader.pages.get(index)?.clone();
        let weak = Arc::downgrade(app);
        Some(Self {
            input,
            source: reader.source_for(index),
            chapter: reader.owner(index).clone(),
            page_id: reader.page_id(index),
            number: reader.local_index(index) + 1,
            cache: app.chapter_cache.clone(),
            services: app.services.clone(),
            current: Arc::new(move || {
                weak.upgrade()
                    .is_some_and(|app| app.generation.load(Ordering::Acquire) == generation)
            }),
        })
    }

    fn imported(&self) -> bool {
        self.chapter.series.source_id == LOCAL_SOURCE
    }

    /// The sources this page offers, in the order the sheet lists them.
    pub(super) fn kinds(&self) -> Vec<SourceKind> {
        match &self.input {
            PageInput::Remote(_) => vec![SourceKind::Original, SourceKind::Cached],
            PageInput::Verified(resource) => match &resource.kind {
                ResourceKind::Local { .. } if self.imported() => vec![SourceKind::Imported],
                ResourceKind::Local { .. } | ResourceKind::Unavailable { .. } => {
                    vec![SourceKind::Download, SourceKind::Original]
                }
                ResourceKind::Remote { .. } => vec![SourceKind::Original],
            },
            #[cfg(test)]
            PageInput::Local { .. } => vec![SourceKind::Download],
        }
    }

    /// Where a downloaded page was fetched from, so it can be fetched again.
    fn origin(&self) -> Result<(Source, reader_sources::Page)> {
        let unavailable = || text::tr("The source of this page is unavailable");
        if self.imported() {
            return Err(unavailable());
        }
        let source = match &self.source {
            Some(source) => source.clone(),
            None => self
                .services
                .source(&self.chapter.series.source_id)
                .map_err(|_| unavailable())?,
        };
        let stored = self
            .services
            .downloads
            .page_inventory(&self.chapter)
            .map_err(|_| unavailable())?
            .into_iter()
            .find(|page| page.resource.id == self.page_id)
            .ok_or_else(unavailable)?;
        Ok((
            source,
            reader_sources::Page {
                id: stored.resource.id,
                url: stored.resource.url,
                headers: stored.resource.headers,
                width: None,
                height: None,
            },
        ))
    }

    /// Whether Original could be fetched, decided without the network.
    fn original_reason(&self) -> Option<String> {
        match &self.input {
            PageInput::Remote(_) => self
                .source
                .is_none()
                .then(|| text::tr("The source of this page is unavailable")),
            PageInput::Verified(resource) => match &resource.kind {
                ResourceKind::Remote { .. } => None,
                _ => self.origin().err(),
            },
            #[cfg(test)]
            PageInput::Local { .. } => Some(text::tr("The source of this page is unavailable")),
        }
    }

    /// The page-cache entry of an online page, from disk only.
    fn cached(&self, cancel: &CancelToken) -> Result<Option<PageImage>> {
        let (PageInput::Remote(page), Some(source)) = (&self.input, &self.source) else {
            return Ok(None);
        };
        let request = self.cache.prepare(
            &self.chapter,
            page,
            source,
            cancel.clone(),
            self.current.clone(),
        )?;
        Ok(self.cache.read_cached(&request)?.map(|read| read.page))
    }

    /// Read one source. Nothing falls back to another source here.
    pub(super) fn read(&self, kind: SourceKind, cancel: &CancelToken) -> Result<PageImage> {
        match kind {
            SourceKind::Cached => self
                .cached(cancel)?
                .ok_or_else(|| text::tr("The cached copy is no longer available")),
            SourceKind::Download | SourceKind::Imported => {
                self.input.read(self.source.as_ref(), cancel)
            }
            SourceKind::Original => match &self.input {
                PageInput::Verified(resource)
                    if matches!(
                        resource.kind,
                        ResourceKind::Local { .. } | ResourceKind::Unavailable { .. }
                    ) =>
                {
                    let (source, page) = self.origin()?;
                    PageInput::Remote(page).read(Some(&source), cancel)
                }
                _ => self.input.read(self.source.as_ref(), cancel),
            },
        }
    }

    /// The source simple Save image uses, without touching the network.
    fn auto_kind(
        &self,
        prefer_cached: bool,
        cancel: &CancelToken,
    ) -> (SourceKind, Option<PageImage>) {
        let kinds = self.kinds();
        if prefer_cached
            && kinds.contains(&SourceKind::Cached)
            && let Ok(Some(page)) = self.cached(cancel)
        {
            return (SourceKind::Cached, Some(page));
        }
        let kind = kinds
            .into_iter()
            .find(|kind| *kind != SourceKind::Cached)
            .unwrap_or(SourceKind::Original);
        (kind, None)
    }

    /// Read what `plan` asks for and build the file to write.
    pub(super) fn export(
        &self,
        plan: &Plan,
        name: &str,
        cancel: &CancelToken,
    ) -> Result<ExportImage> {
        let page = match plan.want {
            Want::Auto { prefer_cached } => match self.auto_kind(prefer_cached, cancel) {
                (_, Some(page)) => page,
                (kind, None) => self.read(kind, cancel)?,
            },
            Want::Exact(kind) => self.read(kind, cancel)?,
        };
        cancel.check().map_err(error)?;
        if plan.convert {
            ExportImage::from_page_converted(&page, name, &plan.preferences)
        } else {
            ExportImage::from_page(&page, name, &plan.preferences, false)
        }
    }

    /// Probe every source for the sheet. Only disk is read: Original is
    /// described, never fetched.
    pub(super) fn choices(&self, cancel: &CancelToken) -> Vec<Choice> {
        self.kinds()
            .into_iter()
            .map(|kind| {
                let probed = match kind {
                    SourceKind::Original => match self.original_reason() {
                        Some(reason) => Err(reason),
                        None => Ok(None),
                    },
                    SourceKind::Cached => match self.cached(cancel) {
                        Ok(Some(page)) => crate::reader_export::page_facts(&page).map(Some),
                        Ok(None) => Err(text::tr("Not cached yet")),
                        Err(message) => Err(message),
                    },
                    SourceKind::Download | SourceKind::Imported => self
                        .read(kind, cancel)
                        .and_then(|page| crate::reader_export::page_facts(&page))
                        .map(Some),
                };
                match probed {
                    Ok(facts) => Choice {
                        kind,
                        reason: None,
                        facts,
                    },
                    Err(reason) => Choice {
                        kind,
                        reason: Some(reason),
                        facts: None,
                    },
                }
            })
            .collect()
    }

    /// The subtitle of Save image (simple): which source it reads and what it
    /// writes. Disk-only, like the sheet's probe.
    pub(super) fn simple_detail(&self, plan: &Plan, cancel: &CancelToken) -> String {
        let Want::Auto { prefer_cached } = plan.want else {
            return String::new();
        };
        let (kind, page) = self.auto_kind(prefer_cached, cancel);
        let page = page.or_else(|| match kind {
            SourceKind::Download | SourceKind::Imported => self.read(kind, cancel).ok(),
            _ => None,
        });
        let facts = page.and_then(|page| crate::reader_export::page_facts(&page).ok());
        let output = crate::reader_export::output_label(&plan.preferences);
        match (plan.convert, facts) {
            (true, _) => format!("{} → {output}", kind.short()),
            (
                false,
                Some(PageFacts {
                    format: Some(format),
                    ..
                }),
            ) => {
                format!("{} · {format}", kind.short())
            }
            (false, Some(PageFacts { format: None, .. })) => {
                format!("{} · {} → {output}", kind.short(), text::tr("split page"))
            }
            (false, None) => format!("{} · {}", kind.short(), text::tr("original file")),
        }
    }
}

/// One source as the sheet shows it.
#[derive(Clone, Debug)]
pub(super) struct Choice {
    pub kind: SourceKind,
    /// Why the source cannot be used now.
    pub reason: Option<String>,
    /// What is on disk, for local sources.
    pub facts: Option<PageFacts>,
}

impl Choice {
    pub(super) fn detail(&self) -> String {
        if let Some(reason) = &self.reason {
            return reason.clone();
        }
        match &self.facts {
            None => text::tr("Downloads again"),
            Some(facts) => {
                let size = text::file_size(facts.bytes);
                let format = facts
                    .format
                    .map_or_else(|| text::tr("Split page"), str::to_owned);
                if self.kind == SourceKind::Cached {
                    format!("{format} · {size} · {}", text::tr("no network"))
                } else {
                    format!("{format} · {size}")
                }
            }
        }
    }

    /// Per format, why it cannot hold this page ("" when it can). Order is
    /// `CaptureFormat`'s.
    pub(super) fn blocked_formats(&self) -> Vec<String> {
        (0..7)
            .map(|index| {
                let format = CaptureFormat::try_from(index).unwrap_or_default();
                self.facts
                    .as_ref()
                    .and_then(|facts| {
                        crate::reader_export::format_limit(format, facts.width, facts.height)
                    })
                    .unwrap_or_default()
            })
            .collect()
    }
}

impl Choice {
    /// Whether the file can be written as it is (a split page is no file).
    fn keepable(&self) -> bool {
        self.facts
            .as_ref()
            .is_none_or(|facts| facts.format.is_some())
    }

    fn row(&self) -> crate::SaveImageSource {
        let facts = self.facts.clone();
        crate::SaveImageSource {
            title: self.kind.title().into(),
            detail: self.detail().into(),
            enabled: self.reason.is_none(),
            keepable: self.keepable(),
            width: facts.as_ref().map_or(0, |facts| facts.width as i32),
            height: facts.as_ref().map_or(0, |facts| facts.height as i32),
            format: facts
                .as_ref()
                .and_then(|facts| facts.format)
                .unwrap_or_default()
                .into(),
            size: facts
                .as_ref()
                .map(|facts| text::file_size(facts.bytes))
                .unwrap_or_default()
                .into(),
            blocked: ModelRc::new(VecModel::from(
                self.blocked_formats()
                    .into_iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
            )),
        }
    }
}

/// The open Save image sheet: which page it is for and what it offered.
pub(super) struct Sheet {
    index: i32,
    page_id: String,
    current: Arc<dyn Fn() -> bool + Send + Sync>,
    choices: Vec<Choice>,
}
pub(super) type SheetState = Arc<Mutex<Option<Sheet>>>;

fn quality_for(preferences: &CapturePreferences, format: CaptureFormat) -> f32 {
    f32::from(match format {
        CaptureFormat::Jpeg => preferences.jpeg_quality,
        CaptureFormat::WebpLossy => preferences.webp_quality,
        CaptureFormat::Avif => preferences.avif_quality,
        CaptureFormat::JxlLossy => preferences.jxl_quality,
        _ => 85,
    })
}

/// Fill in the subtitle of Save image (simple) once the menu is open. Only
/// disk is read; a menu that has closed or moved on ignores the result.
pub(super) fn resolve_simple_detail(app: &Arc<Controller>, ui: &AppWindow, index: i32) {
    ui.set_reader_context_save_detail(SharedString::default());
    let preferences = crate::capture_settings::preferences(ui);
    if !preferences.simple_screencaps {
        return;
    }
    let Some(target) = Target::of(app, index) else {
        return;
    };
    let plan = Plan::simple(preferences);
    let cancel = app.cancel.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let weak = ui.as_weak();
    crate::profiles::spawn(move || {
        let detail = target.simple_detail(&plan, &cancel);
        let current = target.current.clone();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if current() && ui.get_reader_context_visible() && ui.get_reader_context_page() == index
            {
                ui.set_reader_context_save_detail(detail.into());
            }
        });
    });
}

/// Open the sheet for a page and probe its sources in the background.
pub(super) fn open_sheet(app: &Arc<Controller>, ui: &AppWindow, state: &SheetState, index: i32) {
    let Some(target) = Target::of(app, index) else {
        ui.set_reader_context_status(text::tr("Chapter image is unavailable").into());
        return;
    };
    let preferences = crate::capture_settings::preferences(ui);
    *state.lock().unwrap_or_else(|e| e.into_inner()) = Some(Sheet {
        index,
        page_id: target.page_id.clone(),
        current: target.current.clone(),
        choices: Vec::new(),
    });
    ui.set_save_image_subtitle(format!("{} {}", text::tr("Page"), target.number).into());
    ui.set_save_image_sources(ModelRc::default());
    ui.set_save_image_source_index(0);
    ui.set_save_image_error(SharedString::default());
    ui.set_save_image_remember(false);
    // Output starts from the Screenshots settings every time.
    ui.set_save_image_keep(!preferences.convert_saves);
    ui.set_save_image_format(preferences.format.into());
    ui.set_save_image_quality(quality_for(&preferences, preferences.format));
    ui.set_save_image_loading(true);
    ui.set_save_image_open(true);
    let cancel = app.cancel.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let weak = ui.as_weak();
    let state = state.clone();
    let prefer_cached = preferences.save_source == SaveSourcePreference::PreferCached;
    crate::profiles::spawn(move || {
        let choices = target.choices(&cancel);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(sheet) = guard.as_mut().filter(|sheet| {
                sheet.index == index && sheet.page_id == target.page_id && (sheet.current)()
            }) else {
                return;
            };
            // Start where simple Save image would read from, when it can.
            let usable = |kind: SourceKind| {
                choices
                    .iter()
                    .position(|choice| choice.kind == kind && choice.reason.is_none())
            };
            let selected = (prefer_cached)
                .then(|| usable(SourceKind::Cached))
                .flatten()
                .or_else(|| {
                    choices.iter().position(|choice| {
                        choice.kind != SourceKind::Cached && choice.reason.is_none()
                    })
                })
                .or_else(|| choices.iter().position(|choice| choice.reason.is_none()))
                .unwrap_or(0);
            if choices
                .get(selected)
                .is_some_and(|choice| !choice.keepable())
            {
                ui.set_save_image_keep(false);
            }
            ui.set_save_image_sources(ModelRc::new(VecModel::from(
                choices.iter().map(Choice::row).collect::<Vec<_>>(),
            )));
            ui.set_save_image_source_index(selected as i32);
            sheet.choices = choices;
            ui.set_save_image_loading(false);
        });
    });
}

/// Close the sheet if its chapter is gone. Called from the reader's timer.
pub(super) fn stale(state: &SheetState, ui: &AppWindow) {
    if !ui.get_save_image_open() {
        return;
    }
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    if ui.get_section() != 6 || guard.as_ref().is_none_or(|sheet| !(sheet.current)()) {
        guard.take();
        ui.set_save_image_open(false);
    }
}

pub(super) fn cancelled(state: &SheetState) {
    state.lock().unwrap_or_else(|e| e.into_inner()).take();
}

/// The plan the sheet's choices describe, after remembering them if asked.
/// Returns the page index and the page ID the plan was made for.
pub(super) fn confirmed(ui: &AppWindow, state: &SheetState) -> Option<(i32, String, Plan)> {
    let sheet = state.lock().unwrap_or_else(|e| e.into_inner()).take();
    ui.set_save_image_open(false);
    let sheet = sheet.filter(|sheet| (sheet.current)())?;
    let choice = usize::try_from(ui.get_save_image_source_index())
        .ok()
        .and_then(|index| sheet.choices.get(index))
        .filter(|choice| choice.reason.is_none())?;
    let convert = !(ui.get_save_image_keep() && choice.keepable());
    let format = CaptureFormat::try_from(ui.get_save_image_format()).unwrap_or_default();
    let quality = ui.get_save_image_quality();
    let preferences = crate::capture_settings::preferences(ui).with_output(format, quality);
    if ui.get_save_image_remember() {
        use crate::capture_settings::change;
        let source = if choice.kind == SourceKind::Cached {
            SaveSourcePreference::PreferCached
        } else {
            SaveSourcePreference::AsIs
        };
        change(ui, "save_source", source.into());
        change(ui, "convert_saves", i32::from(convert));
        if convert {
            change(ui, "format", format.into());
            if let Some(key) = crate::capture_settings::quality_key(format) {
                change(ui, key, quality.round() as i32);
            }
        }
    }
    Some((
        sheet.index,
        sheet.page_id,
        Plan {
            want: Want::Exact(choice.kind),
            convert,
            preferences,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(format: Option<&'static str>, height: u32) -> Option<PageFacts> {
        Some(PageFacts {
            format,
            bytes: 380 * 1024,
            width: 1200,
            height,
        })
    }

    #[test]
    fn choices_describe_what_is_on_disk_and_what_cannot_be_kept() {
        let original = Choice {
            kind: SourceKind::Original,
            reason: None,
            facts: None,
        };
        assert_eq!(original.detail(), "Downloads again");
        assert!(original.keepable());
        let cached = Choice {
            kind: SourceKind::Cached,
            reason: None,
            facts: facts(Some("WebP"), 1800),
        };
        assert_eq!(cached.detail(), "WebP · 380 KiB · no network");
        assert!(cached.blocked_formats().iter().all(String::is_empty));
        let split = Choice {
            kind: SourceKind::Download,
            reason: None,
            facts: facts(None, 20_000),
        };
        assert_eq!(split.detail(), "Split page · 380 KiB");
        assert!(!split.keepable(), "a split page has no file to keep");
        let blocked = split.blocked_formats();
        assert_eq!(blocked.len(), 7);
        for format in [CaptureFormat::WebpLossless, CaptureFormat::WebpLossy] {
            assert!(!blocked[i32::from(format) as usize].is_empty());
        }
        assert!(blocked[i32::from(CaptureFormat::Avif) as usize].is_empty());
        let missing = Choice {
            kind: SourceKind::Cached,
            reason: Some("Not cached yet".into()),
            facts: None,
        };
        assert_eq!(missing.detail(), "Not cached yet");
    }

    #[test]
    fn simple_plan_follows_the_device_settings() {
        let plan = Plan::simple(CapturePreferences::default());
        assert_eq!(
            plan.want,
            Want::Auto {
                prefer_cached: false
            }
        );
        assert!(!plan.convert, "by default the page is saved as it is");
        let plan = Plan::simple(CapturePreferences {
            save_source: SaveSourcePreference::PreferCached,
            convert_saves: true,
            ..CapturePreferences::default()
        });
        assert_eq!(
            plan.want,
            Want::Auto {
                prefer_cached: true
            }
        );
        assert!(plan.convert);
    }
}
