//! Downloaded chapters and local imports in a backup. Their pages are the
//! files the download store already holds: an export copies them verbatim
//! (stored, never decoded or re-encoded, so strip containers and compressed
//! pages travel as they are) and an import hands them back to the store, which
//! verifies each page against the recorded checksum before a chapter appears.
//!
//! A component's records describe chapters (`job\0<chapter>`: the chapter
//! identity and, for every page, its id, size and SHA-256). The pages are
//! entries under `blobs/<kind>/<chapter key>/<file>` that the archive reader
//! lists and path-checks but never loads. Only finished chapters are carried,
//! and neither a page's source URL nor its request headers: both can hold
//! credentials, and a finished chapter does not need them. Restores add missing
//! chapters; they never replace or delete a file.
use super::{
    archive::{BLOB_PREFIX, Payload, Writer},
    components::{self, key, split},
};
use crate::services::Services;
use reader_domain::ChapterId;
use reader_downloads::{ChapterDownload, DownloadStore, DownloadedPage, JobStatus, PageResource};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::BufReader,
    path::Path,
};

/// The source id that marks a chapter imported from a local file or folder.
const LOCAL: &str = "local";
const MAX_PAGES: usize = 10_000;

pub(crate) fn is_local(chapter: &ChapterId) -> bool {
    chapter.series.source_id == LOCAL
}
fn wants(name: &str, chapter: &ChapterId) -> bool {
    (name == "local_imports") == is_local(chapter)
}
fn blob_directory(name: &str) -> &'static str {
    if name == "local_imports" {
        "local"
    } else {
        "downloads"
    }
}
/// Where a chapter's page `filename` lives in the archive.
pub(crate) fn blob_path(name: &str, chapter: &ChapterId, filename: &str) -> String {
    format!(
        "{BLOB_PREFIX}{}/{}/{filename}",
        blob_directory(name),
        reader_downloads::chapter_key(chapter)
    )
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    chapter: ChapterId,
    pages: Vec<Page>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    id: String,
    bytes: u64,
    sha256: String,
}

fn record_of(job: &ChapterDownload) -> Option<Record> {
    let pages = job
        .pages
        .iter()
        .map(|page| {
            Some(Page {
                id: page.resource.id.clone(),
                bytes: page.bytes,
                sha256: page.sha256.clone()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    (job.status == JobStatus::Complete && !pages.is_empty()).then(|| Record {
        chapter: job.chapter.clone(),
        pages,
    })
}
fn job_of(record: &Record) -> ChapterDownload {
    ChapterDownload {
        schema_version: 1,
        queue_order: 0,
        chapter: record.chapter.clone(),
        pages: record
            .pages
            .iter()
            .enumerate()
            .map(|(index, page)| DownloadedPage {
                resource: PageResource {
                    id: page.id.clone(),
                    url: String::new(),
                    headers: BTreeMap::new(),
                },
                filename: reader_downloads::page_name(index, &page.id),
                bytes: page.bytes,
                sha256: Some(page.sha256.clone()),
            })
            .collect(),
        status: JobStatus::Complete,
        error: None,
    }
}
fn identity(chapter: &ChapterId) -> String {
    key(
        "job",
        &serde_json::to_string(chapter).expect("ChapterId serializes"),
    )
}

/// The finished chapters on this device, as records. Reads manifests only: the
/// pages are verified when they are copied.
pub(crate) fn capture(downloads: &DownloadStore, name: &str) -> Result<Payload, String> {
    let mut payload = components::empty();
    for job in downloads.snapshot().map_err(|e| e.to_string())? {
        if let Some(record) = record_of(&job)
            && wants(name, &record.chapter)
        {
            payload.records.insert(
                identity(&record.chapter),
                serde_json::to_value(&record).map_err(|e| e.to_string())?,
            );
        }
    }
    Ok(payload)
}

pub(crate) fn validate(name: &str, payload: &Payload) -> Result<(), String> {
    let invalid = || format!("Invalid {name} record");
    for (record, value) in &payload.records {
        let Some(("job", id)) = split(record) else {
            return Err(invalid());
        };
        let parsed: Record = serde_json::from_value(value.clone()).map_err(|_| invalid())?;
        let mut ids = BTreeSet::new();
        if serde_json::to_string(&parsed.chapter).ok().as_deref() != Some(id)
            || !wants(name, &parsed.chapter)
            || parsed.chapter.local_id.is_empty()
            || parsed.chapter.series.local_id.is_empty()
            || parsed.chapter.series.source_id.is_empty()
            || parsed.pages.is_empty()
            || parsed.pages.len() > MAX_PAGES
            || parsed.pages.iter().any(|page| {
                page.id.is_empty()
                    || page.bytes == 0
                    || page.sha256.len() != 64
                    || !page
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    || !ids.insert(&page.id)
            })
        {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Write every page of the chapters in `payload` as stored entries, checking
/// each against the record the archive will carry.
pub(crate) fn write_blobs<W: std::io::Write + std::io::Seek>(
    writer: &mut Writer<W>,
    downloads: &DownloadStore,
    name: &str,
    payload: &Payload,
) -> Result<u64, String> {
    let mut written = 0;
    for value in payload.records.values() {
        let record: Record = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        let changed = || "A downloaded chapter changed during export; retry".to_owned();
        let inventory = downloads
            .page_inventory(&record.chapter)
            .map_err(|_| changed())?;
        if inventory.len() != record.pages.len() {
            return Err(changed());
        }
        for (index, (page, held)) in record.pages.iter().zip(&inventory).enumerate() {
            let filename = reader_downloads::page_name(index, &page.id);
            let file = std::fs::File::open(&held.path).map_err(|_| changed())?;
            let (length, sha256) = writer.blob(
                &blob_path(name, &record.chapter, &filename),
                &mut BufReader::new(file),
            )?;
            if length != page.bytes || sha256 != page.sha256 {
                return Err(format!(
                    "A page of {} is damaged on this device; download it again or leave downloads out",
                    record.chapter.local_id
                ));
            }
            written += length;
        }
    }
    Ok(written)
}

/// What restoring a files component does.
#[derive(Clone, Debug, Default)]
pub(crate) struct Plan {
    pub add: Vec<(String, ChapterId, Value)>,
    pub remove: Vec<ChapterId>,
    pub bytes: u64,
}

/// Chapters `after` has and `local` lacks are copied in; chapters `local` has
/// and `after` lacks (only an undo produces them) are removed.
pub(crate) fn plan(
    name: &str,
    local: &Payload,
    after: &Payload,
    plan: &mut Plan,
    notes: &mut Vec<String>,
) {
    for (record, value) in &after.records {
        if !local.records.contains_key(record)
            && let Ok(parsed) = serde_json::from_value::<Record>(value.clone())
        {
            plan.bytes += parsed.pages.iter().map(|page| page.bytes).sum::<u64>();
            plan.add
                .push((name.to_owned(), parsed.chapter, value.clone()));
        }
    }
    for (record, value) in &local.records {
        if !after.records.contains_key(record)
            && let Ok(parsed) = serde_json::from_value::<Record>(value.clone())
        {
            plan.remove.push(parsed.chapter);
        }
    }
    if !plan.add.is_empty() || !plan.remove.is_empty() {
        notes.push(format!(
            "{}: {} {}, {:.1} MiB",
            nova_reader_ui::backend_text::tr(if plan.remove.is_empty() {
                "Files to copy"
            } else {
                "Files to change"
            }),
            plan.add.len() + plan.remove.len(),
            nova_reader_ui::backend_text::tr("chapters"),
            plan.bytes as f64 / (1024.0 * 1024.0)
        ));
    }
}

/// The result of copying chapters in.
#[derive(Default)]
pub(crate) struct Outcome {
    /// Record identities (per component) that now exist on this device.
    pub added: BTreeMap<String, BTreeSet<String>>,
    pub failures: Vec<String>,
}

/// Copy the planned chapters from the archive file into the download store. A
/// chapter that fails (damaged page, no room, already present) is reported and
/// skipped; the others still arrive, each whole.
pub(crate) fn import(services: &Services, archive: &Path, plan: &Plan) -> Result<Outcome, String> {
    use nova_backup::archive::{
        BLOB_PREFIX as PREFIX, MAX_BLOBS, MAX_BYTES, MAX_ENTRIES, MAX_FILE,
    };
    use nova_storage::zip::{Archive, Limits, Policy, Streamed};
    let mut outcome = Outcome::default();
    if plan.add.is_empty() {
        return Ok(outcome);
    }
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = Archive::open_streaming(
        BufReader::new(file),
        Limits {
            max_entries: MAX_ENTRIES,
            max_entry_bytes: MAX_FILE,
            max_expanded_bytes: MAX_BYTES as u64,
        },
        Policy::FILES,
        Streamed {
            prefix: PREFIX,
            max_entries: MAX_BLOBS,
        },
    )
    .map_err(|e| e.to_string())?;
    let cap = reader_engine::current_limits().max_encoded_bytes as u64;
    for (name, chapter, value) in &plan.add {
        let record: Record = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        let result = services
            .downloads
            .import_complete(job_of(&record), |_, page| {
                let path = blob_path(name, &record.chapter, &page.filename);
                let entry = zip
                    .find(&path)
                    .map(|entry| entry.index)
                    .ok_or_else(|| "The backup is missing a downloaded page".to_owned())?;
                let mut bytes = Vec::with_capacity(page.bytes.min(1 << 24) as usize);
                zip.copy(entry, cap.min(page.bytes), &mut bytes)
                    .map_err(|e| e.to_string())?;
                Ok(bytes)
            });
        match result {
            Ok(true) => {
                outcome
                    .added
                    .entry(name.clone())
                    .or_default()
                    .insert(identity(chapter));
            }
            // Already on this device: nothing to add, nothing to undo.
            Ok(false) => {}
            Err(error) => outcome
                .failures
                .push(format!("{}: {error}", chapter.local_id)),
        }
    }
    Ok(outcome)
}

/// Remove chapters an undo takes back. Returns what could not be removed.
pub(crate) fn remove(services: &Services, plan: &Plan) -> Vec<String> {
    plan.remove
        .iter()
        .filter_map(|chapter| {
            services
                .downloads
                .remove(chapter)
                .err()
                .map(|error| format!("{}: {error}", chapter.local_id))
        })
        .collect()
}
