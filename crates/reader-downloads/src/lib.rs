//! Durable ordered chapter bundles. Networking remains in the source adapter;
//! this crate receives bytes through a callback and never opens a URL itself.
use reader_domain::ChapterId;
use reader_engine::{DecodeLimits, decode_image};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Image(#[from] reader_engine::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error("{0}")]
    Invalid(String),
    #[error("download quota exceeded")]
    Quota,
    #[error("chapter is not fully available offline")]
    Incomplete,
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PageResource {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Running,
    Paused,
    Cancelled,
    Failed,
    Complete,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadedPage {
    pub resource: PageResource,
    pub filename: String,
    pub bytes: u64,
    pub sha256: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChapterDownload {
    pub schema_version: u32,
    #[serde(default)]
    pub queue_order: u64,
    pub chapter: ChapterId,
    pub pages: Vec<DownloadedPage>,
    pub status: JobStatus,
    pub error: Option<String>,
}
impl ChapterDownload {
    pub fn completed_pages(&self) -> usize {
        self.pages.iter().filter(|p| p.sha256.is_some()).count()
    }
    pub fn downloaded_bytes(&self) -> u64 {
        self.pages.iter().map(|p| p.bytes).sum()
    }
}
#[derive(Clone, Debug)]
pub struct OfflineChapter {
    pub chapter: ChapterId,
    pub pages: Vec<PathBuf>,
}
struct Inner {
    root: PathBuf,
    quota: u64,
    active: BTreeMap<String, String>,
    limits: DecodeLimits,
    _lock: File,
    next_order: u64,
}
#[derive(Clone)]
pub struct DownloadStore {
    inner: Arc<Mutex<Inner>>,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn chapter_key(chapter: &ChapterId) -> String {
    hash(&serde_json::to_vec(chapter).expect("ChapterId serializes"))
}
fn page_name(index: usize, id: &str) -> String {
    format!("{index:06}-{}.page", hash(id.as_bytes()))
}
fn private_directory(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path)?;
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(Error::Invalid(
            "download directories cannot be symlinks".into(),
        ));
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
/// Existing manifests can contain signed URLs and source authorization headers.
/// Repair old permissive stores before reading any manifests or creating files.
fn protect_existing_tree(root: &Path) -> Result<()> {
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(Error::Invalid(
                    "download storage cannot contain symlinks".into(),
                ));
            }
            if kind.is_dir() {
                #[cfg(unix)]
                fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o700))?;
                directories.push(entry.path());
            } else if kind.is_file() {
                #[cfg(unix)]
                fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o600))?;
            } else {
                return Err(Error::Invalid(
                    "download storage contains a non-regular file".into(),
                ));
            }
        }
    }
    Ok(())
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("pending");
    // A prior killed writer may leave a pending file. Never follow a planted
    // symlink or keep failed-write bytes charged against every subsequent retry.
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let result = (|| {
        let mut options = File::options();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        #[cfg(unix)]
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        // std uses replacement-capable file rename on Windows as well as Unix.
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        File::open(path.parent().unwrap())?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
fn save(root: &Path, job: &ChapterDownload) -> Result<()> {
    let bytes = serde_json::to_vec(job)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(Error::Invalid("oversized download manifest".into()));
    }
    atomic_write(
        &root.join(chapter_key(&job.chapter)).join("manifest.json"),
        &bytes,
    )
}
fn load_manifest(path: &Path) -> Result<ChapterDownload> {
    let file = File::open(path)?;
    if file.metadata()?.len() > 16 * 1024 * 1024 {
        return Err(Error::Invalid("oversized download manifest".into()));
    }
    let job: ChapterDownload = serde_json::from_reader(file)?;
    if job.schema_version != 1
        || job.pages.is_empty()
        || job.pages.len() > 10000
        || job
            .pages
            .iter()
            .enumerate()
            .any(|(i, p)| p.filename != page_name(i, &p.resource.id))
    {
        return Err(Error::Invalid("invalid download manifest".into()));
    }
    Ok(job)
}
fn load(root: &Path, chapter: &ChapterId) -> Result<ChapterDownload> {
    let job = load_manifest(&root.join(chapter_key(chapter)).join("manifest.json"))?;
    if &job.chapter != chapter {
        return Err(Error::Invalid("invalid download manifest".into()));
    }
    Ok(job)
}
fn verify(path: &Path, page: &DownloadedPage) -> Result<bool> {
    let Some(expected) = &page.sha256 else {
        return Ok(false);
    };
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != page.bytes {
        return Ok(false);
    }
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()) == *expected)
}
fn checked_job(root: &Path, chapter: &ChapterId) -> Result<ChapterDownload> {
    let mut job = load(root, chapter)?;
    let mut changed = false;
    for page in &mut job.pages {
        if page.sha256.is_some()
            && !verify(&root.join(chapter_key(chapter)).join(&page.filename), page)?
        {
            page.sha256 = None;
            page.bytes = 0;
            changed = true;
        }
    }
    if job.status == JobStatus::Complete && job.pages.iter().any(|p| p.sha256.is_none()) {
        job.status = JobStatus::Queued;
        job.error = Some("missing or corrupt downloaded page".into());
        changed = true;
    }
    if changed {
        save(root, &job)?;
    }
    Ok(job)
}
fn usage(root: &Path) -> Result<u64> {
    let mut bytes = 0u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        for page in fs::read_dir(entry.path())? {
            let page = page?;
            if page.file_type()?.is_file()
                && page
                    .path()
                    .extension()
                    .is_some_and(|x| x == "page" || x == "pending")
            {
                bytes = bytes
                    .checked_add(page.metadata()?.len())
                    .ok_or(Error::Quota)?;
            }
        }
    }
    Ok(bytes)
}
impl DownloadStore {
    pub fn open(root: impl AsRef<Path>, quota: u64) -> Result<Self> {
        private_directory(root.as_ref())?;
        let root = fs::canonicalize(root)?;
        protect_existing_tree(&root)?;
        let mut options = File::options();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        options.mode(0o600);
        let lock = options.open(root.join(".lock"))?;
        #[cfg(unix)]
        lock.set_permissions(fs::Permissions::from_mode(0o600))?;
        lock.try_lock()
            .map_err(|e| Error::Invalid(format!("download directory is already open: {e}")))?;
        let store = Self {
            inner: Arc::new(Mutex::new(Inner {
                root,
                quota,
                active: BTreeMap::new(),
                limits: DecodeLimits::default(),
                _lock: lock,
                next_order: 1,
            })),
        };
        store.recover()?;
        Ok(store)
    }
    pub fn enqueue(&self, chapter: ChapterId, pages: Vec<PageResource>) -> Result<()> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if pages.is_empty()
            || pages.len() > 10000
            || chapter.local_id.is_empty()
            || chapter.series.local_id.is_empty()
            || chapter.series.source_id.is_empty()
        {
            return Err(Error::Invalid("invalid chapter page list".into()));
        }
        let mut ids = BTreeSet::new();
        if pages.iter().any(|p| p.id.is_empty() || !ids.insert(&p.id)) {
            return Err(Error::Invalid("duplicate or empty page identity".into()));
        }
        let key = chapter_key(&chapter);
        if inner.active.contains_key(&key) {
            return Err(Error::Invalid("chapter already running".into()));
        }
        let directory = inner.root.join(key);
        private_directory(&directory)?;
        if directory.join("manifest.json").exists() {
            return Err(Error::Invalid(
                "chapter already queued; retry existing job or cancel first".into(),
            ));
        }
        let pages = pages
            .into_iter()
            .enumerate()
            .map(|(i, resource)| DownloadedPage {
                filename: page_name(i, &resource.id),
                resource,
                bytes: 0,
                sha256: None,
            })
            .collect();
        let order = inner.next_order;
        let next = order
            .checked_add(1)
            .ok_or_else(|| Error::Invalid("download queue sequence exhausted".into()))?;
        save(
            &inner.root,
            &ChapterDownload {
                schema_version: 1,
                queue_order: order,
                chapter,
                pages,
                status: JobStatus::Queued,
                error: None,
            },
        )?;
        inner.next_order = next;
        Ok(())
    }
    pub fn job(&self, chapter: &ChapterId) -> Result<ChapterDownload> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        checked_job(&inner.root, chapter)
    }
    pub fn jobs(&self) -> Result<Vec<ChapterDownload>> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut jobs = Vec::new();
        for directory in fs::read_dir(&inner.root)? {
            let directory = directory?;
            if !directory.file_type()?.is_dir() {
                continue;
            }
            let path = directory.path().join("manifest.json");
            if !path.exists() {
                continue;
            }
            let file = File::open(path)?;
            if file.metadata()?.len() > 16 * 1024 * 1024 {
                return Err(Error::Invalid("oversized manifest".into()));
            }
            let job: ChapterDownload = serde_json::from_reader(file)?;
            jobs.push(checked_job(&inner.root, &job.chapter)?);
        }
        jobs.sort_by_key(|job| job.queue_order);
        Ok(jobs)
    }
    /// Persisted status hints for list actions, without reading page payloads
    /// or hashing chapter bundles. Call `offline` before opening stored pages.
    /// Manifest publication is atomic, so readers need no lock during disk I/O.
    pub fn statuses(&self) -> Result<BTreeMap<ChapterId, JobStatus>> {
        let root = self
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .root
            .clone();
        let mut statuses = BTreeMap::new();
        for directory in fs::read_dir(&root)? {
            let directory = directory?;
            let kind = match directory.file_type() {
                Ok(kind) => kind,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if !kind.is_dir() {
                continue;
            }
            let job = match load_manifest(&directory.path().join("manifest.json")) {
                Ok(job) => job,
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if directory.file_name() != std::ffi::OsStr::new(&chapter_key(&job.chapter)) {
                return Err(Error::Invalid("invalid download directory identity".into()));
            }
            statuses.insert(job.chapter, job.status);
        }
        Ok(statuses)
    }
    fn status(&self, chapter: &ChapterId, status: JobStatus) -> Result<()> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut job = checked_job(&inner.root, chapter)?;
        if job.status == JobStatus::Complete {
            return Err(Error::Invalid("chapter already complete".into()));
        }
        job.status = status;
        job.error = None;
        save(&inner.root, &job)
    }
    pub fn pause(&self, chapter: &ChapterId) -> Result<()> {
        self.status(chapter, JobStatus::Paused)
    }
    pub fn resume(&self, chapter: &ChapterId) -> Result<()> {
        self.status(chapter, JobStatus::Queued)
    }
    pub fn retry(&self, chapter: &ChapterId) -> Result<()> {
        self.resume(chapter)
    }
    /// Refresh expiring URLs/headers without changing stable logical pages or
    /// discarding verified bytes. A changed page list requires explicit user
    /// reconciliation rather than silently reusing a different edition.
    pub fn refresh_resources(&self, chapter: &ChapterId, pages: Vec<PageResource>) -> Result<()> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.active.contains_key(&chapter_key(chapter)) {
            return Err(Error::Invalid("chapter already running".into()));
        }
        let mut job = checked_job(&inner.root, chapter)?;
        if pages.len() != job.pages.len()
            || pages
                .iter()
                .zip(&job.pages)
                .any(|(new, old)| new.id != old.resource.id)
        {
            return Err(Error::Invalid(
                "changed page identities require explicit reconciliation".into(),
            ));
        }
        for (page, resource) in job.pages.iter_mut().zip(pages) {
            page.resource = resource;
        }
        save(&inner.root, &job)
    }
    pub fn cancel(&self, chapter: &ChapterId) -> Result<()> {
        self.status(chapter, JobStatus::Cancelled)
    }
    /// Explicitly remove originals and the durable job. Cancellation alone
    /// retains partial verified pages so retry does not redownload them.
    pub fn remove(&self, chapter: &ChapterId) -> Result<()> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.active.contains_key(&chapter_key(chapter)) {
            return Err(Error::Invalid("chapter already running".into()));
        }
        fs::remove_dir_all(inner.root.join(chapter_key(chapter)))?;
        #[cfg(unix)]
        File::open(&inner.root)?.sync_all()?;
        Ok(())
    }
    /// Fetch happens outside the lock. Pause/cancel are observed before publish;
    /// callbacks must bound their own streaming buffers and honor HTTP policy.
    pub fn run(
        &self,
        chapter: &ChapterId,
        mut fetch: impl FnMut(&PageResource) -> std::result::Result<Vec<u8>, String>,
    ) -> Result<()> {
        let key = chapter_key(chapter);
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if inner.active.contains_key(&key) {
                return Err(Error::Invalid("chapter already running".into()));
            }
            if inner
                .active
                .values()
                .filter(|source| *source == &chapter.series.source_id)
                .count()
                >= 2
            {
                return Err(Error::Invalid(
                    "source download concurrency limit reached".into(),
                ));
            }
            let mut job = load(&inner.root, chapter)?;
            if !matches!(job.status, JobStatus::Queued | JobStatus::Failed) {
                return Err(Error::Invalid("chapter is not queued".into()));
            }
            job.status = JobStatus::Running;
            job.error = None;
            save(&inner.root, &job)?;
            inner
                .active
                .insert(key.clone(), chapter.series.source_id.clone());
        }
        let result = self.run_pages(chapter, &mut fetch);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.active.remove(&key);
        if let Err(error) = &result {
            let mut job = load(&inner.root, chapter)?;
            if job.status == JobStatus::Running {
                job.status = JobStatus::Failed;
                job.error = Some(error.to_string());
                save(&inner.root, &job)?;
            }
        }
        result
    }
    fn run_pages(
        &self,
        chapter: &ChapterId,
        fetch: &mut impl FnMut(&PageResource) -> std::result::Result<Vec<u8>, String>,
    ) -> Result<()> {
        let count = self.job(chapter)?.pages.len();
        for index in 0..count {
            let resource = {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                let mut job = load(&inner.root, chapter)?;
                if !matches!(job.status, JobStatus::Running | JobStatus::Queued) {
                    return Ok(());
                }
                let path = inner
                    .root
                    .join(chapter_key(chapter))
                    .join(&job.pages[index].filename);
                if verify(&path, &job.pages[index])? {
                    continue;
                }
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                job.pages[index].sha256 = None;
                job.pages[index].bytes = 0;
                save(&inner.root, &job)?;
                job.pages[index].resource.clone()
            };
            let bytes = fetch(&resource).map_err(Error::Invalid)?;
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let mut job = load(&inner.root, chapter)?;
            if !matches!(job.status, JobStatus::Running | JobStatus::Queued) {
                return Ok(());
            }
            // Decode validation rejects truncated streams, not merely forged
            // headers. The engine checks dimensions before allocation.
            decode_image(&bytes, inner.limits)?;
            let path = inner
                .root
                .join(chapter_key(chapter))
                .join(&job.pages[index].filename);
            if usage(&inner.root)?
                .checked_add(bytes.len() as u64)
                .is_none_or(|n| n > inner.quota)
            {
                return Err(Error::Quota);
            }
            atomic_write(&path, &bytes)?;
            job.pages[index].bytes = bytes.len() as u64;
            job.pages[index].sha256 = Some(hash(&bytes));
            save(&inner.root, &job)?;
        }
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut job = load(&inner.root, chapter)?;
        if !matches!(job.status, JobStatus::Running | JobStatus::Queued) {
            return Ok(());
        }
        for page in &job.pages {
            if !verify(
                &inner.root.join(chapter_key(chapter)).join(&page.filename),
                page,
            )? {
                return Err(Error::Incomplete);
            }
        }
        job.status = JobStatus::Complete;
        job.error = None;
        save(&inner.root, &job)
    }
    pub fn offline(&self, chapter: &ChapterId) -> Result<OfflineChapter> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let job = load(&inner.root, chapter)?;
        if job.status != JobStatus::Complete {
            return Err(Error::Incomplete);
        }
        let mut pages = Vec::new();
        for page in &job.pages {
            let path = inner.root.join(chapter_key(chapter)).join(&page.filename);
            if !verify(&path, page)? {
                return Err(Error::Incomplete);
            }
            pages.push(path);
        }
        Ok(OfflineChapter {
            chapter: chapter.clone(),
            pages,
        })
    }
    pub fn recover(&self) -> Result<()> {
        let jobs = self.jobs()?;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !inner.active.is_empty() {
            return Err(Error::Invalid("cannot recover active downloads".into()));
        }
        inner.next_order = jobs
            .iter()
            .map(|job| job.queue_order)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| Error::Invalid("download queue sequence exhausted".into()))?;
        for mut job in jobs {
            let directory = inner.root.join(chapter_key(&job.chapter));
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                if entry.file_type()?.is_file()
                    && entry.path().extension().is_some_and(|x| x == "pending")
                {
                    fs::remove_file(entry.path())?;
                }
            }
            let mut missing = false;
            for page in &mut job.pages {
                if !verify(&directory.join(&page.filename), page)? {
                    page.bytes = 0;
                    page.sha256 = None;
                    missing = true;
                }
            }
            if job.status == JobStatus::Running || (job.status == JobStatus::Complete && missing) {
                job.status = JobStatus::Queued;
            }
            save(&inner.root, &job)?;
        }
        Ok(())
    }
    pub fn import_folder(&self, chapter: ChapterId, folder: impl AsRef<Path>) -> Result<()> {
        let mut files = Vec::new();
        for entry in fs::read_dir(folder)? {
            let entry = entry?;
            if entry.file_type()?.is_file() && supported(&entry.path()) {
                files.push(entry.path());
            }
        }
        files.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
        if files.len() > 10000 {
            return Err(Error::Invalid("too many local pages".into()));
        }
        let pages = files
            .iter()
            .enumerate()
            .map(|(i, _)| PageResource {
                id: format!("local-{i}"),
                url: String::new(),
                headers: BTreeMap::new(),
            })
            .collect();
        self.enqueue(chapter.clone(), pages)?;
        self.run(&chapter, |resource| {
            let index = resource
                .id
                .strip_prefix("local-")
                .unwrap()
                .parse::<usize>()
                .unwrap();
            bounded_read(&files[index], DecodeLimits::default().max_encoded_bytes)
                .map_err(|e| e.to_string())
        })
    }
    pub fn import_cbz(&self, chapter: ChapterId, path: impl AsRef<Path>) -> Result<()> {
        let mut archive = zip::ZipArchive::new(File::open(path)?)?;
        if archive.len() > 10000 {
            return Err(Error::Invalid("too many archive entries".into()));
        }
        let mut entries = Vec::new();
        let cap = DecodeLimits::default().max_encoded_bytes;
        let mut total = 0u64;
        for index in 0..archive.len() {
            let entry = archive.by_index(index)?;
            let name = entry
                .enclosed_name()
                .ok_or_else(|| Error::Invalid("unsafe archive path".into()))?
                .to_owned();
            if entry.is_dir() {
                continue;
            }
            if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
                return Err(Error::Invalid("archive symlink".into()));
            }
            if supported(&name) {
                if entry.size() > cap as u64 {
                    return Err(Error::Invalid("oversized archive page".into()));
                }
                total = total.checked_add(entry.size()).ok_or(Error::Quota)?;
                entries.push((name, index));
            }
        }
        if total > self.inner.lock().unwrap_or_else(|e| e.into_inner()).quota {
            return Err(Error::Quota);
        }
        entries.sort_by(|a, b| natural_cmp(&a.0.to_string_lossy(), &b.0.to_string_lossy()));
        let pages = entries
            .iter()
            .enumerate()
            .map(|(i, _)| PageResource {
                id: format!("archive-{i}"),
                url: String::new(),
                headers: BTreeMap::new(),
            })
            .collect();
        self.enqueue(chapter.clone(), pages)?;
        self.run(&chapter, |resource| {
            let index = resource
                .id
                .strip_prefix("archive-")
                .unwrap()
                .parse::<usize>()
                .unwrap();
            let mut bytes = Vec::new();
            archive
                .by_index(entries[index].1)
                .map_err(|e| e.to_string())?
                .take(cap as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > cap {
                return Err("oversized archive page".into());
            }
            Ok(bytes)
        })
    }
}
fn supported(path: &Path) -> bool {
    path.extension().and_then(|x| x.to_str()).is_some_and(|x| {
        matches!(
            x.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "webp" | "avif"
        )
    })
}
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut x, mut y) = (0, 0);
    while x < a.len() && y < b.len() {
        if a[x].is_ascii_digit() && b[y].is_ascii_digit() {
            let (start_x, start_y) = (x, y);
            while x < a.len() && a[x].is_ascii_digit() {
                x += 1;
            }
            while y < b.len() && b[y].is_ascii_digit() {
                y += 1;
            }
            let mut left = &a[start_x..x];
            let mut right = &b[start_y..y];
            while left.len() > 1 && left[0] == b'0' {
                left = &left[1..];
            }
            while right.len() > 1 && right[0] == b'0' {
                right = &right[1..];
            }
            let order = left.len().cmp(&right.len()).then_with(|| left.cmp(right));
            if order != std::cmp::Ordering::Equal {
                return order;
            }
        } else {
            let order = a[x].to_ascii_lowercase().cmp(&b[y].to_ascii_lowercase());
            if order != std::cmp::Ordering::Equal {
                return order;
            }
            x += 1;
            y += 1;
        }
    }
    (a.len() - x).cmp(&(b.len() - y)).then_with(|| a.cmp(b))
}
fn bounded_read(path: &Path, cap: usize) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > cap as u64 {
        return Err(Error::Invalid("oversized local page".into()));
    }
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > cap {
        return Err(Error::Invalid("oversized local page".into()));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reader_domain::SeriesId;
    fn chapter() -> ChapterId {
        ChapterId::new(SeriesId::new("missing-addon", "series"), "12.5")
    }
    fn pages() -> Vec<PageResource> {
        (0..3)
            .map(|i| PageResource {
                id: format!("{i}"),
                url: format!("https://fixture/{i}"),
                headers: BTreeMap::new(),
            })
            .collect()
    }
    fn png() -> Vec<u8> {
        let mut data = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut data, image::ImageFormat::Png)
            .unwrap();
        data.into_inner()
    }
    #[test]
    fn interrupted_recovery_and_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let store = DownloadStore::open(directory.path(), 1_000_000).unwrap();
        let chapter = chapter();
        store.enqueue(chapter.clone(), pages()).unwrap();
        let mut count = 0;
        assert!(
            store
                .run(&chapter, |_| {
                    count += 1;
                    if count == 2 {
                        Err("process interrupted".into())
                    } else {
                        Ok(png())
                    }
                })
                .is_err()
        );
        assert_eq!(store.job(&chapter).unwrap().completed_pages(), 1);
        drop(store);
        let store = DownloadStore::open(directory.path(), 1_000_000).unwrap();
        store.retry(&chapter).unwrap();
        let mut fetched = 0;
        store
            .run(&chapter, |_| {
                fetched += 1;
                Ok(png())
            })
            .unwrap();
        assert_eq!(fetched, 2);
        let offline = store.offline(&chapter).unwrap();
        assert_eq!(offline.pages.len(), 3);
        fs::write(&offline.pages[1], b"corrupt").unwrap();
        assert!(store.offline(&chapter).is_err());
        store.recover().unwrap();
        assert_eq!(store.job(&chapter).unwrap().status, JobStatus::Queued);
        store.run(&chapter, |_| Ok(png())).unwrap();
        assert!(store.offline(&chapter).is_ok());
    }
    #[test]
    fn quota_pause_and_fake_completion() {
        let directory = tempfile::tempdir().unwrap();
        let store = DownloadStore::open(directory.path(), 1).unwrap();
        let chapter = chapter();
        store.enqueue(chapter.clone(), pages()).unwrap();
        assert!(matches!(
            store.run(&chapter, |_| Ok(png())),
            Err(Error::Quota)
        ));
        assert!(store.offline(&chapter).is_err());
        store.pause(&chapter).unwrap();
        assert!(store.run(&chapter, |_| Ok(png())).is_err());
        store.resume(&chapter).unwrap();
        assert!(store.run(&chapter, |_| Ok(b"fake image".to_vec())).is_err());
        assert_eq!(store.job(&chapter).unwrap().downloaded_bytes(), 0);
    }
    #[test]
    fn running_manifest_recovers_without_losing_verified_page() {
        let directory = tempfile::tempdir().unwrap();
        let store = DownloadStore::open(directory.path(), 1_000_000).unwrap();
        let chapter = chapter();
        store.enqueue(chapter.clone(), pages()).unwrap();
        {
            let inner = store.inner.lock().unwrap();
            let mut job = load(&inner.root, &chapter).unwrap();
            job.status = JobStatus::Running;
            save(&inner.root, &job).unwrap();
            fs::write(
                inner
                    .root
                    .join(chapter_key(&chapter))
                    .join("orphan.pending"),
                b"partial",
            )
            .unwrap();
        }
        drop(store);
        let store = DownloadStore::open(directory.path(), 1_000_000).unwrap();
        assert_eq!(store.job(&chapter).unwrap().status, JobStatus::Queued);
        let later = ChapterId::new(chapter.series.clone(), "later");
        store.enqueue(later.clone(), pages()).unwrap();
        let queue = store.jobs().unwrap();
        assert_eq!(queue[0].chapter, chapter);
        assert_eq!(queue[1].chapter, later);
        assert!(queue[0].queue_order < queue[1].queue_order);
        store.run(&chapter, |_| Ok(png())).unwrap();
        assert!(store.offline(&chapter).is_ok());
    }
    #[test]
    fn pause_during_fetch_refresh_and_atomic_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let store = DownloadStore::open(directory.path(), 1_000_000).unwrap();
        assert!(DownloadStore::open(directory.path(), 1_000_000).is_err());
        let chapter = chapter();
        store.enqueue(chapter.clone(), pages()).unwrap();
        store
            .run(&chapter, |_| {
                store.pause(&chapter).unwrap();
                Ok(png())
            })
            .unwrap();
        assert_eq!(store.job(&chapter).unwrap().completed_pages(), 0);
        let mut refreshed = pages();
        for page in &mut refreshed {
            page.url.push_str("?new-token");
        }
        store.refresh_resources(&chapter, refreshed).unwrap();
        let mut changed = pages();
        changed.swap(0, 1);
        assert!(store.refresh_resources(&chapter, changed).is_err());
        store.resume(&chapter).unwrap();
        store
            .run(&chapter, |page| {
                assert!(page.url.ends_with("?new-token"));
                Ok(png())
            })
            .unwrap();
        assert!(store.offline(&chapter).is_ok());
        let path = directory.path().join("replacement");
        atomic_write(&path, b"old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(fs::read(path).unwrap(), b"new");
    }
    #[test]
    fn local_natural_order_and_archive_traversal_rejection() {
        use zip::write::SimpleFileOptions;
        let directory = tempfile::tempdir().unwrap();
        let input = tempfile::tempdir().unwrap();
        let one = png();
        let mut two = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(3, 2)
            .write_to(&mut two, image::ImageFormat::Png)
            .unwrap();
        fs::write(input.path().join("10.png"), two.get_ref()).unwrap();
        fs::write(input.path().join("2.png"), &one).unwrap();
        let avif = include_bytes!("../../reader-engine/tests/fixtures/tiny.avif");
        fs::write(input.path().join("11.AVIF"), avif).unwrap();
        let store = DownloadStore::open(directory.path(), 1_000_000).unwrap();
        let chapter = chapter();
        store.import_folder(chapter.clone(), input.path()).unwrap();
        let offline = store.offline(&chapter).unwrap();
        assert_eq!(fs::read(&offline.pages[0]).unwrap(), one);
        assert_eq!(offline.pages.len(), 3);
        assert_eq!(fs::read(&offline.pages[2]).unwrap(), avif);
        let avif_archive = input.path().join("avif.cbz");
        let mut zip = zip::ZipWriter::new(File::create(&avif_archive).unwrap());
        zip.start_file("page.avif", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(avif).unwrap();
        zip.finish().unwrap();
        let archived = ChapterId::new(chapter.series.clone(), "avif-archive");
        store.import_cbz(archived.clone(), avif_archive).unwrap();
        let imported = store.offline(&archived).unwrap();
        assert_eq!(fs::read(&imported.pages[0]).unwrap(), avif);
        let archive_path = input.path().join("unsafe.cbz");
        let mut zip = zip::ZipWriter::new(File::create(&archive_path).unwrap());
        zip.start_file("../escape.png", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&png()).unwrap();
        zip.finish().unwrap();
        let other = ChapterId::new(chapter.series.clone(), "unsafe");
        assert!(store.import_cbz(other, &archive_path).is_err());
        assert!(!directory.path().join("escape.png").exists());
    }
    #[cfg(unix)]
    #[test]
    fn sensitive_bundle_permissions_are_private_repaired_and_preserved_on_retry() {
        fn mode(path: &Path) -> u32 {
            fs::metadata(path).unwrap().permissions().mode() & 0o777
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("downloads");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        let store = DownloadStore::open(&root, 1_000_000).unwrap();
        let chapter = chapter();
        let mut resources = pages();
        resources[0].url.push_str("?signed=fixture");
        resources[0]
            .headers
            .insert("Authorization".into(), "Bearer fixture".into());
        store.enqueue(chapter.clone(), resources).unwrap();
        store.run(&chapter, |_| Ok(png())).unwrap();
        let offline = store.offline(&chapter).unwrap();
        let bundle = root.join(chapter_key(&chapter));
        let manifest = bundle.join("manifest.json");
        let lock = root.join(".lock");
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&bundle), 0o700);
        for file in offline.pages.iter().chain([&manifest, &lock]) {
            assert_eq!(mode(file), 0o600);
        }

        // Simulate an installation made by the older permissive implementation.
        for folder in [&root, &bundle] {
            fs::set_permissions(folder, fs::Permissions::from_mode(0o755)).unwrap();
        }
        for file in offline.pages.iter().chain([&manifest, &lock]) {
            fs::set_permissions(file, fs::Permissions::from_mode(0o644)).unwrap();
        }
        drop(store);
        let store = DownloadStore::open(&root, 1_000_000).unwrap();
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&bundle), 0o700);
        for file in offline.pages.iter().chain([&manifest, &lock]) {
            assert_eq!(mode(file), 0o600);
        }

        fs::write(&offline.pages[0], b"corrupt").unwrap();
        assert_eq!(store.job(&chapter).unwrap().status, JobStatus::Queued);
        store.retry(&chapter).unwrap();
        let mut fetched = 0;
        store
            .run(&chapter, |_| {
                fetched += 1;
                Ok(png())
            })
            .unwrap();
        assert_eq!(fetched, 1);
        assert!(store.offline(&chapter).is_ok());
        for file in offline.pages.iter().chain([&manifest, &lock]) {
            assert_eq!(mode(file), 0o600);
        }
    }
}
