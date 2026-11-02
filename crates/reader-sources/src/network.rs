use crate::{Permissions, Result, SourceError};
use reqwest::cookie::CookieStore;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use url::Url;
pub(crate) const BODY_CHUNK_BYTES: usize = 16 * 1024;
type BodyChunkObserver<'a> = &'a mut dyn FnMut(&[u8]) -> Result<()>;
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);
impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release)
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(SourceError::Cancelled)
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpRequest {
    pub url: String,
    #[serde(default = "get_method")]
    pub method: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<String>,
}
fn get_method() -> String {
    "GET".into()
}
#[derive(Clone, Debug, Default)]
pub struct HttpResponse {
    pub status: u16,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
pub trait SourceTransport: Send + Sync {
    /// Revision of settings-owned in-memory session state. Production snapshots
    /// retain their original revision; legacy custom transports may omit it.
    fn session_revision(&self) -> Option<u64> {
        None
    }
    /// Optional operation-local view of a mutable session. Production freezes
    /// the current cookie generation alongside the invocation's settings.
    fn snapshot(&self) -> Option<Arc<dyn SourceTransport>> {
        None
    }
    /// Cookie mutation is opt-in for fixture/custom transports too.
    fn set_cookie(
        &self,
        _origin: &str,
        _name: &str,
        _value: &str,
        cancel: &CancelToken,
    ) -> Result<()> {
        cancel.check()?;
        Err(network("cookie setting not supported by transport"))
    }
    /// Return an opaque profile/session partition for durable page reuse.
    /// Stateful custom transports must opt in; None disables RAM/disk hits.
    fn page_cache_partition(&self, _url: &str) -> Option<String> {
        None
    }
    fn request(
        &self,
        request: &HttpRequest,
        max_bytes: usize,
        cancel: &CancelToken,
    ) -> Result<HttpResponse>;
    /// Native-only observation of successful response body bytes. Observations
    /// are provisional until the returned full response has been validated.
    /// Existing custom transports remain compatible and feed only after their
    /// complete request returns; production feeds while reading the body.
    fn request_with_chunks(
        &self,
        request: &HttpRequest,
        max_bytes: usize,
        cancel: &CancelToken,
        on_chunk: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<HttpResponse> {
        cancel.check()?;
        let response = self.request(request, max_bytes, cancel)?;
        cancel.check()?;
        if response.body.len() > max_bytes {
            return Err(network("response byte limit"));
        }
        if (200..=299).contains(&response.status) {
            for chunk in response.body.chunks(BODY_CHUNK_BYTES) {
                cancel.check()?;
                on_chunk(chunk)?;
                cancel.check()?;
            }
        }
        Ok(response)
    }
}
pub(crate) struct NetworkTransport {
    permissions: Permissions,
    session: Mutex<Arc<CookieSession>>,
    cache_partition: String,
}
#[derive(Default)]
struct CookieSession {
    jar: Arc<reqwest::cookie::Jar>,
    generation: u64,
    clients: Mutex<Vec<PooledClient>>,
}
const MAX_SESSION_CLIENTS: usize = 8;
struct PooledClient {
    host: String,
    addresses: Vec<SocketAddr>,
    client: Arc<reqwest::blocking::Client>,
}
impl CookieSession {
    /// Validate DNS on every hop before looking up a pool. A changed address
    /// set gets a fresh client, so a retained socket cannot bypass rebinding
    /// checks. Pools belong to this exact source/profile session, never globals.
    fn client(
        &self,
        host: &str,
        addresses: &[SocketAddr],
    ) -> Result<Arc<reqwest::blocking::Client>> {
        if addresses.is_empty()
            || addresses
                .iter()
                .any(|a| a.port() != 443 || !is_public_ip(a.ip()))
        {
            return Err(network("DNS resolved to non-public address"));
        }
        let host = host.to_ascii_lowercase();
        let route_addresses = addresses;
        let mut addresses = addresses.to_vec();
        addresses.sort_unstable();
        addresses.dedup();
        let mut clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = clients
            .iter()
            .position(|c| c.host == host && c.addresses == addresses)
        {
            let entry = clients.remove(index);
            let client = entry.client.clone();
            clients.push(entry);
            return Ok(client);
        }
        // Cookie headers are frozen and applied per request below. Clients are
        // deliberately stateless: no default credentials or cookie provider.
        let builder = reqwest::blocking::Client::builder()
            .user_agent("Mozilla/5.0 (compatible; NovaReader/0.1)")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(4)
            .resolve_to_addrs(&host, route_addresses);
        #[cfg(target_os = "android")]
        let builder = builder.tls_backend_preconfigured(android_tls_config()?);
        let client = Arc::new(builder.build().map_err(|_| network("HTTP client failed"))?);
        if clients.len() == MAX_SESSION_CLIENTS {
            clients.remove(0);
        }
        clients.push(PooledClient {
            host,
            addresses,
            client: client.clone(),
        });
        Ok(client)
    }
}
/// Freeze the outgoing header per hop so concurrent jar changes and response
/// deletions cannot hide a cookie-authenticated hop from the redirect guard.
struct HopCookies {
    jar: Arc<dyn CookieStore>,
    url: Url,
    outgoing: Option<reqwest::header::HeaderValue>,
}
impl HopCookies {
    fn new(jar: Arc<dyn CookieStore>, url: &Url) -> Self {
        let mut url = url.clone();
        // Reqwest's CookieService reconstructs the URL from the HTTP URI,
        // which omits fragments.
        url.set_fragment(None);
        Self {
            outgoing: jar.cookies(&url),
            jar,
            url,
        }
    }
    fn has_outgoing_cookies(&self) -> bool {
        self.outgoing.is_some()
    }
}
impl CookieStore for HopCookies {
    fn cookies(&self, url: &Url) -> Option<reqwest::header::HeaderValue> {
        (url == &self.url).then(|| self.outgoing.clone()).flatten()
    }
    fn set_cookies(
        &self,
        headers: &mut dyn Iterator<Item = &reqwest::header::HeaderValue>,
        url: &Url,
    ) {
        self.jar.set_cookies(headers, url);
    }
}
impl NetworkTransport {
    #[cfg(test)]
    pub fn new(permissions: Permissions) -> Self {
        Self::new_scoped(permissions, "test-default".into())
    }
    pub fn new_scoped(permissions: Permissions, cache_partition: String) -> Self {
        Self {
            permissions,
            session: Mutex::new(Arc::new(CookieSession::default())),
            cache_partition,
        }
    }
    pub(crate) fn reset_session(&self) {
        let mut current = self.session.lock().unwrap();
        *current = Arc::new(CookieSession {
            generation: current.generation + 1,
            ..Default::default()
        });
    }
}
fn network(s: impl Into<String>) -> SourceError {
    SourceError::Network(s.into())
}
pub(crate) fn checked_url(raw: &str, permissions: &Permissions) -> Result<Url> {
    if raw.len() > 8192 {
        return Err(network("URL exceeds limit"));
    }
    let url = Url::parse(raw).map_err(|_| network("invalid URL"))?;
    let host = url.host_str().ok_or_else(|| network("missing host"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || !permissions.domains.iter().any(|d| {
            host.eq_ignore_ascii_case(d)
                || host
                    .to_ascii_lowercase()
                    .ends_with(&format!(".{}", d.to_ascii_lowercase()))
        })
    {
        return Err(network("URL denied by source permissions"));
    }
    if let Ok(ip) = host.parse::<IpAddr>()
        && !is_public_ip(ip)
    {
        return Err(network("non-public address"));
    }
    Ok(url)
}
pub(crate) fn checked_cookie_origin(raw: &str, permissions: &Permissions) -> Result<Url> {
    let url = checked_url(raw, permissions)?;
    // Require a canonical origin: no path, credentials, query, fragment or
    // nondefault port, and no URL normalization ambiguity in grants.
    if raw != url.origin().ascii_serialization() {
        return Err(network("cookie origin must be a canonical HTTPS origin"));
    }
    Ok(url)
}
pub(crate) fn valid_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}
pub(crate) fn validate_cookie(
    origin: &str,
    name: &str,
    value: &str,
    permissions: &Permissions,
) -> Result<Url> {
    let url = checked_cookie_origin(origin, permissions)?;
    // RFC 6265 cookie-octet excludes whitespace, quotes, comma, semicolon,
    // backslash, control characters and non-ASCII bytes. Rust owns attributes.
    if !valid_cookie_name(name)
        || value.len() > 4096
        || !value
            .bytes()
            .all(|b| matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
    {
        return Err(network("invalid cookie name or value"));
    }
    if !permissions
        .cookies
        .iter()
        .any(|grant| grant.origin == origin && grant.names.iter().any(|allowed| allowed == name))
    {
        return Err(network("cookie denied by source permissions"));
    }
    Ok(url)
}
pub(crate) fn validate_headers(
    headers: &BTreeMap<String, String>,
    permissions: &Permissions,
) -> Result<()> {
    if headers.len() > 16
        || headers
            .iter()
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>()
            > 8192
    {
        return Err(network("header limit"));
    }
    for (k, v) in headers {
        let name = k.to_ascii_lowercase();
        // Host, Cookie and hop-by-hop headers belong to Rust/the scoped cookie jar.
        if ![
            "accept",
            "accept-language",
            "content-type",
            "user-agent",
            "referer",
            "origin",
            "authorization",
            "x-requested-with",
            "x-integrity-token",
        ]
        .contains(&name.as_str())
            || reqwest::header::HeaderName::from_bytes(k.as_bytes()).is_err()
            || reqwest::header::HeaderValue::from_str(v).is_err()
        {
            return Err(network("unsafe request header"));
        }
        if name == "referer" || name == "origin" {
            let u = checked_url(v, permissions)?;
            if name == "origin"
                && (u.path() != "/" || u.query().is_some() || u.fragment().is_some())
            {
                return Err(network("invalid Origin"));
            }
        }
    }
    Ok(())
}
pub(crate) fn check_status(status: u16) -> Result<()> {
    match status {
        401 => Err(SourceError::AuthenticationRequired),
        403 => Err(SourceError::ChallengeRequired),
        404 => Err(SourceError::NotFound),
        429 => Err(SourceError::RateLimited),
        200..=299 => Ok(()),
        _ => Err(network(format!("HTTP {status}"))),
    }
}
fn check_redirect(current: &Url, next: &Url, sensitive: bool, has_cookies: bool) -> Result<()> {
    if (sensitive || has_cookies) && current.origin() != next.origin() {
        return Err(network("sensitive cross-origin redirect denied"));
    }
    Ok(())
}
fn read_body(
    reader: &mut dyn Read,
    max_bytes: usize,
    cancel: &CancelToken,
    mut on_chunk: Option<BodyChunkObserver<'_>>,
) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut chunk = [0; BODY_CHUNK_BYTES];
    loop {
        cancel.check()?;
        let n = reader
            .read(&mut chunk)
            .map_err(|_| network("response read failed"))?;
        cancel.check()?;
        if n == 0 {
            return Ok(body);
        }
        if n > max_bytes.saturating_sub(body.len()) {
            return Err(network("response byte limit"));
        }
        body.extend_from_slice(&chunk[..n]);
        if let Some(observer) = on_chunk.as_mut() {
            observer(&chunk[..n])?;
            cancel.check()?;
        }
    }
}
impl SourceTransport for NetworkTransport {
    fn session_revision(&self) -> Option<u64> {
        Some(self.session.lock().unwrap().generation)
    }
    fn snapshot(&self) -> Option<Arc<dyn SourceTransport>> {
        Some(Arc::new(Self {
            permissions: self.permissions.clone(),
            session: Mutex::new(self.session.lock().unwrap().clone()),
            cache_partition: self.cache_partition.clone(),
        }))
    }
    fn set_cookie(
        &self,
        origin: &str,
        name: &str,
        value: &str,
        cancel: &CancelToken,
    ) -> Result<()> {
        cancel.check()?;
        let url = validate_cookie(origin, name, value, &self.permissions)?;
        let session = self.session.lock().unwrap();
        cancel.check()?;
        session
            .jar
            .add_cookie_str(&format!("{name}={value}; Secure; HttpOnly; Path=/"), &url);
        Ok(())
    }
    fn page_cache_partition(&self, url: &str) -> Option<String> {
        use sha2::{Digest, Sha256};
        let url = Url::parse(url).ok()?;
        let session = self.session.lock().unwrap();
        let cookies = session.jar.cookies(&url);
        let material = serde_json::to_vec(&(
            &self.cache_partition,
            session.generation,
            cookies.as_ref().map(|value| value.as_bytes()),
        ))
        .ok()?;
        Some(format!("{:x}", Sha256::digest(material)))
    }
    fn request(
        &self,
        input: &HttpRequest,
        max_bytes: usize,
        cancel: &CancelToken,
    ) -> Result<HttpResponse> {
        self.request_internal(input, max_bytes, cancel, None)
    }
    fn request_with_chunks(
        &self,
        input: &HttpRequest,
        max_bytes: usize,
        cancel: &CancelToken,
        on_chunk: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<HttpResponse> {
        self.request_internal(input, max_bytes, cancel, Some(on_chunk))
    }
}
impl NetworkTransport {
    fn request_internal(
        &self,
        input: &HttpRequest,
        max_bytes: usize,
        cancel: &CancelToken,
        on_chunk: Option<BodyChunkObserver<'_>>,
    ) -> Result<HttpResponse> {
        let session = self.session.lock().unwrap().clone();
        cancel.check()?;
        validate_headers(&input.headers, &self.permissions)?;
        let method = if input.method.is_empty() {
            "GET"
        } else {
            input.method.as_str()
        };
        if method != "GET" && !(method == "POST" && self.permissions.http_post) {
            return Err(network("HTTP method not permitted"));
        }
        if input
            .body
            .as_ref()
            .is_some_and(|s| s.len() > 2 * 1024 * 1024)
            || method == "GET" && input.body.is_some()
        {
            return Err(network("invalid HTTP body"));
        }
        let mut url = checked_url(&input.url, &self.permissions)?;
        let sensitive = method == "POST"
            || input.headers.keys().any(|s| {
                s.eq_ignore_ascii_case("authorization")
                    || s.eq_ignore_ascii_case("x-integrity-token")
            });
        let started = Instant::now();
        let deadline = Duration::from_secs(15);
        for hop in 0..=5 {
            cancel.check()?;
            checked_url(url.as_str(), &self.permissions)?;
            let host = url.host_str().unwrap();
            let hostname = host.to_owned();
            let (send, receive) = std::sync::mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("reader-dns".into())
                .spawn(move || {
                    let result = (hostname.as_str(), 443)
                        .to_socket_addrs()
                        .map(|addresses| addresses.collect::<Vec<_>>());
                    let _ = send.send(result);
                })
                .map_err(|_| network("DNS worker unavailable"))?;
            let dns_wait = deadline
                .checked_sub(started.elapsed())
                .ok_or_else(|| network("DNS deadline"))?
                .min(Duration::from_secs(5));
            let addresses = receive
                .recv_timeout(dns_wait)
                .map_err(|_| network("DNS deadline"))?
                .map_err(|_| network("DNS lookup failed"))?;
            if addresses.is_empty() || addresses.iter().any(|a| !is_public_ip(a.ip())) {
                return Err(network("DNS resolved to non-public address"));
            }
            let hop_cookies = HopCookies::new(session.jar.clone(), &url);
            // Keep inherited proxy configuration; direct sockets use the exact
            // freshly validated DNS set, with bounded per-session reuse.
            let client = session.client(host, &addresses)?;
            let remaining = deadline
                .checked_sub(started.elapsed())
                .ok_or_else(|| network("request deadline exceeded"))?;
            let mut request = client
                .request(
                    if method == "POST" {
                        reqwest::Method::POST
                    } else {
                        reqwest::Method::GET
                    },
                    url.clone(),
                )
                .timeout(remaining);
            if let Some(cookie) = hop_cookies.cookies(&hop_cookies.url) {
                request = request.header(reqwest::header::COOKIE, cookie);
            }
            for (k, v) in &input.headers {
                request = request.header(k, v);
            }
            if let Some(body) = &input.body {
                request = request.body(body.clone())
            }
            let mut response = request
                .send()
                .map_err(|_| network("HTTP transfer failed"))?;
            // Set-Cookie stays in Rust, including on redirects; it never enters
            // the response header allowlist exposed to JavaScript.
            let set_cookie = response.headers().get_all(reqwest::header::SET_COOKIE);
            hop_cookies.set_cookies(&mut set_cookie.iter(), &hop_cookies.url);
            cancel.check()?;
            if response.status().is_redirection() {
                if hop == 5 {
                    return Err(network("redirect limit"));
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| network("invalid redirect"))?;
                let next = url
                    .join(location)
                    .map_err(|_| network("invalid redirect"))?;
                checked_url(next.as_str(), &self.permissions)?;
                check_redirect(
                    &url,
                    &next,
                    sensitive,
                    hop_cookies.has_outgoing_cookies() || session.jar.cookies(&url).is_some(),
                )?;
                url = next;
                continue;
            }
            if response
                .content_length()
                .is_some_and(|len| len > max_bytes as u64)
            {
                return Err(network("response byte limit"));
            }
            let status = response.status().as_u16();
            let mut headers = BTreeMap::new();
            // Cookie and authentication values never cross the JavaScript boundary.
            for name in ["content-type", "etag", "last-modified", "retry-after"] {
                if let Some(v) = response.headers().get(name).and_then(|v| v.to_str().ok()) {
                    headers.insert(name.into(), v.into());
                }
            }
            let body = read_body(
                &mut response,
                max_bytes,
                cancel,
                if (200..=299).contains(&status) {
                    on_chunk
                } else {
                    None
                },
            )?;
            return Ok(HttpResponse {
                status,
                url: url.into(),
                headers,
                body,
            });
        }
        Err(network("redirect loop"))
    }
}

pub(crate) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_broadcast()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && a != 0
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 192 && b == 0 && c == 0)
                && !(a == 192 && b == 0 && c == 2)
                && !(a == 192 && b == 88 && c == 99)
                && !(a == 198 && (b == 18 || b == 19))
                && !(a == 198 && b == 51 && c == 100)
                && !(a == 203 && b == 0 && c == 113)
                && a < 224
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(mapped));
            }
            let segments = ip.segments();
            (0x2000..=0x3fff).contains(&segments[0])
                && !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_unique_local()
                && !ip.is_unicast_link_local()
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
                && !(segments[0] == 0x2001 && segments[1] <= 0x01ff)
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && segments[1] <= 0x0fff)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn permissions() -> Permissions {
        Permissions {
            domains: vec!["example.com".into()],
            http_post: false,
            cookies: vec![],
        }
    }
    #[test]
    fn url_and_header_policy_rejects_transport_confusion() {
        for url in [
            "http://example.com/",
            "https://example.com:444/",
            "https://user:password@example.com/",
            "https://example.com.evil.test/",
            "https://evilexample.com/",
            "https://127.0.0.1/",
            "file:///tmp/a",
        ] {
            assert!(checked_url(url, &permissions()).is_err(), "{url}")
        }
        assert!(checked_url("https://cdn.example.com/image", &permissions()).is_ok());
        for (name, value) in [
            ("Cookie", "a=b"),
            ("Host", "other.example"),
            ("Connection", "keep-alive"),
            ("Referer", "https://evil.test/"),
            ("Origin", "https://example.com/path"),
            ("Accept", "x\r\nCookie: secret"),
        ] {
            assert!(
                validate_headers(
                    &BTreeMap::from([(name.into(), value.into())]),
                    &permissions()
                )
                .is_err(),
                "{name}"
            )
        }
        assert!(
            validate_headers(
                &BTreeMap::from([
                    ("Referer".into(), "https://example.com/chapter".into()),
                    ("Origin".into(), "https://example.com".into())
                ]),
                &permissions()
            )
            .is_ok()
        );
    }
    #[test]
    fn all_reserved_address_classes_are_denied() {
        for ip in [
            "0.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.1.1",
            "192.168.1.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::ffff:127.0.0.1",
            "fe80::1",
            "fc00::1",
            "2001:db8::1",
            "2002::1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip}")
        }
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }
    #[test]
    fn cookie_jars_are_explicitly_session_scoped() {
        let first = NetworkTransport::new(permissions());
        let second = NetworkTransport::new(permissions());
        let url = Url::parse("https://example.com/").unwrap();
        first
            .session
            .lock()
            .unwrap()
            .jar
            .add_cookie_str("session=private; Path=/; Secure", &url);
        assert!(first.session.lock().unwrap().jar.cookies(&url).is_some());
        assert!(second.session.lock().unwrap().jar.cookies(&url).is_none());
        let before = second.page_cache_partition(url.as_str()).unwrap();
        second
            .session
            .lock()
            .unwrap()
            .jar
            .add_cookie_str("session=private; Path=/; Secure", &url);
        assert_ne!(before, second.page_cache_partition(url.as_str()).unwrap());
        let other_profile = NetworkTransport::new_scoped(permissions(), "other-profile".into());
        assert_ne!(
            before,
            other_profile.page_cache_partition(url.as_str()).unwrap()
        );
    }
    #[test]
    fn connection_pools_reuse_only_validated_dns_and_the_same_session() {
        let transport = NetworkTransport::new(permissions());
        let session = transport.session.lock().unwrap().clone();
        let first: SocketAddr = "1.1.1.1:443".parse().unwrap();
        let second: SocketAddr = "8.8.8.8:443".parse().unwrap();
        let client = session.client("example.com", &[first, second]).unwrap();
        assert!(Arc::ptr_eq(
            &client,
            &session
                .client("example.com", &[second, first, first])
                .unwrap()
        ));
        assert!(!Arc::ptr_eq(
            &client,
            &session.client("example.com", &[first]).unwrap()
        ));
        assert!(!Arc::ptr_eq(
            &client,
            &session.client("cdn.example.com", &[first, second]).unwrap()
        ));
        for addresses in [
            vec![],
            vec!["127.0.0.1:443".parse().unwrap()],
            vec![first, "10.0.0.1:443".parse().unwrap()],
            vec!["1.1.1.1:80".parse().unwrap()],
        ] {
            assert!(session.client("example.com", &addresses).is_err());
        }
        let other = NetworkTransport::new_scoped(permissions(), "other-profile".into());
        let other = other.session.lock().unwrap().clone();
        assert!(!Arc::ptr_eq(
            &client,
            &other.client("example.com", &[first, second]).unwrap()
        ));
        // A retained transport snapshot shares exactly the old session; reset
        // gives subsequent operations fresh pools as well as a new cookie jar.
        transport.reset_session();
        let next = transport.session.lock().unwrap().clone();
        assert!(!Arc::ptr_eq(
            &client,
            &next.client("example.com", &[first, second]).unwrap()
        ));
        assert!(Arc::ptr_eq(
            &client,
            &session.client("example.com", &[first, second]).unwrap()
        ));
        session.jar.add_cookie_str(
            "session=secret; Path=/; Secure",
            &Url::parse("https://example.com/").unwrap(),
        );
        let built = client.get("https://example.com/").build().unwrap();
        assert!(
            !built.headers().contains_key(reqwest::header::COOKIE),
            "pooled clients never retain request credentials"
        );
        for index in 0..MAX_SESSION_CLIENTS {
            session
                .client(&format!("cdn{index}.example.com"), &[first])
                .unwrap();
        }
        assert_eq!(session.clients.lock().unwrap().len(), MAX_SESSION_CLIENTS);
        assert!(
            !Arc::ptr_eq(
                &client,
                &session.client("example.com", &[first, second]).unwrap()
            ),
            "least recently used client is evicted"
        );
    }
    fn cookie_permissions() -> Permissions {
        Permissions {
            cookies: vec![crate::CookiePermission {
                origin: "https://example.com".into(),
                names: vec!["mature".into()],
            }],
            ..permissions()
        }
    }
    #[test]
    fn cookie_setting_is_host_only_secure_bounded_and_exactly_permissioned() {
        let transport = NetworkTransport::new(cookie_permissions());
        let token = CancelToken::default();
        transport
            .set_cookie("https://example.com", "mature", "1", &token)
            .unwrap();
        let session = transport.session.lock().unwrap();
        let cookie = |url: &str| session.jar.cookies(&Url::parse(url).unwrap());
        assert_eq!(cookie("https://example.com/any/path").unwrap(), "mature=1");
        assert!(cookie("http://example.com/").is_none());
        assert!(cookie("https://cdn.example.com/").is_none());
        assert!(cookie("https://other.example/").is_none());
        drop(session);
        for (origin, name, value) in [
            ("https://cdn.example.com", "mature", "1"),
            ("https://example.com", "other", "1"),
            ("https://example.com", "Mature", "1"),
            ("https://example.com/", "mature", "1"),
            ("https://example.com:443", "mature", "1"),
            ("https://example.com/path", "mature", "1"),
            ("https://example.com?x", "mature", "1"),
            ("https://example.com#x", "mature", "1"),
            ("http://example.com", "mature", "1"),
            ("https://example.com", "mature", "x; Domain=example.com"),
            ("https://example.com", "mature", "x\r\nCookie: other=1"),
            ("https://example.com", "mature", "has space"),
            ("https://example.com", "mature", "\"quoted\""),
            ("https://example.com", "mature", "a,b"),
            ("https://example.com", "mature", "a\\b"),
            ("https://example.com", "mature", "é"),
        ] {
            assert!(
                transport.set_cookie(origin, name, value, &token).is_err(),
                "{origin} {name}"
            );
        }
        assert!(
            transport
                .set_cookie("https://example.com", "mature", &"x".repeat(4097), &token)
                .is_err()
        );
        transport
            .set_cookie("https://example.com", "mature", "0", &token)
            .unwrap();
        token.cancel();
        assert!(matches!(
            transport.set_cookie("https://example.com", "mature", "1", &token),
            Err(SourceError::Cancelled)
        ));
        assert_eq!(
            transport
                .session
                .lock()
                .unwrap()
                .jar
                .cookies(&Url::parse("https://example.com/").unwrap())
                .unwrap(),
            "mature=0"
        );
        assert!(
            NetworkTransport::new(permissions())
                .set_cookie(
                    "https://example.com",
                    "mature",
                    "1",
                    &CancelToken::default()
                )
                .is_err()
        );
    }
    #[test]
    fn stale_cookie_generation_cannot_change_current_partition_or_cookie() {
        let transport = NetworkTransport::new(cookie_permissions());
        let url = "https://example.com/page.png";
        let token = CancelToken::default();
        let initial = transport.page_cache_partition(url);
        let old = transport.snapshot().unwrap();
        assert_eq!(old.session_revision(), transport.session_revision());
        transport.reset_session();
        assert_ne!(old.session_revision(), transport.session_revision());
        assert_ne!(initial, transport.page_cache_partition(url));
        transport
            .set_cookie("https://example.com", "mature", "0", &token)
            .unwrap();
        let current = transport.page_cache_partition(url);
        old.set_cookie("https://example.com", "mature", "1", &token)
            .unwrap();
        assert_eq!(current, transport.page_cache_partition(url));
        assert_ne!(current, old.page_cache_partition(url));
        assert_eq!(
            transport
                .session
                .lock()
                .unwrap()
                .jar
                .cookies(&Url::parse(url).unwrap())
                .unwrap(),
            "mature=0"
        );
    }
    #[test]
    fn cookie_authenticated_redirects_stay_on_exact_origin() {
        let start = Url::parse("https://example.com/list").unwrap();
        let same = start.join("/next").unwrap();
        let subdomain = Url::parse("https://cdn.example.com/next").unwrap();
        let other = Url::parse("https://other.example/next").unwrap();
        assert!(check_redirect(&start, &same, false, true).is_ok());
        assert!(check_redirect(&start, &subdomain, false, true).is_err());
        assert!(check_redirect(&start, &other, false, true).is_err());
        assert!(check_redirect(&start, &subdomain, true, false).is_err());
        assert!(check_redirect(&start, &subdomain, false, false).is_ok());
    }
    #[test]
    fn hop_cookie_snapshot_survives_concurrent_insert_and_redirect_deletion() {
        use std::sync::Barrier;
        // Simulate reqwest's delayed CookieStore::cookies call and response
        // Set-Cookie handling without making a network connection. Barriers
        // force a shared-jar insert between freezing the hop and sending, then
        // force response deletion before redirect admission.
        for initially_authenticated in [false, true] {
            let url = Url::parse("https://example.com/list").unwrap();
            let next = Url::parse("https://cdn.example.com/next").unwrap();
            let jar = Arc::new(reqwest::cookie::Jar::default());
            if initially_authenticated {
                jar.add_cookie_str("session=original; Secure; Path=/", &url);
            }
            let hop = Arc::new(HopCookies::new(jar.clone(), &url));
            let inserted = Arc::new(Barrier::new(2));
            let sent = Arc::new(Barrier::new(2));
            let cleared = Arc::new(Barrier::new(2));
            let worker = {
                let (jar, hop, url) = (jar.clone(), hop.clone(), url.clone());
                let (inserted, sent, cleared) = (inserted.clone(), sent.clone(), cleared.clone());
                std::thread::spawn(move || {
                    jar.add_cookie_str("session=concurrent; Secure; Path=/", &url);
                    inserted.wait();
                    sent.wait();
                    let expired = reqwest::header::HeaderValue::from_static(
                        "session=; Max-Age=0; Secure; Path=/",
                    );
                    hop.set_cookies(&mut std::iter::once(&expired), &url);
                    cleared.wait();
                })
            };
            inserted.wait();
            assert_eq!(jar.cookies(&url).unwrap(), "session=concurrent");
            let outgoing = hop.cookies(&url);
            if initially_authenticated {
                assert_eq!(outgoing.as_ref().unwrap(), "session=original");
            } else {
                assert!(
                    outgoing.is_none(),
                    "a late insert must not appear on the frozen hop"
                );
            }
            assert_eq!(outgoing.is_some(), hop.has_outgoing_cookies());
            assert!(hop.cookies(&next).is_none());
            sent.wait();
            cleared.wait();
            worker.join().unwrap();
            assert!(
                jar.cookies(&url).is_none(),
                "response cookies still update the real jar"
            );
            assert_eq!(
                hop.cookies(&url),
                outgoing,
                "response deletion cannot rewrite what was sent"
            );
            assert_eq!(
                check_redirect(
                    &url,
                    &next,
                    false,
                    hop.has_outgoing_cookies() || jar.cookies(&url).is_some()
                )
                .is_err(),
                initially_authenticated,
                "cross-origin admission must follow the actual outgoing Cookie header"
            );
        }
    }
    #[test]
    fn native_body_observer_receives_bounded_chunks_before_eof_and_preserves_final_bytes() {
        use std::sync::atomic::AtomicUsize;
        struct ObservedReader {
            bytes: std::io::Cursor<Vec<u8>>,
            reads: Arc<AtomicUsize>,
        }
        impl Read for ObservedReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.reads.fetch_add(1, Ordering::Relaxed);
                self.bytes.read(buffer)
            }
        }
        let bytes = (0..BODY_CHUNK_BYTES * 3 + 7)
            .map(|n| n as u8)
            .collect::<Vec<_>>();
        let reads = Arc::new(AtomicUsize::new(0));
        let mut reader = ObservedReader {
            bytes: std::io::Cursor::new(bytes.clone()),
            reads: reads.clone(),
        };
        let mut observed = Vec::new();
        let mut calls = 0;
        let full = read_body(
            &mut reader,
            bytes.len(),
            &CancelToken::default(),
            Some(&mut |chunk| {
                calls += 1;
                assert_eq!(
                    reads.load(Ordering::Relaxed),
                    calls,
                    "callback must run before the next body read"
                );
                assert!(chunk.len() <= BODY_CHUNK_BYTES);
                observed.extend_from_slice(chunk);
                Ok(())
            }),
        )
        .unwrap();
        assert_eq!(calls, 4);
        assert_eq!(reads.load(Ordering::Relaxed), 5);
        assert_eq!(observed, bytes);
        assert_eq!(full, bytes);
        assert_eq!(
            read_body(
                &mut std::io::Cursor::new(bytes.clone()),
                bytes.len(),
                &CancelToken::default(),
                None
            )
            .unwrap(),
            bytes
        );
    }
    #[test]
    fn native_body_observer_checks_limits_cancellation_and_transfer_errors_before_delivery() {
        let mut calls = 0;
        assert!(
            read_body(
                &mut std::io::Cursor::new(b"oversized"),
                3,
                &CancelToken::default(),
                Some(&mut |_| {
                    calls += 1;
                    Ok(())
                })
            )
            .is_err()
        );
        assert_eq!(calls, 0, "over-limit bytes must never reach the observer");
        let token = CancelToken::default();
        assert!(matches!(
            read_body(
                &mut std::io::Cursor::new(vec![1; BODY_CHUNK_BYTES * 2]),
                BODY_CHUNK_BYTES * 2,
                &token,
                Some(&mut |_| {
                    calls += 1;
                    token.cancel();
                    Ok(())
                })
            ),
            Err(SourceError::Cancelled)
        ));
        assert_eq!(calls, 1);
        struct CancelOnRead(CancelToken);
        impl Read for CancelOnRead {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                buffer[0] = 1;
                self.0.cancel();
                Ok(1)
            }
        }
        let token = CancelToken::default();
        assert!(matches!(
            read_body(
                &mut CancelOnRead(token.clone()),
                8,
                &token,
                Some(&mut |_| {
                    calls += 1;
                    Ok(())
                })
            ),
            Err(SourceError::Cancelled)
        ));
        assert_eq!(
            calls, 1,
            "cancellation during a blocking read must suppress its chunk"
        );
        struct FailingReader(bool);
        impl Read for FailingReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.0 {
                    return Err(std::io::Error::other("fixture failure"));
                }
                self.0 = true;
                buffer[0] = 7;
                Ok(1)
            }
        }
        let mut observed = Vec::new();
        assert!(matches!(
            read_body(
                &mut FailingReader(false),
                8,
                &CancelToken::default(),
                Some(&mut |chunk| {
                    observed.extend_from_slice(chunk);
                    Ok(())
                })
            ),
            Err(SourceError::Network(_))
        ));
        assert_eq!(
            observed,
            vec![7],
            "previews stay provisional when the final transfer fails"
        );
    }
}

#[cfg(target_os = "android")]
fn android_tls_config() -> Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| network("TLS versions unavailable"))
    .map(|builder| builder.with_root_certificates(roots).with_no_client_auth())
}
