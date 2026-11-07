use crate::{
    COMPRESSED_LIMIT, CatalogueError, EXPANDED_LIMIT, MAX_DEPTH, Result, UpstreamCatalogue,
    convert, decompress_gzip, invalid, native::https_url, proto,
};
use prost::Message;
use reqwest::{
    blocking::Client,
    header::{ACCEPT_ENCODING, ETAG, IF_NONE_MATCH},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

pub struct FetchResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub bytes: Vec<u8>,
}
/// Injectable transport keeps catalogue tests deterministic and network-free.
pub trait Transport {
    fn fetch(&self, url: &str, etag: Option<&str>) -> Result<FetchResponse>;
}
pub struct HttpTransport {
    client: Client,
}
impl HttpTransport {
    pub fn new() -> Result<Self> {
        let builder = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("nova-reader/0.1 catalogue")
            .redirect(nova_net::redirect::https_only(5, "catalogue"));
        #[cfg(target_os = "android")]
        let builder = builder.tls_backend_preconfigured(
            nova_net::tls::client_config().map_err(|e| CatalogueError::Network(e.to_string()))?,
        );
        let client = builder
            .build()
            .map_err(|e| CatalogueError::Network(e.to_string()))?;
        Ok(Self { client })
    }
}
impl Transport for HttpTransport {
    fn fetch(&self, url: &str, etag: Option<&str>) -> Result<FetchResponse> {
        https_url(url)?;
        let mut request = self.client.get(url).header(ACCEPT_ENCODING, "identity");
        if let Some(etag) = etag {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = request
            .send()
            .map_err(|e| CatalogueError::Network(e.to_string()))?;
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if status == 304 {
            return Ok(FetchResponse {
                status,
                etag,
                bytes: Vec::new(),
            });
        }
        if status != 200 {
            return Err(CatalogueError::Network(format!("HTTP {status}")));
        }
        if response
            .content_length()
            .is_some_and(|n| n > COMPRESSED_LIMIT as u64)
        {
            return Err(CatalogueError::Limit);
        }
        let bytes =
            nova_net::read_capped(response, COMPRESSED_LIMIT as u64).map_err(|e| match e {
                nova_net::ReadError::TooLarge => CatalogueError::Limit,
                nova_net::ReadError::Io(e) => CatalogueError::Network(e.to_string()),
            })?;
        Ok(FetchResponse {
            status,
            etag,
            bytes,
        })
    }
}

struct Cached {
    etag: Option<String>,
    bytes: Vec<u8>,
}
/// Cache updates become visible only after the entire graph decodes correctly;
/// an interrupted or failed refresh retains the previous valid cached graph.
pub struct CatalogueClient<T = HttpTransport> {
    transport: T,
    cache: BTreeMap<String, Cached>,
}
impl CatalogueClient<HttpTransport> {
    pub fn new() -> Result<Self> {
        Ok(Self::with_transport(HttpTransport::new()?))
    }
}
impl<T: Transport> CatalogueClient<T> {
    pub fn with_transport(transport: T) -> Self {
        Self {
            transport,
            cache: BTreeMap::new(),
        }
    }
    pub fn fetch_keiyoushi(&mut self, repository: &str) -> Result<UpstreamCatalogue> {
        let repository = https_url(repository)?.to_string();
        let mut visited = BTreeSet::new();
        let mut pending = BTreeMap::new();
        let mut budget = Budget::default();
        let root = self.load(&repository, &mut pending, &mut budget)?;
        proto::guard(&root, true)?;
        let index = proto::Index::decode(root.as_slice()).map_err(invalid)?;
        visited.insert(repository.clone());
        let list = self.resolve(&repository, &index, &mut visited, &mut pending, &mut budget)?;
        let catalogue = convert(&repository, index, list)?;
        // Bound cache memory even if a caller refreshes many different URLs.
        self.cache = pending;
        Ok(catalogue)
    }
    fn load(
        &self,
        url: &str,
        pending: &mut BTreeMap<String, Cached>,
        budget: &mut Budget,
    ) -> Result<Vec<u8>> {
        let cached = self.cache.get(url);
        let response = self
            .transport
            .fetch(url, cached.and_then(|c| c.etag.as_deref()))?;
        let entry = match response.status {
            304 => {
                let old = cached.ok_or_else(|| invalid("304 without a cached catalogue"))?;
                Cached {
                    etag: response.etag.or_else(|| old.etag.clone()),
                    bytes: old.bytes.clone(),
                }
            }
            200 => Cached {
                etag: response.etag,
                bytes: response.bytes,
            },
            status => return Err(CatalogueError::Network(format!("HTTP {status}"))),
        };
        budget.compressed += entry.bytes.len();
        if budget.compressed > COMPRESSED_LIMIT {
            return Err(CatalogueError::Limit);
        }
        let expanded = decompress_gzip(&entry.bytes)?;
        budget.expanded += expanded.len();
        if budget.expanded > EXPANDED_LIMIT {
            return Err(CatalogueError::Limit);
        }
        pending.insert(url.into(), entry);
        Ok(expanded)
    }
    fn resolve(
        &self,
        current: &str,
        index: &proto::Index,
        visited: &mut BTreeSet<String>,
        pending: &mut BTreeMap<String, Cached>,
        budget: &mut Budget,
    ) -> Result<proto::ExtensionList> {
        match index.extensions.as_ref() {
            Some(proto::Extensions::Embedded(list)) => Ok(list.clone()),
            Some(proto::Extensions::Url(link)) => {
                let url = https_url(current)?.join(link).map_err(invalid)?;
                let url = https_url(url.as_str())?.to_string();
                if visited.len() >= MAX_DEPTH || !visited.insert(url.clone()) {
                    return Err(CatalogueError::Recursion);
                }
                let data = self.load(&url, pending, budget)?;
                // Linked endpoints may supply either another Index wrapper or
                // the schema's bare ExtensionList. No executable payload is read.
                proto::guard(&data, true)?;
                if let Ok(linked) = proto::Index::decode(data.as_slice())
                    && linked.extensions.is_some()
                {
                    return self.resolve(&url, &linked, visited, pending, budget);
                }
                proto::guard(&data, false)?;
                proto::ExtensionList::decode(data.as_slice()).map_err(invalid)
            }
            None => Err(invalid("index has no extension list")),
        }
    }
}
#[derive(Default)]
struct Budget {
    compressed: usize,
    expanded: usize,
}
