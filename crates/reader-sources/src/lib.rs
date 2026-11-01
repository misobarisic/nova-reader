//! Reader-only executable source packages. No video or Stremio dependencies.
mod network;
mod package;
mod runtime;
mod settings;
pub use network::{CancelToken, HttpRequest, HttpResponse, SourceTransport};
pub use package::{Manifest, Permissions, SourceDescriptor, SourceRegistry};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use settings::{SettingField, SettingKind};
use std::{collections::BTreeMap, sync::Arc};

pub type Result<T> = std::result::Result<T, SourceError>;
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
        let Some(partition) = self.transport.page_cache_partition(url) else {
            return Ok(None);
        };
        let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
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
        self.fetch_page_attempt(url, headers, cancel, true)
    }
    fn fetch_page_attempt(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        cancel: &CancelToken,
        refresh: bool,
    ) -> Result<Vec<u8>> {
        self.validate_page_request(url, headers)?;
        if let Some(path) = url.strip_prefix(&format!("addon://{}/", self.manifest.id)) {
            cancel.check()?;
            return package::asset(&self.archive, &self.manifest, path);
        }
        let response = self.transport.request(
            &HttpRequest {
                url: url.into(),
                headers: headers.clone(),
                ..Default::default()
            },
            32 * 1024 * 1024,
            cancel,
        )?;
        cancel.check()?;
        if !self.active.load(std::sync::atomic::Ordering::Acquire) {
            return Err(SourceError::Cancelled);
        }
        if response.body.len() > 32 * 1024 * 1024 {
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
            return self.fetch_page_attempt(&page.url, &page.headers, cancel, false);
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Series {
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
}
