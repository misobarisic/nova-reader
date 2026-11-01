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
        if !self.active.load(std::sync::atomic::Ordering::Acquire) {
            return Err(SourceError::Unavailable);
        }
        if let Some(path) = url.strip_prefix(&format!("addon://{}/", self.manifest.id)) {
            cancel.check()?;
            return package::asset(&self.archive, &self.manifest, path);
        }
        network::checked_url(url, &self.manifest.permissions)?;
        network::validate_headers(headers, &self.manifest.permissions)?;
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
        network::check_status(response.status)?;
        Ok(response.body)
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
