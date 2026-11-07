//! Worker-safe orchestration shared by the UI and restart journey tests.
use reader_domain::{Chapter, ChapterId, DomainStore, LibraryEntry, Series, SeriesId};
use reader_downloads::{DownloadStore, PageResource};
use reader_sources::{CancelToken, Source, SourceRegistry};
use serde_json::json;
use std::{collections::BTreeSet, path::Path};

pub type Result<T> = std::result::Result<T, String>;
pub fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn fetch_download_page(
    source: &Source,
    page: &PageResource,
    cancel: &CancelToken,
    compression: crate::image_storage::ImageCompression,
) -> Result<Vec<u8>> {
    let original = source
        .fetch_page_limited(
            &page.url,
            &page.headers,
            cancel,
            reader_engine::current_limits().max_encoded_bytes,
        )
        .map_err(error)?;
    cancel.check().map_err(error)?;
    // Sources serve images. Bytes that look like Reader's own stored layout
    // would later be read back as one, so they never reach the store.
    crate::page_store::check_fetched(&original)?;
    let bytes = if compression.enabled {
        // A tall page becomes strips when downloads may split; anything the
        // codec cannot represent keeps its source bytes. The download store
        // still validates every committed page and checksum.
        let limits = reader_engine::current_limits();
        crate::page_store::storage_form(
            &original,
            compression,
            compression.split_plan(),
            limits,
            limits.max_encoded_bytes,
            &|| !cancel.is_cancelled(),
        )
        .unwrap_or(original)
    } else {
        original
    };
    cancel.check().map_err(error)?;
    Ok(bytes)
}

pub(crate) fn chapter_from_source(
    series: &SeriesId,
    chapter: reader_sources::Chapter,
) -> Result<Chapter> {
    if chapter.series_id != series.local_id {
        return Err("Source returned a chapter for another series".into());
    }
    let uploaded_at = crate::timeline::parse_source_date(chapter.uploaded_at.as_deref());
    Ok(Chapter {
        id: ChapterId::new(series.clone(), chapter.id),
        title: chapter.title.clone(),
        label: chapter.title,
        number: chapter.number,
        volume: None,
        source_order: chapter.source_order as i64,
        language: chapter.language,
        scanlator: chapter.scanlator,
        uploaded_at,
    })
}

// Retain vanished chapters so bookmarks, history and offline bundles survive
// source edits. Fresh rows replace matching IDs, never reading progress.
pub(crate) fn merge_details(
    state: &mut reader_domain::ReaderState,
    series: Series,
    chapters: Vec<Chapter>,
) {
    state.series.retain(|saved| saved.id != series.id);
    state.series.push(series);
    for chapter in chapters {
        state.chapters.retain(|saved| saved.id != chapter.id);
        state.chapters.push(chapter);
    }
}

#[derive(Clone)]
pub struct Services {
    pub domain: DomainStore,
    pub downloads: DownloadStore,
    pub sources: SourceRegistry,
}
impl Services {
    pub fn open(path: &Path) -> Result<Self> {
        // Foreground activities and headless jobs share one registry lock.
        static OPEN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        static CURRENT: std::sync::OnceLock<Services> = std::sync::OnceLock::new();
        let _open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
        let domain = DomainStore::open(path).map_err(error)?;
        if let Some(current) = CURRENT.get() {
            return Ok(current.clone());
        }
        crate::image_limits::install();
        let services = Self {
            domain,
            downloads: DownloadStore::open(path.join("chapters"), 8 * 1024 * 1024 * 1024)
                .map_err(error)?,
            sources: SourceRegistry::open(path.join("addons")).map_err(error)?,
        };
        services
            .downloads
            .set_page_validator(std::sync::Arc::new(crate::page_store::validate_stored));
        crate::sync::initialize(&services.domain).map_err(error)?;
        // The directory, selection and per-profile registry settings are settled
        // before addon sync captures the selected profile's domain.
        crate::profiles::install_registry(services.sources.clone());
        let preferences = crate::profiles::initialize()?;
        if preferences.active_profile != services.domain.active_profile() {
            services
                .domain
                .set_profile(&preferences.active_profile)
                .map_err(error)?;
            services
                .domain
                .update_with_kind(reader_domain::UpdateKind::Seed, |_| Ok(()))
                .map_err(error)?;
        }
        crate::profiles::apply_registry_profile();
        crate::sync_addons::initialize(&services)?;
        let _ = CURRENT.set(services.clone());
        Ok(services)
    }
    pub fn install(&self, bytes: &[u8]) -> Result<String> {
        crate::sync_addons::local_change(self, || {
            let source = self.sources.install_zip(bytes).map_err(error)?;
            // Selecting a ZIP is an explicit installation. Permission-expanding
            // updates stay disabled until the user enables them in Browse.
            if !source.enabled
                && !self
                    .domain
                    .snapshot()
                    .map_err(error)?
                    .sources
                    .iter()
                    .any(|s| s.addon_id == source.id)
            {
                self.sources.set_enabled(&source.id, true).map_err(error)?;
            }
            self.domain
                .update(|state| {
                    state.sources.retain(|s| s.addon_id != source.id);
                    state.sources.push(reader_domain::SourceDescriptor {
                        id: source.id.clone(),
                        addon_id: source.id.clone(),
                        name: source.name,
                        language: source.language,
                        version: source.version,
                    });
                    Ok(())
                })
                .map_err(error)?;
            Ok(source.id)
        })
    }
    pub fn source(&self, source_id: &str) -> Result<Source> {
        if let Ok(source) = self.sources.source(source_id) {
            return Ok(source);
        }
        let mut packages = self.sources.list().map_err(error)?;
        packages.sort_by_key(|p| std::cmp::Reverse(p.id.len()));
        packages
            .into_iter()
            .find(|p| source_id.starts_with(&format!("{}.", p.id)))
            .ok_or_else(|| {
                "This source is unavailable. Downloaded chapters remain readable.".into()
            })
            .and_then(|p| self.sources.source(&p.id).map_err(error))
    }
    /// Browse/search row: captures a hint when present but never withdraws one,
    /// because listings commonly omit fields that the details response carries.
    pub fn series_listing(&self, source_id: &str, dto: reader_sources::Series) -> Series {
        let id = SeriesId::new(source_id, &dto.id);
        if let Err(error) = crate::content_presentation::Settings::record_listing_hint(
            &id,
            dto.presentation.as_deref(),
        ) {
            eprintln!("Nova Reader: could not save content presentation hint: {error}");
        }
        Self::series_domain(id, dto)
    }
    /// Full details response: authoritative, so a missing hint withdraws it.
    pub fn series(&self, source_id: &str, dto: reader_sources::Series) -> Series {
        let id = SeriesId::new(source_id, &dto.id);
        // Capture before discarding source-only metadata.
        if let Err(error) =
            crate::content_presentation::Settings::record_hint(&id, dto.presentation.as_deref())
        {
            eprintln!("Nova Reader: could not save content presentation hint: {error}");
        }
        Self::series_domain(id, dto)
    }
    fn series_domain(id: SeriesId, dto: reader_sources::Series) -> Series {
        Series {
            publication_status: reader_domain::PublicationStatus::from_source(
                dto.status.as_deref(),
            ),
            id,
            title: dto.title,
            description: dto.description.unwrap_or_default(),
            authors: dto.authors,
            genres: dto.genres,
            cover_url: dto.cover_url,
            canonical_url: (!dto.url.is_empty()).then_some(dto.url),
        }
    }
    pub fn save_details(&self, series: Series, chapters: Vec<Chapter>) -> Result<()> {
        self.domain
            .update(|state| {
                merge_details(state, series, chapters);
                Ok(())
            })
            .map_err(error)
    }
    pub fn details(&self, series: &Series, cancel: &CancelToken) -> Result<(Series, Vec<Chapter>)> {
        let source = self.source(&series.id.source_id)?;
        let (series, chapters) = self.fetch_details_from(&source, series, cancel)?;
        self.save_details(series.clone(), chapters.clone())?;
        crate::metadata_refresh::note_manual_success(self, &series.id);
        Ok((series, chapters))
    }
    // The scheduler must validate ownership and its baseline before publication.
    pub(crate) fn fetch_details_from(
        &self,
        source: &Source,
        series: &Series,
        cancel: &CancelToken,
    ) -> Result<(Series, Vec<Chapter>)> {
        let request = json!({"sourceId":series.id.source_id,"id":series.id.local_id,"seriesId":series.id.local_id,"url":series.canonical_url});
        let dto: reader_sources::Series = serde_json::from_value(
            source
                .call_cancelled("details", &request, cancel)
                .map_err(error)?,
        )
        .map_err(error)?;
        if dto.id != series.id.local_id {
            return Err("Source changed the series identity".into());
        }
        let list: reader_sources::ChapterList = serde_json::from_value(
            source
                .call_cancelled("chapters", &request, cancel)
                .map_err(error)?,
        )
        .map_err(error)?;
        let series = self.series(&series.id.source_id, dto);
        let chapters = list
            .chapters
            .into_iter()
            .map(|chapter| chapter_from_source(&series.id, chapter))
            .collect::<Result<Vec<_>>>()?;
        Ok((series, chapters))
    }
    pub fn pages(
        &self,
        chapter: &ChapterId,
        cancel: &CancelToken,
    ) -> Result<(Source, Vec<reader_sources::Page>)> {
        let source = self.source(&chapter.series.source_id)?;
        let result = source.call_cancelled("pages", &json!({"sourceId":chapter.series.source_id,"seriesId":chapter.series.local_id,"chapterId":chapter.local_id,"id":chapter.local_id}), cancel).map_err(error)?;
        let pages: reader_sources::PageList = serde_json::from_value(result).map_err(error)?;
        if pages.pages.is_empty() {
            return Err("This chapter has no pages".into());
        }
        Ok((source, pages.pages))
    }
    pub fn add_library(&self, series: &SeriesId) -> Result<()> {
        self.domain
            .update(|state| {
                if !state.library.iter().any(|l| &l.series == series) {
                    state.library.push(LibraryEntry {
                        reading_status: Default::default(),
                        series: series.clone(),
                        categories: BTreeSet::new(),
                        preferences: None,
                        bookmarked: false,
                        added_at: now(),
                    });
                }
                Ok(())
            })
            .map_err(error)
    }
    pub fn download(&self, chapter: &ChapterId, cancel: &CancelToken) -> Result<()> {
        self.prepare_download(chapter, cancel)?;
        self.run_queued_download(chapter, cancel)
    }
    pub fn prepare_download(&self, chapter: &ChapterId, cancel: &CancelToken) -> Result<()> {
        if self
            .downloads
            .job(chapter)
            .is_ok_and(|job| job.status == reader_downloads::JobStatus::Complete)
        {
            return Ok(());
        }
        let (_, pages) = self.pages(chapter, cancel)?;
        let pages = pages
            .into_iter()
            .map(|p| PageResource {
                id: p.id,
                url: p.url,
                headers: p.headers,
            })
            .collect();
        match self.downloads.job(chapter) {
            Ok(job) if job.status == reader_downloads::JobStatus::Complete => return Ok(()),
            Ok(_) => {
                self.downloads
                    .refresh_resources(chapter, pages)
                    .map_err(error)?;
                self.downloads.resume(chapter).map_err(error)?;
            }
            Err(_) => self
                .downloads
                .enqueue(chapter.clone(), pages)
                .map_err(error)?,
        }
        Ok(())
    }
    pub fn run_queued_download(&self, chapter: &ChapterId, cancel: &CancelToken) -> Result<()> {
        if self.downloads.job(chapter).map_err(error)?.status != reader_downloads::JobStatus::Queued
        {
            return Ok(());
        }
        let source = self.source(&chapter.series.source_id)?;
        // Snapshot once per transfer. Verified pages are skipped by the store,
        // so changing compression never rewrites existing downloads.
        let compression = crate::image_storage::download_compression(&source.descriptor().id);
        self.downloads
            .run(chapter, |page| {
                fetch_download_page(&source, page, cancel, compression)
            })
            .map_err(error)
    }
    /// Atomically claim the next persisted queued job before resolving its
    /// source. Unavailable sources become Failed, allowing the next call to
    /// advance to another chapter rather than repeatedly selecting that job.
    pub fn run_next_download(&self, cancel: &CancelToken) -> Result<Option<ChapterId>> {
        cancel.check().map_err(error)?;
        let mut compression = None;
        let mut source = None;
        self.downloads
            .run_next(|chapter, page| {
                if source.is_none() {
                    source = Some(self.source(&chapter.series.source_id)?);
                    compression = Some(crate::image_storage::download_compression(
                        &source.as_ref().unwrap().descriptor().id,
                    ));
                }
                fetch_download_page(source.as_ref().unwrap(), page, cancel, compression.unwrap())
            })
            .map_err(error)
    }
    pub fn import_local(&self, path: &Path, folder: bool) -> Result<SeriesId> {
        let title = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        // Local identity derives from content path, never a remote/expiring URL.
        // Reimports receive a new edition; existing progress is left intact.
        let local_id = format!(
            "{}-{}",
            now(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        );
        let series = Series {
            publication_status: Default::default(),
            id: SeriesId::new("local", local_id),
            title: if title.is_empty() {
                "Local chapter".into()
            } else {
                title
            },
            description: String::new(),
            authors: vec![],
            genres: vec![],
            cover_url: None,
            canonical_url: None,
        };
        let chapter = Chapter {
            id: ChapterId::new(series.id.clone(), "chapter"),
            title: series.title.clone(),
            label: "1".into(),
            number: Some("1".into()),
            volume: None,
            source_order: 0,
            language: None,
            scanlator: None,
            uploaded_at: None,
        };
        if folder {
            self.downloads
                .import_folder(chapter.id.clone(), path)
                .map_err(error)?;
        } else {
            self.downloads
                .import_cbz(chapter.id.clone(), path)
                .map_err(error)?;
        }
        self.save_details(series.clone(), vec![chapter])?;
        self.add_library(&series.id)?;
        Ok(series.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Date, Month, OffsetDateTime, UtcOffset};

    #[test]
    fn unavailable_queued_source_fails_after_claim_and_does_not_starve_next_download() {
        const CHILD: &str = "NOVA_READER_NEXT_DOWNLOAD_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "services::tests::unavailable_queued_source_fails_after_claim_and_does_not_starve_next_download", "--nocapture"])
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
        use std::io::Write;
        let directory = tempfile::tempdir().unwrap();
        let services = Services::open(directory.path()).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/addons/neutral-fixture");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
        let mut files = vec!["manifest.json".to_owned(), "main.js".to_owned()];
        files.extend(
            manifest["assets"]
                .as_array()
                .unwrap()
                .iter()
                .map(|asset| asset.as_str().unwrap().to_owned()),
        );
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for file in files {
            archive
                .start_file(&file, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive
                .write_all(&std::fs::read(root.join(file)).unwrap())
                .unwrap();
        }
        let package = archive.finish().unwrap().into_inner();
        services.sources.install_zip(&package).unwrap();
        services
            .sources
            .set_enabled("neutral-fixture", true)
            .unwrap();
        let missing = ChapterId::new(SeriesId::new("removed-source", "series"), "missing");
        services
            .downloads
            .enqueue(
                missing.clone(),
                vec![PageResource {
                    id: "page".into(),
                    url: "https://fixture.invalid/page.png".into(),
                    headers: Default::default(),
                }],
            )
            .unwrap();
        let cancel = CancelToken::default();
        let source = services.source("neutral-fixture").unwrap();
        let item = source.browse(&json!({"page": 1})).unwrap().items.remove(0);
        let series = services.series("neutral-fixture", item);
        let (_, chapters) = services.details(&series, &cancel).unwrap();
        let chapter = chapters
            .iter()
            .find(|chapter| chapter.id.local_id.ends_with("mixed"))
            .unwrap();
        services.prepare_download(&chapter.id, &cancel).unwrap();
        assert!(
            services
                .run_next_download(&cancel)
                .unwrap_err()
                .contains("unavailable")
        );
        let failed = services
            .downloads
            .snapshot()
            .unwrap()
            .into_iter()
            .find(|job| job.chapter == missing)
            .unwrap();
        assert_eq!(failed.status, reader_downloads::JobStatus::Failed);
        assert_eq!(failed.completed_pages(), 0);
        assert_eq!(
            services.run_next_download(&cancel).unwrap(),
            Some(chapter.id.clone())
        );
        assert!(services.downloads.offline(&chapter.id).is_ok());
        assert_eq!(services.run_next_download(&cancel).unwrap(), None);
    }

    fn provider_chapter(uploaded_at: Option<&str>) -> reader_sources::Chapter {
        // Mirrors the YiiMadara synthetic chapter fixture's fractional chapter
        // and date-only uploadedAt, with optional scanlator metadata.
        serde_json::from_value(json!({
            "id":"/webtoon/clockwork/chapter-2.5",
            "seriesId":"/webtoon/clockwork",
            "title":"Chapter 2.5",
            "number":"2.5",
            "sourceOrder":0,
            "url":"https://manhwa18.cc/webtoon/clockwork/chapter-2.5",
            "language":"en",
            "scanlator":"Clockwork Scans",
            "uploadedAt":uploaded_at
        }))
        .unwrap()
    }

    #[test]
    fn provider_date_reaches_domain_without_changing_chapter_metadata() {
        let series = SeriesId::new("manhwa18cc.en", "/webtoon/clockwork");
        let chapter = chapter_from_source(&series, provider_chapter(Some("2026-10-09"))).unwrap();
        assert_eq!(
            chapter.id,
            ChapterId::new(series, "/webtoon/clockwork/chapter-2.5")
        );
        assert_eq!(chapter.title, "Chapter 2.5");
        assert_eq!(chapter.label, "Chapter 2.5");
        assert_eq!(chapter.number.as_deref(), Some("2.5"));
        assert_eq!(chapter.source_order, 0);
        assert_eq!(chapter.language.as_deref(), Some("en"));
        assert_eq!(chapter.scanlator.as_deref(), Some("Clockwork Scans"));
        let utc = OffsetDateTime::from_unix_timestamp(chapter.uploaded_at.unwrap()).unwrap();
        let offset = UtcOffset::local_offset_at(utc).unwrap_or(UtcOffset::UTC);
        assert_eq!(
            utc.to_offset(offset).date(),
            Date::from_calendar_date(2026, Month::October, 9).unwrap()
        );
    }

    #[test]
    fn absent_and_invalid_provider_dates_remain_unknown() {
        let series = SeriesId::new("manhwa18cc.en", "/webtoon/clockwork");
        for date in [
            None,
            Some(""),
            Some("2026-02-30"),
            Some("unknown"),
            Some("2026-10-09T12:00:00"),
        ] {
            let chapter = chapter_from_source(&series, provider_chapter(date)).unwrap();
            assert_eq!(chapter.uploaded_at, None, "date: {date:?}");
        }
    }

    #[test]
    fn provider_datetime_keeps_its_instant_across_utc_day_boundary() {
        let series = SeriesId::new("manhwa18cc.en", "/webtoon/clockwork");
        let chapter =
            chapter_from_source(&series, provider_chapter(Some("2026-10-09T00:30:00+02:00")))
                .unwrap();
        let expected = Date::from_calendar_date(2026, Month::October, 8)
            .unwrap()
            .with_hms(22, 30, 0)
            .unwrap()
            .assume_utc()
            .unix_timestamp();
        assert_eq!(chapter.uploaded_at, Some(expected));
    }

    #[test]
    fn chapter_conversion_keeps_the_series_identity_guard() {
        let series = SeriesId::new("manhwa18cc.en", "/webtoon/another-series");
        assert_eq!(
            chapter_from_source(&series, provider_chapter(Some("2026-10-09"))).unwrap_err(),
            "Source returned a chapter for another series"
        );
    }
}

#[cfg(test)]
mod download_page_tests {
    use super::*;
    use crate::{
        image_storage::{ImageCompression, ImageStorageFormat},
        page_store::{PageImage, SplitPlan},
    };
    use reader_engine::PageSource;
    use reader_sources::{HttpRequest, HttpResponse, SourceTransport};
    use sha2::Digest;
    use std::{io::Write, sync::Arc};

    struct Transport(Vec<u8>);
    impl SourceTransport for Transport {
        fn request(
            &self,
            request: &HttpRequest,
            _: usize,
            cancel: &CancelToken,
        ) -> reader_sources::Result<HttpResponse> {
            cancel.check()?;
            Ok(HttpResponse {
                status: 200,
                url: request.url.clone(),
                body: self.0.clone(),
                ..Default::default()
            })
        }
    }
    fn source(directory: &Path, body: Vec<u8>) -> (SourceRegistry, Source) {
        let registry = SourceRegistry::open(directory).unwrap();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let manifest = json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"download-fixture","name":"Download fixture","version":"1.0.0","language":"en","nsfw":false,"entry":"main.js","permissions":{"domains":["example.com"],"httpPost":false},"capabilities":["pages"]});
        zip.start_file("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(manifest.to_string().as_bytes()).unwrap();
        zip.start_file("main.js", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"globalThis.nova={request:()=>({pages:[]})};")
            .unwrap();
        registry
            .install_zip(&zip.finish().unwrap().into_inner())
            .unwrap();
        registry.set_enabled("download-fixture", true).unwrap();
        let source = registry
            .source("download-fixture")
            .unwrap()
            .with_transport(Arc::new(Transport(body)));
        (registry, source)
    }
    fn textured(width: u32, height: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(width, height, |x, y| {
            let n = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503) ^ (y / 9);
            image::Rgba([n as u8, (n >> 8) as u8, (n >> 16) as u8, 255])
        })
    }
    fn png(image: &image::RgbaImage) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }
    fn resource() -> PageResource {
        PageResource {
            id: "p".into(),
            url: "https://example.com/p.png".into(),
            headers: Default::default(),
        }
    }
    fn compression(split: bool, enabled: bool) -> ImageCompression {
        ImageCompression {
            enabled,
            format: ImageStorageFormat::Webp,
            lossless_inputs: true,
            split_tall_pages: split,
            split_threshold_rows: 1024,
            strip_rows: 256,
            ..ImageCompression::downloads_default()
        }
    }
    fn fetch(directory: &Path, body: &[u8], compression: ImageCompression) -> Result<Vec<u8>> {
        let (_registry, source) = source(directory, body.to_vec());
        fetch_download_page(&source, &resource(), &CancelToken::default(), compression)
    }
    fn granularity(bytes: Vec<u8>) -> Option<u32> {
        PageImage::from_stored(bytes).unwrap().row_granularity()
    }

    #[test]
    fn new_download_pages_split_only_when_download_compression_and_the_toggle_are_on() {
        let directory = tempfile::tempdir().unwrap();
        let original = textured(40, 1500);
        let body = png(&original);
        // Compression and toggle on: strips, in the download format, same pixels.
        let split = fetch(directory.path(), &body, compression(true, true)).unwrap();
        assert_eq!(granularity(split.clone()), Some(256));
        let page = PageImage::from_stored(split).unwrap();
        assert_eq!(
            page.dimensions(reader_engine::current_limits()).unwrap(),
            reader_engine::PageDimensions {
                width: 40,
                height: 1500
            }
        );
        assert_eq!(
            page.decode(reader_engine::current_limits()).unwrap(),
            original
        );
        // Toggle off: a whole recompressed image.
        let whole = fetch(directory.path(), &body, compression(false, true)).unwrap();
        assert_eq!(granularity(whole), None);
        // Compression off: the source bytes, whatever the toggle says.
        assert_eq!(
            fetch(directory.path(), &body, compression(true, false)).unwrap(),
            body
        );
        // A page that is not tall enough is never split.
        let short = png(&textured(40, 900));
        assert_eq!(
            granularity(fetch(directory.path(), &short, compression(true, true)).unwrap()),
            None
        );
    }

    #[test]
    fn a_source_cannot_hand_the_download_store_a_stored_container() {
        let directory = tempfile::tempdir().unwrap();
        let container = crate::page_store::split_form(
            &png(&textured(40, 1500)),
            compression(true, true),
            Some(SplitPlan {
                threshold_rows: 1024,
                strip_rows: 256,
            }),
            reader_engine::current_limits(),
            32 << 20,
            &|| true,
        )
        .unwrap()
        .unwrap();
        for settings in [
            compression(true, true),
            compression(false, true),
            compression(true, false),
        ] {
            assert!(
                fetch(directory.path(), &container, settings).is_err(),
                "{settings:?}"
            );
        }
    }

    #[test]
    fn split_downloads_commit_verify_and_stay_when_the_toggle_changes() {
        let directory = tempfile::tempdir().unwrap();
        let original = textured(40, 1500);
        let (_registry, source) = source(&directory.path().join("addons"), png(&original));
        let store = DownloadStore::open(directory.path().join("chapters"), 64 << 20).unwrap();
        store.set_page_validator(Arc::new(crate::page_store::validate_stored));
        let chapter = ChapterId::new(SeriesId::new("download-fixture", "series"), "chapter");
        store.enqueue(chapter.clone(), vec![resource()]).unwrap();
        let cancel = CancelToken::default();
        store
            .run(&chapter, |page| {
                fetch_download_page(&source, page, &cancel, compression(true, true))
            })
            .unwrap();
        // Sha-256 and length verification accept the container as one file.
        let offline = store.offline(&chapter).unwrap();
        let stored = std::fs::read(&offline.pages[0]).unwrap();
        let inventory = store.page_inventory(&chapter).unwrap();
        assert_eq!(inventory[0].bytes, stored.len() as u64);
        assert_eq!(
            inventory[0].sha256.as_deref(),
            Some(format!("{:x}", sha2::Sha256::digest(&stored)).as_str())
        );
        let page = PageImage::from_stored(stored.clone()).unwrap();
        assert_eq!(page.row_granularity(), Some(256));
        assert_eq!(
            page.decode(reader_engine::current_limits()).unwrap(),
            original
        );
        // A completed chapter is never run again, so the settings that later
        // change cannot rewrite it.
        assert!(store.retry(&chapter).is_err());
        let mut fetched = 0;
        assert!(
            store
                .run(&chapter, |_| {
                    fetched += 1;
                    Err("must not fetch".into())
                })
                .is_err()
        );
        assert_eq!(fetched, 0);
        assert_eq!(
            std::fs::read(&store.offline(&chapter).unwrap().pages[0]).unwrap(),
            stored
        );
    }

    #[test]
    fn only_pages_downloaded_after_a_toggle_change_follow_it() {
        let directory = tempfile::tempdir().unwrap();
        let (_registry, source) =
            source(&directory.path().join("addons"), png(&textured(40, 1500)));
        let store = DownloadStore::open(directory.path().join("chapters"), 64 << 20).unwrap();
        store.set_page_validator(Arc::new(crate::page_store::validate_stored));
        let chapter = ChapterId::new(SeriesId::new("download-fixture", "series"), "chapter");
        let second = PageResource {
            id: "q".into(),
            ..resource()
        };
        store
            .enqueue(chapter.clone(), vec![resource(), second])
            .unwrap();
        let cancel = CancelToken::default();
        let mut calls = 0;
        assert!(
            store
                .run(&chapter, |page| {
                    calls += 1;
                    if calls == 2 {
                        return Err("interrupted".into());
                    }
                    fetch_download_page(&source, page, &cancel, compression(true, true))
                })
                .is_err()
        );
        let first = store.page_inventory(&chapter).unwrap()[0].path.clone();
        let split = std::fs::read(&first).unwrap();
        assert_eq!(granularity(split.clone()), Some(256));
        // The toggle is off for the rest of the chapter.
        store.retry(&chapter).unwrap();
        store
            .run(&chapter, |page| {
                fetch_download_page(&source, page, &cancel, compression(false, true))
            })
            .unwrap();
        let pages = store.offline(&chapter).unwrap().pages;
        assert_eq!(
            std::fs::read(&pages[0]).unwrap(),
            split,
            "verified page untouched"
        );
        assert_eq!(granularity(std::fs::read(&pages[1]).unwrap()), None);
    }

    #[test]
    fn a_page_that_is_neither_an_image_nor_a_whole_container_is_not_committed() {
        let directory = tempfile::tempdir().unwrap();
        let original = textured(40, 1500);
        let (_registry, source) = source(&directory.path().join("addons"), png(&original));
        let store = DownloadStore::open(directory.path().join("chapters"), 64 << 20).unwrap();
        store.set_page_validator(Arc::new(crate::page_store::validate_stored));
        let chapter = ChapterId::new(SeriesId::new("download-fixture", "series"), "chapter");
        store.enqueue(chapter.clone(), vec![resource()]).unwrap();
        let mut container = fetch_download_page(
            &source,
            &resource(),
            &CancelToken::default(),
            compression(true, true),
        )
        .unwrap();
        container.truncate(container.len() - 8);
        assert!(store.run(&chapter, |_| Ok(container.clone())).is_err());
        assert_eq!(store.job(&chapter).unwrap().completed_pages(), 0);
    }
}
