//! Original source reuse for capture. Disk display variants are never admitted.
use super::*;

pub(super) struct Bytes(pub Arc<Vec<u8>>);
impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

const RETAINED_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Scope {
    Local(PathBuf, u64, Option<std::time::SystemTime>),
    Remote(String),
}
pub(super) fn scope(input: &PageInput, source: Option<&Source>) -> Result<Option<Scope>> {
    match input {
        PageInput::Local { path, .. } => {
            let metadata = std::fs::metadata(path).map_err(error)?;
            Ok(Some(Scope::Local(
                path.clone(),
                metadata.len(),
                metadata.modified().ok(),
            )))
        }
        PageInput::Verified(resource) => match &resource.kind {
            crate::page_inventory::ResourceKind::Local { path, .. } => {
                let metadata = std::fs::metadata(path).map_err(error)?;
                Ok(Some(Scope::Local(
                    path.clone(),
                    metadata.len(),
                    metadata.modified().ok(),
                )))
            }
            crate::page_inventory::ResourceKind::Remote { source, page } => source
                .page_cache_scope(&page.url, &page.headers)
                .map(|v| v.map(Scope::Remote))
                .map_err(error),
            crate::page_inventory::ResourceKind::Unavailable { .. } => Ok(None),
        },
        PageInput::Remote(page) => source
            .ok_or_else(|| text::tr("Source unavailable"))?
            .page_cache_scope(&page.url, &page.headers)
            .map(|scope| scope.map(Scope::Remote))
            .map_err(error),
    }
}
#[derive(Clone)]
struct Original {
    bytes: Weak<Vec<u8>>,
    scope: Scope,
}
/// Weak references never extend the image worker's encoded-memory allowance.
#[derive(Default)]
pub(super) struct Originals {
    generation: u64,
    pages: BTreeMap<usize, Original>,
}
impl Originals {
    pub fn record(
        &mut self,
        generation: u64,
        index: usize,
        bytes: &Arc<Vec<u8>>,
        scope: Option<Scope>,
        original: bool,
    ) {
        if self.generation != generation {
            self.pages.clear();
            self.generation = generation;
        }
        self.pages.retain(|_, entry| entry.bytes.strong_count() > 0);
        // A window refresh can reuse the same original. A different response
        // from the compressed disk cache must remove its old provenance.
        if !original {
            if self.pages.get(&index).is_some_and(|entry| {
                !entry.bytes.ptr_eq(&Arc::downgrade(bytes)) || Some(&entry.scope) != scope.as_ref()
            }) {
                self.pages.remove(&index);
            }
            return;
        }
        if let Some(scope) = scope {
            self.pages.insert(
                index,
                Original {
                    bytes: Arc::downgrade(bytes),
                    scope,
                },
            );
        } else {
            self.pages.remove(&index);
        }
        while self.pages.len() > 128 {
            self.pages.pop_first();
        }
    }
    pub fn session(&self, generation: u64, indices: impl Iterator<Item = usize>) -> Session {
        let originals = indices
            .filter_map(|index| {
                (generation == self.generation)
                    .then(|| self.pages.get(&index).cloned())
                    .flatten()
                    .map(|entry| (index, entry))
            })
            .collect();
        Session(Arc::new(Mutex::new(Cache {
            originals,
            ..Cache::default()
        })))
    }
}
#[derive(Default)]
struct Cache {
    originals: BTreeMap<usize, Original>,
    retained: BTreeMap<usize, Arc<Vec<u8>>>,
    disabled: bool,
}
#[derive(Clone, Default)]
pub(super) struct Session(Arc<Mutex<Cache>>);
impl Session {
    /// Reserve the maximum before a fetch can grow the retained set. On a
    /// tight admission failure the caller drops retention and retries within
    /// the original compositor budget, preserving previously supported sizes.
    pub fn reservation(&self) -> u64 {
        if self.0.lock().unwrap_or_else(|e| e.into_inner()).disabled {
            0
        } else {
            RETAINED_BYTES as u64
        }
    }
    pub fn release(&self) {
        let mut cache = self.0.lock().unwrap_or_else(|e| e.into_inner());
        cache.retained.clear();
        cache.disabled = true;
    }
    pub fn read(
        &self,
        index: usize,
        input: &PageInput,
        source: Option<&Source>,
        cancel: &CancelToken,
    ) -> Result<Arc<Vec<u8>>> {
        cancel.check().map_err(error)?;
        let before = scope(input, source)?;
        let reused = {
            let cache = self.0.lock().unwrap_or_else(|e| e.into_inner());
            cache
                .originals
                .get(&index)
                .filter(|entry| Some(&entry.scope) == before.as_ref())
                .and_then(|entry| entry.bytes.upgrade())
        };
        let was_reused = reused.is_some();
        let bytes = if let Some(bytes) = reused {
            bytes
        } else {
            Arc::new(input.read(source, cancel)?)
        };
        cancel.check().map_err(error)?;
        let after = scope(input, source)?;
        if was_reused && before != after {
            return Err(text::tr("Image action cancelled"));
        }
        // A fetch can update cookies. Such a response is valid for this read,
        // but cannot be reused under the pre-fetch authorization context.
        if let Some(before) = before.filter(|scope| Some(scope) == after.as_ref()) {
            let mut cache = self.0.lock().unwrap_or_else(|e| e.into_inner());
            cache.originals.insert(
                index,
                Original {
                    bytes: Arc::downgrade(&bytes),
                    scope: before,
                },
            );
            if !cache.disabled && bytes.len() <= RETAINED_BYTES {
                cache.retained.remove(&index);
                let mut used: usize = cache.retained.values().map(|bytes| bytes.len()).sum();
                while used + bytes.len() > RETAINED_BYTES {
                    if let Some((_, old)) = cache.retained.pop_first() {
                        used -= old.len();
                    } else {
                        break;
                    }
                }
                cache.retained.insert(index, bytes.clone());
            }
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reader_sources::{HttpRequest, HttpResponse, SourceRegistry, SourceTransport};
    use std::{io::Write, sync::atomic::AtomicUsize, time::Instant};
    struct Transport {
        reads: AtomicUsize,
        partition: AtomicU64,
        delay: std::time::Duration,
        body: Vec<u8>,
    }
    impl SourceTransport for Transport {
        fn page_cache_partition(&self, _: &str) -> Option<String> {
            Some(self.partition.load(Ordering::Acquire).to_string())
        }
        fn request(
            &self,
            request: &HttpRequest,
            _: usize,
            cancel: &CancelToken,
        ) -> reader_sources::Result<HttpResponse> {
            cancel.check()?;
            self.reads.fetch_add(1, Ordering::AcqRel);
            std::thread::sleep(self.delay);
            Ok(HttpResponse {
                status: 200,
                url: request.url.clone(),
                body: self.body.clone(),
                ..Default::default()
            })
        }
    }
    fn source(
        path: &std::path::Path,
        delay: std::time::Duration,
    ) -> (SourceRegistry, Source, Arc<Transport>, PageInput) {
        let registry = SourceRegistry::open(path).unwrap();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let manifest = json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"capture-fixture","name":"Capture fixture","version":"1.0.0","language":"en","entry":"main.js","permissions":{"domains":["example.com"]},"capabilities":["pages"]});
        for (name, bytes) in [
            ("manifest.json", manifest.to_string().into_bytes()),
            (
                "main.js",
                b"globalThis.nova={request:()=>({pages:[]})};".to_vec(),
            ),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        registry
            .install_zip(&zip.finish().unwrap().into_inner())
            .unwrap();
        registry.set_enabled("capture-fixture", true).unwrap();
        let transport = Arc::new(Transport {
            reads: AtomicUsize::new(0),
            partition: AtomicU64::new(0),
            delay,
            body: {
                let mut bytes = std::io::Cursor::new(Vec::new());
                image::RgbaImage::from_pixel(64, 64, image::Rgba([10, 20, 30, 255]))
                    .write_to(&mut bytes, image::ImageFormat::Png)
                    .unwrap();
                bytes.into_inner()
            },
        });
        let source = registry
            .source("capture-fixture")
            .unwrap()
            .with_transport(transport.clone());
        let input = PageInput::Remote(reader_sources::Page {
            id: "p".into(),
            url: "https://example.com/p.png".into(),
            headers: BTreeMap::new(),
            width: None,
            height: None,
        });
        (registry, source, transport, input)
    }
    #[test]
    fn preview_adjust_and_save_reuse_originals_and_revalidate_authority() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, source, transport, input) = source(dir.path(), std::time::Duration::ZERO);
        let cancel = CancelToken::default();
        let bytes = Arc::new(input.read(Some(&source), &cancel).unwrap());
        let mut originals = Originals::default();
        originals.record(4, 0, &bytes, scope(&input, Some(&source)).unwrap(), true);
        let session = originals.session(4, [0].into_iter());
        for _ in 0..3 {
            assert!(Arc::ptr_eq(
                &bytes,
                &session.read(0, &input, Some(&source), &cancel).unwrap()
            ));
        }
        assert_eq!(
            transport.reads.load(Ordering::Acquire),
            1,
            "loaded page, opening, Preview and Save share one original response"
        );
        transport.partition.fetch_add(1, Ordering::AcqRel);
        session.read(0, &input, Some(&source), &cancel).unwrap();
        assert_eq!(
            transport.reads.load(Ordering::Acquire),
            2,
            "new profile/transport scope cannot reuse old bytes"
        );
        cancel.cancel();
        assert!(session.read(0, &input, Some(&source), &cancel).is_err());
        registry.set_enabled("capture-fixture", false).unwrap();
        assert!(
            session
                .read(0, &input, Some(&source), &CancelToken::default())
                .is_err()
        );
        assert_eq!(transport.reads.load(Ordering::Acquire), 2);
    }
    #[test]
    fn compressed_responses_and_old_generations_never_seed_originals() {
        let mut originals = Originals::default();
        let bytes = Arc::new(vec![1, 2, 3]);
        let stamp = Some(Scope::Remote("profile".into()));
        originals.record(1, 0, &bytes, stamp.clone(), true);
        assert_eq!(
            originals
                .session(2, [0].into_iter())
                .0
                .lock()
                .unwrap()
                .originals
                .len(),
            0
        );
        originals.record(1, 0, &bytes, stamp.clone(), false);
        assert_eq!(
            originals.pages.len(),
            1,
            "same response keeps provenance when re-windowing"
        );
        let compressed = Arc::new(vec![4, 5]);
        originals.record(1, 0, &compressed, stamp, false);
        assert!(originals.pages.is_empty());
    }
    fn capture(
        input: PageInput,
        source: Option<Source>,
        width: f64,
        height: f64,
    ) -> ChapterScreencap {
        let page = crate::chapter_capture::Rect {
            x: 0.,
            y: 0.,
            width,
            height,
        };
        ChapterScreencap {
            viewport: crate::chapter_capture::Rect {
                height: height.min(1000.),
                ..page
            },
            reference: 0,
            pages: vec![crate::chapter_capture::PageRect {
                index: 0,
                rect: page,
            }],
            inputs: BTreeMap::from([(0, (input, source))]),
            originals: Session::default(),
            options: reader_engine::DisplayOptions::default(),
            brightness: 100,
            device_scale: 1.,
            output_scale: None,
            known_dimensions: BTreeSet::from([0]),
            layout_anchor: 0,
            layout_gaps: vec![],
            paged_fit: None,
            naming: BTreeMap::from([(
                0,
                (
                    ChapterId::new(reader_domain::SeriesId::new("capture-fixture", "s"), "c"),
                    1,
                ),
            )]),
            background: [0, 0, 0, 255],
        }
    }
    #[test]
    fn complete_preview_and_native_export_fetch_once_and_keep_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let (_, source, transport, input) = source(dir.path(), std::time::Duration::ZERO);
        let mut capture = capture(input, Some(source), 64., 64.);
        let cancel = CancelToken::default();
        for scale in [Some(0.5), Some(1.), None] {
            capture.output_scale = scale;
            let (image, naming) =
                reader_capture_editor::render_capture(&capture, &cancel, None).unwrap();
            assert_eq!(image.width, if scale == Some(0.5) { 32 } else { 64 });
            assert!(
                image
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| *pixel == [10, 20, 30, 255])
            );
            assert_eq!(
                naming.source_y,
                Some(if scale == Some(0.5) { 1 } else { 0 })
            );
        }
        assert_eq!(
            transport.reads.load(Ordering::Acquire),
            1,
            "real compositor preview/adjust/native export must share originals"
        );
    }
    #[test]
    fn tight_native_capture_releases_retention_instead_of_reducing_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tall.png");
        // One decoded RGBA allocation and the crop fit the 128 MiB budget,
        // but cannot also reserve 16 MiB for optional session source retention.
        image::RgbaImage::from_pixel(1000, 30000, image::Rgba([70, 80, 90, 255]))
            .save(&path)
            .unwrap();
        let capture = capture(
            PageInput::Local {
                path,
                id: "tall".into(),
            },
            None,
            1000.,
            30000.,
        );
        let (image, _) =
            reader_capture_editor::render_capture(&capture, &CancelToken::default(), None).unwrap();
        assert_eq!((image.width, image.height), (1000, 1000));
        assert_eq!(capture.originals.reservation(), 0);
        assert!(
            image
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [70, 80, 90, 255])
        );
    }
    #[test]
    fn source_reuse_removes_repeated_request_latency() {
        let dir = tempfile::tempdir().unwrap();
        let (_, source, transport, input) =
            source(dir.path(), std::time::Duration::from_millis(20));
        let cancel = CancelToken::default();
        let mut capture = capture(input, Some(source), 64., 64.);
        capture.originals.release();
        let started = Instant::now();
        for scale in [Some(0.5), Some(1.), None] {
            capture.output_scale = scale;
            reader_capture_editor::render_capture(&capture, &cancel, None).unwrap();
        }
        let before = started.elapsed();
        capture.originals = Session::default();
        let started = Instant::now();
        for scale in [Some(0.5), Some(1.), None] {
            capture.output_scale = scale;
            reader_capture_editor::render_capture(&capture, &cancel, None).unwrap();
        }
        let after = started.elapsed();
        assert_eq!(transport.reads.load(Ordering::Acquire), 4);
        eprintln!(
            "capture compositor open/preview/native export, 20 ms fixture request: uncached {before:?}, session reuse {after:?}; requests 3 -> 1"
        );
    }
}
