//! Reader-only executable source packages. No video or Stremio dependencies.
mod network;
mod package;
mod runtime;
mod settings;
pub use network::{
    CancelToken, HttpRequest, HttpResponse, PageChunkControl, PrefixResponse, SourceTransport,
};
pub use package::{CookiePermission, Manifest, Permissions, SourceDescriptor, SourceRegistry};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use settings::{SettingField, SettingKind};
use std::{collections::BTreeMap, sync::Arc};

/// Bytes fetched specifically for a disposable preview. An incomplete prefix
/// cannot establish original-image availability or offline chapter state.
pub struct PagePrefix {
    pub bytes: Vec<u8>,
    pub complete: bool,
}
pub type Result<T> = std::result::Result<T, SourceError>;
type PageChunkObserver<'a> = &'a mut dyn FnMut(&[u8]);
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("invalid reader package: {0}")]
    Package(String),
    #[error("source is disabled or unavailable")]
    Unavailable,
    #[error("interactive challenge required")]
    ChallengeRequired,
    #[error("authentication required")]
    AuthenticationRequired,
    #[error("source rate limited the request")]
    RateLimited,
    #[error("source resource not found")]
    NotFound,
    #[error("source markup or response changed: {0}")]
    ParseChanged(String),
    #[error("operation cancelled")]
    Cancelled,
    #[error("network request failed: {0}")]
    Network(String),
    #[error("source execution failed: {0}")]
    Script(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Clone)]
pub struct SourceAvailability(Arc<std::sync::atomic::AtomicBool>);
impl SourceAvailability {
    /// A read-only, memory-only capability. Registry changes revoke old guards.
    pub fn is_available(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// A cheap retained proof of the current request policy. Registry revocation,
/// live source settings and transport session changes remain visible to clones.
/// It deliberately retains neither the source archive nor executable script.
#[derive(Clone)]
pub struct PageAccess {
    manifest: Arc<Manifest>,
    settings: Arc<std::sync::Mutex<Value>>,
    transport: Arc<dyn SourceTransport>,
    active: Arc<std::sync::atomic::AtomicBool>,
}
impl PageAccess {
    pub fn validate_page_request(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<()> {
        if !self.active.load(std::sync::atomic::Ordering::Acquire) {
            return Err(SourceError::Unavailable);
        }
        if let Some(path) = url.strip_prefix(&format!("addon://{}/", self.manifest.id)) {
            return if self.manifest.assets.iter().any(|asset| asset == path) {
                Ok(())
            } else {
                Err(SourceError::NotFound)
            };
        }
        network::checked_url(url, &self.manifest.permissions)?;
        network::validate_headers(headers, &self.manifest.permissions)
    }
    /// Unknown transports opt out of reuse; all material stays opaque.
    pub fn page_cache_scope(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        use sha2::{Digest, Sha256};
        self.validate_page_request(url, headers)?;
        let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        let Some(partition) = self.transport.page_cache_partition(url) else {
            return Ok(None);
        };
        let material = serde_json::to_vec(&(&*self.manifest, &*settings, url, headers, partition))
            .map_err(|e| SourceError::Script(e.to_string()))?;
        Ok(Some(format!("{:x}", Sha256::digest(material))))
    }
}

#[derive(Clone)]
pub struct Source {
    pub(crate) manifest: Manifest,
    pub(crate) settings: Arc<std::sync::Mutex<Value>>,
    pub(crate) script: Arc<String>,
    pub(crate) archive: Arc<Vec<u8>>,
    pub(crate) transport: Arc<dyn SourceTransport>,
    pub(crate) active: Arc<std::sync::atomic::AtomicBool>,
}
impl Source {
    pub fn availability(&self) -> SourceAvailability {
        SourceAvailability(self.active.clone())
    }
    pub fn descriptor(&self) -> SourceDescriptor {
        SourceDescriptor::new(
            &self.manifest,
            self.active.load(std::sync::atomic::Ordering::Acquire),
        )
    }
    pub fn call(&self, op: &str, request: &Value) -> Result<Value> {
        self.call_cancelled(op, request, &CancelToken::default())
    }
    pub fn call_cancelled(&self, op: &str, request: &Value, cancel: &CancelToken) -> Result<Value> {
        if !self.active.load(std::sync::atomic::Ordering::Acquire) {
            return Err(SourceError::Unavailable);
        }
        runtime::invoke(self, op, request, cancel)
    }
    pub fn fetch_page(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>> {
        self.fetch_page_limited(url, headers, cancel, 32 * 1024 * 1024)
    }
    /// Native callers supply their device image policy (or a stricter cover bound).
    /// Permissions, session checks and refreshed URLs retain the ordinary policy.
    pub fn fetch_page_limited(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        cancel: &CancelToken,
        max_bytes: usize,
    ) -> Result<Vec<u8>> {
        if max_bytes == 0 || max_bytes == usize::MAX {
            return Err(SourceError::Network("invalid image byte limit".into()));
        }
        self.fetch_page_attempt(url, headers, cancel, max_bytes, true, &mut None)
    }
    /// Preview-only fetch with the ordinary permission/session/refresh policy.
    /// A partial result is explicitly distinct from a complete source original.
    pub fn fetch_page_until(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        cancel: &CancelToken,
        max_bytes: usize,
        on_chunk: &mut dyn FnMut(&[u8]) -> Result<PageChunkControl>,
    ) -> Result<PagePrefix> {
        self.fetch_page_until_attempt(url, headers, cancel, max_bytes, true, on_chunk)
    }
    fn fetch_page_until_attempt(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        cancel: &CancelToken,
        max_bytes: usize,
        refresh: bool,
        on_chunk: &mut dyn FnMut(&[u8]) -> Result<PageChunkControl>,
    ) -> Result<PagePrefix> {
        if max_bytes == 0 || max_bytes == usize::MAX {
            return Err(SourceError::Network("invalid image byte limit".into()));
        }
        self.validate_page_request(url, headers)?;
        let transport = {
            let _settings = self.settings.lock().unwrap();
            self.transport
                .snapshot()
                .unwrap_or_else(|| self.transport.clone())
        };
        let revision = transport.session_revision();
        self.check_page_work(cancel, revision)?;
        let mut observed = 0usize;
        let mut observer = |chunk: &[u8]| {
            for chunk in chunk.chunks(network::BODY_CHUNK_BYTES) {
                self.check_page_work(cancel, revision)?;
                observed = observed
                    .checked_add(chunk.len())
                    .filter(|n| *n <= max_bytes)
                    .ok_or_else(|| SourceError::Network("image byte limit".into()))?;
                let control = on_chunk(chunk)?;
                self.check_page_work(cancel, revision)?;
                if control.is_break() {
                    return Ok(control);
                }
            }
            Ok(PageChunkControl::Continue(()))
        };
        let transfer =
            if let Some(path) = url.strip_prefix(&format!("addon://{}/", self.manifest.id)) {
                let body = package::asset(&self.archive, &self.manifest, path)?;
                if body.len() > max_bytes {
                    return Err(SourceError::Network("image byte limit".into()));
                }
                let mut end = body.len();
                let mut seen = 0;
                for chunk in body.chunks(network::BODY_CHUNK_BYTES) {
                    seen += chunk.len();
                    if observer(chunk)?.is_break() {
                        end = seen;
                        break;
                    }
                }
                PrefixResponse {
                    complete: end == body.len(),
                    response: HttpResponse {
                        status: 200,
                        url: url.into(),
                        body: body[..end].to_vec(),
                        ..Default::default()
                    },
                }
            } else {
                transport.request_until(
                    &HttpRequest {
                        url: url.into(),
                        headers: headers.clone(),
                        ..Default::default()
                    },
                    max_bytes,
                    cancel,
                    &mut observer,
                )?
            };
        self.check_page_work(cancel, revision)?;
        if transfer.response.body.len() > max_bytes {
            return Err(SourceError::Network("image byte limit".into()));
        }
        if refresh
            && matches!(transfer.response.status, 401 | 403 | 507)
            && self
                .manifest
                .capabilities
                .iter()
                .any(|op| op == "resolvePage")
        {
            let result =
                self.call_cancelled("resolvePage", &serde_json::json!({"url":url}), cancel)?;
            let pages: PageList = serde_json::from_value(result)
                .map_err(|_| SourceError::ParseChanged("invalid refreshed page".into()))?;
            if pages.pages.len() != 1 {
                return Err(SourceError::ParseChanged(
                    "refresh must return exactly one page".into(),
                ));
            }
            let page = &pages.pages[0];
            return self.fetch_page_until_attempt(
                &page.url,
                &page.headers,
                cancel,
                max_bytes,
                false,
                on_chunk,
            );
        }
        network::check_status(transfer.response.status)?;
        Ok(PagePrefix {
            bytes: transfer.response.body,
            complete: transfer.complete,
        })
    }
    /// Observe native page bytes as they arrive, while retaining the same
    /// authoritative full-byte result and policy as `fetch_page`. Chunks are
    /// provisional, ordered, at most 16 KiB, and never emitted for HTTP errors.
    /// The observer runs synchronously and should treat preview failures as
    /// best-effort; only the final returned bytes may enter durable caches.
    pub fn fetch_page_with_chunks(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        cancel: &CancelToken,
        on_chunk: &mut dyn FnMut(&[u8]),
    ) -> Result<Vec<u8>> {
        self.fetch_page_with_chunks_limited(url, headers, cancel, 32 * 1024 * 1024, on_chunk)
    }
    /// Native streaming equivalent with a caller-supplied device policy.
    pub fn fetch_page_with_chunks_limited(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        cancel: &CancelToken,
        max_bytes: usize,
        on_chunk: &mut dyn FnMut(&[u8]),
    ) -> Result<Vec<u8>> {
        if max_bytes == 0 || max_bytes == usize::MAX {
            return Err(SourceError::Network("invalid image byte limit".into()));
        }
        self.fetch_page_attempt(url, headers, cancel, max_bytes, true, &mut Some(on_chunk))
    }
    fn observe_page_chunk(
        &self,
        chunk: &[u8],
        cancel: &CancelToken,
        session_revision: Option<u64>,
        on_chunk: &mut dyn FnMut(&[u8]),
    ) -> Result<()> {
        for chunk in chunk.chunks(network::BODY_CHUNK_BYTES) {
            self.check_page_work(cancel, session_revision)?;
            on_chunk(chunk);
            self.check_page_work(cancel, session_revision)?;
        }
        Ok(())
    }
    fn check_page_work(&self, cancel: &CancelToken, session_revision: Option<u64>) -> Result<()> {
        cancel.check()?;
        if !self.active.load(std::sync::atomic::Ordering::Acquire)
            || self.transport.session_revision() != session_revision
        {
            return Err(SourceError::Cancelled);
        }
        Ok(())
    }
    fn fetch_page_attempt(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        cancel: &CancelToken,
        max_bytes: usize,
        refresh: bool,
        on_chunk: &mut Option<PageChunkObserver<'_>>,
    ) -> Result<Vec<u8>> {
        self.validate_page_request(url, headers)?;
        let transport = {
            let _settings = self.settings.lock().unwrap();
            self.transport
                .snapshot()
                .unwrap_or_else(|| self.transport.clone())
        };
        let session_revision = transport.session_revision();
        self.check_page_work(cancel, session_revision)?;
        if let Some(path) = url.strip_prefix(&format!("addon://{}/", self.manifest.id)) {
            let body = package::asset(&self.archive, &self.manifest, path)?;
            if body.len() > max_bytes {
                return Err(SourceError::Network("image byte limit".into()));
            }
            if let Some(observer) = on_chunk.as_mut() {
                self.observe_page_chunk(&body, cancel, session_revision, observer)?;
            }
            self.check_page_work(cancel, session_revision)?;
            return Ok(body);
        }
        let request = HttpRequest {
            url: url.into(),
            headers: headers.clone(),
            ..Default::default()
        };
        let response = if let Some(observer) = on_chunk.as_mut() {
            let mut observed = 0usize;
            transport.request_with_chunks(&request, max_bytes, cancel, &mut |chunk| {
                if chunk.len() > max_bytes.saturating_sub(observed) {
                    return Err(SourceError::Network("image byte limit".into()));
                }
                observed += chunk.len();
                self.observe_page_chunk(chunk, cancel, session_revision, observer)
            })?
        } else {
            transport.request(&request, max_bytes, cancel)?
        };
        self.check_page_work(cancel, session_revision)?;
        if response.body.len() > max_bytes {
            return Err(SourceError::Network("image byte limit".into()));
        }
        if refresh
            && matches!(response.status, 401 | 403 | 507)
            && self
                .manifest
                .capabilities
                .iter()
                .any(|op| op == "resolvePage")
        {
            let result =
                self.call_cancelled("resolvePage", &serde_json::json!({"url":url}), cancel)?;
            let pages: PageList = serde_json::from_value(result)
                .map_err(|_| SourceError::ParseChanged("invalid refreshed page".into()))?;
            if pages.pages.len() != 1 {
                return Err(SourceError::ParseChanged(
                    "refresh must return exactly one page".into(),
                ));
            }
            let page = &pages.pages[0];
            return self.fetch_page_attempt(
                &page.url,
                &page.headers,
                cancel,
                max_bytes,
                false,
                on_chunk,
            );
        }
        network::check_status(response.status)?;
        Ok(response.body)
    }
    /// A lightweight memory-only request capability without executable/ZIP data.
    pub fn page_access(&self) -> PageAccess {
        PageAccess {
            manifest: Arc::new(self.manifest.clone()),
            settings: self.settings.clone(),
            transport: self.transport.clone(),
            active: self.active.clone(),
        }
    }
    /// Recheck the current source capability before serving a RAM/disk hit.
    pub fn validate_page_request(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<()> {
        self.page_access().validate_page_request(url, headers)
    }
    /// Opaque authorization/profile/settings scope; no credential leaves here.
    pub fn page_cache_scope(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        self.page_access().page_cache_scope(url, headers)
    }
    /// Explicit fixture/custom transport injection; the production registry always uses checked HTTP.
    pub fn with_transport(mut self, transport: Arc<dyn SourceTransport>) -> Self {
        self.transport = transport;
        self
    }
    pub fn capabilities(&self, request: &Value) -> Result<Value> {
        self.call("capabilities", request)
    }
    pub fn filters(&self, request: &Value) -> Result<Value> {
        self.call("filters", request)
    }
    pub fn browse(&self, request: &Value) -> Result<Listing> {
        self.typed("browse", request)
    }
    pub fn search(&self, request: &Value) -> Result<Listing> {
        self.typed("search", request)
    }
    pub fn details(&self, request: &Value) -> Result<Series> {
        self.typed("details", request)
    }
    pub fn chapters(&self, request: &Value) -> Result<ChapterList> {
        self.typed("chapters", request)
    }
    pub fn pages(&self, request: &Value) -> Result<PageList> {
        self.typed("pages", request)
    }
    fn typed<T: serde::de::DeserializeOwned>(&self, op: &str, request: &Value) -> Result<T> {
        serde_json::from_value(self.call(op, request)?)
            .map_err(|e| SourceError::ParseChanged(e.to_string()))
    }
}
// Hints are advisory. A future addon value must not invalidate a whole listing.
fn deserialize_presentation<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(value
        .as_str()
        .filter(|s| matches!(*s, "gallery" | "reader"))
        .map(str::to_owned))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Series {
    /// Optional entry-level UI suggestion. Unknown values retain Reader fallback.
    #[serde(
        default,
        deserialize_with = "deserialize_presentation",
        skip_serializing_if = "Option::is_none"
    )]
    pub presentation: Option<String>,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub status: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub items: Vec<Series>,
    #[serde(default)]
    pub has_next_page: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chapter {
    pub id: String,
    pub series_id: String,
    pub title: String,
    #[serde(default)]
    pub number: Option<String>,
    pub source_order: usize,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub scanlator: Option<String>,
    #[serde(default)]
    pub uploaded_at: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChapterList {
    pub chapters: Vec<Chapter>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PageList {
    pub pages: Vec<Page>,
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    struct ScopedTransport;
    impl SourceTransport for ScopedTransport {
        fn page_cache_partition(&self, _url: &str) -> Option<String> {
            Some("test-public-profile".into())
        }
        fn request(&self, _: &HttpRequest, _: usize, _: &CancelToken) -> Result<HttpResponse> {
            panic!("permission/cache-scope proof must not fetch")
        }
    }
    #[test]
    fn page_cache_proofs_validate_permissions_settings_headers_and_revocation_without_http() {
        let mut manifest: Manifest = serde_json::from_str(include_str!(
            "../../../examples/addons/neutral-fixture/manifest.json"
        ))
        .unwrap();
        manifest.permissions.domains = vec!["example.com".into()];
        let source = Source {
            manifest,
            settings: Arc::new(std::sync::Mutex::new(serde_json::json!({}))),
            script: Arc::new(String::new()),
            archive: Arc::new(vec![]),
            transport: Arc::new(ScopedTransport),
            active: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };
        let url = "https://example.com/page.png";
        let headers = BTreeMap::new();
        let archive_references = Arc::strong_count(&source.archive);
        let retained = source.page_access();
        assert_eq!(
            Arc::strong_count(&source.archive),
            archive_references,
            "lightweight request proofs cannot retain the ZIP"
        );
        let first = source.page_cache_scope(url, &headers).unwrap().unwrap();
        assert_eq!(
            retained.page_cache_scope(url, &headers).unwrap().as_ref(),
            Some(&first)
        );
        assert_eq!(
            first,
            source.page_cache_scope(url, &headers).unwrap().unwrap()
        );
        source.settings.lock().unwrap()["credential"] = "changed".into();
        assert_ne!(
            retained.page_cache_scope(url, &headers).unwrap().as_ref(),
            Some(&first)
        );
        assert_ne!(
            first,
            source.page_cache_scope(url, &headers).unwrap().unwrap()
        );
        assert!(
            source
                .validate_page_request("https://denied.example/page.png", &headers)
                .is_err()
        );
        assert!(
            source
                .validate_page_request(url, &BTreeMap::from([("Cookie".into(), "secret".into())]))
                .is_err()
        );
        assert_ne!(
            source.page_cache_scope(url, &headers).unwrap(),
            source
                .page_cache_scope(
                    url,
                    &BTreeMap::from([("Authorization".into(), "Bearer different".into())])
                )
                .unwrap()
        );
        assert!(
            source
                .validate_page_request("addon://neutral-fixture/pages/tall.png", &headers)
                .is_ok()
        );
        assert!(
            source
                .validate_page_request("addon://neutral-fixture/../private", &headers)
                .is_err()
        );
        source
            .active
            .store(false, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            source.page_cache_scope(url, &headers),
            Err(SourceError::Unavailable)
        ));
        assert!(matches!(
            retained.page_cache_scope(url, &headers),
            Err(SourceError::Unavailable)
        ));
    }
    #[test]
    fn delayed_invocation_cannot_restore_cookies_after_settings_toggle() {
        use std::{
            io::{Cursor, Write},
            sync::{Mutex, mpsc},
        };
        struct Gate {
            entered: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
        }
        struct DelayedTransport {
            native: Arc<dyn SourceTransport>,
            gate: Arc<Gate>,
        }
        impl SourceTransport for DelayedTransport {
            fn snapshot(&self) -> Option<Arc<dyn SourceTransport>> {
                Some(Arc::new(Self {
                    native: self.native.snapshot().unwrap(),
                    gate: self.gate.clone(),
                }))
            }
            fn set_cookie(
                &self,
                origin: &str,
                name: &str,
                value: &str,
                token: &CancelToken,
            ) -> Result<()> {
                self.native.set_cookie(origin, name, value, token)
            }
            fn request(
                &self,
                input: &HttpRequest,
                _: usize,
                _: &CancelToken,
            ) -> Result<HttpResponse> {
                self.gate.entered.send(()).unwrap();
                self.gate
                    .release
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                Ok(HttpResponse {
                    status: 200,
                    url: input.url.clone(),
                    ..Default::default()
                })
            }
        }
        let manifest = serde_json::json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"cookies","name":"Cookies","version":"1.0.0","language":"en","entry":"main.js","permissions":{"domains":["example.com"],"cookies":[{"origin":"https://example.com","names":["mature"]}]},"capabilities":["filters"],"configuration":[{"key":"mature","name":"Mature","kind":"bool","default":false}]});
        let script = b"globalThis.novaProvider={handle(r){if(r.wait)nova.http.get('https://example.com');return JSON.parse(nova.cookies.set('https://example.com','mature',r.settings.mature?'1':'0'));}};";
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            ("main.js", script.to_vec()),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&data).unwrap();
        }
        let directory = tempfile::tempdir().unwrap();
        let registry = SourceRegistry::open(directory.path()).unwrap();
        registry
            .install_zip(&zip.finish().unwrap().into_inner())
            .unwrap();
        registry.set_enabled("cookies", true).unwrap();
        registry
            .set_settings("cookies", serde_json::json!({"mature":true}))
            .unwrap();
        let source = registry.source("cookies").unwrap();
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let delayed = source.clone().with_transport(Arc::new(DelayedTransport {
            native: source.transport.clone(),
            gate: Arc::new(Gate {
                entered,
                release: Mutex::new(release_rx),
            }),
        }));
        let worker =
            std::thread::spawn(move || delayed.call("filters", &serde_json::json!({"wait":true})));
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        registry
            .set_settings("cookies", serde_json::json!({"mature":false}))
            .unwrap();
        source.call("filters", &serde_json::json!({})).unwrap();
        let url = "https://example.com/page.png";
        let headers = BTreeMap::new();
        let current = source.page_cache_scope(url, &headers).unwrap();
        release.send(()).unwrap();
        assert_eq!(
            worker.join().unwrap().unwrap(),
            serde_json::json!({"ok":true})
        );
        assert_eq!(current, source.page_cache_scope(url, &headers).unwrap());
        assert!(
            !registry.settings("cookies").unwrap()["mature"]
                .as_bool()
                .unwrap()
        );
    }
    #[test]
    fn streaming_checks_live_owner_revision_after_a_barrier_without_rejecting_cookie_updates() {
        use std::sync::Barrier;
        struct PausedTransport {
            native: Arc<dyn SourceTransport>,
            entered: Arc<Barrier>,
            release: Arc<Barrier>,
        }
        impl SourceTransport for PausedTransport {
            fn session_revision(&self) -> Option<u64> {
                self.native.session_revision()
            }
            fn snapshot(&self) -> Option<Arc<dyn SourceTransport>> {
                Some(Arc::new(Self {
                    native: self.native.snapshot().unwrap(),
                    entered: self.entered.clone(),
                    release: self.release.clone(),
                }))
            }
            fn request(&self, _: &HttpRequest, _: usize, _: &CancelToken) -> Result<HttpResponse> {
                panic!("streaming must use the incremental transport method")
            }
            fn request_with_chunks(
                &self,
                request: &HttpRequest,
                _: usize,
                _: &CancelToken,
                observer: &mut dyn FnMut(&[u8]) -> Result<()>,
            ) -> Result<HttpResponse> {
                observer(b"first")?;
                self.entered.wait();
                self.release.wait();
                observer(b"second")?;
                Ok(HttpResponse {
                    status: 200,
                    url: request.url.clone(),
                    body: b"firstsecond".to_vec(),
                    ..Default::default()
                })
            }
        }
        for reset in [false, true] {
            let mut manifest: Manifest = serde_json::from_str(include_str!(
                "../../../examples/addons/neutral-fixture/manifest.json"
            ))
            .unwrap();
            manifest.permissions.domains = vec!["example.com".into()];
            manifest.permissions.cookies = vec![CookiePermission {
                origin: "https://example.com".into(),
                names: vec!["mature".into()],
            }];
            let native = Arc::new(network::NetworkTransport::new(manifest.permissions.clone()));
            let entered = Arc::new(Barrier::new(2));
            let release = Arc::new(Barrier::new(2));
            let source = Source {
                manifest,
                settings: Arc::new(std::sync::Mutex::new(serde_json::json!({}))),
                script: Arc::new(String::new()),
                archive: Arc::new(vec![]),
                transport: Arc::new(PausedTransport {
                    native: native.clone(),
                    entered: entered.clone(),
                    release: release.clone(),
                }),
                active: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            };
            let worker = std::thread::spawn(move || {
                let mut chunks = Vec::new();
                let result = source.fetch_page_with_chunks(
                    "https://example.com/page",
                    &BTreeMap::new(),
                    &CancelToken::default(),
                    &mut |chunk| chunks.extend_from_slice(chunk),
                );
                (result, chunks)
            });
            entered.wait();
            if reset {
                native.reset_session();
            } else {
                native
                    .set_cookie(
                        "https://example.com",
                        "mature",
                        "1",
                        &CancelToken::default(),
                    )
                    .unwrap();
            }
            release.wait();
            let (result, chunks) = worker.join().unwrap();
            if reset {
                assert!(matches!(result, Err(SourceError::Cancelled)));
                assert_eq!(
                    chunks, b"first",
                    "a stale snapshot must not publish the next chunk"
                );
            } else {
                assert_eq!(result.unwrap(), b"firstsecond");
                assert_eq!(
                    chunks, b"firstsecond",
                    "cookie value changes keep the same session generation"
                );
            }
        }
    }
}
