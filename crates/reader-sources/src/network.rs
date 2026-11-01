use crate::{Permissions, Result, SourceError};
use reqwest::cookie::CookieStore;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    net::{IpAddr, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use url::Url;
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
}
pub(crate) struct NetworkTransport {
    permissions: Permissions,
    jar: Arc<reqwest::cookie::Jar>,
    cache_partition: String,
}
impl NetworkTransport {
    #[cfg(test)]
    pub fn new(permissions: Permissions) -> Self {
        Self::new_scoped(permissions, "test-default".into())
    }
    pub fn new_scoped(permissions: Permissions, cache_partition: String) -> Self {
        Self {
            permissions,
            jar: Arc::new(reqwest::cookie::Jar::default()),
            cache_partition,
        }
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
impl SourceTransport for NetworkTransport {
    fn page_cache_partition(&self, url: &str) -> Option<String> {
        use sha2::{Digest, Sha256};
        let url = Url::parse(url).ok()?;
        let cookies = self.jar.cookies(&url);
        let material = serde_json::to_vec(&(
            &self.cache_partition,
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
            let remaining = deadline
                .checked_sub(started.elapsed())
                .ok_or_else(|| network("request deadline exceeded"))?;
            // Keep inherited proxy configuration; direct sockets use this validated DNS snapshot.
            let builder = reqwest::blocking::Client::builder()
                .user_agent("Mozilla/5.0 (compatible; NovaReader/0.1)")
                .redirect(reqwest::redirect::Policy::none())
                .timeout(remaining)
                .connect_timeout(remaining.min(Duration::from_secs(5)))
                .cookie_provider(self.jar.clone())
                .resolve_to_addrs(host, &addresses);
            #[cfg(target_os = "android")]
            let builder = builder.tls_backend_preconfigured(android_tls_config()?);
            let client = builder.build().map_err(|_| network("HTTP client failed"))?;
            let mut request = client.request(
                if method == "POST" {
                    reqwest::Method::POST
                } else {
                    reqwest::Method::GET
                },
                url.clone(),
            );
            for (k, v) in &input.headers {
                request = request.header(k, v);
            }
            if let Some(body) = &input.body {
                request = request.body(body.clone())
            }
            let mut response = request
                .send()
                .map_err(|_| network("HTTP transfer failed"))?;
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
                if (sensitive || self.jar.cookies(&url).is_some()) && url.origin() != next.origin()
                {
                    return Err(network("sensitive cross-origin redirect denied"));
                }
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
            let mut body = Vec::new();
            let mut chunk = [0; 16384];
            loop {
                cancel.check()?;
                let n = response
                    .read(&mut chunk)
                    .map_err(|_| network("response read failed"))?;
                if n == 0 {
                    break;
                }
                if body.len() + n > max_bytes {
                    return Err(network("response byte limit"));
                }
                body.extend_from_slice(&chunk[..n]);
            }
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
            .jar
            .add_cookie_str("session=private; Path=/; Secure", &url);
        assert!(first.jar.cookies(&url).is_some());
        assert!(second.jar.cookies(&url).is_none());
        let before = second.page_cache_partition(url.as_str()).unwrap();
        second
            .jar
            .add_cookie_str("session=private; Path=/; Secure", &url);
        assert_ne!(before, second.page_cache_partition(url.as_str()).unwrap());
        let other_profile = NetworkTransport::new_scoped(permissions(), "other-profile".into());
        assert_ne!(
            before,
            other_profile.page_cache_partition(url.as_str()).unwrap()
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
