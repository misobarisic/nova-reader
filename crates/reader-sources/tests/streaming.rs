use reader_sources::{
    CancelToken, HttpRequest, HttpResponse, Result, Source, SourceError, SourceRegistry,
    SourceTransport,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::{Cursor, Write},
    sync::{Arc, Mutex},
};

fn source() -> (tempfile::TempDir, SourceRegistry, Source) {
    let manifest = json!({"formatVersion":2,"contract":"nova-reader1","api":"^1.0","id":"streaming","name":"Streaming","version":"1.0.0","language":"en","entry":"main.js","permissions":{"domains":["example.com"]},"capabilities":["resolvePage"]});
    let script = b"globalThis.novaProvider={handle(){return {pages:[{id:'page',url:'https://example.com/refreshed'}]};}};";
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("main.js", script.to_vec()),
    ] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&bytes).unwrap();
    }
    let directory = tempfile::tempdir().unwrap();
    let registry = SourceRegistry::open(directory.path()).unwrap();
    registry
        .install_zip(&zip.finish().unwrap().into_inner())
        .unwrap();
    registry.set_enabled("streaming", true).unwrap();
    let source = registry.source("streaming").unwrap();
    (directory, registry, source)
}
struct LegacyTransport {
    status: u16,
    bytes: Vec<u8>,
    seen: Mutex<Vec<String>>,
}
impl SourceTransport for LegacyTransport {
    fn request(
        &self,
        request: &HttpRequest,
        _: usize,
        token: &CancelToken,
    ) -> Result<HttpResponse> {
        token.check()?;
        self.seen.lock().unwrap().push(request.url.clone());
        Ok(HttpResponse {
            status: self.status,
            url: request.url.clone(),
            body: self.bytes.clone(),
            ..Default::default()
        })
    }
}
#[test]
fn legacy_transport_observation_is_bounded_and_preserves_complete_fetch_api() {
    let (_directory, _registry, source) = source();
    let bytes = vec![7; 16 * 1024 * 2 + 11];
    let transport = Arc::new(LegacyTransport {
        status: 200,
        bytes: bytes.clone(),
        seen: Mutex::new(vec![]),
    });
    let source = source.with_transport(transport.clone());
    let mut observed = Vec::new();
    let full = source
        .fetch_page_with_chunks(
            "https://example.com/page",
            &BTreeMap::new(),
            &CancelToken::default(),
            &mut |chunk| {
                assert!(chunk.len() <= 16 * 1024);
                observed.extend_from_slice(chunk);
            },
        )
        .unwrap();
    assert_eq!(full, bytes);
    assert_eq!(observed, bytes);
    assert_eq!(
        source
            .fetch_page(
                "https://example.com/page",
                &BTreeMap::new(),
                &CancelToken::default()
            )
            .unwrap(),
        bytes
    );
    assert_eq!(transport.seen.lock().unwrap().len(), 2);
}
#[test]
fn error_responses_permissions_and_cancelled_requests_never_emit_body_bytes() {
    let (_directory, _registry, source) = source();
    for status in [401, 403, 404, 429, 500] {
        let transport = Arc::new(LegacyTransport {
            status,
            bytes: b"private error markup".to_vec(),
            seen: Mutex::new(vec![]),
        });
        let source = source.clone().with_transport(transport.clone());
        let mut calls = 0;
        let error = source
            .fetch_page_with_chunks(
                "https://example.com/page",
                &BTreeMap::new(),
                &CancelToken::default(),
                &mut |_| {
                    calls += 1;
                },
            )
            .unwrap_err();
        assert_eq!(calls, 0);
        assert!(match status {
            401 => matches!(error, SourceError::AuthenticationRequired),
            403 => matches!(error, SourceError::ChallengeRequired),
            404 => matches!(error, SourceError::NotFound),
            429 => matches!(error, SourceError::RateLimited),
            _ => matches!(error, SourceError::Network(_)),
        });
    }
    let transport = Arc::new(LegacyTransport {
        status: 200,
        bytes: vec![1],
        seen: Mutex::new(vec![]),
    });
    let source = source.with_transport(transport.clone());
    let mut calls = 0;
    for (url, headers) in [
        ("http://example.com/page", BTreeMap::new()),
        ("https://unlisted.example/page", BTreeMap::new()),
        (
            "https://example.com/page",
            BTreeMap::from([("Cookie".into(), "secret=private".into())]),
        ),
    ] {
        assert!(
            source
                .fetch_page_with_chunks(url, &headers, &CancelToken::default(), &mut |_| {
                    calls += 1;
                })
                .is_err()
        );
    }
    let token = CancelToken::default();
    token.cancel();
    assert!(matches!(
        source.fetch_page_with_chunks(
            "https://example.com/page",
            &BTreeMap::new(),
            &token,
            &mut |_| {
                calls += 1;
            }
        ),
        Err(SourceError::Cancelled)
    ));
    assert_eq!(calls, 0);
    assert!(transport.seen.lock().unwrap().is_empty());
}

struct IncrementalTransport {
    events: Arc<Mutex<Vec<&'static str>>>,
}
impl SourceTransport for IncrementalTransport {
    fn request(&self, request: &HttpRequest, _: usize, _: &CancelToken) -> Result<HttpResponse> {
        Ok(HttpResponse {
            status: 403,
            url: request.url.clone(),
            body: b"do not preview rejected token".to_vec(),
            ..Default::default()
        })
    }
    fn request_with_chunks(
        &self,
        request: &HttpRequest,
        _: usize,
        token: &CancelToken,
        on_chunk: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<HttpResponse> {
        if !request.url.ends_with("/refreshed") {
            self.events.lock().unwrap().push("rejected");
            return self.request(request, 0, token);
        }
        self.events.lock().unwrap().push("first read");
        on_chunk(b"first")?;
        self.events.lock().unwrap().push("second read");
        on_chunk(b"second")?;
        self.events.lock().unwrap().push("complete");
        Ok(HttpResponse {
            status: 200,
            url: request.url.clone(),
            body: b"firstsecond".to_vec(),
            ..Default::default()
        })
    }
}
#[test]
fn refreshed_page_streams_before_completion_and_excludes_rejected_body() {
    let (_directory, _registry, source) = source();
    let events = Arc::new(Mutex::new(vec![]));
    let source = source.with_transport(Arc::new(IncrementalTransport {
        events: events.clone(),
    }));
    let mut observed = Vec::new();
    let full = source
        .fetch_page_with_chunks(
            "https://example.com/expired",
            &BTreeMap::new(),
            &CancelToken::default(),
            &mut |chunk| {
                events.lock().unwrap().push("observer");
                observed.extend_from_slice(chunk);
            },
        )
        .unwrap();
    assert_eq!(full, b"firstsecond");
    assert_eq!(observed, full);
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            "rejected",
            "first read",
            "observer",
            "second read",
            "observer",
            "complete"
        ]
    );
}
#[test]
fn cancellation_and_revocation_inside_observer_stop_stream_before_next_read() {
    for revoke in [false, true] {
        let (_directory, registry, source) = source();
        let events = Arc::new(Mutex::new(vec![]));
        let source = source.with_transport(Arc::new(IncrementalTransport {
            events: events.clone(),
        }));
        let token = CancelToken::default();
        let mut calls = 0;
        let result = source.fetch_page_with_chunks(
            "https://example.com/refreshed",
            &BTreeMap::new(),
            &token,
            &mut |_| {
                calls += 1;
                if revoke {
                    registry.disable("streaming").unwrap();
                } else {
                    token.cancel();
                }
            },
        );
        assert!(matches!(result, Err(SourceError::Cancelled)));
        assert_eq!(calls, 1);
        assert_eq!(*events.lock().unwrap(), vec!["first read"]);
    }
}
#[test]
fn oversized_legacy_response_is_rejected_before_preview_delivery() {
    let (_directory, _registry, source) = source();
    let source = source.with_transport(Arc::new(LegacyTransport {
        status: 200,
        bytes: vec![1; 32 * 1024 * 1024 + 1],
        seen: Mutex::new(vec![]),
    }));
    let mut calls = 0;
    assert!(matches!(
        source.fetch_page_with_chunks(
            "https://example.com/page",
            &BTreeMap::new(),
            &CancelToken::default(),
            &mut |_| {
                calls += 1;
            }
        ),
        Err(SourceError::Network(_))
    ));
    assert_eq!(calls, 0);
}

struct RevisionedTransport {
    current: Arc<std::sync::atomic::AtomicU64>,
    frozen: Option<u64>,
    change_at_completion: bool,
}
impl SourceTransport for RevisionedTransport {
    fn session_revision(&self) -> Option<u64> {
        Some(
            self.frozen
                .unwrap_or_else(|| self.current.load(std::sync::atomic::Ordering::Acquire)),
        )
    }
    fn snapshot(&self) -> Option<Arc<dyn SourceTransport>> {
        Some(Arc::new(Self {
            current: self.current.clone(),
            frozen: self.session_revision(),
            change_at_completion: self.change_at_completion,
        }))
    }
    fn request(&self, request: &HttpRequest, _: usize, _: &CancelToken) -> Result<HttpResponse> {
        Ok(HttpResponse {
            status: 200,
            url: request.url.clone(),
            body: b"complete".to_vec(),
            ..Default::default()
        })
    }
    fn request_with_chunks(
        &self,
        request: &HttpRequest,
        max: usize,
        token: &CancelToken,
        observer: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<HttpResponse> {
        observer(b"complete")?;
        if self.change_at_completion {
            self.current
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        self.request(request, max, token)
    }
}
#[test]
fn changed_session_revision_rejects_observation_and_final_completion() {
    for change_at_completion in [false, true] {
        let (_directory, _registry, source) = source();
        let revision = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let source = source.with_transport(Arc::new(RevisionedTransport {
            current: revision.clone(),
            frozen: None,
            change_at_completion,
        }));
        let mut calls = 0;
        let result = source.fetch_page_with_chunks(
            "https://example.com/page",
            &BTreeMap::new(),
            &CancelToken::default(),
            &mut |_| {
                calls += 1;
                if !change_at_completion {
                    revision.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                }
            },
        );
        assert!(matches!(result, Err(SourceError::Cancelled)));
        assert_eq!(calls, 1);
    }
}

#[test]
fn native_tighter_page_limit_reaches_transport_and_survives_refresh() {
    struct Limited {
        caps: Mutex<Vec<usize>>,
        oversized: bool,
    }
    impl SourceTransport for Limited {
        fn request(
            &self,
            request: &HttpRequest,
            cap: usize,
            _: &CancelToken,
        ) -> Result<HttpResponse> {
            self.caps.lock().unwrap().push(cap);
            Ok(HttpResponse {
                status: if request.url.ends_with("expired") {
                    403
                } else {
                    200
                },
                url: request.url.clone(),
                body: if self.oversized {
                    vec![0; cap + 1]
                } else {
                    vec![1; 4]
                },
                ..Default::default()
            })
        }
    }
    let (_directory, registry, source) = source();
    let transport = Arc::new(Limited {
        caps: Mutex::default(),
        oversized: false,
    });
    let source = source.with_transport(transport.clone());
    let token = CancelToken::default();
    assert_eq!(
        source
            .fetch_page_limited("https://example.com/expired", &BTreeMap::new(), &token, 16)
            .unwrap(),
        vec![1; 4]
    );
    assert_eq!(*transport.caps.lock().unwrap(), vec![16, 16]);
    for cap in [0, usize::MAX] {
        assert!(
            source
                .fetch_page_limited("https://example.com/page", &BTreeMap::new(), &token, cap)
                .is_err()
        );
    }
    assert!(
        source
            .fetch_page_limited("https://elsewhere.test/page", &BTreeMap::new(), &token, 16)
            .is_err()
    );
    token.cancel();
    assert!(
        source
            .fetch_page_limited("https://example.com/page", &BTreeMap::new(), &token, 16)
            .is_err()
    );
    assert_eq!(transport.caps.lock().unwrap().len(), 2);
    let oversized = source.clone().with_transport(Arc::new(Limited {
        caps: Mutex::default(),
        oversized: true,
    }));
    assert!(
        oversized
            .fetch_page_limited(
                "https://example.com/page",
                &BTreeMap::new(),
                &CancelToken::default(),
                16
            )
            .is_err()
    );
    registry.disable("streaming").unwrap();
    assert!(
        source
            .fetch_page_limited(
                "https://example.com/page",
                &BTreeMap::new(),
                &CancelToken::default(),
                16
            )
            .is_err()
    );
}

#[test]
fn native_image_policy_can_exceed_baseline_for_full_and_streamed_fetches() {
    let (_directory, _registry, source) = source();
    let bytes = vec![9; 32 * 1024 * 1024 + 1];
    let source = source.with_transport(Arc::new(LegacyTransport {
        status: 200,
        bytes: bytes.clone(),
        seen: Mutex::new(vec![]),
    }));
    let token = CancelToken::default();
    assert!(
        source
            .fetch_page("https://example.com/page", &BTreeMap::new(), &token)
            .is_err()
    );
    assert_eq!(
        source
            .fetch_page_limited(
                "https://example.com/page",
                &BTreeMap::new(),
                &token,
                bytes.len()
            )
            .unwrap(),
        bytes
    );
    let mut observed = 0;
    assert_eq!(
        source
            .fetch_page_with_chunks_limited(
                "https://example.com/page",
                &BTreeMap::new(),
                &token,
                bytes.len(),
                &mut |chunk| observed += chunk.len()
            )
            .unwrap(),
        bytes
    );
    assert_eq!(observed, bytes.len());
}
