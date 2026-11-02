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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DownloadProgress {
    pub queued: usize,
    pub running: usize,
    pub completed_pages: usize,
    pub total_pages: usize,
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
fn is_job_key(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        name.len() == 64
            && name
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}
fn is_page_file(name: &str, extension: &str) -> bool {
    let Some(stem) = name.strip_suffix(extension) else {
        return false;
    };
    stem.len() == 71
        && stem.as_bytes()[..6].iter().all(u8::is_ascii_digit)
        && stem.as_bytes()[6] == b'-'
        && is_job_key(std::ffi::OsStr::new(&stem[7..]))
}
fn owned_directories(root: &Path) -> Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if is_job_key(&entry.file_name()) && kind.is_dir() {
            directories.push(entry.path());
        }
    }
    Ok(directories)
}
/// Only generated files in a hash-named job directory belong to the store.
/// Never recursively delete an unknown directory or a user's unrelated files.
fn clean_job_files(directory: &Path, retained: &BTreeSet<String>) -> Result<()> {
    if fs::symlink_metadata(directory)?.file_type().is_symlink() {
        return Err(Error::Invalid(
            "download directory cannot be a symlink".into(),
        ));
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name != "manifest.pending"
            && !is_page_file(name, ".pending")
            && !(is_page_file(name, ".page") && !retained.contains(name))
        {
            continue;
        }
        if !entry.file_type()?.is_file() {
            return Err(Error::Invalid(
                "generated download file is not a regular file".into(),
            ));
        }
        fs::remove_file(entry.path())?;
    }
    Ok(())
}
fn clean_uncommitted_directory(directory: &Path) -> Result<()> {
    clean_job_files(directory, &BTreeSet::new())?;
    if fs::read_dir(directory)?.next().is_none() {
        fs::remove_dir(directory)?;
        #[cfg(unix)]
        File::open(directory.parent().unwrap())?.sync_all()?;
    }
    Ok(())
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
    // Cleanup also runs if a writer unwinds. A killed process is reconciled by
    // recover(), which recognizes the same generated pending filenames.
    struct PendingFile(PathBuf);
    impl Drop for PendingFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _pending = PendingFile(temporary.clone());
    (|| {
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
    })()
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
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(Error::Invalid(
            "download manifest is not a regular file".into(),
        ));
    }
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
    let directory = root.join(chapter_key(chapter));
    let kind = fs::symlink_metadata(&directory)?.file_type();
    if !kind.is_dir() || kind.is_symlink() {
        return Err(Error::Invalid(
            "download directory is not a regular directory".into(),
        ));
    }
    let job = load_manifest(&directory.join("manifest.json"))?;
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
    let directory = root.join(chapter_key(chapter));
    let mut retained = BTreeSet::new();
    for page in &mut job.pages {
        if verify(&directory.join(&page.filename), page)? {
            retained.insert(page.filename.clone());
        } else if page.sha256.is_some() || page.bytes != 0 {
            page.sha256 = None;
            page.bytes = 0;
            changed = true;
        }
    }
    // Includes corrupt tracked bytes, renamed pages whose manifest commit was
    // interrupted, and partial writes. Only verified committed pages survive.
    clean_job_files(&directory, &retained)?;
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
    for directory in owned_directories(root)? {
        for page in fs::read_dir(directory)? {
            let page = page?;
            if page.file_type()?.is_file()
                && page.file_name().to_str().is_some_and(|name| {
                    is_page_file(name, ".page") || is_page_file(name, ".pending")
                })
            {
                bytes = bytes
                    .checked_add(page.metadata()?.len())
                    .ok_or(Error::Quota)?;
            }
        }
    }
    Ok(bytes)
}
/// Releases admission and reconciles interrupted writes on both returned errors
/// and unwinding callbacks. Process termination is handled by startup recovery.
struct ActiveRun {
    store: DownloadStore,
    chapter: ChapterId,
    finished: bool,
}
impl ActiveRun {
    fn finish(&mut self, error: Option<String>) -> Result<()> {
        let mut inner = self.store.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.active.remove(&chapter_key(&self.chapter));
        self.finished = true;
        let mut job = checked_job(&inner.root, &self.chapter)?;
        if job.status == JobStatus::Running
            && let Some(error) = error
        {
            job.status = JobStatus::Failed;
            job.error = Some(error);
            save(&inner.root, &job)?;
        }
        Ok(())
    }
}
impl Drop for ActiveRun {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.finish(Some("download worker interrupted".into()));
        }
    }
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
        private_directory(&directory)?;
        let result = save(
            &inner.root,
            &ChapterDownload {
                schema_version: 1,
                queue_order: order,
                chapter,
                pages,
                status: JobStatus::Queued,
                error: None,
            },
        );
        if result.is_err() && !directory.join("manifest.json").exists() {
            clean_uncommitted_directory(&directory)?;
        }
        result?;
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
        for directory in owned_directories(&inner.root)? {
            let job = match load_manifest(&directory.join("manifest.json")) {
                Ok(job) => job,
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if directory.file_name() != Some(std::ffi::OsStr::new(&chapter_key(&job.chapter))) {
                return Err(Error::Invalid("invalid download directory identity".into()));
            }
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
        for directory in owned_directories(&root)? {
            let job = match load_manifest(&directory.join("manifest.json")) {
                Ok(job) => job,
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if directory.file_name() != Some(std::ffi::OsStr::new(&chapter_key(&job.chapter))) {
                return Err(Error::Invalid("invalid download directory identity".into()));
            }
            statuses.insert(job.chapter, job.status);
        }
        Ok(statuses)
    }
    /// Metadata-only foreground-service progress. Inactive jobs are excluded;
    /// completed page counts describe committed hashes, without opening payloads.
    pub fn progress(&self) -> Result<DownloadProgress> {
        let root = self
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .root
            .clone();
        let mut progress = DownloadProgress::default();
        for directory in owned_directories(&root)? {
            let job = match load_manifest(&directory.join("manifest.json")) {
                Ok(job) => job,
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if directory.file_name() != Some(std::ffi::OsStr::new(&chapter_key(&job.chapter))) {
                return Err(Error::Invalid("invalid download directory identity".into()));
            }
            match job.status {
                JobStatus::Queued => progress.queued += 1,
                JobStatus::Running => progress.running += 1,
                _ => continue,
            }
            progress.completed_pages = progress
                .completed_pages
                .checked_add(job.completed_pages())
                .ok_or_else(|| Error::Invalid("download progress overflow".into()))?;
            progress.total_pages = progress
                .total_pages
                .checked_add(job.pages.len())
                .ok_or_else(|| Error::Invalid("download progress overflow".into()))?;
        }
        Ok(progress)
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
        let mut active = ActiveRun {
            store: self.clone(),
            chapter: chapter.clone(),
            finished: false,
        };
        let result = self.run_pages(chapter, &mut fetch);
        active.finish(result.as_ref().err().map(ToString::to_string))?;
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
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !inner.active.is_empty() {
            return Err(Error::Invalid("cannot recover active downloads".into()));
        }
        let mut highest_order = 0;
        for directory in owned_directories(&inner.root)? {
            let manifest = match load_manifest(&directory.join("manifest.json")) {
                Ok(job) => job,
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    // An initial enqueue may have died before its first atomic
                    // manifest publication. No committed pages exist to retain.
                    clean_uncommitted_directory(&directory)?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if directory.file_name() != Some(std::ffi::OsStr::new(&chapter_key(&manifest.chapter)))
            {
                return Err(Error::Invalid("invalid download directory identity".into()));
            }
            let mut job = checked_job(&inner.root, &manifest.chapter)?;
            highest_order = highest_order.max(job.queue_order);
            if job.status == JobStatus::Running {
                job.status = JobStatus::Queued;
                job.error = None;
                save(&inner.root, &job)?;
            }
        }
        inner.next_order = highest_order
            .checked_add(1)
            .ok_or_else(|| Error::Invalid("download queue sequence exhausted".into()))?;
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
            "png" | "jpg" | "jpeg" | "webp" | "avif" | "jxl"
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
                    .join(page_name(0, "0"))
                    .with_extension("pending"),
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
    fn cancelled_and_panicking_workers_keep_verified_pages_without_leaking_admission() {
        let directory = tempfile::tempdir().unwrap();
        let store = DownloadStore::open(directory.path(), 1_000_000).unwrap();
        let chapter = chapter();
        store.enqueue(chapter.clone(), pages()).unwrap();
        let mut fetched = 0;
        store
            .run(&chapter, |_| {
                fetched += 1;
                if fetched == 2 {
                    store.cancel(&chapter).unwrap();
                }
                Ok(png())
            })
            .unwrap();
        let job = store.job(&chapter).unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
        assert_eq!(job.completed_pages(), 1);
        assert!(store.inner.lock().unwrap().active.is_empty());
        assert_eq!(
            fs::read_dir(directory.path().join(chapter_key(&chapter)))
                .unwrap()
                .count(),
            2
        );
        store.recover().unwrap();
        assert_eq!(store.job(&chapter).unwrap().status, JobStatus::Cancelled);
        store.resume(&chapter).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.run(&chapter, |_| -> std::result::Result<Vec<u8>, String> {
                panic!("synthetic worker panic");
            })
        }));
        assert!(result.is_err());
        assert!(store.inner.lock().unwrap().active.is_empty());
        assert_eq!(store.job(&chapter).unwrap().status, JobStatus::Failed);
        assert_eq!(store.job(&chapter).unwrap().completed_pages(), 1);
        store.retry(&chapter).unwrap();
        let mut resumed = 0;
        store
            .run(&chapter, |_| {
                resumed += 1;
                Ok(png())
            })
            .unwrap();
        assert_eq!(resumed, 2);
        assert!(store.offline(&chapter).is_ok());
    }
    #[test]
    fn recovery_removes_only_generated_uncommitted_or_corrupt_bytes_and_frees_quota() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = png();
        let store = DownloadStore::open(directory.path(), bytes.len() as u64 * 3).unwrap();
        let chapter = chapter();
        store.enqueue(chapter.clone(), pages()).unwrap();
        let bundle = directory.path().join(chapter_key(&chapter));
        {
            let inner = store.inner.lock().unwrap();
            let mut job = load(&inner.root, &chapter).unwrap();
            fs::write(bundle.join(&job.pages[0].filename), &bytes).unwrap();
            job.pages[0].sha256 = Some(hash(&bytes));
            job.pages[0].bytes = bytes.len() as u64;
            fs::write(bundle.join(&job.pages[1].filename), b"corrupt").unwrap();
            job.pages[1].sha256 = Some(hash(&bytes));
            job.pages[1].bytes = bytes.len() as u64;
            job.status = JobStatus::Running;
            save(&inner.root, &job).unwrap();
            // Rename completed, but the manifest still has no hash for page 3.
            atomic_write(&bundle.join(&job.pages[2].filename), &bytes).unwrap();
            fs::write(bundle.join("manifest.pending"), b"unfinished JSON").unwrap();
            fs::write(bundle.join(page_name(50, "orphan")), b"orphan").unwrap();
            fs::write(
                bundle
                    .join(page_name(51, "orphan"))
                    .with_extension("pending"),
                b"partial",
            )
            .unwrap();
            fs::write(bundle.join("notes.page"), b"unrelated file").unwrap();
        }
        let unowned = directory.path().join("personal-notes");
        fs::create_dir(&unowned).unwrap();
        fs::write(unowned.join("notes.pending"), b"unrelated directory").unwrap();
        // A copied manifest in an unrelated folder must not become a duplicate job.
        fs::copy(bundle.join("manifest.json"), unowned.join("manifest.json")).unwrap();
        drop(store);
        let store = DownloadStore::open(directory.path(), bytes.len() as u64 * 3).unwrap();
        let job = store.job(&chapter).unwrap();
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.completed_pages(), 1);
        assert_eq!(job.downloaded_bytes(), bytes.len() as u64);
        assert_eq!(usage(directory.path()).unwrap(), bytes.len() as u64);
        assert_eq!(store.jobs().unwrap().len(), 1);
        assert!(!bundle.join(&job.pages[1].filename).exists());
        assert!(!bundle.join(&job.pages[2].filename).exists());
        assert!(!bundle.join("manifest.pending").exists());
        assert_eq!(
            fs::read(bundle.join("notes.page")).unwrap(),
            b"unrelated file"
        );
        assert_eq!(
            fs::read(unowned.join("notes.pending")).unwrap(),
            b"unrelated directory"
        );
        let mut fetched = 0;
        store
            .run(&chapter, |_| {
                fetched += 1;
                Ok(bytes.clone())
            })
            .unwrap();
        assert_eq!(fetched, 2);
        assert_eq!(usage(directory.path()).unwrap(), bytes.len() as u64 * 3);
    }
    #[test]
    fn metadata_progress_never_reads_or_repairs_payloads_and_excludes_inactive_jobs() {
        let directory = tempfile::tempdir().unwrap();
        let store = DownloadStore::open(directory.path(), 1_000_000).unwrap();
        let chapter = chapter();
        store.enqueue(chapter.clone(), pages()).unwrap();
        let other = ChapterId::new(chapter.series.clone(), "other");
        store.enqueue(other.clone(), pages()).unwrap();
        store.pause(&other).unwrap();
        let complete = ChapterId::new(chapter.series.clone(), "complete");
        store.enqueue(complete.clone(), pages()).unwrap();
        store.run(&complete, |_| Ok(png())).unwrap();
        let bundle = directory.path().join(chapter_key(&chapter));
        {
            let inner = store.inner.lock().unwrap();
            let mut job = load(&inner.root, &chapter).unwrap();
            job.status = JobStatus::Running;
            for page in job.pages.iter_mut().take(2) {
                page.bytes = 123;
                page.sha256 = Some(hash(b"committed before corruption"));
            }
            save(&inner.root, &job).unwrap();
            // A directory instead of a page would make payload verification or
            // cleanup fail, proving this path reads only the published manifest.
            fs::create_dir(bundle.join(&job.pages[0].filename)).unwrap();
            fs::write(bundle.join(&job.pages[1].filename), b"corrupt").unwrap();
        }
        assert_eq!(
            store.progress().unwrap(),
            DownloadProgress {
                queued: 0,
                running: 1,
                completed_pages: 2,
                total_pages: 3,
            }
        );
        assert_eq!(
            fs::read(bundle.join(page_name(1, "1"))).unwrap(),
            b"corrupt"
        );
        fs::remove_dir(bundle.join(page_name(0, "0"))).unwrap();
        let queued = ChapterId::new(chapter.series.clone(), "queued");
        store.enqueue(queued, pages()).unwrap();
        assert_eq!(
            store.progress().unwrap(),
            DownloadProgress {
                queued: 1,
                running: 1,
                completed_pages: 2,
                total_pages: 6,
            }
        );
    }
    #[cfg(unix)]
    #[test]
    fn recovery_refuses_symlinks_without_touching_external_payloads() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let store = DownloadStore::open(directory.path(), 1_000_000).unwrap();
        let chapter = chapter();
        store.enqueue(chapter.clone(), pages()).unwrap();
        let target = outside.path().join("payload");
        fs::write(&target, b"unrelated bytes").unwrap();
        let link = directory
            .path()
            .join(chapter_key(&chapter))
            .join(page_name(0, "0"));
        symlink(&target, &link).unwrap();
        assert!(store.recover().is_err());
        assert_eq!(fs::read(&target).unwrap(), b"unrelated bytes");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        drop(store);
        assert!(DownloadStore::open(directory.path(), 1_000_000).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"unrelated bytes");
    }
    #[test]
    #[ignore = "invoked only by killed_writer_recovery via a subprocess"]
    fn crash_writer_child() {
        let Some(root) = std::env::var_os("NOVA_DOWNLOAD_CRASH_ROOT") else {
            return;
        };
        let ready = std::env::var_os("NOVA_DOWNLOAD_CRASH_READY").unwrap();
        let root = PathBuf::from(root);
        let store = DownloadStore::open(&root, 1_000_000).unwrap();
        let chapter = chapter();
        store.enqueue(chapter.clone(), pages()).unwrap();
        let mut fetched = 0;
        store
            .run(&chapter, |_| {
                fetched += 1;
                if fetched == 2 {
                    let inner = store.inner.lock().unwrap();
                    let job = load(&inner.root, &chapter).unwrap();
                    let bundle = root.join(chapter_key(&chapter));
                    // Inject the precise crash windows using the production writer:
                    // a renamed page before its hash commit, and a partial page write.
                    atomic_write(&bundle.join(&job.pages[1].filename), &png()).unwrap();
                    fs::write(
                        bundle
                            .join(&job.pages[2].filename)
                            .with_extension("pending"),
                        b"torn write",
                    )
                    .unwrap();
                    let initial = root.join(chapter_key(&ChapterId::new(
                        chapter.series.clone(),
                        "initial-enqueue",
                    )));
                    private_directory(&initial).unwrap();
                    fs::write(initial.join("manifest.pending"), b"{\"schema_version\":1").unwrap();
                    drop(inner);
                    fs::write(&ready, b"ready").unwrap();
                    loop {
                        std::thread::park();
                    }
                }
                Ok(png())
            })
            .unwrap();
    }
    #[test]
    fn killed_writer_recovery_keeps_committed_pages_and_removes_crash_artifacts() {
        use std::{
            process::{Child, Command},
            time::{Duration, Instant},
        };
        struct KillOnDrop(Child);
        impl Drop for KillOnDrop {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("downloads");
        let ready = directory.path().join("child-ready");
        let mut child = KillOnDrop(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::crash_writer_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("NOVA_DOWNLOAD_CRASH_ROOT", &root)
                .env("NOVA_DOWNLOAD_CRASH_READY", &ready)
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "crash fixture exited before its injected write"
            );
            assert!(
                Instant::now() < deadline,
                "crash fixture did not reach its injected write"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        let store = DownloadStore::open(&root, png().len() as u64 * 3).unwrap();
        let chapter = chapter();
        assert_eq!(store.job(&chapter).unwrap().completed_pages(), 1);
        assert_eq!(store.job(&chapter).unwrap().status, JobStatus::Queued);
        assert_eq!(
            fs::read_dir(root.join(chapter_key(&chapter)))
                .unwrap()
                .count(),
            2
        );
        assert!(
            !root
                .join(chapter_key(&ChapterId::new(
                    chapter.series.clone(),
                    "initial-enqueue"
                )))
                .exists()
        );
        let mut fetched = 0;
        store
            .run(&chapter, |_| {
                fetched += 1;
                Ok(png())
            })
            .unwrap();
        assert_eq!(fetched, 2);
        assert!(store.offline(&chapter).is_ok());
        assert_eq!(usage(&root).unwrap(), png().len() as u64 * 3);
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
        let jxl = include_bytes!("../../reader-engine/tests/fixtures/jxl/rgb.jxl");
        fs::write(input.path().join("12.JXL"), jxl).unwrap();
        let store = DownloadStore::open(directory.path(), 1_000_000).unwrap();
        let chapter = chapter();
        store.import_folder(chapter.clone(), input.path()).unwrap();
        let offline = store.offline(&chapter).unwrap();
        assert_eq!(fs::read(&offline.pages[0]).unwrap(), one);
        assert_eq!(offline.pages.len(), 4);
        assert_eq!(fs::read(&offline.pages[2]).unwrap(), avif);
        assert_eq!(fs::read(&offline.pages[3]).unwrap(), jxl);
        let avif_archive = input.path().join("avif.cbz");
        let mut zip = zip::ZipWriter::new(File::create(&avif_archive).unwrap());
        zip.start_file("page.avif", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(avif).unwrap();
        zip.start_file("page2.jxl", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(jxl).unwrap();
        zip.finish().unwrap();
        let archived = ChapterId::new(chapter.series.clone(), "avif-archive");
        store.import_cbz(archived.clone(), avif_archive).unwrap();
        let imported = store.offline(&archived).unwrap();
        assert_eq!(fs::read(&imported.pages[0]).unwrap(), avif);
        assert_eq!(fs::read(&imported.pages[1]).unwrap(), jxl);
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
