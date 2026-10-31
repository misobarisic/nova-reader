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

#[derive(Clone)]
pub struct Services {
    pub domain: DomainStore,
    pub downloads: DownloadStore,
    pub sources: SourceRegistry,
}
impl Services {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            domain: DomainStore::open(path).map_err(error)?,
            downloads: DownloadStore::open(path.join("chapters"), 8 * 1024 * 1024 * 1024)
                .map_err(error)?,
            sources: SourceRegistry::open(path.join("addons")).map_err(error)?,
        })
    }
    pub fn install(&self, bytes: &[u8]) -> Result<String> {
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
    pub fn series(&self, source_id: &str, dto: reader_sources::Series) -> Series {
        Series {
            id: SeriesId::new(source_id, dto.id),
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
                state.series.retain(|s| s.id != series.id);
                state.series.push(series);
                // Retain vanished chapters so their bookmarks, history and offline
                // bundles survive source edits. Fresh rows replace matching IDs.
                for chapter in chapters {
                    state.chapters.retain(|c| c.id != chapter.id);
                    state.chapters.push(chapter);
                }
                Ok(())
            })
            .map_err(error)
    }
    pub fn details(&self, series: &Series, cancel: &CancelToken) -> Result<(Series, Vec<Chapter>)> {
        let source = self.source(&series.id.source_id)?;
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
        self.save_details(series.clone(), chapters.clone())?;
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
        let compression = crate::image_storage::load().downloads;
        self.downloads
            .run(chapter, |page| {
                let original = source
                    .fetch_page(&page.url, &page.headers, cancel)
                    .map_err(error)?;
                cancel.check().map_err(error)?;
                let bytes = if compression.enabled {
                    // Some tall pages exceed a codec's dimensional limit.
                    // Preserve the original rather than resizing or losing it;
                    // the download store still validates all committed bytes.
                    crate::reader_export::reencode_image(
                        &original,
                        compression.format,
                        compression.quality,
                        reader_engine::DecodeLimits::default(),
                    )
                    .unwrap_or(original)
                } else {
                    original
                };
                cancel.check().map_err(error)?;
                Ok(bytes)
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
